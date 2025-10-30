use std::{collections::BTreeMap, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Instant};

use parking_lot::RwLock;
use rust_decimal::Decimal;

use crate::{EstimatedFill, Level, OrderBookError, OrderbookDelta, OrderbookSnapshot, Side};

#[derive(Debug)]
pub struct BookInner {
    pub bids: BTreeMap<Decimal, Decimal>,
    pub asks: BTreeMap<Decimal, Decimal>,
    pub last_seq: Option<u64>,
    pub last_update_ts: Option<Instant>,
}

#[derive(Clone)]
pub struct OrderBook {
    symbol: String,
    inner: Arc<RwLock<BookInner>>,
    pub resync_required: Arc<AtomicBool>,
}

impl OrderBook {
    pub fn new(symbol: &str) -> Self {
        let inner = BookInner {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_seq: None,
            last_update_ts: None,
        };
        Self {
            symbol: symbol.to_string(),
            inner: Arc::new(RwLock::new(inner)),
            resync_required: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn best_bid(&self) -> Option<Level> {
        let inner = self.inner.read();
        inner.bids.iter().rev().next().map(|(price, size)| Level {
            price: *price,
            size: *size,
        })
    }

    pub fn best_ask(&self) -> Option<Level> {
        let inner = self.inner.read();
        inner.asks.iter().next().map(|(price, size)| Level {
            price: *price,
            size: *size,
        })
    }

    pub fn top_n(&self, side: Side, n: usize) -> Vec<Level> {
        let inner = self.inner.read();
        match side {
            Side::Bid => inner.bids.iter().rev().take(n).map(|(price, size)| Level {
                price: *price,
                size: *size,
            }).collect(),
            Side::Ask => inner.asks.iter().take(n).map(|(price, size)| Level {
                price: *price,
                size: *size,
            }).collect(),
        }
    }

    // Apply initial REST snapshot (resets state)
    pub fn apply_snapshot(&self, snapshot: OrderbookSnapshot) -> Result<(), OrderBookError>{
        let mut w = self.inner.write();
        w.bids.clear();
        w.asks.clear();
        for lvl in snapshot.bids {
            if lvl.size > Decimal::ZERO {
                w.bids.insert(lvl.price, lvl.size);
            }
        }
        for lvl in snapshot.asks {
            if lvl.size > Decimal::ZERO {
                w.asks.insert(lvl.price, lvl.size);
            }
        }
        w.last_seq = snapshot.sequence;
        w.last_update_ts = Some(Instant::now());
        self.resync_required.store(false, Ordering::SeqCst);
        Ok(())
    }

    // Apply incremental delta (sequence-checked)
    pub fn apply_delta(&self, delta: OrderbookDelta) -> Result<(), OrderBookError>{
        let mut w = self.inner.write();
        // sequence check
        if let Some(prev) = w.last_seq {
            if let Some(d_prev) = delta.prev_sequence {
                if d_prev != prev {
                    // gap detected
                    self.resync_required.store(true, Ordering::SeqCst);
                    return Err(OrderBookError::SequenceError { expected: prev, got: d_prev });
                }
            }
        }
        // apply bids
        for lvl in delta.bids {
            if lvl.size == Decimal::ZERO {
                w.bids.remove(&lvl.price);
            } else {
                w.bids.insert(lvl.price, lvl.size);
            }
        }
        // apply asks
        for lvl in delta.asks {
            if lvl.size == Decimal::ZERO {
                w.asks.remove(&lvl.price);
            } else {
                w.asks.insert(lvl.price, lvl.size);
            }
        }
        w.last_seq = delta.sequence;
        w.last_update_ts = Some(Instant::now());
        Ok(())
    }

    pub fn estimate_fill_price(&self, side: Side, mut target_size: Decimal) -> Result<EstimatedFill, OrderBookError> {
        if target_size <= Decimal::ZERO {
            return Err(OrderBookError::InvalidArgument("size must be > 0".into()));
        }
        let r = self.inner.read();
        let mut total_qty = Decimal::ZERO;
        let mut total_notional = Decimal::ZERO;

        match side {
            Side::Bid => {
                // to sell base, you hit asks (we're buying base from asks)
                for (price, size) in r.asks.iter() {
                    if target_size <= Decimal::ZERO { break; }
                    let take = if *size <= target_size { *size } else { target_size };
                    total_qty += take;
                    total_notional += take * *price;
                    target_size -= take;
                }
            }
            Side::Ask => {
                // to buy base, you take bids (we're selling base into bids)
                for (price, size) in r.bids.iter().rev() {
                    if target_size <= Decimal::ZERO { break; }
                    let take = if *size <= target_size { *size } else { target_size };
                    total_qty += take;
                    total_notional += take * *price;
                    target_size -= take;
                }
            }
        }

        if target_size > Decimal::ZERO {
            return Err(OrderBookError::InsufficientDepth);
        }
        let avg_price = total_notional / total_qty;
        Ok(EstimatedFill { avg_price, total_qty, total_notional })
    }

    pub fn snapshot_copy(&self) -> OrderbookSnapshot {
        let r = self.inner.read();
        let bids = r.bids.iter().rev().map(|(p,s)| Level{price:*p, size:*s}).collect();
        let asks = r.asks.iter().map(|(p,s)| Level{price:*p, size:*s}).collect();
        OrderbookSnapshot { symbol: self.symbol.clone(), bids, asks, sequence: r.last_seq, ts: todo!() }
    }
}