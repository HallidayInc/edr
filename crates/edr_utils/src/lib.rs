#![warn(missing_docs)]

//! Shared utilities and markers used across EDR crates.

// Note: Marker traits for chain families are intentionally omitted.
// Use hooks like `GasEstimateAdjuster` to customize behavior instead of
// compile-time flags for specific stacks.

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

/// Generic type-constructor trait used across crates to associate concrete types
/// based on an input marker type.
pub mod types {
    /// A generic type constructor: given a marker type `T`, provides an
    /// associated `Type`.
    pub trait TypeConstructor<T> {
        /// The constructed type associated with `T`.
        type Type;
    }
}
