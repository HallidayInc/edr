//! Lenient RPC transaction type for Tempo (chain 4217).
//!
//! Tempo's account-abstraction transactions (`type: 0x76`) do not carry the
//! top-level `value`/`input`/`to`/`r`/`s`/`v` fields that the standard L1 RPC
//! transaction requires, and nest the signature under a `signature` object.
//! Forking only reads these blocks, so we parse the AA transactions leniently
//! and fall back to a post-EIP 155 legacy transaction, preserving the
//! authoritative `from`/`hash` via `FakeableSignature::with_address_unchecked`.

use edr_block_api::Block;
use edr_chain_l1::rpc::transaction::{L1RpcTransaction, L1RpcTransactionWithSignature};
use edr_chain_spec_rpc::{RpcTransaction, RpcTypeFrom};
use edr_primitives::{Address, Bytes, B256, U256};
use edr_transaction::TransactionAndBlock;
use serde::{Deserialize, Serialize};

use crate::{
    rpc::transaction::{GenericRpcTransactionConversionError, GenericRpcTransactionWithSignature},
    transaction::SignedTransactionWithFallbackToPostEip155,
};

/// RPC transaction for Tempo, tolerating account-abstraction (`0x76`) txs.
///
/// Standard transactions deserialize into [`Self::Known`] and take the proven
/// generic path unchanged. Account-abstraction transactions fall through to the
/// fully lenient [`Self::Aa`] variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TempoRpcTransaction {
    /// A standard transaction that the generic RPC transaction can parse.
    Known(GenericRpcTransactionWithSignature),
    /// A Tempo account-abstraction (`0x76`) transaction.
    Aa(TempoAaRpcTransaction),
}

// `#[derive(Default)]` cannot target a non-unit enum variant, so we implement it
// manually. Local transactions are never account-abstraction transactions.
impl Default for TempoRpcTransaction {
    fn default() -> Self {
        Self::Known(GenericRpcTransactionWithSignature::default())
    }
}

/// Nested signature object of a Tempo account-abstraction transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoAaSignature {
    /// ECDSA signature r
    #[serde(default)]
    pub r: U256,
    /// ECDSA signature s
    #[serde(default)]
    pub s: U256,
    /// ECDSA recovery id
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "alloy_serde::quantity::opt"
    )]
    pub v: Option<u64>,
    /// Y-parity
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "alloy_serde::quantity::opt"
    )]
    pub y_parity: Option<bool>,
}

/// A Tempo account-abstraction (`0x76`) RPC transaction.
///
/// Only `hash` and `from` are required; every other field is optional so that
/// the missing/nested fields of an AA transaction parse without error. Unknown
/// fields (`feeToken`, `calls`, `nonceKey`, ...) are dropped by serde.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoAaRpcTransaction {
    /// hash of the transaction
    pub hash: B256,
    /// address of the sender
    pub from: Address,
    /// hash of the block where this transaction was in
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_hash: Option<B256>,
    /// block number where this transaction was in
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "alloy_serde::quantity::opt"
    )]
    pub block_number: Option<u64>,
    /// integer of the transaction's index position in the block
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "alloy_serde::quantity::opt"
    )]
    pub transaction_index: Option<u64>,
    /// the number of transactions made by the sender prior to this one
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "alloy_serde::quantity::opt"
    )]
    pub nonce: Option<u64>,
    /// address of the receiver
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Address>,
    /// value transferred in Wei
    #[serde(default)]
    pub value: Option<U256>,
    /// gas price provided by the sender in Wei
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "alloy_serde::quantity::opt"
    )]
    pub gas_price: Option<u128>,
    /// gas provided by the sender
    #[serde(default)]
    pub gas: Option<U256>,
    /// the data sent along with the transaction
    #[serde(default)]
    pub input: Option<Bytes>,
    /// chain ID
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "alloy_serde::quantity::opt"
    )]
    pub chain_id: Option<u64>,
    /// integer of the transaction type
    #[serde(
        rename = "type",
        default,
        skip_serializing_if = "Option::is_none",
        with = "alloy_serde::quantity::opt"
    )]
    pub transaction_type: Option<u8>,
    /// access list
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_list: Option<Vec<edr_eip2930::AccessListItem>>,
    /// max fee per gas
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "alloy_serde::quantity::opt"
    )]
    pub max_fee_per_gas: Option<u128>,
    /// max priority fee per gas
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "alloy_serde::quantity::opt"
    )]
    pub max_priority_fee_per_gas: Option<u128>,
    /// nested signature object
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<TempoAaSignature>,
}

