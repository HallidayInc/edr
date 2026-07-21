use alloy_consensus::BlockHeader as _;
use alloy_rpc_types::eth::{Block, BlockTransactions, Header};
use edr_block_api::{BlockAndTotalDifficulty, EthBlockData};
use edr_block_header::{BlobGas, BlockHeader, TempoExecutionMetadata};
use edr_chain_spec::ExecutableTransaction;
use edr_chain_spec_rpc::{GetBlockNumber, RpcEthBlock};
use edr_primitives::{B256, U256};
use serde::{Deserialize, Serialize};
use tempo_alloy::rpc::TempoHeaderResponse;
use tempo_primitives::TempoHeader;

use crate::transaction::TempoSignedTransaction;

/// Tempo's canonical JSON-RPC block representation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TempoRpcBlock<TransactionT>(pub Block<TransactionT, TempoHeaderResponse>);

impl<T> GetBlockNumber for TempoRpcBlock<T> {
    fn number(&self) -> Option<u64> {
        Some(self.0.header.inner.inner.number())
    }
}

impl<T> RpcEthBlock for TempoRpcBlock<T> {
    fn state_root(&self) -> &B256 {
        &self.0.header.inner.inner.inner.state_root
    }

    fn timestamp(&self) -> u64 {
        self.0.header.inner.inner.timestamp()
    }

    fn total_difficulty(&self) -> Option<&U256> {
        self.0.header.inner.total_difficulty.as_ref()
    }
}

/// Error converting a Tempo RPC block into EDR's block representation.
#[derive(Debug, thiserror::Error)]
pub enum TempoRpcBlockConversionError<TransactionErrorT> {
    /// A transaction could not be converted.
    #[error(transparent)]
    Transaction(TransactionErrorT),
}

impl<RpcTransactionT> TryFrom<TempoRpcBlock<RpcTransactionT>>
    for EthBlockData<TempoSignedTransaction>
where
    RpcTransactionT: TryInto<TempoSignedTransaction>,
{
    type Error = TempoRpcBlockConversionError<RpcTransactionT::Error>;

    fn try_from(value: TempoRpcBlock<RpcTransactionT>) -> Result<Self, Self::Error> {
        let TempoRpcBlock(block) = value;
        let transactions = block
            .transactions
            .into_transactions_vec()
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()
            .map_err(TempoRpcBlockConversionError::Transaction)?;

        let rpc_header = block.header.inner;
        let hash = rpc_header.hash;
        let total_size = rpc_header.size.unwrap_or_default();
        let tempo_header = rpc_header.inner;
        let proposer_public_key = tempo_header
            .consensus_context
            .as_ref()
            .map(|context| B256::from(&context.proposer));
        let timestamp_millis_part = tempo_header.timestamp_millis_part;
        let header = tempo_header.inner;

        Ok(Self {
            header: BlockHeader {
                parent_hash: header.parent_hash,
                ommers_hash: header.ommers_hash,
                beneficiary: header.beneficiary,
                state_root: header.state_root,
                transactions_root: header.transactions_root,
                receipts_root: header.receipts_root,
                logs_bloom: header.logs_bloom,
                difficulty: header.difficulty,
                number: header.number,
                gas_limit: header.gas_limit,
                gas_used: header.gas_used,
                timestamp: header.timestamp,
                extra_data: header.extra_data,
                mix_hash: header.mix_hash,
                nonce: header.nonce,
                base_fee_per_gas: header.base_fee_per_gas.map(u128::from),
                withdrawals_root: header.withdrawals_root,
                blob_gas: header.blob_gas_used.and_then(|gas_used| {
                    header.excess_blob_gas.map(|excess_gas| BlobGas {
                        gas_used,
                        excess_gas,
                    })
                }),
                parent_beacon_block_root: header.parent_beacon_block_root,
                requests_hash: header.requests_hash,
                block_access_list_hash: header.block_access_list_hash,
                tempo_execution: Some(TempoExecutionMetadata {
                    timestamp_millis_part,
                    // Both public Tempo networks use this consensus epoch length.
                    epoch_length: std::num::NonZeroU64::new(21_600)
                        .expect("Tempo epoch length is non-zero"),
                    proposer_public_key,
                }),
            },
            transactions,
            ommer_hashes: block.uncles,
            withdrawals: block.withdrawals.map(|withdrawals| withdrawals.0),
            hash,
            rlp_size: total_size.saturating_to(),
        })
    }
}

impl<BlockT, SignedTransactionT> From<BlockAndTotalDifficulty<BlockT, SignedTransactionT>>
    for TempoRpcBlock<B256>
where
    BlockT: edr_block_api::Block<SignedTransactionT>,
    SignedTransactionT: ExecutableTransaction,
{
    fn from(value: BlockAndTotalDifficulty<BlockT, SignedTransactionT>) -> Self {
        let block_header = value.block.block_header();
        let metadata = block_header.tempo_execution;
        let timestamp_millis_part = metadata.map_or(0, |metadata| metadata.timestamp_millis_part);
        let inner = alloy_consensus::Header {
            parent_hash: block_header.parent_hash,
            ommers_hash: block_header.ommers_hash,
            beneficiary: block_header.beneficiary,
            state_root: block_header.state_root,
            transactions_root: block_header.transactions_root,
            receipts_root: block_header.receipts_root,
            logs_bloom: block_header.logs_bloom,
            difficulty: block_header.difficulty,
            number: block_header.number,
            gas_limit: block_header.gas_limit,
            gas_used: block_header.gas_used,
            timestamp: block_header.timestamp,
            extra_data: block_header.extra_data.clone(),
            mix_hash: block_header.mix_hash,
            nonce: block_header.nonce,
            base_fee_per_gas: block_header
                .base_fee_per_gas
                .map(|fee| fee.try_into().expect("base fee must fit into u64")),
            withdrawals_root: block_header.withdrawals_root,
            blob_gas_used: block_header.blob_gas.as_ref().map(|blob| blob.gas_used),
            excess_blob_gas: block_header.blob_gas.as_ref().map(|blob| blob.excess_gas),
            parent_beacon_block_root: block_header.parent_beacon_block_root,
            requests_hash: block_header.requests_hash,
            block_access_list_hash: block_header.block_access_list_hash,
            ..Default::default()
        };
        let tempo_header = TempoHeader {
            general_gas_limit: 0,
            shared_gas_limit: 0,
            timestamp_millis_part,
            inner,
            consensus_context: None,
        };
        let rpc_header = Header {
            hash: *value.block.block_hash(),
            inner: tempo_header,
            total_difficulty: value.total_difficulty,
            size: Some(U256::from(value.block.rlp_size())),
        };
        let header = TempoHeaderResponse {
            timestamp_millis: block_header
                .timestamp
                .saturating_mul(1000)
                .saturating_add(timestamp_millis_part),
            inner: rpc_header,
        };
        let transactions = BlockTransactions::Hashes(
            value
                .block
                .transactions()
                .iter()
                .map(|transaction| *transaction.transaction_hash())
                .collect(),
        );

        Self(Block {
            header,
            uncles: value.block.ommer_hashes().to_vec(),
            transactions,
            withdrawals: value
                .block
                .withdrawals()
                .map(|withdrawals| alloy_rpc_types::eth::Withdrawals(withdrawals.to_vec())),
        })
    }
}
