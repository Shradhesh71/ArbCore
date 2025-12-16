use std::{collections::{HashMap, VecDeque}, sync::Arc, pin::Pin, future::Future};

use orderbook::{OrderbookDelta, OrderbookSnapshot, OrderBook, OrderBookError};
use tokio::sync::{Mutex, mpsc};

use crate::errors::MarketDataError;

/// Message type adapters/manager forward into per-symbol worker.
/// - Snapshot: a REST snapshot (full state)
/// - Delta: incremental update (ws delta)
#[derive(Debug, Clone)]
pub enum SymbolMessage {
    Snapshot(OrderbookSnapshot),
    Delta(OrderbookDelta),
}

/// Producer sends normalized updates for a symbol to the per-symbol sequencer.
/// Typically adapters will send parsed deltas into these channels.
pub type SymbolUpdateTx = mpsc::Sender<SymbolMessage>;
pub type SymbolUpdateRx = mpsc::Receiver<SymbolMessage>;

/// Snapshot provider type:
/// an async function that takes symbol (String) and returns an OrderbookSnapshot.
/// Use Pin<Box<dyn Future>> to allow storing it as an Arc<Fn(...)>.
pub type SnapshotFn = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<OrderbookSnapshot, MarketDataError>> + Send>> + Send + Sync>;

// Per-symbol worker configuration
#[derive(Debug, Clone)]
pub struct SymbolWorkerConfig {
    /// maximum number of deltas to buffer before forcing resync
    pub max_buffered_deltas: usize,
    /// maximum time to wait for a snapshot after worker creation (ms)
    pub snapshot_wait_ms: u64,
    /// maximum acceptable sequence gap before forcing resync (0 = strict, >0 = tolerant)
    /// For high-frequency trading, recommend 10-50 to avoid unnecessary REST calls
    pub max_acceptable_sequence_gap: u64,
}

impl Default for SymbolWorkerConfig {
    fn default() -> Self {
        Self {
            max_buffered_deltas: 5_000,
            snapshot_wait_ms: 5_000,
            max_acceptable_sequence_gap: 1000, // Accept gaps up to 100 for low latency (Binance is fast)
        }
    }
}

/// High-level manager that wires adapters -> per-symbol workers -> orderbook
pub struct MarketDataManager {
    /// map symbol -> sender for that symbol's sequencer
    symbol_senders: Arc<tokio::sync::Mutex<HashMap<String, SymbolUpdateTx>>>,
    /// optional per-symbol snapshot provider map (symbol -> SnapshotFn). If available, used for resync.
    snapshot_providers: Arc<Mutex<HashMap<String, SnapshotFn>>>,
    /// common worker config
    cfg: SymbolWorkerConfig,
}

impl MarketDataManager {
    pub fn new() -> Self {
        Self {
            symbol_senders: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            snapshot_providers: Arc::new(Mutex::new(HashMap::new())),
            cfg: SymbolWorkerConfig::default()
        }
    }

    /// Ensure a per-symbol worker exists. Returns a Sender<SymbolMessage> for the given symbol.
    /// Optionally provide a snapshot_fn which will be used on resync requests.
    ///
    /// If a worker already exists, the existing sender is returned and the snapshot_fn is stored (overrides previous).
    pub async fn ensure_symbol_with_snapshot_provider(
        &self,
        symbol: &str,
        snapshot_fn: Option<SnapshotFn>,
    ) -> Result<SymbolUpdateTx, MarketDataError> {
        // / fast path: check existing
        {
            let senders = self.symbol_senders.lock().await;
            if let  Some(tx) = senders.get(symbol) {
                // store/override snapshot provider if supplied
                if let Some(sf) = snapshot_fn {
                    let mut providers = self.snapshot_providers.lock().await;
                    providers.insert(symbol.to_string(), sf);
                }
                return Ok(tx.clone());
            }
        }

         // create channel for this symbol and spawn worker
        let (tx, rx) = mpsc::channel::<SymbolMessage>(2048);
        {
            let mut senders = self.symbol_senders.lock().await;
            senders.insert(symbol.to_string(), tx.clone());
        }
        {
            // store snapshot provider if supplied
            if let Some(sf) = snapshot_fn {
                let mut providers = self.snapshot_providers.lock().await;
                providers.insert(symbol.to_string(), sf);
            }
        }
        //  spawn per-symbol worker 
        let sym = symbol.to_string();
        let cfg = self.cfg.clone();
        let providers = self.snapshot_providers.clone();
        tokio::spawn(async move {
            if let Err(e) = per_symbol_worker(sym.clone(), rx, providers, cfg).await {
                println!("symbol worker {} terminated with error: {:?}", sym, e);
            } else {
                println!("symbol worker {} ended normally", sym);
            }
        });

        Ok(tx)
    }

