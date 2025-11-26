// =================================================================================
// MODULE: Trading Engine Orchestrator
// DESCRIPTION:
// The central nervous system of the bot. It ties together the Exchange Layer,
// Strategy Layer, and Risk Management Layer.
//
// Features:
// - Finite State Machine (FSM) for Bot Lifecycle
// - Performance Tracking (PnL, Win Rate)
// - Monadic Pipeline Execution
// - Graceful Error Recovery
// =================================================================================

use crate::monad::{MResult, unit, fail, BotError, Bind, log_info};
use crate::exchange::{ExchangeClient, BinanceClient, OrderType, OrderSide};
use crate::trader::{Strategy, VolumeBreakoutStrategy, RiskManager, TradeInstruction};
use std::sync::Arc;
use std::fmt::Display;

// --- Bot State Machine ---

#[derive(Debug, PartialEq, Clone)]
pub enum BotState {
    Initializing,
    Syncing,
    Trading,
    Paused(String), // Paused with reason
    Terminating,
}

impl Display for BotState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BotState::Initializing => write!(f, "INITIALIZING"),
            BotState::Syncing => write!(f, "SYNCING DATA"),
            BotState::Trading => write!(f, "ACTIVE TRADING"),
            BotState::Paused(r) => write!(f, "PAUSED [{}]", r),
            BotState::Terminating => write!(f, "TERMINATING"),
        }
    }
}

// --- Configuration ---

#[derive(Clone)]
pub struct BotConfig {
    pub symbol: String,
    pub api_key: String,
    pub secret_key: String,
    pub strategy_risk_factor: f64,
}

// --- Performance Metrics ---

pub struct PerformanceTracker {
    trades_executed: u32,
    successful_trades: u32,
    failed_trades: u32,
    total_volume_traded: f64,
    start_time: std::time::SystemTime,
}

impl PerformanceTracker {
    pub fn new() -> Self {
        PerformanceTracker {
            trades_executed: 0,
            successful_trades: 0,
            failed_trades: 0,
            total_volume_traded: 0.0,
            start_time: std::time::SystemTime::now(),
        }
    }

    pub fn record_trade(&mut self, volume: f64) {
        self.trades_executed += 1;
        self.successful_trades += 1;
        self.total_volume_traded += volume;
    }

    pub fn record_error(&mut self) {
        self.failed_trades += 1;
    }

    pub fn get_uptime_secs(&self) -> u64 {
        self.start_time.elapsed().unwrap().as_secs()
    }

    pub fn print_summary(&self) {
        println!("| --- Performance Summary ---");
        println!("| Uptime: {}s", self.get_uptime_secs());
        println!("| Trades Executed: {}", self.trades_executed);
        println!("| Volume Traded: {:.2}", self.total_volume_traded);
        println!("| Failed Attempts: {}", self.failed_trades);
        println!("| ---------------------------");
    }
}

// --- The Engine ---

pub struct TradingEngine {
    state: BotState,
    config: BotConfig,
    client: Arc<dyn ExchangeClient>,
    strategy: Box<dyn Strategy>,
    risk_manager: RiskManager,
    metrics: PerformanceTracker,
}

impl TradingEngine {
    pub fn new(config: BotConfig) -> Self {
        // Factory pattern for initialization
        let client = Arc::new(BinanceClient::new(&config.api_key, &config.secret_key));
        
        // Initializing the specific strategy implementation
        let strategy = Box::new(VolumeBreakoutStrategy::new(2500.0)); // Min 2500 volume
        
        // Initializing risk management with 2% risk per trade and 1.5% stop loss
        let risk_manager = RiskManager::new(0.02, 0.015);

        TradingEngine {
            state: BotState::Initializing,
            config,
            client,
            strategy,
            risk_manager,
            metrics: PerformanceTracker::new(),
        }
    }

    /// The primary execution cycle.
    /// Returns MResult<()> to indicate cycle success or failure.
    pub fn tick(&mut self) -> MResult<()> {
        match self.state {
            BotState::Initializing => self.handle_init(),
            BotState::Syncing => self.handle_sync(),
            BotState::Trading => self.handle_trading(),
            BotState::Paused(_) => self.handle_paused(),
            BotState::Terminating => Ok(()), // Do nothing
        }
    }

    // --- State Handlers ---

