use std::sync::Arc;

use edr_block_api::{sync::SyncBlock, GenesisBlockFactory, GenesisBlockOptions};
use edr_block_header::{BlockConfig, BlockHeader};
use edr_block_local::EthLocalBlock;
use edr_block_remote::FetchRemoteReceiptError;
use edr_chain_config::ChainConfig;
use edr_chain_l1::{
    block::EthBlockBuilder,
    receipt::L1BlockReceipt,
    rpc::{call::L1CallRequest, transaction::L1RpcTransactionRequest},
    L1ChainSpec,
};
use edr_chain_spec::{
    BlockEnvChainSpec, BlockEnvExt, BlockEnvForHardfork, BlockEnvTrait, ChainSpec,
    ContextChainSpec, HardforkChainSpec, TransactionValidation,
};
use edr_chain_spec_block::BlockChainSpec;
use edr_chain_spec_evm::{
    CfgEnv, Context, ContextForChainSpec, Database, EvmChainSpec, ExecutionResultAndState,
    Inspector, InterpreterResult, Journal, PrecompileProvider, TransactionError,
};
use edr_chain_spec_provider::ProviderChainSpec;
use edr_chain_spec_receipt::ReceiptChainSpec;
use edr_chain_spec_rpc::{RpcBlockChainSpec, RpcChainSpec};
use edr_eip1559::BaseFeeParams;
use edr_eip7892::ScheduledBlobParams;
use edr_primitives::HashMap;
use edr_provider::{time::TimeSinceEpoch, ProviderSpec, TransactionFailureReason};
use edr_receipt::{log::FilterLog, ExecutionReceiptChainSpec};
use edr_state_api::StateDiff;

use crate::{
    eip2718::TypedEnvelope,
    precompiles::{injective_remote_storage_call, InjectivePrecompiles},
    receipt::GenericExecutionReceiptBuilder,
    rpc::{
        block::GenericRpcBlock, receipt::GenericRpcTransactionReceipt,
        transaction::GenericRpcTransactionWithSignature,
    },
    spec::HeaderAndEvmSpecWithFallback,
    GenericChainSpec, InjectiveChainSpec,
};

impl BlockChainSpec for InjectiveChainSpec {
    type Block =
        dyn SyncBlock<Arc<Self::Receipt>, Self::SignedTransaction, Error = Self::FetchReceiptError>;

    type BlockBuilder<'builder, BlockchainErrorT: 'static + std::error::Error + Send + Sync> =
        EthBlockBuilder<
            'builder,
            Self::Receipt,
            Self::Block,
            BlockchainErrorT,
            Self,
            Self::ExecutionReceiptBuilder,
            Self,
            Self::LocalBlock,
        >;

    type FetchReceiptError =
        FetchRemoteReceiptError<<Self::Receipt as TryFrom<Self::RpcReceipt>>::Error>;
}

impl BlockEnvChainSpec for InjectiveChainSpec {
    type BlockEnv<'header, BlockHeaderT>
        = HeaderAndEvmSpecWithFallback<'header, BlockHeaderT>
    where
        BlockHeaderT: 'header + BlockEnvForHardfork<Self::Hardfork>;
}

impl ChainSpec for InjectiveChainSpec {
    type HaltReason = edr_chain_l1::HaltReason;
    type SignedTransaction = crate::transaction::SignedTransactionWithFallbackToPostEip155;
}

impl ContextChainSpec for InjectiveChainSpec {
    type Context = edr_mirror::MirrorContext;
}

impl EvmChainSpec for InjectiveChainSpec {
    type EvmContext<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug> = Context<
        BlockT,
        Self::SignedTransaction,
        CfgEnv<Self::Hardfork>,
        DatabaseT,
        Journal<DatabaseT>,
        Self::Context,
    >;

    type PrecompileProvider<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug> =
        InjectivePrecompiles;

    fn new_precompile_provider<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug>(
        hardfork: Self::Hardfork,
    ) -> Self::PrecompileProvider<BlockT, DatabaseT> {
        InjectivePrecompiles::new(hardfork)
    }

    fn dry_run<
        BlockT: BlockEnvTrait + BlockEnvExt,
        DatabaseT: Database + core::fmt::Debug,
        PrecompileProviderT: PrecompileProvider<
            ContextForChainSpec<Self, BlockT, DatabaseT>,
            Output = InterpreterResult,
        >,
    >(
        block: BlockT,
        cfg: CfgEnv<Self::Hardfork>,
        transaction: Self::SignedTransaction,
        database: DatabaseT,
        precompile_provider: PrecompileProviderT,
        mirror_config: Option<edr_chain_config::NativeTokenMirror>,
    ) -> Result<
        ExecutionResultAndState<Self::HaltReason>,
        TransactionError<
            DatabaseT::Error,
            <Self::SignedTransaction as TransactionValidation>::ValidationError,
        >,
    > {
        <GenericChainSpec as EvmChainSpec>::dry_run(
            block,
            cfg,
            transaction,
            database,
            precompile_provider,
            mirror_config,
        )
    }

