// =================================================================================
// MODULE: Functional Core & Error Handling
// DESCRIPTION:
// This module provides the foundational Monadic abstractions used throughout the
// High-Frequency Trading (HFT) bot. It implements a custom Result type wrapper,
// providing "Railway Oriented Programming" capabilities to ensure robust error
// propagation and state management without the "try-catch" spaghetti code.
//
// The goal is to ensure that every operation in the trading pipeline is atomic,
// traceable, and composable.
// =================================================================================

use std::fmt::{Debug, Display, Formatter};
use std::time::{SystemTime, UNIX_EPOCH};

/// A specialized error type for the Trading Bot to categorize failures properly.
#[derive(Clone, PartialEq)]
pub enum BotError {
    NetworkFailure(String),
    StrategyError(String),
    ExchangeError(String),
    RiskViolation(String),
    ConfigurationError(String),
    InternalStateError(String),
}

impl Debug for BotError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            BotError::NetworkFailure(msg) => write!(f, "[NETWORK] {}", msg),
            BotError::StrategyError(msg) => write!(f, "[STRATEGY] {}", msg),
            BotError::ExchangeError(msg) => write!(f, "[EXCHANGE] {}", msg),
            BotError::RiskViolation(msg) => write!(f, "[RISK] {}", msg),
            BotError::ConfigurationError(msg) => write!(f, "[CONFIG] {}", msg),
            BotError::InternalStateError(msg) => write!(f, "[INTERNAL] {}", msg),
        }
    }
}

impl Display for BotError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// The core Monad type alias.
/// Wraps a standard Result but utilizes our specific BotError.
pub type MResult<T> = Result<T, BotError>;

/// The `Bind` trait defines the monadic behavior.
/// In functional programming, this corresponds to `flatMap` or `>>=`.
pub trait Bind<T> {
    /// Transforms the inner value `T` into another `MResult<U>`.
    /// If the current state is Err, the function `f` is skipped.
    fn bind<U, F>(self, f: F) -> MResult<U>
    where
        F: FnOnce(T) -> MResult<U>;

    /// Transforms the inner value `T` into `U` while keeping the context (Result) wrapper.
    /// Standard `map` operation.
    fn map_data<U, F>(self, f: F) -> MResult<U>
    where
        F: FnOnce(T) -> U;

    /// Performs a side-effect (like logging) without consuming or modifying the value.
    /// Essential for debugging monadic chains.
    fn inspect<F>(self, f: F) -> MResult<T>
    where
        F: FnOnce(&T);

    /// A specialized inspect that only runs if the result is an Error.
    fn inspect_err<F>(self, f: F) -> MResult<T>
    where
        F: FnOnce(&BotError);

    /// Recovers from an error state.
    /// If the state is Ok, `f` is skipped.
    /// If the state is Err, `f` allows returning a valid `T` (or a new Error).
    fn catch<F>(self, f: F) -> MResult<T>
    where
        F: FnOnce(BotError) -> MResult<T>;

    /// Asserts a condition on the inner value.
    /// If the predicate returns false, the success is converted to a generic error.
    fn filter_monad<F>(self, predicate: F, error_msg: &str) -> MResult<T>
    where
        F: FnOnce(&T) -> bool;
}

impl<T> Bind<T> for MResult<T> {
    fn bind<U, F>(self, f: F) -> MResult<U>
    where
        F: FnOnce(T) -> MResult<U>,
    {
        match self {
            Ok(val) => f(val),
            Err(e) => Err(e),
        }
    }

    fn map_data<U, F>(self, f: F) -> MResult<U>
    where
        F: FnOnce(T) -> U,
    {
        match self {
            Ok(val) => Ok(f(val)),
            Err(e) => Err(e),
        }
    }

    fn inspect<F>(self, f: F) -> MResult<T>
    where
        F: FnOnce(&T),
    {
        if let Ok(ref val) = self {
            f(val);
        }
        self
    }

    fn inspect_err<F>(self, f: F) -> MResult<T>
    where
        F: FnOnce(&BotError),
    {
        if let Err(ref e) = self {
            f(e);
        }
        self
    }

    fn catch<F>(self, f: F) -> MResult<T>
    where
        F: FnOnce(BotError) -> MResult<T>,
    {
        match self {
            Ok(val) => Ok(val),
            Err(e) => f(e),
        }
    }

    fn filter_monad<F>(self, predicate: F, error_msg: &str) -> MResult<T>
    where
        F: FnOnce(&T) -> bool,
    {
        match self {
            Ok(val) => {
                if predicate(&val) {
                    Ok(val)
                } else {
                    Err(BotError::StrategyError(error_msg.to_string()))
                }
            }
            Err(e) => Err(e),
        }
    }
}

// --- Helper Functions ---

/// Wraps a value into the Monad context (Unit).
pub fn unit<T>(val: T) -> MResult<T> {
    Ok(val)
}

/// Wraps an error message into the Monad failure context.
pub fn fail_msg<T>(msg: &str) -> MResult<T> {
    Err(BotError::InternalStateError(msg.to_string()))
}

/// Helper to wrap a specific error type.
pub fn fail<T>(error: BotError) -> MResult<T> {
    Err(error)
}

/// Utility for logging with timestamps, used often in monadic chains.
pub fn log_info(msg: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    println!("[INFO] [{}] {}", now, msg);
}

/// Utility for combining two Monads.
/// Returns Ok only if both inputs are Ok.
pub fn zip<T, U>(first: MResult<T>, second: MResult<U>) -> MResult<(T, U)> {
    match first {
        Ok(v1) => match second {
            Ok(v2) => Ok((v1, v2)),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    }
}

/// Helper to retry an operation N times.
/// This is a recursive functional retry mechanism.
pub fn retry<T, F>(mut attempts: u32, f: F) -> MResult<T>
where
    F: Fn() -> MResult<T> + Clone,
{
    let result = f();
    match result {
        Ok(v) => Ok(v),
        Err(e) => {
            if attempts <= 1 {
                Err(e)
            } else {
                // In a real async world, we would await a sleep here.
                // For now, we just recurse.
                retry(attempts - 1, f)
            }
        }
    }
}

// =================================================================================
// UNIT TESTS (Simulated)
// =================================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monad_chain() {
        let start = unit(10);
        let result = start
            .bind(|x| unit(x * 2))
            .bind(|x| unit(x + 5));
        
        assert_eq!(result, Ok(25));
    }

    #[test]
    fn test_monad_failure() {
        let start = unit(10);
        let result: MResult<i32> = start
            .bind(|_| fail_msg("Broken chain"))
            .bind(|x| unit(x * 2)); // Should not execute

        match result {
            Err(BotError::InternalStateError(msg)) => assert_eq!(msg, "Broken chain"),
            _ => panic!("Expected InternalStateError"),
        }
    }
}
