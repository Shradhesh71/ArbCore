use rust_decimal::{Decimal, prelude::One};
use rust_decimal_macros::dec;

use crate::FeeInfo;
use marketdata::TriView;

#[derive(Debug, Clone)]
pub struct Opportunity {
    pub start_currency: String,
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
    
    static CYCLE_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = CYCLE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Get fees for each symbol
    let fee_ab = fee_map.get(&view.sym_ab).map(|f| f.taker).unwrap_or(dec!(0));
    let fee_bc = fee_map.get(&view.sym_bc).map(|f| f.taker).unwrap_or(dec!(0));
    let fee_ac = fee_map.get(&view.sym_ac).map(|f| f.taker).unwrap_or(dec!(0));

    // ===================================================================
    // CYCLE 1: AB -> BC -> AC
    // Example: BTCUSDT(ab) -> ETHUSDT(bc) -> ETHBTC(ac)
    // Start with 1 unit of base currency (USDT)
    // 1. Buy BTC with USDT at ask price on BTCUSDT
    // 2. Buy ETH with BTC at ask price on ETHBTC (inverse of ac) 
    // 3. Sell ETH for USDT at bid price on ETHUSDT
    // ===================================================================
    if let (Some(ask_ab), Some(bid_bc), Some(ask_ac)) = (
        view.asks_ab.first().map(|(p, _)| *p),
        view.bids_bc.first().map(|(p, _)| *p),
        view.asks_ac.first().map(|(p, _)| *p),
    ) {
        let start = dec!(10000); // Start with 10000 USDT
        
        // Step 1: Buy BTC with USDT on BTCUSDT (ab)
        let btc_amount = (start / ask_ab) * (Decimal::one() - fee_ab);
        
        // Step 2: Buy ETH with BTC on ETHBTC (we need to use 1/ask_ac because ETHBTC = ETH/BTC)
        let eth_amount = (btc_amount / ask_ac) * (Decimal::one() - fee_ac);
        
        // Step 3: Sell ETH for USDT on ETHUSDT (bc)
        let final_usdt = (eth_amount * bid_bc) * (Decimal::one() - fee_bc);
        
        let profit1 = final_usdt - start;
        let profit_pct1 = (profit1 / start) * dec!(100);

        if count % 100 == 0 {
            println!("🔍 Cycle #{}: {} -> {} -> {} | profit={:.8} USDT ({:.4}%) | prices: {}={:.2}, {}={:.2}, {}={:.8}", 
                count, view.sym_ab, view.sym_ac, view.sym_bc, 
                profit1, profit_pct1,
                view.sym_ab, ask_ab, 
                view.sym_bc, bid_bc, 
                view.sym_ac, ask_ac);
        }

        if profit1 > min_profit_abs {
            out.push(Opportunity {
                start_currency: "USDT".to_string(),
                estimated_profit: profit1,
                implied_price: bid_bc / (ask_ab * ask_ac),
                legs: vec![
                    LegEstimate {
                        symbol: view.sym_ab.clone(),
                        side_buy: true,
                        price: ask_ab,
                        qty: start / ask_ab,
                    },
                    LegEstimate {
                        symbol: view.sym_ac.clone(),
                        side_buy: true,
                        price: ask_ac,
                        qty: btc_amount / ask_ac,
                    },
                    LegEstimate {
                        symbol: view.sym_bc.clone(),
                        side_buy: false, // SELL ETH for USDT
                        price: bid_bc,
                        qty: eth_amount,
                    },
                ],
            });
        }
    }

    // ===================================================================
    // CYCLE 2: BC -> AC -> AB (Reverse direction)
    // Example: ETHUSDT(bc) -> ETHBTC(ac) -> BTCUSDT(ab)
    // Start with 1 unit of base currency (USDT)
    // 1. Buy ETH with USDT at ask price on ETHUSDT
    // 2. Sell ETH for BTC at bid price on ETHBTC
    // 3. Sell BTC for USDT at bid price on BTCUSDT
    // ===================================================================
    if let (Some(ask_bc), Some(bid_ac), Some(bid_ab)) = (
        view.asks_bc.first().map(|(p, _)| *p),
        view.bids_ac.first().map(|(p, _)| *p),
        view.bids_ab.first().map(|(p, _)| *p),
    ) {
        let start = dec!(10000); // Start with 10000 USDT
        
        // Step 1: Buy ETH with USDT on ETHUSDT (bc)
        let eth_amount = (start / ask_bc) * (Decimal::one() - fee_bc);
        
        // Step 2: Sell ETH for BTC on ETHBTC (ac)
        let btc_amount = (eth_amount * bid_ac) * (Decimal::one() - fee_ac);
        
        // Step 3: Sell BTC for USDT on BTCUSDT (ab)
        let final_usdt = (btc_amount * bid_ab) * (Decimal::one() - fee_ab);
        
        let profit2 = final_usdt - start;
        let profit_pct2 = (profit2 / start) * dec!(100);

        if count % 100 == 0 {
            println!("🔄 Cycle #{}: {} -> {} -> {} | profit={:.8} USDT ({:.4}%) | prices: {}={:.2}, {}={:.8}, {}={:.2}", 
                count, view.sym_bc, view.sym_ac, view.sym_ab,
                profit2, profit_pct2,
                view.sym_bc, ask_bc,
                view.sym_ac, bid_ac,
                view.sym_ab, bid_ab);
        }

        if profit2 > min_profit_abs {
            out.push(Opportunity {
                start_currency: "USDT".to_string(),
                estimated_profit: profit2,
                implied_price: (bid_ab * bid_ac) / ask_bc,
                legs: vec![
                    LegEstimate {
                        symbol: view.sym_bc.clone(),
                        side_buy: true,
                        price: ask_bc,
                        qty: start / ask_bc,
                    },
                    LegEstimate {
                        symbol: view.sym_ac.clone(),
                        side_buy: false, // SELL ETH for BTC
                        price: bid_ac,
                        qty: eth_amount,
                    },
                    LegEstimate {
                        symbol: view.sym_ab.clone(),
                        side_buy: false, // SELL BTC for USDT
                        price: bid_ab,
                        qty: btc_amount,
                    },
                ],
            });
        }
    }

    out
}