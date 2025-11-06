use std::{collections::HashMap, sync::{Arc, atomic::{AtomicU64, Ordering}}, time::{Duration, Instant}};
use rust_decimal_macros::dec;
use parking_lot::Mutex;
use rust_decimal::Decimal;
use tokio::sync::mpsc;

use crate::{Opportunity, StrategyConfig, TriView, detect_triangular_opportunities, planner::build_trade_plan};

#[derive(Debug, Clone)]
pub struct OrderLeg {
    pub symbol: String,
    pub buy_base: bool,
    pub price_hint: Decimal,
    pub qty: Decimal,
    pub is_taker: bool,
    pub leg_idx: u8, // index in plan for correlating reports
}

#[derive(Debug, Clone)]
pub struct TradePlan {
    pub id: String,
    pub start_currency: String,
    pub estimated_profit: Decimal,
    pub estimated_profit_pct: Decimal,
    pub legs: Vec<OrderLeg>,
    pub created_ts: Instant,
}

/// Execution report coming from execution subsystem (one per leg or aggregated)
#[derive(Debug, Clone)]
pub enum ExecStatus {
    New,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
    Error(String),
}

/// Execution report for a single leg (or plan-level aggregated)
#[derive(Debug, Clone)]
pub struct ExecutionReport {
    pub plan_id: String,
    pub leg_idx: u8,
    pub filled_qty: Decimal,
    pub remaining_qty: Decimal,
    pub status: ExecStatus,
    pub timestamp: Instant,
}

/// Cancel request for execution subsystem
#[derive(Debug, Clone)]
pub struct CancelRequest {
    pub plan_id: String,
    pub leg_idx: Option<u8>, // none -> cancel all legs of plan
}

/// In-flight plan tracking (stateful)
struct InFlightPlan {
    plan: TradePlan,
    /// per-leg filled quantity
    filled: Vec<Decimal>,
    /// when the plan was submitted (used for timeouts)
    submitted_at: Instant,
    /// per-plan timeout in milliseconds
    timeout_ms: u64,
}

/// Lightweight metrics (atomic counters)
#[derive(Debug, Default)]
pub struct StrategyMetrics {
    pub opportunities_seen: AtomicU64,
    pub plans_created: AtomicU64,
    pub plans_submitted: AtomicU64,
    pub plans_executed: AtomicU64,
    pub plans_cancelled: AtomicU64,
    pub plans_rejected: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct RiskCheckRequest {
    pub plan: TradePlan,
}

/// Result of risk check (simple)
#[derive(Debug, Clone)]
pub struct RiskCheckResult {
    pub approved: bool,
    pub reason: Option<String>,
}

/// Channel type aliases (adjust buffer sizes as needed)
pub type TriRx = mpsc::UnboundedReceiver<TriView>;
pub type ExecTx = mpsc::UnboundedSender<TradePlan>;
pub type ExecRespRx = mpsc::UnboundedReceiver<ExecutionReport>;
pub type ExecCancelTx = mpsc::UnboundedSender<CancelRequest>;
pub type RiskTx = mpsc::UnboundedSender<RiskCheckRequest>;
pub type RiskRespRx = mpsc::UnboundedReceiver<RiskCheckResult>;


/// StrategyEngine wiring
// #[allow(dead_code)]
pub struct StrategyEngine {
    pub cfg: StrategyConfig,
    exec_tx: ExecTx,
    exec_cancel_tx: ExecCancelTx,
    // risk_tx: Option<RiskTx>,
    /// in-flight plans (protected by mutex)
    inflight: Arc<Mutex<HashMap<String, InFlightPlan>>>,
    pub metrics: Arc<StrategyMetrics>,
}

impl StrategyEngine {
    /// Create engine. All channels are expected to be created by caller (wiring to execution / risk).
    pub fn new(
        cfg: StrategyConfig,
        exec_tx: ExecTx,
        exec_cancel_tx: ExecCancelTx,
        // risk_tx: Option<RiskTx>,
    ) -> Self {
        Self {
            cfg,
            exec_tx,
            exec_cancel_tx,
            // risk_tx,
            inflight: Arc::new(Mutex::new(HashMap::new())),
            metrics: Arc::new(StrategyMetrics::default()),
        }
    }

