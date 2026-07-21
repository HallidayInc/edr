use alloy_rpc_types::eth::Transaction;
use edr_block_api::Block;
use edr_chain_spec::ExecutableTransaction as _;
use edr_chain_spec_rpc::{RpcTransaction, RpcTypeFrom};
use edr_primitives::B256;
use edr_transaction::{BlockDataForTransaction, TransactionAndBlock};
use serde::{Deserialize, Serialize};
use tempo_hardfork::TempoHardfork;
use tempo_primitives::TempoTxEnvelope;

use crate::transaction::TempoSignedTransaction;

/// Tempo's canonical JSON-RPC transaction representation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TempoRpcTransaction(pub Transaction<TempoTxEnvelope>);

impl Default for TempoRpcTransaction {
    fn default() -> Self {
        let transaction = TempoSignedTransaction::default();
        Self(Transaction {
            inner: transaction.recovered().clone(),
            block_hash: None,
            block_number: None,
            transaction_index: None,
            effective_gas_price: None,
            block_timestamp: None,
        })
    }
}

impl RpcTransaction for TempoRpcTransaction {
    fn block_hash(&self) -> Option<&B256> {
        self.0.block_hash.as_ref()
    }
}

impl TryFrom<TempoRpcTransaction> for TempoSignedTransaction {
    type Error = std::convert::Infallible;

    fn try_from(value: TempoRpcTransaction) -> Result<Self, Self::Error> {
        Ok(Self::new(value.0.into_recovered()))
    }
}

impl<BlockT: Block<TempoSignedTransaction>>
    RpcTypeFrom<TransactionAndBlock<BlockT, TempoSignedTransaction>> for TempoRpcTransaction
{
    type Hardfork = TempoHardfork;

    fn rpc_type_from(
        value: &TransactionAndBlock<BlockT, TempoSignedTransaction>,
        _hardfork: Self::Hardfork,
    ) -> Self {
        let (block_hash, block_number, transaction_index, block_timestamp, base_fee) = value
            .block_data
            .as_ref()
            .map(
                |BlockDataForTransaction {
                     block,
                     transaction_index,
                 }| {
                    let header = block.block_header();
                    (
                        Some(*block.block_hash()),
                        Some(header.number),
                        Some(*transaction_index),
                        Some(header.timestamp),
                        header.base_fee_per_gas.unwrap_or_default(),
                    )
                },
            )
            .unwrap_or((None, None, None, None, 0));

        Self(Transaction {
            inner: value.transaction.recovered().clone(),
            block_hash: (!value.is_pending).then_some(block_hash).flatten(),
            block_number: (!value.is_pending).then_some(block_number).flatten(),
            transaction_index: (!value.is_pending).then_some(transaction_index).flatten(),
            effective_gas_price: value.transaction.effective_gas_price(base_fee),
            block_timestamp: (!value.is_pending).then_some(block_timestamp).flatten(),
        })
    }
}
