#![warn(missing_docs)]

//! Utility types and functions used across the EDR codebase.

use std::sync::Arc;

/// Types related to random number generation.
pub mod random;
/// Types related to the Rust type system.
pub mod types;

/// Hook for chain families to adjust gas estimates.
/// Implement this on your chain spec to customize `eth_estimateGas` results
/// without hardcoding chain IDs or introducing dependency cycles.
pub trait GasEstimateAdjuster {
    /// Adjusts an `eth_estimateGas` result (in gas units).
    /// Default implementation returns the input unchanged.
    fn adjust_estimate_gas(estimate: u64) -> u64 {
        estimate
    }
}

/// Trait for casting an `Arc<T>` into an `Arc<Self>`.
pub trait CastArcFrom<T: ?Sized> {
    /// Converts an `Arc<T>` into an `Arc<Self>`.
    fn cast_arc_from(value: Arc<T>) -> Arc<Self>;
}

/// Trait for casting an `Arc<Self>` into an `Arc<T>`.
pub trait CastArcInto<T: ?Sized> {
    /// Converts an `Arc<Self>` into an `Arc<T>`.
    fn cast_arc_into(self: Arc<Self>) -> Arc<T>;
}

impl<T: ?Sized, U: ?Sized + CastArcFrom<T>> CastArcInto<U> for T {
    fn cast_arc_into(self: Arc<Self>) -> Arc<U> {
        U::cast_arc_from(self)
    }
}
