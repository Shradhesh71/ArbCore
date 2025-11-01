use rust_decimal::{Decimal, prelude::One};
use rust_decimal_macros::dec;

use crate::FeeInfo;

#[derive(Debug, Clone)]
pub struct TriView {
    pub sym_ab: String, // A/B
    pub bids_ab: Vec<(Decimal, Decimal)>, // price, size (best-first)
    pub asks_ab: Vec<(Decimal, Decimal)>,
    pub sym_bc: String, // B/C
    pub bids_bc: Vec<(Decimal, Decimal)>,
    pub asks_bc: Vec<(Decimal, Decimal)>,
    pub sym_ac: String, // A/C
    pub bids_ac: Vec<(Decimal, Decimal)>,
    pub asks_ac: Vec<(Decimal, Decimal)>,
}

#[derive(Debug, Clone)]
pub struct Opportunity {
    pub start_currency: String, // e.g., C
    pub estimated_profit: Decimal,
    pub implied_price: Decimal,
    pub legs: Vec<LegEstimate>,
}

#[derive(Debug, Clone)]
pub struct LegEstimate {
    pub symbol: String,
    pub side_buy: bool, // true if we buy base on this leg
    pub price: Decimal, // expected execution price (avg)
    pub qty: Decimal,
}

pub fn detect_triangular_opportunities(
    view: &TriView, 
    fee_map: &std::collections::HashMap<String, FeeInfo>,
    min_profit_abs: Decimal
) -> Vec<Opportunity> {
    let mut out = Vec::new();

    // compute cycle C -> B -> A -> C
    let start = dec!(1);
    let ask_bc = view.asks_bc.first().map(| (p, _)| *p);
    let ask_ab = view.asks_ab.first().map(| (p, _)| *p);
    let bid_ac = view.bids_ac.first().map( |(p,_)| *p);

    if let (Some(ask_bc), Some(ask_ab), Some(bid_ac)) = (ask_bc, ask_ab, bid_ac) {
        // fee per
        let fee_bc = fee_map.get(&view.sym_bc).map(|f| f.taker).unwrap_or(dec!(0));
        let fee_ab = fee_map.get(&view.sym_ab).map(|f| f.taker).unwrap_or(dec!(0));
        let fee_ac = fee_map.get(&view.sym_ac).map(|f| f.taker).unwrap_or(dec!(0));

        // simulate: start C -> buy B at ask_bc (we lose fee on notional)
        let b_amount = (start / ask_bc) * (Decimal::one() - fee_bc);

        let a_amount = (b_amount / ask_ab) * (Decimal::one() - fee_ab);

        let final_c = a_amount * bid_ac * (Decimal::one() - fee_ac);
        let profit = final_c - start;

        if profit > min_profit_abs {
            out.push(Opportunity {
                start_currency: "C".to_string(),
                estimated_profit: profit,
                implied_price: bid_ac - (ask_ab * ask_bc),
                legs: vec![
                    LegEstimate{
                        symbol: view.sym_bc.clone(),
                        side_buy: true,
                        price: ask_bc, 
                        qty: start
                    },
                    LegEstimate{
                        symbol: view.sym_ab.clone(),
                        side_buy: true,
                        price: ask_ab,
                        qty: b_amount
                    },
                    LegEstimate{
                        symbol: view.sym_ac.clone(),
                        side_buy: true,
                        price: bid_ac,
                        qty: a_amount
                    }
                ]
            });
        }

    }

    out
}