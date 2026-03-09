# Mock Execution Demo Flowchart

This flowchart illustrates the flow of the `mock_execution_demo.rs` program from step 0 to step 8, including nested functions and their one-line descriptions.

```mermaid
flowchart TD
    Start([Start]) --> InitLog[Initialize logging<br/>tracing_subscriber::fmt().init() - Initializes logging system]

    InitLog --> DBSetup[Database Setup]

    subgraph DBSetup [Step 0: Database Setup]
        direction TB
        DBConnect[Storage::connect() - Connects to database with connection pool]
        CheckFills[storage.trades().latest_fills() - Retrieves latest trade fills from database]
        DBConnect --> CheckFills
    end

    DBSetup --> MDSetup[Market Data Setup]

    subgraph MDSetup [Step 1: Setup Market Data Manager + Binance Adapter]
        direction TB
        MDManager[MarketDataManager::new() - Creates a new market data manager]
        Adapter[BinanceSpotAdapter::new() - Creates a new Binance spot adapter]
        MDManager --> Adapter
    end

    MDSetup --> Wire[Wire Adapter]

    subgraph Wire [Step 2: Symbols Wire]
        direction TB
        WireFunc[wire_adapter_to_manager() - Wires the exchange adapter to the market data manager for specified symbols]
    end

    Wire --> Sleep[Sleep 2 seconds<br/>sleep() - Waits for market data to start receiving updates]

    Sleep --> TriView[Create TriView Builder]

    subgraph TriView [Step 3: Create TriView Builder]
        direction TB
        TriBuilder[TriViewBuilder::new() - Creates a new TriView builder with snapshot getter]
        subgraph SnapGetter [snapshot_getter closure]
            direction LR
            GetSnap[adapter.get_snapshot() - Gets orderbook snapshot from adapter]
            Convert[Convert to OrderbookSnapshot - Transforms adapter snapshot to orderbook format]
            GetSnap --> Convert
        end
        Subscribe[subscribe_triangle() - Subscribes to triangle arbitrage view with polling]
        TriBuilder --> SnapGetter
        SnapGetter --> Subscribe
    end

    TriView --> StratConfig[Setup Strategy Config]

    subgraph StratConfig [Step 4: Setup Strategy Config]
        direction TB
        Config[Create strategy_config - Sets up fees, limits, aggression, and lot rules for strategy]
    end

    StratConfig --> Channels[Create Channels]

    subgraph Channels [Step 5: Create Channels for Communication]
        direction TB
        Unbounded[mpsc::unbounded_channel() - Creates unbounded channels for TriView updates]
        Bounded[mpsc::channel(128) - Creates bounded channels for execution]
        ForwardSpawn[tokio::spawn() - Spawns task to forward messages between bounded and unbounded channels]
        Unbounded --> Bounded --> ForwardSpawn
    end

    Channels --> Engine[Create Strategy Engine]

    subgraph Engine [Step 6: Create and Start Strategy Engine]
        direction TB
        NewEngine[StrategyEngine::new() - Creates new strategy engine with config and channels]
        WithStorage[with_storage() - Adds database storage to engine for persistence]
        Start[start() - Starts the engine with detector and monitor loops]
        NewEngine --> WithStorage --> Start
    end

    Engine --> MockExec[Start Mock Execution]

    subgraph MockExec [Step 7: Start Mock Execution Layer]
        direction TB
        SpawnMock[spawn_mock_execution() - Spawns mock execution handler for trade plans]
        ForwardSpawns[tokio::spawn() - Spawns tasks to forward messages between channels]
        SpawnMock --> ForwardSpawns
    end

    MockExec --> Run[Run System]

    subgraph Run [Step 8: Run System and Monitor]
        direction TB
        Select[tokio::select!() - Runs select loop to monitor for shutdown signal or task completion]
    end

    Run --> End([End])
```