    fn dry_run_with_inspector<
        BlockT: BlockEnvTrait + BlockEnvExt,
        DatabaseT: Database + core::fmt::Debug,
        InspectorT: Inspector<ContextForChainSpec<Self, BlockT, DatabaseT>>,
        PrecompileProviderT: PrecompileProvider<
            ContextForChainSpec<Self, BlockT, DatabaseT>,
            Output = InterpreterResult,
        >,
    >(
        block: BlockT,
        cfg: CfgEnv<Self::Hardfork>,
        transaction: Self::SignedTransaction,
        database: DatabaseT,
        precompile_provider: PrecompileProviderT,
        inspector: InspectorT,
        mirror_config: Option<edr_chain_config::NativeTokenMirror>,
    ) -> Result<
        ExecutionResultAndState<Self::HaltReason>,
        TransactionError<
            DatabaseT::Error,
            <Self::SignedTransaction as TransactionValidation>::ValidationError,
        >,
    > {
        <GenericChainSpec as EvmChainSpec>::dry_run_with_inspector(
            block,
            cfg,
            transaction,
            database,
            precompile_provider,
            inspector,
            mirror_config,
        )
    }
}

impl ExecutionReceiptChainSpec for InjectiveChainSpec {
    type ExecutionReceipt<LogT> = TypedEnvelope<edr_receipt::Execution<LogT>>;
}

impl GenesisBlockFactory for InjectiveChainSpec {
    type GenesisBlockCreationError =
        <L1ChainSpec as GenesisBlockFactory>::GenesisBlockCreationError;
    type LocalBlock = EthLocalBlock<
        <Self as ReceiptChainSpec>::Receipt,
        <Self as BlockChainSpec>::FetchReceiptError,
        Self::Hardfork,
        <Self as ChainSpec>::SignedTransaction,
    >;

    fn genesis_block(
        genesis_diff: StateDiff,
        block_config: &BlockConfig<Self::Hardfork>,
        options: GenesisBlockOptions<Self::Hardfork>,
    ) -> Result<Self::LocalBlock, Self::GenesisBlockCreationError> {
        GenericChainSpec::genesis_block(genesis_diff, block_config, options)
    }
}

impl HardforkChainSpec for InjectiveChainSpec {
    type Hardfork = edr_chain_l1::Hardfork;
}

impl ProviderChainSpec for InjectiveChainSpec {
    const MIN_ETHASH_DIFFICULTY: u64 = L1ChainSpec::MIN_ETHASH_DIFFICULTY;

    fn chain_configs() -> &'static HashMap<u64, ChainConfig<Self::Hardfork>> {
        GenericChainSpec::chain_configs()
    }

    fn default_base_fee_params() -> &'static BaseFeeParams<Self::Hardfork> {
        GenericChainSpec::default_base_fee_params()
    }

    fn next_base_fee_per_gas(
        header: &BlockHeader,
        hardfork: Self::Hardfork,
        default_base_fee_params: &BaseFeeParams<Self::Hardfork>,
    ) -> u128 {
        GenericChainSpec::next_base_fee_per_gas(header, hardfork, default_base_fee_params)
    }

    fn default_schedulded_blob_params() -> Option<ScheduledBlobParams> {
        GenericChainSpec::default_schedulded_blob_params()
    }
}

impl ReceiptChainSpec for InjectiveChainSpec {
    type ExecutionReceiptBuilder = GenericExecutionReceiptBuilder;
    type Receipt = L1BlockReceipt<<Self as ExecutionReceiptChainSpec>::ExecutionReceipt<FilterLog>>;
}

impl RpcBlockChainSpec for InjectiveChainSpec {
    type RpcBlock<DataT>
        = GenericRpcBlock<DataT>
    where
        DataT: serde::de::DeserializeOwned + serde::Serialize;
}

impl RpcChainSpec for InjectiveChainSpec {
    type RpcCallRequest = L1CallRequest;
    type RpcReceipt = GenericRpcTransactionReceipt;
    type RpcTransaction = GenericRpcTransactionWithSignature;
    type RpcTransactionRequest = L1RpcTransactionRequest;
}

impl<TimerT: Clone + TimeSinceEpoch> ProviderSpec<TimerT> for InjectiveChainSpec {
    type PooledTransaction = edr_chain_l1::L1PooledTransaction;
    type TransactionRequest = crate::transaction::GenericTransactionRequest<Self>;

    fn cast_halt_reason(reason: Self::HaltReason) -> TransactionFailureReason<Self::HaltReason> {
        <L1ChainSpec as ProviderSpec<TimerT>>::cast_halt_reason(reason)
    }

    fn remote_storage_resolver() -> Option<edr_state_remote::RemoteStorageResolver> {
        Some(injective_remote_storage_call)
    }
}