    pub async fn ensure_symbol(&self, symbol: &str) -> Result<SymbolUpdateTx, MarketDataError> {
        self.ensure_symbol_with_snapshot_provider(symbol, None).await
    }

    /// Get the sender for a specific symbol (used by adapter wiring)
    pub async fn get_sender(&self, symbol: &str) -> Option<SymbolUpdateTx> {
        let senders = self.symbol_senders.lock().await;
        senders.get(symbol).cloned()
    }
}

/// The per-symbol worker: single writer for orderbook state, ensures sequence correctness.
/// It expects callers to send SymbolMessage::Snapshot for initial state or a snapshot provider to exist.
/// Buffer deltas until a snapshot is applied. On sequence gap -> attempt resync via snapshot provider.
async fn per_symbol_worker(
    symbol: String,
    mut rx: SymbolUpdateRx,
    snapshot_providers: Arc<Mutex<HashMap<String, SnapshotFn>>>,
    cfg: SymbolWorkerConfig
) -> Result<(), MarketDataError> {
    let book = OrderBook::new(&symbol);

    // buffer of deltas received before snapshot applied
    let mut buffered: VecDeque<OrderbookDelta> = VecDeque::with_capacity(1024);
    let mut snapshot_applied = false;

    // If first incoming messages are deltas, buffer them until snapshot_or_timeout.
    // We'll also proactively fetch a snapshot immediately if provider exists.
    // If provider not present, we rely on adapters to send a Snapshot message.

    // If provider exists, try to fetch initial snapshot proactively (best-effort)
    if let Some(snap) = fetch_snapshot_for(&snapshot_providers, &symbol).await {
        match book.apply_snapshot(snap.clone()) {
            Ok(_) => {
                snapshot_applied = true;
                println!("{} snapshot initialized (seq={:?})", symbol, snap.sequence);
            }
            Err(e) => {
                println!("{} failed to apply initial snapshot: {:?}", symbol, e);
            }
        }
    } else {
        println!("{} waiting for snapshot from adapter", symbol);
    }

    // main loop: process messages, buffer deltas until snapshot applied
    while let Some(msg) = rx.recv().await {
        match msg {
            SymbolMessage::Snapshot(snap) => {
                // apply snapshot and clear buffered deltas (they're likely stale)
                match book.apply_snapshot(snap.clone()) {
                    Ok(_) => {
                        snapshot_applied = true;
                        // Clear buffered deltas - they're likely too old to apply after snapshot
                        if !buffered.is_empty() {
                            buffered.clear();
                        }
                    }
                    Err(_e) => {
                        // Snapshot failed, just continue and wait for next update
                        buffered.clear();
                    }
                }
            }

            SymbolMessage::Delta(delta ) => {
                // If snapshot not yet applied, buffer delta (with cap)
                if !snapshot_applied {
                    if buffered.len() >= cfg.max_buffered_deltas {
                        buffered.clear();
                        // try resync proactively
                        if let Some(new_snap) = fetch_snapshot_for(&snapshot_providers, &symbol).await {
                            if let Err(err) = book.apply_snapshot(new_snap) {
                                println!("failed to apply resync snapshot for {}: {:?}", symbol, err);
                            } else {
                                snapshot_applied = true;
                            }
                        }
                    } else {
                        buffered.push_back(delta);
                    }
                    continue;
                }

                // Snapshot already applied: try to apply delta with gap tolerance
                if let Err(e) = book.apply_delta_with_gap_tolerance(delta.clone(), cfg.max_acceptable_sequence_gap) {
                    // Large sequence gap detected, need resync
                    if cfg.max_acceptable_sequence_gap > 0 {
                        // Log only when we actually need to resync (rare with gap tolerance)
                        if let OrderBookError::SequenceError { expected, got } = e {
                            let gap = if got > expected { got - expected } else { expected - got };
                            println!("{} large sequence gap detected: {} (threshold: {}), resyncing...", 
                                symbol, gap, cfg.max_acceptable_sequence_gap);
                        }
                    }
                    
                    // Fetch fresh snapshot only for large gaps
                    if let Some(new_snap) = fetch_snapshot_for(&snapshot_providers, &symbol).await {
                        let _ = book.apply_snapshot(new_snap.clone());
                    } else {
                        book.resync_required.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                // Continue processing - small gaps are handled transparently
            }
        }
    }
    Ok(())
}

// Helper: find snapshot provider if any
async fn fetch_snapshot_for(
    providers: &Arc<Mutex<HashMap<String, SnapshotFn>>>,
    symbol: &str,
) -> Option<OrderbookSnapshot> {
    let mp = providers.lock().await;
    if let Some(sf) = mp.get(symbol) {
        // call snapshot fn
        match sf(symbol.to_string()).await {
            Ok(snap) => Some(snap),
            Err(_e) => None
        }
    } else {
        None
    }
}