    /// Start engine main loop(s). Spawns two tasks:
    /// - detector loop: reads tri_rx, runs detector, prepares plans and submits
    /// - exec monitor loop: reads exec_resp_rx and updates in-flight plans
    ///
    /// The function returns immediately; loops run in background until tri_rx/execution channels close.
    pub fn start(self, tri_rx: TriRx, exec_resp_rx: ExecRespRx) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
        let me = Arc::new(self);
        
        let me_clone = me.clone();
        // detector task
        let detector_handle = tokio::spawn(async move {
            me_clone.detector_loop(tri_rx).await;
        });

        let me2 = me.clone();
        // execution response monitor
        let monitor_handle = tokio::spawn(async move {
            me2.execution_monitor_loop(exec_resp_rx).await;
        });
        
        (detector_handle, monitor_handle)
    }

    /// Detector loop: receive TriView ticks, detect, pre-check, plan, optional risk check, submit
    async fn detector_loop(self: Arc<Self>, mut tri_rx: TriRx) {
        let cfg = &self.cfg;
        let fee_map = &cfg.fee_map;
        let _depth_factor = cfg.limits.depth_fill_factor;
        let _min_profit_abs = cfg.limits.min_profit_abs;
        let max_concurrent = cfg.max_concurrent_plans;

        while let Some(view) = tri_rx.recv().await {
            self.metrics.opportunities_seen.fetch_add(1, Ordering::Relaxed);

            // decide start_amount (use configured max_notional or a sane default)
            let start_amount = if cfg.limits.max_notional > Decimal::ZERO {
                cfg.limits.max_notional
            } else {
                dec!(100) // fallback start amount (quote currency)
            };

            // pure detection
            let ops = detect_triangular_opportunities(&view, fee_map, start_amount );

            if ops.is_empty() {
                // debug!("no opportunities");
                continue;
            }

            // iterate ops (small list)
            for op in ops {
                // concurrency check
                let infl_count = {
                    let infl = self.inflight.lock();
                    infl.len()
                };
                if infl_count >= max_concurrent {
                    self.metrics.plans_rejected.fetch_add(1, Ordering::Relaxed);
                    // debug!("rejecting plan because inflight {} >= max {}", infl_count, max_concurrent);
                    continue;
                }

                // pre-trade checks done in helper: (profit thresholds, exposure)
                if !self.pre_trade_checks(&op).await {
                    self.metrics.plans_rejected.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                // convert to TradePlan (rounding using lot rules)
                let plan = match build_trade_plan(&op, &self.cfg) {
                    Ok(plan) => plan,
                    Err(_) => {
                        self.metrics.plans_rejected.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };

                // optional risk check: for now, skip the complex risk checking
                let risk_ok = true; // TODO: implement proper risk checking

                if !risk_ok {
                    self.metrics.plans_rejected.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                // register inflight plan BEFORE submission
                let mut infl = self.inflight.lock();
                let timeout_ms = self.cfg.order_timeout_ms;
                let infl_plan = InFlightPlan {
                    filled: vec![Decimal::ZERO; plan.legs.len()],
                    plan: plan.clone(),
                    submitted_at: Instant::now(),
                    timeout_ms,
                };
                infl.insert(plan.id.clone(), infl_plan);
                drop(infl);

                // submit to execution (unbounded send)
                match self.exec_tx.send(plan.clone()) {
                    Ok(_) => {
                        self.metrics.plans_submitted.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        // remove inflight
                        let mut infl = self.inflight.lock();
                        infl.remove(&plan.id);
                        self.metrics.plans_rejected.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }

    }

    /// Basic pre-trade checks: profit thresholds, simple concurrency (most checks done earlier)
    async fn pre_trade_checks(&self, op: &Opportunity) -> bool {
        if op.estimated_profit <= self.cfg.limits.min_profit_abs {
            return false;
        }
        if self.cfg.limits.min_profit_pct > Decimal::ZERO {
            // Calculate start amount from the first leg quantity and implied price
            let start_amount = if let Some(first_leg) = op.legs.first() {
                first_leg.qty * op.implied_price
            } else {
                return false; // No legs available
            };
            let profit_pct = op.estimated_profit / start_amount;
            if profit_pct < self.cfg.limits.min_profit_pct {
                return false;
            }
        }
        true
    }

    pub async fn list_inflight(&self) -> Vec<(String, usize, u64)> { // returns (plan_id, legs_count, age_ms)
        let now = Instant::now();
        let infl = self.inflight.lock();
        infl.iter().map(|(id, p)| {
            let age = now.duration_since(p.submitted_at).as_millis() as u64;
            (id.clone(), p.plan.legs.len(), age)
        }).collect()
    }

    pub async fn remove_inflight(&self, plan_id: &str) {
        let mut infl = self.inflight.lock();
        if infl.remove(plan_id).is_some() {
            self.metrics.plans_executed.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Scan for timed-out plans and cancel them
    async fn scan_timeouts(&self) {
        let now = Instant::now();
        let mut to_cancel: Vec<String> = Vec::new();
        {
            let infl = self.inflight.lock();
            for (plan_id, p) in infl.iter() {
                let elapsed = now.duration_since(p.submitted_at);
                if elapsed.as_millis() as u64 > p.timeout_ms {
                    to_cancel.push(plan_id.clone());
                }
            }
        }

        for plan_id in to_cancel {
            let _ = self.exec_cancel_tx.send(CancelRequest { plan_id: plan_id.clone(), leg_idx: None });
            let mut infi = self.inflight.lock();
            if infi.remove(&plan_id).is_some() {
                self.metrics.plans_cancelled.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

     /// Handle a single execution report
    async fn handle_exec_report(&self, report: ExecutionReport) {
        let plan_id = &report.plan_id;
        let mut remove_plan = false;
        {
            let mut infl = self.inflight.lock();
            if let Some(p) = infl.get_mut(plan_id) {
                let idx = report.leg_idx as usize;
                if idx < p.filled.len() {
                    p.filled[idx] = p.filled[idx].saturating_add(report.filled_qty);
                }

                match report.status {
                    ExecStatus::Filled => {
                        let all_legged = p.filled.iter().zip(p.plan.legs.iter()).all(|(filled, leg)| *filled >= leg.qty);
                        if all_legged {
                            remove_plan = true;
                        }
                    }
                    ExecStatus::Cancelled => {
                        remove_plan = true;  // consider plan finished (for simplicity)
                    }
                    ExecStatus::Rejected => {
                        let _ = self.exec_cancel_tx.send(CancelRequest { plan_id: plan_id.clone(), leg_idx: None });
                        remove_plan = true;  // cancel remaining legs proactively
                    }
                    ExecStatus::PartiallyFilled => {
                        // decide policy: here we choose to cancel remaining legs and mark plan for reconciliation
                        let _ = self.exec_cancel_tx.send(CancelRequest { plan_id: plan_id.clone(), leg_idx: None });
                        remove_plan = true;
                    }
                    ExecStatus::New => {
                        // nothing to do
                    }
                    ExecStatus::Error(_) => {},
                }
            }
            if remove_plan {
                infl.remove(plan_id);
            }
        }
        if remove_plan {
            self.metrics.plans_executed.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Execution monitor loop: receives ExecutionReport messages and updates inflight plans.
    /// Handles:
    ///  - marking legs filled/partial
    ///  - if all legs filled -> mark plan executed and remove inflight
    ///  - on timeout detection (scan inflight periodically), send cancel requests
    async fn execution_monitor_loop(self: Arc<Self>, mut exec_resp_rx: ExecRespRx) {
        let mut ticker = tokio::time::interval(Duration::from_millis(50));

        loop {
            tokio::select! {
                maybe_report = exec_resp_rx.recv() => {
                    match maybe_report {
                        Some(report) => {
                            self.handle_exec_report(report).await;
                        }
                        None => {
                            // Channel closed, exit loop
                            break;
                        }
                    }
                }

                _ = ticker.tick() => {
                    // scan for timed-out in-flight plans
                    self.scan_timeouts().await;
                }
            }
        }
    }
}