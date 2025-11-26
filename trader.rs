// =================================================================================
// MODULE: Trading Strategy & Risk Management
// DESCRIPTION:
// This module contains the "Brain" of the bot. It implements Technical Analysis
// indicators, Strategy Signal generation, and a rigorous Risk Management layer.
//
// Key components:
// - Technical Indicators (RSI, MA, VWAP)
// - Strategy Interfaces
// - Signal Aggregation
// - Position Sizing (Kelly Criterion / Fixed Fractional)
// =================================================================================

use crate::monad::{MResult, unit, fail, BotError, Bind};
use crate::exchange::{Ticker, OrderSide, Balance};
use std::collections::VecDeque;

// --- Signal & Analysis Structures ---

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum MarketRegime {
    Bullish,
    Bearish,
    Sideways,
    Volatile,
}

#[derive(Debug, Clone)]
pub struct Signal {
    pub symbol: String,
    pub side: OrderSide,
    pub strength: f64, // 0.0 to 1.0
    pub regime: MarketRegime,
    pub timestamp: u64,
    pub reason: String,
}

/// The output of the Risk Manager: A fully validated instruction.
#[derive(Debug)]
pub struct TradeInstruction {
    pub symbol: String,
    pub side: OrderSide,
    pub amount: f64,
    pub limit_price: Option<f64>,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
}

// --- Technical Analysis Components ---

/// Trait for any technical indicator.
pub trait Indicator {
    fn update(&mut self, price: f64);
    fn value(&self) -> Option<f64>;
    fn reset(&mut self);
}

/// Simple Moving Average Implementation.
pub struct SMA {
    period: usize,
    history: VecDeque<f64>,
}

impl SMA {
    pub fn new(period: usize) -> Self {
        SMA {
            period,
            history: VecDeque::with_capacity(period),
        }
    }
}

impl Indicator for SMA {
    fn update(&mut self, price: f64) {
        if self.history.len() >= self.period {
            self.history.pop_front();
        }
        self.history.push_back(price);
    }

    fn value(&self) -> Option<f64> {
        if self.history.len() < self.period {
            return None;
        }
        let sum: f64 = self.history.iter().sum();
        Some(sum / self.period as f64)
    }

    fn reset(&mut self) {
        self.history.clear();
    }
}

/// Relative Strength Index Implementation.
pub struct RSI {
    period: usize,
    gains: VecDeque<f64>,
    losses: VecDeque<f64>,
    prev_price: Option<f64>,
}

impl RSI {
    pub fn new(period: usize) -> Self {
        RSI {
            period,
            gains: VecDeque::new(),
            losses: VecDeque::new(),
            prev_price: None,
        }
    }
}

impl Indicator for RSI {
    fn update(&mut self, price: f64) {
        if let Some(prev) = self.prev_price {
            let change = price - prev;
            let gain = if change > 0.0 { change } else { 0.0 };
            let loss = if change < 0.0 { -change } else { 0.0 };

            if self.gains.len() >= self.period { self.gains.pop_front(); }
            if self.losses.len() >= self.period { self.losses.pop_front(); }

            self.gains.push_back(gain);
            self.losses.push_back(loss);
        }
        self.prev_price = Some(price);
    }

    fn value(&self) -> Option<f64> {
        if self.gains.len() < self.period { return None; }
        
        let avg_gain: f64 = self.gains.iter().sum::<f64>() / self.period as f64;
        let avg_loss: f64 = self.losses.iter().sum::<f64>() / self.period as f64;

        if avg_loss == 0.0 { return Some(100.0); }
        
        let rs = avg_gain / avg_loss;
        Some(100.0 - (100.0 / (1.0 + rs)))
    }

    fn reset(&mut self) {
        self.gains.clear();
        self.losses.clear();
        self.prev_price = None;
    }
}

// --- Strategy Implementation ---

pub trait Strategy {
    fn process_tick(&mut self, ticker: &Ticker) -> MResult<Option<Signal>>;
}

pub struct VolumeBreakoutStrategy {
    sma_short: SMA,
    sma_long: SMA,
    rsi: RSI,
    volume_threshold: f64,
}

