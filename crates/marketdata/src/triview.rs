//! TriView builder: produce consistent lightweight views of three orderbooks (triangle)
//! and publish them to subscribers.
//!
//! Design:
//! - User provides a SnapshotGetter: `Arc<dyn Fn(&str) -> BoxFuture<Result<OrderbookSnapshot, MarketDataError>>>`
//!   which returns the latest snapshot for a symbol. This keeps this module decoupled from where OrderBook
//!   instances live (manager, registry, etc).
//! - For each triangle subscription, a background task periodically fetches snapshots for the three symbols,
//!   builds a TriView (top-N), and sends it down a bounded channel to subscribers.
//! - Poll interval and top_n are configurable per-subscription.
//!
//! Wiring idea:
//! - Your per-symbol worker or orderbook registry should expose a small async getter like:
//!     `async fn get_snapshot(symbol: &str) -> Result<OrderbookSnapshot, MarketDataError>`
//!   Pass that into `TriViewBuilder::new(snapshot_getter)`.
//!
//! - Strategy crate can call `builder.subscribe_triangle(("A/B","B/C","A/C"), top_n, interval)`
//!   and receive `mpsc::Receiver<TriView>` to drive strategy decisions.

use crate::errors::MarketDataError;
use crate::types::{OrderbookSnapshot, Level};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant};

/// SnapshotGetter type: async function that returns the latest OrderbookSnapshot for a given symbol.
pub type SnapshotGetter = Arc<dyn Fn(&str) -> BoxFuture<'static, Result<OrderbookSnapshot, MarketDataError>> + Send + Sync>;

/// TriView: lightweight view for strategy. Best-first ordering:
/// - bids: best-first (descending price)
/// - asks: best-first (ascending price)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriView {
    pub sym_ab: String,
    pub bids_ab: Vec<(rust_decimal::Decimal, rust_decimal::Decimal)>, // (price, size) best-first
    pub asks_ab: Vec<(rust_decimal::Decimal, rust_decimal::Decimal)>,

    pub sym_bc: String,
    pub bids_bc: Vec<(rust_decimal::Decimal, rust_decimal::Decimal)>,
    pub asks_bc: Vec<(rust_decimal::Decimal, rust_decimal::Decimal)>,

    pub sym_ac: String,
    pub bids_ac: Vec<(rust_decimal::Decimal, rust_decimal::Decimal)>,
    pub asks_ac: Vec<(rust_decimal::Decimal, rust_decimal::Decimal)>,
}

impl TriView {
    /// convenience: create empty tri-view
    pub fn empty(a: &str, b: &str, c: &str) -> Self {
        Self {
            sym_ab: a.to_string(),
            bids_ab: vec![],
            asks_ab: vec![],
            sym_bc: b.to_string(),
            bids_bc: vec![],
            asks_bc: vec![],
            sym_ac: c.to_string(),
            bids_ac: vec![],
            asks_ac: vec![],
        }
    }
}

/// Subscription handle returned to caller
pub struct TriSubscription {
    /// receiver the caller listens on for TriView updates
    pub rx: mpsc::Receiver<TriView>,
    /// optional shutdown sender you can use to stop this subscription task
    pub shutdown: oneshot::Sender<()>,
}

/// Builder that accepts a SnapshotGetter and can create multiple triangle subscriptions.
pub struct TriViewBuilder {
    snapshot_getter: SnapshotGetter,
}

impl TriViewBuilder {
    /// Create a builder given a snapshot getter closure.
    /// Example snapshot_getter: Arc::new(move |s: &str| Box::pin(async move { orderbook_registry.get_snapshot(s).await }))
    pub fn new(snapshot_getter: SnapshotGetter) -> Self {
        Self { snapshot_getter }
    }

    /// Subscribe to a triangle of three symbols.
    /// - `sym_ab`, `sym_bc`, `sym_ac` strings must match how snapshots are requested (adapter symbols).
    /// - `top_n` - number of levels per side to include (best-first).
    /// - `interval` - how often to poll snapshots (Duration).
    /// Returns a TriSubscription containing a Receiver<TriView>.
    pub fn subscribe_triangle(
        &self,
        sym_ab: String,
        sym_bc: String,
        sym_ac: String,
        top_n: usize,
        interval: Duration,
    ) -> TriSubscription {
        let (tx, rx) = mpsc::channel::<TriView>(64);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let getter = self.snapshot_getter.clone();

        // spawn background task
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // ensure first tick runs immediately
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let _last_emit = Instant::now() - interval;

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        // fetch three snapshots in parallel
                        let g = getter.clone();
                        let a_fut = (g)(&sym_ab);
                        let b_fut = (g)(&sym_bc);
                        let c_fut = (g)(&sym_ac);

                        // run concurrently
                        let (a_res, b_res, c_res) = tokio::join!(a_fut, b_fut, c_fut);

                        match (a_res, b_res, c_res) {
                            (Ok(a_snap), Ok(b_snap), Ok(c_snap)) => {
                                // build tri view picking top_n levels
                                let tv = build_triview_from_snapshots(&sym_ab, &a_snap, &sym_bc, &b_snap, &sym_ac, &c_snap, top_n);
                                
                                // Log TriView statistics periodically
                                static TRIVIEW_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                                let count = TRIVIEW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                if count % 10 == 0 {
                                    println!("📈 TriView #{}: {} bids_ab={} asks_ab={}, {} bids_bc={} asks_bc={}, {} bids_ac={} asks_ac={}",
                                        count,
                                        tv.sym_ab, tv.bids_ab.len(), tv.asks_ab.len(),
                                        tv.sym_bc, tv.bids_bc.len(), tv.asks_bc.len(),
                                        tv.sym_ac, tv.bids_ac.len(), tv.asks_ac.len());
                                }
                                
                                // simple backpressure: don't spam if receiver is slow; use try_send fallback
                                match tx.try_send(tv.clone()) {
                                    Ok(_) => { }
                                    Err(_e) => {
                                        // if full, attempt send with timeout short
                                        let _sent = false;
                                        let send_fut = tx.send(tv);
                                        match tokio::time::timeout(Duration::from_millis(100), send_fut).await {
                                            Ok(Ok(_)) => { }
                                            _ => {
                                                // drop this tick; log at debug
                                                tracing::debug!("TriViewBuilder: subscriber slow, dropping tick for {}-{}-{}", sym_ab, sym_bc, sym_ac);
                                            }
                                        }
                                    }
                                }
                            }
                            (ea, eb, ec) => {
                                tracing::debug!("TriViewBuilder: snapshot fetch errors: {:?}, {:?}, {:?}", ea.err(), eb.err(), ec.err());
                                // continue — next tick will try again
                            }
                        }
                    }