impl TempoAaRpcTransaction {
    /// Synthesizes an [`L1RpcTransactionWithSignature`] from the lenient AA
    /// fields, defaulting missing values so the generic tolerant conversion can
    /// treat the transaction as a post-EIP 155 legacy transaction.
    fn into_l1_with_signature(self) -> L1RpcTransactionWithSignature {
        let transaction = L1RpcTransaction {
            hash: self.hash,
            nonce: self.nonce.unwrap_or_default(),
            block_hash: self.block_hash,
            block_number: self.block_number,
            transaction_index: self.transaction_index,
            from: self.from,
            to: self.to,
            value: self.value.unwrap_or_default(),
            gas_price: self.gas_price.unwrap_or_default(),
            gas: self.gas.unwrap_or_default(),
            input: self.input.unwrap_or_default(),
            chain_id: self.chain_id,
            // Force the unrecognized-type path so the generic conversion falls
            // back to a post-EIP 155 legacy transaction.
            transaction_type: self.transaction_type.or(Some(0x76)),
            access_list: self.access_list,
            max_fee_per_gas: self.max_fee_per_gas,
            max_priority_fee_per_gas: self.max_priority_fee_per_gas,
            max_fee_per_blob_gas: None,
            blob_versioned_hashes: None,
            authorization_list: None,
        };

        let (r, s, v, y_parity) = self
            .signature
            .map(|signature| {
                (
                    signature.r,
                    signature.s,
                    signature.v.unwrap_or_default(),
                    signature.y_parity,
                )
            })
            .unwrap_or_default();

        L1RpcTransactionWithSignature::new(transaction, r, s, v, y_parity)
    }
}

impl RpcTransaction for TempoRpcTransaction {
    fn block_hash(&self) -> Option<&B256> {
        match self {
            Self::Known(transaction) => transaction.block_hash(),
            Self::Aa(transaction) => transaction.block_hash.as_ref(),
        }
    }
}

impl<BlockT: Block<SignedTransactionWithFallbackToPostEip155>>
    RpcTypeFrom<TransactionAndBlock<BlockT, SignedTransactionWithFallbackToPostEip155>>
    for TempoRpcTransaction
{
    type Hardfork = edr_chain_l1::Hardfork;

    fn rpc_type_from(
        value: &TransactionAndBlock<BlockT, SignedTransactionWithFallbackToPostEip155>,
        hardfork: Self::Hardfork,
    ) -> Self {
        // Local transactions are never account-abstraction transactions.
        Self::Known(GenericRpcTransactionWithSignature::rpc_type_from(
            value, hardfork,
        ))
    }
}

impl TryFrom<TempoRpcTransaction> for SignedTransactionWithFallbackToPostEip155 {
    type Error = GenericRpcTransactionConversionError;

    fn try_from(value: TempoRpcTransaction) -> Result<Self, Self::Error> {
        match value {
            TempoRpcTransaction::Known(transaction) => transaction.try_into(),
            TempoRpcTransaction::Aa(transaction) => {
                GenericRpcTransactionWithSignature::from(transaction.into_l1_with_signature())
                    .try_into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use edr_chain_spec::ExecutableTransaction as _;
    use edr_primitives::{address, b256};
    use edr_rpc_client::jsonrpc;

    use super::*;
    use crate::rpc::block::GenericRpcBlock;

    // A real Tempo `0x76` account-abstraction transaction alongside a synthetic
    // standard transaction, in a trimmed `eth_getBlockByNumber` response.
    const DATA: &str = include_str!("../../data/tempo-0x76-block.json");

    #[test]
    fn parses_and_converts_tempo_aa_block() {
        let response: jsonrpc::Response<GenericRpcBlock<TempoRpcTransaction>> =
            serde_json::from_str(DATA).expect("block with a 0x76 transaction should deserialize");

        let block = match response.data {
            jsonrpc::ResponseData::Error { .. } => unreachable!("Payload above is a success"),
            jsonrpc::ResponseData::Success { result } => result,
        };

        assert_eq!(block.transactions.len(), 2);

        // The account-abstraction transaction parses into the lenient variant.
        let aa = &block.transactions[0];
        assert!(matches!(aa, TempoRpcTransaction::Aa(_)));

        // The synthetic standard transaction takes the proven generic path.
        let known = &block.transactions[1];
        assert!(matches!(known, TempoRpcTransaction::Known(_)));

        // Every transaction converts to a signed transaction.
        let signed: Vec<SignedTransactionWithFallbackToPostEip155> = block
            .transactions
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<_, _>>()
            .expect("both transactions should convert");

        // The `0x76` transaction preserves the authoritative `from` and `hash`.
        assert_eq!(
            *signed[0].caller(),
            address!("bc1aa4421f6cb9dfea313f89d5c410f8d4be4bd1")
        );
        assert_eq!(
            *signed[0].transaction_hash(),
            b256!("fe28fc90acb748b9dce632fb6d4cc0c231424f11684e76478b8cff7133ecc060")
        );

        assert_eq!(
            *signed[1].caller(),
            address!("00000000000000000000000000000000000000aa")
        );
        assert_eq!(
            *signed[1].transaction_hash(),
            b256!("1111111111111111111111111111111111111111111111111111111111111111")
        );
    }
}
