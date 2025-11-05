use std::{collections::HashMap, sync::{Arc, atomic::{AtomicU64, Ordering}, mpsc}, time::{Duration, Instant}};
use rust_decimal_macros::dec;
use uuid::Uuid;
use parking_lot::Mutex;
use rust_decimal::Decimal;

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
pub type TriRx = mpsc::Receiver<TriView>;
pub type ExecTx = mpsc::Sender<TradePlan>;
pub type ExecRespRx = mpsc::Receiver<ExecutionReport>;
pub type ExecCancelTx = mpsc::Sender<CancelRequest>;
pub type RiskTx = mpsc::Sender<RiskCheckRequest>;
pub type RiskRespRx = mpsc::Receiver<RiskCheckResult>;


/// StrategyEngine wiring
pub struct StrategyEngine {
    pub cfg: StrategyConfig,
    tri_rx: TriRx,
    exec_tx: ExecTx,
    exec_resp_rx: ExecRespRx,
    exec_cancel_tx: ExecCancelTx,
    risk_tx: Option<RiskTx>,
    risk_resp_rx: Option<RiskRespRx>, // optional paired receiver if you want direct results
    /// in-flight plans (protected by mutex)
    inflight: Arc<Mutex<HashMap<String, InFlightPlan>>>,
    pub metrics: Arc<StrategyMetrics>,
}

impl StrategyEngine {
    /// Create engine. All channels are expected to be created by caller (wiring to execution / risk).
    pub fn new(
        cfg: StrategyConfig,
        tri_rx: TriRx,
        exec_tx: ExecTx,
        exec_resp_rx: ExecRespRx,
        exec_cancel_tx: ExecCancelTx,
        risk_tx: Option<RiskTx>,
        risk_resp_rx: Option<RiskRespRx>,
    ) -> Self {
        Self {
            cfg,
            tri_rx,
            exec_tx,
            exec_resp_rx,
            exec_cancel_tx,
            risk_tx,
            risk_resp_rx,
            inflight: Arc::new(Mutex::new(HashMap::new())),
            metrics: Arc::new(StrategyMetrics::default()),
        }
    }

    /// Start engine main loop(s). Spawns two tasks:
    /// - detector loop: reads tri_rx, runs detector, prepares plans and submits
    /// - exec monitor loop: reads exec_resp_rx and updates in-flight plans
    ///
    /// The function returns immediately; loops run in background until tri_rx/execution channels close.
    pub fn start(self: Arc<Self>) {
        let me = self.clone();
        // detector task
        tokio::task::spawn_blocking(move || {
            me.detector_loop().await;
        });

        let me2 = self.clone();
        // execution response monitor
        tokio::spawn(async move {
            // me2.execution_monitor_loop().await;
        });
    }

    /// Detector loop: receive TriView ticks, detect, pre-check, plan, optional risk check, submit
    async fn detector_loop(self: Arc<Self>) {
        let cfg = &self.cfg;
        let fee_map = &cfg.fee_map;
        let depth_factor = cfg.limits.depth_fill_factor;
        let min_profit_abs = cfg.limits.min_profit_abs;
        let max_concurrent = cfg.max_concurrent_plans;

        while let Ok(view) = self.tri_rx.recv() {
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
                let plan = build_trade_plan(&op,&self.cfg);

                // optional risk check: synchronous-ish via sending request and awaiting response if risk_resp_rx present
                let risk_ok = if let (Some(risk_tx), Some(mut risk_resp_rx)) = (self.risk_tx.clone(), self.risk_resp_rx.clone()) {
                    let req = RiskCheckRequest { plan: plan.clone() };
                    match risk_tx.send(req).await {
                        Ok(_) => {
                            // wait for single response (this is simplistic; production should correlate req/resp)
                            match tokio::time::timeout(Duration::from_millis(self.cfg.order_timeout_ms), risk_resp_rx.recv()).await {
                                Ok(Some(resp)) => {
                                    if resp.approved {
                                        true
                                    } else {
                                        // warn!("risk rejected plan {}: {:?}", plan.id, resp.reason);
                                        false
                                    }
                                }
                                _ => {
                                    // warn!("risk check timeout or channel closed; rejecting plan {}", plan.id);
                                    false
                                }
                            }
                        }
                        Err(e) => {
                            // warn!("failed to send risk check: {}", e);
                            false
                        }
                    }
                } else {
                    true
                };

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

                // submit to execution (await send to respect backpressure)
                match self.exec_tx.send(plan.clone()) {
                    Ok(_) => {
                        self.metrics.plans_submitted.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
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
            if infi.remove(&plan_id) .is_some() {
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
    async fn execution_monitor_loop(mut self: Arc<Self>) {
        // periodic timeout scanner
        let timeout_scan_interval = Duration::from_millis(50);
        let mut ticker = tokio::time::interval(timeout_scan_interval);

        loop {
            tokio::select! {
                maybe = Arc::get_mut(&mut self).unwrap().exec_resp_rx.recv() => {
                    match maybe {
                        Some(report) => {
                            self.handle_exec_report(report).await;
                        }
                        None => {
                            info!("exec_resp_rx closed; exiting monitor loop");
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