    fn handle_init(&mut self) -> MResult<()> {
        log_info("Engine initializing... Verifying exchange connectivity.");
        
        self.client.check_connectivity()
            .bind(|latency| {
                log_info(&format!("Connection OK. Latency: {}ms", latency));
                self.state = BotState::Syncing;
                unit(())
            })
            .catch(|e| {
                log_info(&format!("Init Failed: {:?}", e));
                // In production we might retry or panic. Here we pause.
                self.state = BotState::Paused("Connection Failure".to_string());
                unit(())
            })
    }

    fn handle_sync(&mut self) -> MResult<()> {
        // In a real bot, we would load historical candles here to warm up indicators.
        // For this version, we'll simulate a warm-up by fetching a ticker.
        log_info("Syncing market data and warming up indicators...");
        
        self.client.fetch_ticker(&self.config.symbol)
            .bind(|ticker| {
                self.strategy.process_tick(&ticker)?; // Feed initial data
                log_info("Indicators warmed up.");
                self.state = BotState::Trading;
                unit(())
            })
    }

    fn handle_paused(&mut self) -> MResult<()> {
        // Simple logic to attempt recovery every tick
        log_info("Bot is PAUSED. Attempting recovery...");
        self.state = BotState::Initializing;
        unit(())
    }

    fn handle_trading(&mut self) -> MResult<()> {
        // MONADIC TRADING PIPELINE
        // The core logic flow:
        // 1. Fetch Market Data -> 2. Strategy Analysis -> 3. Risk Calculation -> 4. Execution
        
        let symbol = self.config.symbol.clone();

        let pipeline = self.client.fetch_ticker(&symbol)
            // Step 1: Log price
            .inspect(|ticker| {
                if ticker.timestamp % 10 == 0 { // Reduce log noise
                    println!(">>> [MARKET] {} | Price: {:.2} | Vol: {:.0}", ticker.symbol, ticker.price, ticker.volume_1h);
                }
            })
            
            // Step 2: Strategy Analysis
            .bind(|ticker| {
                // We map the strategy result. If None (No Signal), we stop the chain early via specific error or handle logic
                // Here we return a tuple to keep ticker data for the next step
                match self.strategy.process_tick(&ticker) {
                    Ok(Some(signal)) => unit((ticker, signal)),
                    Ok(None) => fail(BotError::InternalStateError("No Signal".to_string())), // Expected 'failure' to stop chain
                    Err(e) => fail(e)
                }
            })

            // Step 3: Log Signal
            .inspect(|(_, signal)| {
                log_info(&format!("SIGNAL DETECTED: {:?} [{}] Strength: {:.2}", signal.side, signal.reason, signal.strength));
            })

            // Step 4: Risk Management & Balance Check
            .bind(|(ticker, signal)| {
                // We need the balance to calculate position size
                self.client.fetch_balance("USDT")
                    .bind(|balance| {
                        self.risk_manager.calculate_entry(signal, &balance, ticker.price)
                    })
            })

            // Step 5: Execution
            .bind(|instruction| {
                self.execute_instruction(instruction)
            });

        // Pipeline Result Handling
        match pipeline {
            Ok(order_id) => {
                log_info(&format!("Cycle Complete. Order ID: {}", order_id));
                Ok(())
            },
            Err(BotError::InternalStateError(msg)) if msg == "No Signal" => {
                // This is a normal non-event
                Ok(())
            },
            Err(e) => {
                // Real error handling
                log_info(&format!("Cycle Error: {:?}", e));
                self.metrics.record_error();
                Ok(()) // We return Ok to keep the loop running, unless critical
            }
        }
    }

    fn execute_instruction(&self, instr: TradeInstruction) -> MResult<String> {
        println!("\n| $$$EXECUTING TRADE$$$");
        println!("| Symbol: {}", instr.symbol);
        println!("| Side:   {:?}", instr.side);
        println!("| Size:   {}", instr.amount);
        println!("| Price:  {:?}", instr.limit_price);
        println!("| -----------------------");

        let order_type = if instr.limit_price.is_some() { OrderType::Limit } else { OrderType::Market };
        
        // Execute the trade via exchange client
        self.client.execute_order(
            &instr.symbol,
            instr.side,
            order_type,
            instr.amount,
            instr.limit_price
        )
    }

    /// Manually trigger a status report
    pub fn report_status(&self) {
        println!("\n=== ENGINE STATUS REPORT ===");
        println!("State: {}", self.state);
        self.metrics.print_summary();
        println!("============================");
    }
}
