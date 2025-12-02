/// Example: Wire StrategyEngine with Mock Execution Layer
///
/// This demonstrates a complete live testing setup with:
/// - Real market data from Binance via adapters
/// - Real orderbook management
/// - Real TriView detection
/// - MOCK execution (no real orders, no money, no keys)
///
/// Perfect for testing strategy logic before connecting to real execution.

use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};
use tracing::{info, error};

use adapters::{BinanceSpotAdapter, ExchangeAdapter};
use marketdata::{
    manager::MarketDataManager,
    triview::TriViewBuilder,
};
use strategy::{
    engine::StrategyEngine,
    config::{StrategyConfig, FeeInfo, Limits, Aggression, LotRule},
};
use stroage::Storage;
use execution::spawn_mock_execution;
use rust_decimal_macros::dec;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 Starting Mock Execution Demo");
    info!("mode: PAPER TRADING (No real orders, no money)");


    // ============
    // 0) database setup
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@localhost:5432/tri_arb".to_string());
    
    info!("🗄️  Connecting to database...");
    let storage = Storage::connect(&database_url, 5).await?;
    
    // Check existing fills
    let existing_fills = storage.trades().latest_fills(1).await?;
    if existing_fills.is_empty() {
        info!("Database connected - no existing trade fills");
    } else {
        let total = storage.trades().latest_fills(1000).await?.len();
        info!("Database connected - {} existing trade fills", total);
    }

    // ====================================================================
    // 1) Setup Market Data Manager + Binance Adapter
    let md_manager = Arc::new(MarketDataManager::new());
    let adapter = BinanceSpotAdapter::new();

    // Wire adapter to market data manager
    let symbols_to_wire = vec![
        "BTCUSDT".to_string(),
        "ETHUSDT".to_string(),
        "ETHBTC".to_string(),
    ];
    
    marketdata::adapter_wiring::wire_adapter_to_manager(
        md_manager.clone(),
        adapter.clone(),
        symbols_to_wire.clone(),
    ).await?;

    info!("✅ Market data adapter connected");

    // ====================================================================
    // 2) Symbols already wired above
    info!("Subscribed to symbols: {:?}", symbols_to_wire);

    // Give market data a moment to start receiving updates
    sleep(Duration::from_secs(2)).await;

    // ====================================================================
    // 3) Create TriView Builder
    let adapter_for_triview = adapter.clone();
    let snapshot_getter = Arc::new(move |symbol: &str| {
        let adapter_clone = adapter_for_triview.clone();
        let sym = symbol.to_string();
        Box::pin(async move {
            // Get snapshot from adapter
            let adapter_snapshot = adapter_clone.get_snapshot(&sym)
                .await
                .map_err(|e| marketdata::errors::MarketDataError::Other(format!("adapter error: {:?}", e)))?;
            
            // Convert to orderbook snapshot
            let ob_snapshot = orderbook::OrderbookSnapshot {
                symbol: adapter_snapshot.symbol,
                sequence: adapter_snapshot.sequence,
                ts: Some(std::time::Instant::now().elapsed().as_nanos()),
                bids: adapter_snapshot.bids,
                asks: adapter_snapshot.asks,
            };
            
            Ok(ob_snapshot)
        }) as futures::future::BoxFuture<'static, Result<orderbook::OrderbookSnapshot, marketdata::errors::MarketDataError>>
    });

    let triview_builder = TriViewBuilder::new(snapshot_getter);

    // Subscribe to the triangle (BTCUSDT, ETHUSDT, ETHBTC)
    let tri_subscription = triview_builder.subscribe_triangle(
        "BTCUSDT".to_string(),
        "ETHUSDT".to_string(),
        "ETHBTC".to_string(),
        10,                            // top 10 levels per side
        Duration::from_millis(100),    // poll every 100ms
    );

    info!("TriView builder subscribed to triangle");

    // ====================================================================
    // 4) Setup Strategy Config
    let mut fee_map = std::collections::HashMap::new();
    fee_map.insert("BTCUSDT".to_string(), FeeInfo { maker: dec!(0.0001), taker: dec!(0.001) });
    fee_map.insert("ETHUSDT".to_string(), FeeInfo { maker: dec!(0.0001), taker: dec!(0.001) });
    fee_map.insert("ETHBTC".to_string(), FeeInfo { maker: dec!(0.0001), taker: dec!(0.001) });

    let mut lot_rules = std::collections::HashMap::new();
    lot_rules.insert("BTCUSDT".to_string(), LotRule { 
        min_size: dec!(0.001),     // Increased from 0.00001 to 0.001
        step_size: dec!(0.00001),
        min_notional: dec!(10.0),
    });
    lot_rules.insert("ETHUSDT".to_string(), LotRule { 
        min_size: dec!(0.01),      // Increased from 0.0001 to 0.01
        step_size: dec!(0.0001),
        min_notional: dec!(10.0),
    });
    lot_rules.insert("ETHBTC".to_string(), LotRule { 
        min_size: dec!(0.01),      // Increased from 0.001 to 0.01
        step_size: dec!(0.001),
        min_notional: dec!(0.0001),
    });

    let strategy_config = StrategyConfig {
        triangle: vec![
            "BTCUSDT".to_string(),
            "ETHUSDT".to_string(),
            "ETHBTC".to_string(),
        ],
        base_currency: "USDT".to_string(),
        fee_map,
        limits: Limits {
            max_notional: dec!(10000.0),    // Increased from $100 to $10,000 notional
            min_profit_abs: dec!(0.01),     // minimum $0.01 profit (very low for testing)
            min_profit_pct: dec!(0.0001),   // minimum 0.01% profit (very low for testing)
            depth_fill_factor: 0.8,         // use 80% of available depth
        },
        aggression: Aggression::Aggressive,
        max_concurrent_plans: 3,
        order_timeout_ms: 5000, // 5 second timeout for orders
        lot_rules,
    };

    info!("Strategy config loaded");

    // ====================================================================
    // 5) Create Channels for Strategy <-> Execution Communication
    // Strategy receives TriView updates - need to convert bounded to unbounded
    let tri_subscription_rx = tri_subscription.rx;
    let (tri_tx, tri_rx) = mpsc::unbounded_channel();
    
    // Spawn task to forward from bounded to unbounded channel
    tokio::spawn(async move {
        let mut rx = tri_subscription_rx;
        while let Some(view) = rx.recv().await {
            if tri_tx.send(view).is_err() {
                break;
            }
        }
    });

    // Strategy -> Execution: TradePlan
    let (plan_tx, mut plan_rx) = mpsc::unbounded_channel();

    // Execution -> Strategy: ExecutionReport
    let (exec_resp_tx, exec_resp_rx) = mpsc::unbounded_channel();

    // Strategy -> Execution: CancelRequest
    let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel();

    info!("Communication channels created");

    // ====================================================================
    // 6) Create and Start Strategy Engine
    let engine = StrategyEngine::new(
        strategy_config,
        plan_tx,        // where to send TradePlans
        cancel_tx,      // where to send CancelRequests
    )
    .with_storage(Arc::new(storage));  // Enable database persistence

    info!("Strategy engine created with database persistence");

    let (detector_handle, monitor_handle) = engine.start(tri_rx, exec_resp_rx);
   
    info!("Strategy engine started");

    // ====================================================================
    // 7) Start Mock Execution Layer
    // Convert unbounded to bounded channels for mock execution
    let (plan_tx_bounded, plan_rx_bounded) = mpsc::channel(128);
    let (exec_resp_tx_bounded, mut exec_resp_rx_bounded) = mpsc::channel(128);
    let (cancel_tx_bounded, cancel_rx_bounded) = mpsc::channel(128);
    
    // Forward unbounded -> bounded
    tokio::spawn(async move {
        while let Some(plan) = plan_rx.recv().await {
            if plan_tx_bounded.send(plan).await.is_err() { break; }
        }
    });
    
    tokio::spawn(async move {
        while let Some(report) = exec_resp_rx_bounded.recv().await {
            if exec_resp_tx.send(report).is_err() { break; }
        }
    });
    
    tokio::spawn(async move {
        while let Some(cancel) = cancel_rx.recv().await {
            if cancel_tx_bounded.send(cancel).await.is_err() { break; }
        }
    });

    let mock_exec_handle = spawn_mock_execution(
        plan_rx_bounded,
        exec_resp_tx_bounded,
        cancel_rx_bounded,
    );

    info!("Mock execution layer started");

    // ====================================================================
    // 8) Run System and Monitor
    info!("System is LIVE - monitoring for arbitrage opportunities...");
    info!("press Ctrl+C to stop");

    // Run for a period or until Ctrl+C
    // In production, you'd use signal handlers for graceful shutdown
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("🛑 Received shutdown signal");
        }
        result = detector_handle => {
            match result {
                Ok(_) => info!("✅ Detector loop completed"),
                Err(e) => error!("❌ Detector loop error: {}", e),
            }
        }
        result = monitor_handle => {
            match result {
                Ok(_) => info!("✅ Monitor loop completed"),
                Err(e) => error!("❌ Monitor loop error: {}", e),
            }
        }
        result = mock_exec_handle => {
            match result {
                Ok(_) => info!("✅ Mock execution completed"),
                Err(e) => error!("❌ Mock execution error: {}", e),
            }
        }
    }

    info!("👋 Shutting down gracefully...");

    Ok(())
}
