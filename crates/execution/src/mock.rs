use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, oneshot};
use tokio::time::{sleep, Duration};
use tracing::{debug, info};

use strategy::engine::{
    TradePlan,
    ExecutionReport,
    ExecStatus,
    CancelRequest,
};

/// Spawn a mock execution worker.
///
/// - `plan_rx` receives TradePlan objects from StrategyEngine.exec_tx
/// - `exec_resp_tx` is where ExecutionReport objects will be sent (StrategyEngine.exec_resp_rx)
/// - `exec_cancel_rx` receives CancelRequest objects from StrategyEngine.exec_cancel_tx
///
/// Behavior:
/// - For taker legs (`is_taker = true`): simulate full fill after a small delay (e.g., 20 ms)
/// - For passive legs: simulate partial fill / unfilled after a longer delay
/// - When a cancel is received, send Cancelled reports for remaining legs and stop that plan.
pub fn spawn_mock_execution(
    mut plan_rx: mpsc::Receiver<TradePlan>,
    exec_resp_tx: mpsc::Sender<ExecutionReport>,
    mut exec_cancel_rx: mpsc::Receiver<CancelRequest>,
) -> tokio::task::JoinHandle<()> {
    // plan_id -> cancel signal channel
    let cancel_map: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let cancel_map_for_canceller = cancel_map.clone();
    let exec_resp_tx_for_canceller = exec_resp_tx.clone();

    // Task: listen for cancel requests and signal per-plan workers
    tokio::spawn(async move {
        while let Some(cancel) = exec_cancel_rx.recv().await {
            let plan_id = cancel.plan_id;
            let mut map = cancel_map_for_canceller.lock().await;
            if let Some(tx) = map.remove(&plan_id) {
                let _ = tx.send(());
                debug!("execution::mock: signalled cancel for plan {}", plan_id);
            } else {
                debug!("execution::mock: cancel for unknown plan {}", plan_id);
            }

            // Optionally, you could emit a plan-level cancelled report here if you want.
            // For now, per-leg workers send their own Cancelled reports.
            let _ = &exec_resp_tx_for_canceller;
        }
    });

    // Main execution loop: accept trade plans and spin per-plan workers
    tokio::spawn(async move {
        while let Some(plan) = plan_rx.recv().await {
            let resp_tx = exec_resp_tx.clone();
            let cancel_map_inner = cancel_map.clone();

            tokio::spawn(async move {
                let (cancel_tx, mut cancel_rx_oneshot) = oneshot::channel();
                {
                    let mut map = cancel_map_inner.lock().await;
                    map.insert(plan.id.clone(), cancel_tx);
                }

                info!("execution::mock: received plan {}", plan.id);

                // Simulate each leg in order
                for leg in plan.legs.iter() {
                    // Check cancel before starting this leg
                    if cancel_rx_oneshot.try_recv().is_ok() {
                        let report = ExecutionReport {
                            plan_id: plan.id.clone(),
                            leg_idx: leg.leg_idx,
                            filled_qty: Decimal::ZERO,
                            remaining_qty: leg.qty,
                            status: ExecStatus::Cancelled,
                            timestamp: std::time::Instant::now(),
                        };
                        let _ = resp_tx.send(report).await;
                        continue;
                    }

                    if leg.is_taker {
                        // Taker -> assume fast full fill
                        sleep(Duration::from_millis(20)).await;

                        let report = ExecutionReport {
                            plan_id: plan.id.clone(),
                            leg_idx: leg.leg_idx,
                            filled_qty: leg.qty,
                            remaining_qty: Decimal::ZERO,
                            status: ExecStatus::Filled,
                            timestamp: std::time::Instant::now(),
                        };
                        let _ = resp_tx.send(report).await;
                    } else {
                        // Passive -> simulate partial or no fill
                        tokio::select! {
                            _ = sleep(Duration::from_millis(200)) => {
                                // Here we simulate no fill / partial fill.
                                // For now: 0 filled, all remaining, PartiallyFilled.
                                let report = ExecutionReport {
                                    plan_id: plan.id.clone(),
                                    leg_idx: leg.leg_idx,
                                    filled_qty: Decimal::ZERO,
                                    remaining_qty: leg.qty,
                                    status: ExecStatus::PartiallyFilled,
                                    timestamp: std::time::Instant::now(),
                                };
                                let _ = resp_tx.send(report).await;
                            }
                            _ = &mut cancel_rx_oneshot => {
                                let report = ExecutionReport {
                                    plan_id: plan.id.clone(),
                                    leg_idx: leg.leg_idx,
                                    filled_qty: Decimal::ZERO,
                                    remaining_qty: leg.qty,
                                    status: ExecStatus::Cancelled,
                                    timestamp: std::time::Instant::now(),
                                };
                                let _ = resp_tx.send(report).await;
                            }
                        }
                    }
                }

                // cleanup
                let mut map = cancel_map_inner.lock().await;
                map.remove(&plan.id);
                debug!("execution::mock: plan {} finished", plan.id);
            });
        }

        debug!("execution::mock: plan_rx closed, exiting");
    })
}
