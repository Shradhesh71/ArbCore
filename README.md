# triangular arb

```
P(A/B) = price of A in B

P(B/C) = price of B in C

implied P(A/C) = P(A/B) * P(B/C)

if P(A/C)_direct differs such that
profit ≈ (P(A/C)_direct - implied P(A/C)) × size > fees + slippage,
you can buy/sell across the three legs to lock profit
```


We’re building a **high-performance cryptocurrency triangular arbitrage engine in Rust**.
It connects to exchanges, maintains live L2 orderbooks, detects profitable price cycles across three trading pairs (like BTC/USDT → ETH/BTC → ETH/USDT), and automatically generates executable trade plans.

The system is modular, production-style, and designed like a real HFT engine.

---

## **What’s Completed So Far**

### **1. Core Crates & Architecture**

We built a clean multi-crate Rust workspace:

* **orderbook crate**
  Holds a full depth orderbook engine (snapshots, deltas, sequence checks, fill estimation).
  Production-grade API for applying CEX-style market data.

* **strategy crate**
  Implements:

  * Triangular arbitrage detection
  * Trade plan creation
  * Config system (TOML)
  * Execution engine interface
  * Metrics counters
  * Mock execution module
  * Trade planner with lot rules, rounding, fee logic
    Fully done.

* **marketdata crate**
  Handles:

  * Exchange adapter wiring
  * Per-symbol workers
  * Snapshot + delta sequencing
  * Buffered deltas
  * Resyncs
  * WebSocket helpers
  * TriView builder (top-N view for 3 symbols)
    This crate is mostly complete — only adapter implementations remain.

Everything fits together like a real-world CEX arb stack.

---

## **2. Completed Internal Components**

### **orderbook**

* Snapshot & delta types
* Depth book with price levels
* Gap detection
* Sequence validation
* Apply snapshot/delta
* Fill simulation
* Error system

### **strategy**

* Arbitrage finder (`detect_triangular_opportunities`)
* Trade engine (`engine.rs`)
* Planner: build executable trade plans
* Config: limits, fees, aggression mode, lot rules
* Mock execution engine for dev/test
* Metrics counters
* Clean folder structure

### **marketdata**

* Manager: per-symbol workers with sequencing
* Snapshot provider integration
* WS reconnect logic
* Adapter wiring layer
* TriView builder (fetch snapshots for 3 symbols at interval)
* Types, errors, helpers

You now have the full backbone required for a real arbitrage system.

---

## **3. What’s Left / Next Steps**

### **A) Implement real exchange adapters**

(Example: Binance Spot)

* REST depth snapshot → `OrderbookSnapshot`
* WS incremental depth feed → `OrderbookDelta`
* Mapping updates into canonical types
  This is the next major step.

### **B) Integrate adapters with marketdata manager**

Use the `adapter_wiring.rs` helper we wrote:

* registers snapshot provider
* forwards deltas into per-symbol workers

### **C) End-to-end test**

* Feed live Binance orderbooks
* Receive TriView updates
* Strategy detects cycles
* Mock executor prints simulated trades

### **Optional afterward**

* Multi-venue arbitrage
* Real execution
* Risk engine
* PnL logging
* DEX adapter (Jupiter) for CEX ↔ DEX arb

---
