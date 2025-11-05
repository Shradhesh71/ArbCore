use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{Opportunity, OrderLeg, StrategyConfig, TradePlan, errors::StrategyError};

/// Round `qty` down to nearest `step_size`. If step_size == 0, returns qty unchanged.
/// Uses floor rounding to avoid exceeding limits.
pub fn round_qty_to_step(qty: Decimal, step_size: Decimal) -> Decimal {
    if step_size <= Decimal::ZERO {
        return qty;
    }
    // steps = floor(qty / step_size)
    let steps = (qty / step_size).floor();
    (steps * step_size).max(Decimal::ZERO)
}

/// Build a TradePlan from an Opportunity using lot rules. Returns Err if plan invalid.
pub fn build_trade_plan(op: &Opportunity, cfg: &StrategyConfig) -> Result<TradePlan, StrategyError> {
    let id = Uuid::new_v4().to_string();
    let mut legs: Vec<OrderLeg> = Vec::with_capacity(op.legs.len());

    for (idx, l) in op.legs.iter().enumerate() {
        // fetch lot rule for symbol
        let rule_opt = cfg.lot_rules.get(&l.symbol);
        let mut qty = l.qty;

        if let Some(rule) = rule_opt {
            if rule.step_size > Decimal::ZERO {
                qty = round_qty_to_step(qty, rule.step_size);
            }
            // enforce min_size
            if rule.min_size > Decimal::ZERO {
                if qty < rule.min_size {
                    qty = rule.min_size;
                }
            }
            // check min_notional (price_hint * qty)
            if rule.min_notional > Decimal::ZERO {
                let notional = qty * l.price;
                if notional < rule.min_notional {
                    return Err(StrategyError::MinNotional(l.symbol.clone(), notional, rule.min_notional));
                }
            }
        } else {
            // no rule — ensure qty > 0
            if qty <= Decimal::ZERO {
                return Err(StrategyError::QtyRoundedZero(l.symbol.clone()));
            }
        }

        if qty <= Decimal::ZERO {
            return Err(StrategyError::QtyRoundedZero(l.symbol.clone()));
        }

        let is_taker = matches!(cfg.aggression, crate::config::Aggression::Aggressive);

        legs.push(OrderLeg {
            symbol: l.symbol.clone(),
            buy_base: l.side_buy,
            price_hint: l.price,
            qty,
            is_taker,
            leg_idx: idx as u8,
        });
    }

    Ok(TradePlan {
        id,
        start_currency: op.start_currency.clone(),
        estimated_profit: op.estimated_profit,
        estimated_profit_pct: op.estimated_profit / op.legs.first().map(|l| l.qty * op.implied_price).unwrap_or(Decimal::ONE),
        legs,
        created_ts: std::time::Instant::now(),
    })
}