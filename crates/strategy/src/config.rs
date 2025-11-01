use std::{collections::HashMap, fs, path::Path};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::errors::StrategyError;

fn default_zero() -> Decimal {
    Decimal::ZERO
}

/// Fee info (per-symbol or per-exchange)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeInfo {
    /// maker fee in decimal (e.g., 0.0002 for 0.02%)
    #[serde(default = "default_zero")]
    pub maker: Decimal,

    #[serde(default = "default_zero")]
    pub taker: Decimal,
}

impl FeeInfo {
    pub fn total(&self) -> Decimal {
        self.maker + self.taker
    }
}

impl Default for FeeInfo {
    fn default() -> Self {
        FeeInfo { maker: Decimal::ZERO, taker: Decimal::ZERO }
    }
}

/// Aggression mode controls order types and trade urgency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Aggression {
    /// Use market/taker orders for highest probability of fill
    Aggressive,
    /// Use limit/maker orders; safer on fees but execution is not guaranteed
    Passive,
}

impl Default for Aggression {
    fn default() -> Self {
        Aggression::Aggressive
    }
}


/// Limits and safety controls for the strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limits {
    /// max trade notional in quote currency (e.g., max USDT per triangular attempt)
    #[serde(default)]
    pub max_notional: Decimal,
    /// minimum absolute profit required in quote currency
    #[serde(default)]
    pub min_profit_abs: Decimal,
    /// minimum profit percentage required (e.g., 0.001 == 0.1%)
    #[serde(default)]
    pub min_profit_pct: Decimal,
    /// fraction of available depth to attempt to consume (0.0 - 1.0)
    #[serde(default = "default_depth_fill_factor")]
    pub depth_fill_factor: f32,
}

fn default_depth_fill_factor() -> f32 {
    0.6
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_notional: Decimal::ZERO,
            min_profit_abs: Decimal::ZERO,
            min_profit_pct: Decimal::ZERO,
            depth_fill_factor: default_depth_fill_factor(),
        }
    }
}

/// Lot rules for rounding and validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LotRule {
    #[serde(default)]
    pub min_size: Decimal,
    #[serde(default)]
    pub step_size: Decimal,
    #[serde(default)]
    pub min_notional: Decimal,
}

/// Primary Strategy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    /// Symbol order for the triangle. Example: ["A/B", "B/C", "A/C"]
    pub triangle: Vec<String>,

    /// Base currency used as cycle start/end (e.g., "USDT")
    pub base_currency: String,

    /// Per-symbol or per-exchange fee map. Keys should match symbols used in triangle or exchange identifiers.
    #[serde(default)]
    pub fee_map: HashMap<String, FeeInfo>,

    /// Limits and safety knobs
    #[serde(default)]
    pub limits: Limits,

    /// Aggression mode (aggressive = taker orders, passive = limit orders)
    #[serde(default)]
    pub aggression: Aggression,

    /// Max concurrent trade plans allowed
    #[serde(default = "default_concurrency")]
    pub max_concurrent_plans: usize,

    /// Order timeout in milliseconds (time after which an unfilled order is considered stale)
    #[serde(default = "default_order_timeout_ms")]
    pub order_timeout_ms: u64,

    /// Optional per-symbol lot/tick configuration (symbol -> (min_size, step_size, min_notional))
    #[serde(default)]
    pub lot_rules: HashMap<String, LotRule>,
}

fn default_concurrency() -> usize { 4 }
fn default_order_timeout_ms() -> u64 { 500 } // 500 ms default

impl Default for StrategyConfig {
    fn default() -> Self {
        StrategyConfig {
            triangle: vec!["BTC/USDT".into(), "ETH/BTC".into(), "ETH/USDT".into()],
            base_currency: "USDT".into(),
            fee_map: HashMap::new(),
            limits: Limits::default(),
            aggression: Aggression::default(),
            max_concurrent_plans: default_concurrency(),
            order_timeout_ms: default_order_timeout_ms(),
            lot_rules: HashMap::new(),
        }
    }
}

impl StrategyConfig {
    /// Load config from a TOML file path
    pub fn from_toml_file<P: AsRef<Path>>(path: P) -> Result<Self, StrategyError> {
        let s = fs::read_to_string(path)?;
        Self::from_toml_str(&s)
    }

    /// Load config from a TOML string
    pub fn from_toml_str(s: &str) -> Result<Self, StrategyError> {
        toml::from_str::<StrategyConfig>(s).map_err(|e| StrategyError::Parse(e.to_string()))
    }

    pub fn validate(&self) -> Result<(), StrategyError> {
        if self.triangle.len() != 3 {
            return Err(StrategyError::Validation("triangle must contain exactly 3 symbols".into()));
        }
        if self.base_currency.trim().is_empty() {
            return Err(StrategyError::Validation("base_currency must be set".into()));
        }
        if self.max_concurrent_plans == 0 {
            return Err(StrategyError::Validation("max_concurrent_plans must be > 0".into()));
        }
        if !(0.0..=1.0).contains(&self.limits.depth_fill_factor) {
            return Err(StrategyError::Validation("limits.depth_fill_factor must be between 0.0 and 1.0".into()));
        }
        if self.limits.min_profit_pct < Decimal::ZERO {
            return Err(StrategyError::Validation("limits.min_profit_pct must be >= 0".into()));
        }
        // ensure fee_map entries are non-negative
        for (k, f) in &self.fee_map {
            if f.maker < Decimal::ZERO || f.taker < Decimal::ZERO {
                return Err(StrategyError::Validation(format!("fee values must be >= 0 for key {}", k)));
            }
        }
        // validate lot rules
        for (sym, rule) in &self.lot_rules {
            if rule.min_size < Decimal::ZERO || rule.step_size < Decimal::ZERO || rule.min_notional < Decimal::ZERO {
                return Err(StrategyError::Validation(format!("lot_rules must be >= 0 for {}", sym)));
            }
        }
        
        Ok(())
    }
}