impl VolumeBreakoutStrategy {
    pub fn new(vol_threshold: f64) -> Self {
        VolumeBreakoutStrategy {
            sma_short: SMA::new(5), // Fast MA
            sma_long: SMA::new(20), // Slow MA
            rsi: RSI::new(14),
            volume_threshold: vol_threshold,
        }
    }
}

impl Strategy for VolumeBreakoutStrategy {
    fn process_tick(&mut self, ticker: &Ticker) -> MResult<Option<Signal>> {
        // Update Indicators
        self.sma_short.update(ticker.price);
        self.sma_long.update(ticker.price);
        self.rsi.update(ticker.price);

        // Check if we have enough data
        let ma_short_val = match self.sma_short.value() { Some(v) => v, None => return unit(None) };
        let ma_long_val = match self.sma_long.value() { Some(v) => v, None => return unit(None) };
        let rsi_val = match self.rsi.value() { Some(v) => v, None => return unit(None) };

        // 1. Volume Condition
        if ticker.volume_1h < self.volume_threshold {
            return unit(None); // Not enough liquidity
        }

        // 2. Trend Condition (Golden Cross)
        let is_uptrend = ma_short_val > ma_long_val;
        
        // 3. Oscillator Condition
        let is_oversold = rsi_val < 30.0;
        let is_overbought = rsi_val > 70.0;

        // Decision Logic
        if is_uptrend && rsi_val > 50.0 && rsi_val < 70.0 {
            // Trend following BUY
            unit(Some(Signal {
                symbol: ticker.symbol.clone(),
                side: OrderSide::Buy,
                strength: 0.8,
                regime: MarketRegime::Bullish,
                timestamp: ticker.timestamp,
                reason: format!("Golden Cross (S:{:.2}/L:{:.2}) + Vol {:.0}", ma_short_val, ma_long_val, ticker.volume_1h),
            }))
        } else if !is_uptrend && is_overbought {
            // Mean reversion SELL
            unit(Some(Signal {
                symbol: ticker.symbol.clone(),
                side: OrderSide::Sell,
                strength: 0.6,
                regime: MarketRegime::Bearish,
                timestamp: ticker.timestamp,
                reason: format!("Bearish Cross + Overbought RSI {:.2}", rsi_val),
            }))
        } else {
            unit(None)
        }
    }
}

// --- Risk Management Implementation ---

pub struct RiskManager {
    max_account_risk_per_trade: f64, // e.g., 0.01 (1%)
    max_leverage: f64,
    stop_loss_pct: f64,
}

impl RiskManager {
    pub fn new(risk_per_trade: f64, stop_loss: f64) -> Self {
        RiskManager {
            max_account_risk_per_trade: risk_per_trade,
            max_leverage: 1.0, // No leverage for spot
            stop_loss_pct: stop_loss,
        }
    }

    /// Calculates the position size based on account balance and risk parameters.
    /// Utilizes a simplified fixed-fractional money management method.
    pub fn calculate_entry(&self, signal: Signal, balance: &Balance, current_price: f64) -> MResult<TradeInstruction> {
        // Validate Balance
        if balance.free <= 0.0 {
            return fail(BotError::RiskViolation("Insufficient free balance".to_string()));
        }

        // Calculate maximum risk amount in Quote currency
        let risk_amount = balance.free * self.max_account_risk_per_trade * signal.strength;
        
        // Ensure minimum trade size (simplified logic)
        if risk_amount < 10.0 {
             return fail(BotError::RiskViolation("Calculated position size below exchange minimum".to_string()));
        }

        // Calculate Quantity
        let quantity = risk_amount / current_price;

        // Calculate Stop Loss & Take Profit
        let (sl, tp) = match signal.side {
            OrderSide::Buy => (
                current_price * (1.0 - self.stop_loss_pct),
                current_price * (1.0 + (self.stop_loss_pct * 2.0)) // 1:2 Risk/Reward
            ),
            OrderSide::Sell => (
                current_price * (1.0 + self.stop_loss_pct),
                current_price * (1.0 - (self.stop_loss_pct * 2.0))
            ),
        };

        // Construct final instruction
        unit(TradeInstruction {
            symbol: signal.symbol,
            side: signal.side,
            amount: (quantity * 1000.0).round() / 1000.0, // Round to 3 decimals
            limit_price: Some(current_price), // Assuming Limit entry at current price
            stop_loss: Some(sl),
            take_profit: Some(tp),
        })
    }
}