                    // allow external shutdown
                    _ = &mut shutdown_rx => {
                        tracing::info!("TriViewBuilder: shutdown requested for {}-{}-{}", sym_ab, sym_bc, sym_ac);
                        break;
                    }
                }
            }

            tracing::info!("TriViewBuilder: subscription task ended for {}-{}-{}", sym_ab, sym_bc, sym_ac);
        });

        TriSubscription { rx, shutdown: shutdown_tx }
    }
}

/// Build a TriView from three OrderbookSnapshot objects.
/// - Uses top_n levels per side; expects the snapshots' bids/asks ordering is best-first as described in orderbook::types.
fn build_triview_from_snapshots(
    sym_ab: &str,
    snap_ab: &OrderbookSnapshot,
    sym_bc: &str,
    snap_bc: &OrderbookSnapshot,
    sym_ac: &str,
    snap_ac: &OrderbookSnapshot,
    top_n: usize,
) -> TriView {
    // helpers to convert Level -> (price, size)
    fn take_top(levels: &Vec<Level>, top_n: usize) -> Vec<(rust_decimal::Decimal, rust_decimal::Decimal)> {
        use rust_decimal::Decimal;
        // orderbook::types expects caller to provide best-first ordering:
        // - bids: best-first descending
        // - asks: best-first ascending
        // We will take slice up to top_n and map to tuples.
        let mut out: Vec<(Decimal, Decimal)> = vec![];
        for (i, lvl) in levels.iter().enumerate() {
            if i >= top_n { break; }
            out.push((lvl.price, lvl.size));
        }
        out
    }

    let bids_ab = take_top(&snap_ab.bids, top_n);
    let asks_ab = take_top(&snap_ab.asks, top_n);
    let bids_bc = take_top(&snap_bc.bids, top_n);
    let asks_bc = take_top(&snap_bc.asks, top_n);
    let bids_ac = take_top(&snap_ac.bids, top_n);
    let asks_ac = take_top(&snap_ac.asks, top_n);

    TriView {
        sym_ab: sym_ab.to_string(),
        bids_ab,
        asks_ab,
        sym_bc: sym_bc.to_string(),
        bids_bc,
        asks_bc,
        sym_ac: sym_ac.to_string(),
        bids_ac,
        asks_ac,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OrderbookSnapshot, Level};

    // helper snapshot generator
    fn make_snapshot(sym: &str, bids: Vec<(f64,f64)>, asks: Vec<(f64,f64)>) -> OrderbookSnapshot {
        use rust_decimal::Decimal;
        use std::str::FromStr;
        OrderbookSnapshot {
            symbol: sym.to_string(),
            bids: bids.into_iter().map(|(p,s)| Level { 
                price: Decimal::from_str(&p.to_string()).unwrap_or_default(), 
                size: Decimal::from_str(&s.to_string()).unwrap_or_default() 
            }).collect(),
            asks: asks.into_iter().map(|(p,s)| Level { 
                price: Decimal::from_str(&p.to_string()).unwrap_or_default(), 
                size: Decimal::from_str(&s.to_string()).unwrap_or_default() 
            }).collect(),
            sequence: None,
            ts: None,
        }
    }

    #[tokio::test]
    async fn triview_builder_emits() {
        // implement a trivial snapshot getter that returns the same snapshot
        let snap_ab = make_snapshot("A/B", vec![(2.0, 10.0)], vec![(3.0, 5.0)]);
        let snap_bc = make_snapshot("B/C", vec![(100.0, 1.0)], vec![(101.0, 2.0)]);
        let snap_ac = make_snapshot("A/C", vec![(200.0, 1.0)], vec![(201.0, 2.0)]);

        let getter: SnapshotGetter = Arc::new(move |s: &str| {
            let a = snap_ab.clone();
            let b = snap_bc.clone();
            let c = snap_ac.clone();
            let s = s.to_string();
            Box::pin(async move {
                match s.as_str() {
                    "A/B" => Ok(a),
                    "B/C" => Ok(b),
                    "A/C" => Ok(c),
                    _ => Err(MarketDataError::Other("unknown".into())),
                }
            })
        });

        let builder = TriViewBuilder::new(getter);
        let sub = builder.subscribe_triangle("A/B".into(), "B/C".into(), "A/C".into(), 1, Duration::from_millis(50));
        let mut rx = sub.rx;
        // wait a tick for emission
        let tv = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await.expect("should receive").expect("some");
        assert_eq!(tv.sym_ab, "A/B");
        assert_eq!(tv.bids_ab.len(), 1);
    }
}
