// use std::sync::Arc;

// use futures::future::BoxFuture;
// use orderbook::OrderbookSnapshot;
// use tracing::{debug, error, info};

// use crate::errors::MarketDataError;


// /// SnapshotFn matches manager::SnapshotFn: Arc<dyn Fn(String) -> BoxFuture<'static, Result<OrderbookSnapshot, MarketDataError>> + Send + Sync>
// pub type SnapshotFn = Arc<dyn Fn(String) -> BoxFuture<'static, Result<OrderbookSnapshot, MarketDataError>> + Send + Sync>;

// // TODO: Uncomment when adapters crate is implemented
// /*
// /// Build a SnapshotFn from a boxed/clonable adapter reference.
// /// Accepts an `Arc<A>` where A: ExchangeAdapter + 'static + Send + Sync.
// ///
// /// Example:
// ///   let snapshot_fn = snapshot_fn_from_adapter(Arc::new(binance_adapter));
// pub fn snapshot_fn_from_adapter<A>(adapter: Arc<A>) -> SnapshotFn 
// where 
//     A: ExchangeAdapter + ?Sized + Send + Sync + 'static,
// {
//     Arc::new(move |symbol: String| {
//         let a = adapter.clone();
//         Box::pin(async move {
//             // adapter::get_snapshot returns Result<OrderbookSnapshot, ExchangeError> in your adapter impl.
//             // Map adapter errors to MarketDataError.
//             match a.get_snapshot(&symbol).await {
//                 Ok(snap) => {
//                     debug!("snapshot_fn_from_adapter: fetched snapshot for {}", symbol);
//                     Ok(snap)
//                 }
//                 Err(e) => {
//                     error!("snapshot_fn_from_adapter: adapter get_snapshot error for {}: {:?}", symbol, e);
//                     Err(MarketDataError::Other(format!("adapter get_snapshot error: {:?}", e)))
//                 }
//             }
//         })
//     })    
// }
// */


// /// Convenience helper used by per-symbol worker to attempt a resync once via the provided SnapshotFn.
// /// Returns Ok(true) if resync applied, Ok(false) if no provider existed, Err if provider exists but failed.
// pub async fn resync_once(
//     symbol: &str,
//     provider_opt: Option<SnapshotFn>,
// ) -> Result<bool, MarketDataError> {
//     if let Some(provider) = provider_opt {
//         match provider(symbol.to_string()).await {
//             Ok(snap) => {
//                 // caller must apply snapshot to the orderbook
//                 info!("resync_once: fetched snapshot for {}", symbol);
//                 // return snapshot to caller via Ok(true) — but we can't apply here because worker holds the OrderBook instance.
//                 // To keep interface simple, return true and attach the snapshot via a pattern where manager obtains it directly.
//                 // But for this helper, we just signal success (the manager supplied the provider and can call provider itself).
//                 // In practice, per_symbol_worker used snapshot_fn directly, so this is mostly a convenience wrapper.
//                 let _ = snap; // placeholder hint - actual code path in per_symbol_worker called provider directly
//                 Ok(true)
//             }
//             Err(e) => {
//                 error!("resync_once: provider failed for {}: {:?}", symbol, e);
//                 Err(e)
//             }
//         }
//     } else {
//         debug!("resync_once: no snapshot provider for {}", symbol);
//         Ok(false)
//     }
// }