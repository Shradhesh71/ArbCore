use std::{collections::{HashMap, VecDeque}, sync::Arc, pin::Pin, future::Future};

use orderbook::{OrderbookDelta, OrderbookSnapshot, OrderBook};
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
}

impl Default for SymbolWorkerConfig {
    fn default() -> Self {
        Self {
            max_buffered_deltas: 5_000, // conservative default
            snapshot_wait_ms: 5_000,
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
            }
            Err(e) => {
                println!("failed to apply proactive snapshot for {}: {:?}", symbol, e);
            }
        }
    } else {
        println!("no snapshot provider for {}; will wait for Snapshot message from adapter", symbol);
    }

    // main loop: process messages, buffer deltas until snapshot applied
    while let Some(msg) = rx.recv().await {
        match msg {
            SymbolMessage::Snapshot(snap) => {
                // apply snapshot and drain buffered deltas (if any) in arrival order.
                match book.apply_snapshot(snap.clone()) {
                    Ok(_) => {
                        snapshot_applied = true;
                        // drain buffered deltas
                        while let Some(delta) = buffered.pop_front() {
                            if let Err(e) = apply_delta_with_resync_if_needed(&symbol, &book, delta.clone(), &snapshot_providers).await {
                                println!("error applying buffered delta for {}: {:?} -> triggering resync", symbol, e);
                                // If applying buffered delta failed, attempt full resync and clear buffered
                                if let Some(new_snap) = fetch_snapshot_for(&snapshot_providers, &symbol).await {
                                    if let Err(err) = book.apply_snapshot(new_snap) {
                                        println!("failed to apply resync snapshot for {}: {:?}", symbol, err);
                                    } else {
                                        println!("resync snapshot applied successfully for {}", symbol);
                                    }
                                } else {
                                    println!("no snapshot provider to resync {}", symbol);
                                }
                                buffered.clear();
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        println!("apply_snapshot failed for {}: {:?}", symbol, e);
                        // If snapshot application fails, attempt resync if provider exists
                        if let Some(new_snap) = fetch_snapshot_for(&snapshot_providers, &symbol).await {
                            if let Err(err) = book.apply_snapshot(new_snap) {
                                println!("failed to apply fetched snapshot for {}: {:?}", symbol, err);
                            } else {
                                snapshot_applied = true;
                            }
                        }
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

                // Snapshot already applied: try to apply delta.
                if let Err(e) = book.apply_delta(delta.clone()) {
                    // sequence or other error -> attempt resync via provider
                    println!("apply_delta error for {}: {:?}. Attempting resync.", symbol, e);
                    // try to use snapshot provider to resync
                    if let Some(new_snap) = fetch_snapshot_for(&snapshot_providers, &symbol).await {
                        match book.apply_snapshot(new_snap.clone()) {
                            Ok(_) => {
                                println!("resync snapshot applied for {} after delta error", symbol);
                                // not guaranteed buffered deltas include current delta; try to re-apply it
                                if let Err(e2) = book.apply_delta(delta.clone()) {
                                    println!("failed to reapply delta after resync for {}: {:?}", symbol, e2);
                                }
                            }
                            Err(e3) => {
                                println!("failed to apply resync snapshot for {}: {:?}", symbol, e3);
                            }
                        }
                    } else {
                        // no snapshot provider — set resync_required flag in orderbook and continue
                        println!("no snapshot provider available for {}; marking resync_required", symbol);
                        // OrderBook has exposed resync_required atomic in previous design
                        book.resync_required.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                } else {
                    // success
                    // optionally: emit notification to subscribers / tri-view builder here
                }
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
            Err(_e) => {
                None
            }
        }
    } else {
        None
    }
}

/// Helper: attempt apply delta, and if a sequence error occurs, perform resync via snapshot provider.
/// Returns Ok(()) if delta applied (or resync succeeded and delta applied); Err otherwise.
async fn apply_delta_with_resync_if_needed(
    symbol: &str,
    book: &OrderBook,
    delta: OrderbookDelta,
    snapshot_providers: &Arc<Mutex<HashMap<String, SnapshotFn>>>,
) -> Result<(), MarketDataError> {
    match book.apply_delta(delta.clone()) {
        Ok(_) => return Ok(()),
        Err(e) => {
            println!("apply_delta returned error for {}: {:?}. Attempting resync", symbol, e);
            // attempt to fetch snapshot
            let provider_opt = {
                let mp = snapshot_providers.lock().await;
                mp.get(symbol).cloned()
            };
            if let Some(sf) = provider_opt {
                match sf(symbol.to_string()).await {
                    Ok(snap) => {
                        if let Err(e2) = book.apply_snapshot(snap) {
                            println!("apply_snapshot failed during resync for {}: {:?}", symbol, e2);
                            return Err(MarketDataError::Other(format!("resync apply_snapshot failed: {:?}", e2)));
                        }
                        // after resync, try to apply delta again
                        if let Err(e3) = book.apply_delta(delta) {
                            println!("delta reapply failed after resync for {}: {:?}", symbol, e3);
                            return Err(MarketDataError::Other(format!("delta reapply failed after resync: {:?}", e3)));
                        }
                        return Ok(());
                    }
                    Err(e_fetch) => {
                        println!("snapshot provider error for {}: {:?}", symbol, e_fetch);
                        return Err(MarketDataError::Other(format!("snapshot provider error: {:?}", e_fetch)));
                    }
                }
            } else {
                // no provider
                println!("no snapshot provider for {} — cannot resync", symbol);
                return Err(MarketDataError::Other("no snapshot provider".into()));
            }
        }
    }
}
