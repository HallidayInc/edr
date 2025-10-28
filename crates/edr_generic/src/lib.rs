//! A slightly more flexible chain specification for Ethereum Layer 1 chain.

mod eip2718;
mod receipt;
mod rpc;
mod spec;
mod transaction;

/// Generic chain type
pub const CHAIN_TYPE: &str = "generic";
/// Arbitrum chain type
pub const ARB_CHAIN_TYPE: &str = "arb";

/// The chain specification for Ethereum Layer 1 that is a bit more lenient
/// and allows for more flexibility in contrast to
/// [`L1ChainSpec`](edr_chain_l1::L1ChainSpec).
///
/// Specifically:
/// - it allows unknown transaction types (treats them as legacy
///   [`Eip155`](edr_transaction::signed::Eip155) transactions)
/// - it allows remote blocks with missing `nonce` and `mix_hash` fields
/// - it allows missing `blob_gas` fields in Cancun or above
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, alloy_rlp::RlpEncodable)]
pub struct GenericChainSpec;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, alloy_rlp::RlpEncodable)]
pub struct ArbChainSpec;

pub trait GenericChainSpecFamily: Copy + Default {}

impl GenericChainSpecFamily for GenericChainSpec {}
impl GenericChainSpecFamily for ArbChainSpec {}

impl edr_utils::GasEstimateAdjuster for GenericChainSpec {
    fn adjust_estimate_gas(estimate: u64) -> u64 {
        estimate
    }
}

impl edr_utils::GasEstimateAdjuster for ArbChainSpec {
    fn adjust_estimate_gas(estimate: u64) -> u64 {
        const MIN_BUFFER: u64 = 25_000;
        let fractional_buffer = estimate / 5; // 20%
        let buffer = fractional_buffer.max(MIN_BUFFER);
        estimate.saturating_add(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::{ArbChainSpec, GenericChainSpec};

    fn adjust_generic(estimate: u64) -> u64 {
        <GenericChainSpec as edr_utils::GasEstimateAdjuster>::adjust_estimate_gas(estimate)
    }

    fn adjust_arb(estimate: u64) -> u64 {
        <ArbChainSpec as edr_utils::GasEstimateAdjuster>::adjust_estimate_gas(estimate)
    }

    #[test]
    fn generic_estimates_are_unmodified() {
        let estimate = 40_000;
        assert_eq!(adjust_generic(estimate), estimate);
    }

    #[test]
    fn arb_applies_minimum_buffer_for_small_estimates() {
        let estimate = 40_000;
        assert_eq!(adjust_arb(estimate), estimate + 25_000);
    }

    #[test]
    fn arb_scales_buffer_with_estimate() {
        let estimate = 500_000;
        assert_eq!(adjust_arb(estimate), estimate + 100_000);
    }

    #[test]
    fn arb_saturates_on_large_inputs() {
        let estimate = u64::MAX - 10_000;
        assert_eq!(adjust_arb(estimate), u64::MAX);
    }
}
