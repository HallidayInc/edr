use std::{
    num::NonZeroU64,
    sync::{Arc, LazyLock},
};

use edr_block_api::{sync::SyncBlock, GenesisBlockFactory, GenesisBlockOptions};
use edr_block_header::{BlockConfig, BlockHeader};
use edr_block_local::EthLocalBlock;
use edr_block_remote::FetchRemoteReceiptError;
use edr_chain_config::{ChainConfig, ForkCondition, HardforkActivation, HardforkActivations};
use edr_chain_l1::{
    block::EthBlockBuilder, rpc::transaction::L1RpcTransactionRequest, L1_GENESIS_BLOCK_EXTRA_DATA,
};
use edr_chain_spec::{
    BlockEnvChainSpec, BlockEnvConstructor, BlockEnvExt, BlockEnvForHardfork, BlockEnvTrait,
    ChainSpec, ContextChainSpec, HardforkChainSpec,
};
use edr_chain_spec_block::BlockChainSpec;
use edr_chain_spec_evm::{
    result::EVMError, CfgEnv, Context, ContextForChainSpec, Database, EvmChainSpec,
    ExecuteEvm as _, ExecutionResultAndState, Inspector, InterpreterResult, Journal,
    JournalTrait as _, LocalContext, NoOpInspector, PrecompileProvider, TransactionError,
};
use edr_chain_spec_provider::ProviderChainSpec;
use edr_chain_spec_receipt::ReceiptChainSpec;
use edr_chain_spec_rpc::{RpcBlockChainSpec, RpcChainSpec};
use edr_eip1559::BaseFeeParams;
use edr_eip7892::ScheduledBlobParams;
use edr_primitives::{Bytes, HashMap, B256, U256};
use edr_provider::{time::TimeSinceEpoch, ProviderSpec, TransactionFailureReason};
use edr_receipt::{log::FilterLog, ExecutionReceiptChainSpec};
use edr_state_api::StateDiff;
use revm_context::BlockEnv;
use tempo_hardfork::constants::gas::{
    tempo_t7_next_block_base_fee, TEMPO_T0_BASE_FEE, TEMPO_T1_BASE_FEE, TEMPO_T7_BASE_FEE_PARAMS,
};
use tempo_hardfork::TempoHardfork;
use tempo_revm::{
    gas_params::tempo_gas_params, TempoBlockEnv as NativeTempoBlockEnv, TempoEvm,
    TempoHaltReason as NativeTempoHaltReason, TempoInvalidTransaction,
};

use crate::{
    eip2718::TypedEnvelope,
    receipt::GenericExecutionReceiptBuilder,
    rpc::{
        receipt::TempoRpcTransactionReceipt, tempo_block::TempoRpcBlock,
        tempo_transaction::TempoRpcTransaction,
    },
    transaction::{TempoCallRequest, TempoSignedTransaction, TempoTransactionRequest},
    TempoChainSpec,
};

const TEMPO_EPOCH_LENGTH: NonZeroU64 = NonZeroU64::new(21_600).unwrap();
const TEMPO_MAINNET_CHAIN_ID: u64 = 4217;
const TEMPO_TESTNET_CHAIN_ID: u64 = 42431;

/// Serializable adapter for Tempo's native halt reason.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum TempoHaltReason {
    /// Standard EVM halt.
    Ethereum(edr_chain_spec::EvmHaltReason),
    /// A subblock transaction failed to pay its fee.
    SubblockTxFeePayment,
}

impl From<edr_chain_spec::EvmHaltReason> for TempoHaltReason {
    fn from(value: edr_chain_spec::EvmHaltReason) -> Self {
        Self::Ethereum(value)
    }
}

impl From<NativeTempoHaltReason> for TempoHaltReason {
    fn from(value: NativeTempoHaltReason) -> Self {
        match value {
            NativeTempoHaltReason::Ethereum(reason) => Self::Ethereum(reason),
            NativeTempoHaltReason::SubblockTxFeePayment => Self::SubblockTxFeePayment,
        }
    }
}

/// EDR block environment backed by Tempo's native block environment.
#[derive(Clone, Debug)]
pub struct TempoBlockEnv(NativeTempoBlockEnv);

impl<'header, HeaderT> BlockEnvConstructor<'header, TempoHardfork, &'header HeaderT>
    for TempoBlockEnv
where
    HeaderT: BlockEnvForHardfork<TempoHardfork>,
{
    fn new_block_env(
        header: &'header HeaderT,
        hardfork: TempoHardfork,
        scheduled_blob_params: Option<&'header ScheduledBlobParams>,
    ) -> Self {
        let proposer_public_key = header
            .proposer_public_key_for_hardfork(hardfork)
            .and_then(|key| tempo_primitives::ed25519::PublicKey::try_from(key).ok());

        Self(NativeTempoBlockEnv {
            inner: BlockEnv {
                number: header.number_for_hardfork(hardfork),
                beneficiary: header.beneficiary_for_hardfork(hardfork),
                timestamp: header.timestamp_for_hardfork(hardfork),
                gas_limit: header.gas_limit_for_hardfork(hardfork),
                basefee: header.basefee_for_hardfork(hardfork),
                difficulty: header.difficulty_for_hardfork(hardfork),
                prevrandao: header.prevrandao_for_hardfork(hardfork),
                blob_excess_gas_and_price: header
                    .blob_excess_gas_and_price_for_hardfork(hardfork, scheduled_blob_params),
                slot_num: 0,
            },
            timestamp_millis_part: header.timestamp_millis_part_for_hardfork(hardfork),
            epoch_length: {
                let value = header.epoch_length_for_hardfork(hardfork);
                if value == NonZeroU64::MIN {
                    TEMPO_EPOCH_LENGTH
                } else {
                    value
                }
            },
            proposer_public_key,
        })
    }
}

impl BlockEnvTrait for TempoBlockEnv {
    fn number(&self) -> U256 {
        self.0.number()
    }

    fn beneficiary(&self) -> edr_primitives::Address {
        self.0.beneficiary()
    }

    fn timestamp(&self) -> U256 {
        self.0.timestamp()
    }

    fn gas_limit(&self) -> u64 {
        self.0.gas_limit()
    }

    fn basefee(&self) -> u64 {
        self.0.basefee()
    }

    fn difficulty(&self) -> U256 {
        self.0.difficulty()
    }

    fn prevrandao(&self) -> Option<B256> {
        self.0.prevrandao()
    }

    fn blob_excess_gas_and_price(&self) -> Option<edr_chain_spec::BlobExcessGasAndPrice> {
        self.0.blob_excess_gas_and_price()
    }
}

impl BlockEnvExt for TempoBlockEnv {
    fn timestamp_millis_part(&self) -> u64 {
        self.0.timestamp_millis_part
    }

    fn epoch_length(&self) -> NonZeroU64 {
        self.0.epoch_length
    }

    fn proposer_public_key(&self) -> Option<B256> {
        self.0.proposer_public_key.as_ref().map(B256::from)
    }
}

impl BlockChainSpec for TempoChainSpec {
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

impl BlockEnvChainSpec for TempoChainSpec {
    type BlockEnv<'header, HeaderT>
        = TempoBlockEnv
    where
        HeaderT: 'header + BlockEnvForHardfork<Self::Hardfork>;
}

impl ChainSpec for TempoChainSpec {
    type HaltReason = TempoHaltReason;
    type SignedTransaction = TempoSignedTransaction;
}

impl ContextChainSpec for TempoChainSpec {
    type Context = ();
}

fn configure_tempo(mut cfg: CfgEnv<TempoHardfork>) -> CfgEnv<TempoHardfork> {
    cfg.gas_params = tempo_gas_params(cfg.spec);
    cfg.tx_chain_id_check = true;
    if cfg.tx_gas_limit_cap.is_none() {
        cfg.tx_gas_limit_cap = cfg.spec.tx_gas_limit_cap();
    }
    cfg
}

fn map_tempo_error<DatabaseErrorT>(
    error: EVMError<DatabaseErrorT, TempoInvalidTransaction>,
) -> TransactionError<DatabaseErrorT, TempoInvalidTransaction> {
    match error {
        EVMError::Custom(error) => TransactionError::Custom(error),
        EVMError::CustomAny(error) => TransactionError::CustomAny(error),
        EVMError::Database(error) => TransactionError::Database(error),
        EVMError::Header(error) => TransactionError::InvalidHeader(error),
        EVMError::Transaction(TempoInvalidTransaction::EthInvalidTransaction(
            edr_chain_spec::EvmTransactionValidationError::LackOfFundForMaxFee { fee, balance },
        )) => TransactionError::LackOfFundForMaxFee { fee, balance },
        EVMError::Transaction(error) => TransactionError::InvalidTransaction(error),
    }
}

impl EvmChainSpec for TempoChainSpec {
    type EvmContext<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug> =
        tempo_revm::evm::TempoContext<DatabaseT>;

    type PrecompileProvider<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug> =
        revm_handler::EthPrecompiles;

    fn new_precompile_provider<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug>(
        hardfork: Self::Hardfork,
    ) -> Self::PrecompileProvider<BlockT, DatabaseT> {
        // `TempoEvm` installs the canonical Tempo precompiles itself. This
        // provider only satisfies EDR's generic execution interface and is
        // deliberately ignored by both execution methods below.
        revm_handler::EthPrecompiles::new(hardfork.into())
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
        _precompile_provider: PrecompileProviderT,
        _mirror_config: Option<edr_chain_config::NativeTokenMirror>,
    ) -> Result<
        ExecutionResultAndState<Self::HaltReason>,
        TransactionError<DatabaseT::Error, TempoInvalidTransaction>,
    > {
        let block = NativeTempoBlockEnv {
            inner: BlockEnv {
                number: block.number(),
                beneficiary: block.beneficiary(),
                timestamp: block.timestamp(),
                gas_limit: block.gas_limit(),
                basefee: block.basefee(),
                difficulty: block.difficulty(),
                prevrandao: block.prevrandao(),
                blob_excess_gas_and_price: block.blob_excess_gas_and_price(),
                slot_num: 0,
            },
            timestamp_millis_part: block.timestamp_millis_part(),
            epoch_length: block.epoch_length(),
            proposer_public_key: block
                .proposer_public_key()
                .and_then(|key| tempo_primitives::ed25519::PublicKey::try_from(key).ok()),
        };
        let context = Context {
            block,
            tx: transaction.tempo_tx_env(),
            cfg: configure_tempo(cfg),
            journaled_state: Journal::new(database),
            chain: (),
            local: LocalContext::default(),
            error: Ok(()),
        };
        TempoEvm::new(context, NoOpInspector)
            .replay()
            .map(|result| {
                ExecutionResultAndState::new(
                    result.result.map_haltreason(TempoHaltReason::from),
                    result.state,
                )
            })
            .map_err(map_tempo_error)
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
        _precompile_provider: PrecompileProviderT,
        inspector: InspectorT,
        _mirror_config: Option<edr_chain_config::NativeTokenMirror>,
    ) -> Result<
        ExecutionResultAndState<Self::HaltReason>,
        TransactionError<DatabaseT::Error, TempoInvalidTransaction>,
    > {
        let block = NativeTempoBlockEnv {
            inner: BlockEnv {
                number: block.number(),
                beneficiary: block.beneficiary(),
                timestamp: block.timestamp(),
                gas_limit: block.gas_limit(),
                basefee: block.basefee(),
                difficulty: block.difficulty(),
                prevrandao: block.prevrandao(),
                blob_excess_gas_and_price: block.blob_excess_gas_and_price(),
                slot_num: 0,
            },
            timestamp_millis_part: block.timestamp_millis_part(),
            epoch_length: block.epoch_length(),
            proposer_public_key: block
                .proposer_public_key()
                .and_then(|key| tempo_primitives::ed25519::PublicKey::try_from(key).ok()),
        };
        let context = Context {
            block,
            tx: transaction.tempo_tx_env(),
            cfg: configure_tempo(cfg),
            journaled_state: Journal::new(database),
            chain: (),
            local: LocalContext::default(),
            error: Ok(()),
        };
        TempoEvm::new(context, inspector)
            .replay()
            .map(|result| {
                ExecutionResultAndState::new(
                    result.result.map_haltreason(TempoHaltReason::from),
                    result.state,
                )
            })
            .map_err(map_tempo_error)
    }
}

impl ExecutionReceiptChainSpec for TempoChainSpec {
    type ExecutionReceipt<LogT> = TypedEnvelope<edr_receipt::Execution<LogT>>;
}

impl GenesisBlockFactory for TempoChainSpec {
    type GenesisBlockCreationError =
        <edr_chain_l1::L1ChainSpec as GenesisBlockFactory>::GenesisBlockCreationError;
    type LocalBlock = EthLocalBlock<
        <Self as ReceiptChainSpec>::Receipt,
        <Self as BlockChainSpec>::FetchReceiptError,
        Self::Hardfork,
        <Self as ChainSpec>::SignedTransaction,
    >;

    fn genesis_block(
        genesis_diff: StateDiff,
        block_config: &BlockConfig<Self::Hardfork>,
        mut options: GenesisBlockOptions<Self::Hardfork>,
    ) -> Result<Self::LocalBlock, Self::GenesisBlockCreationError> {
        options.extra_data = Some(
            options
                .extra_data
                .unwrap_or(Bytes::copy_from_slice(L1_GENESIS_BLOCK_EXTRA_DATA)),
        );
        EthLocalBlock::with_genesis_state(genesis_diff.into(), block_config, options)
    }
}

impl HardforkChainSpec for TempoChainSpec {
    type Hardfork = TempoHardfork;
}

fn tempo_chain_config(chain_id: u64, name: &str) -> ChainConfig<TempoHardfork> {
    let hardfork_activations = TempoHardfork::VARIANTS
        .iter()
        .filter_map(|hardfork| {
            let timestamp = match chain_id {
                TEMPO_MAINNET_CHAIN_ID => hardfork.mainnet_activation_timestamp(),
                TEMPO_TESTNET_CHAIN_ID => hardfork.moderato_activation_timestamp(),
                _ => None,
            }?;
            Some(HardforkActivation {
                condition: ForkCondition::Timestamp(timestamp),
                hardfork: *hardfork,
            })
        })
        .collect();

    ChainConfig {
        name: name.to_owned(),
        hardfork_activations: HardforkActivations::new(hardfork_activations),
        base_fee_params: TEMPO_T7_BASE_FEE_PARAMS.into(),
        bpo_hardfork_schedule: None,
        native_token_mirror: None,
    }
}

static TEMPO_CHAIN_CONFIGS: LazyLock<HashMap<u64, ChainConfig<TempoHardfork>>> =
    LazyLock::new(|| {
        [
            (
                TEMPO_MAINNET_CHAIN_ID,
                tempo_chain_config(TEMPO_MAINNET_CHAIN_ID, "Tempo Mainnet"),
            ),
            (
                TEMPO_TESTNET_CHAIN_ID,
                tempo_chain_config(TEMPO_TESTNET_CHAIN_ID, "Tempo Moderato"),
            ),
        ]
        .into_iter()
        .collect()
    });
static TEMPO_BASE_FEE_PARAMS: BaseFeeParams<TempoHardfork> =
    BaseFeeParams::Constant(TEMPO_T7_BASE_FEE_PARAMS);

impl ProviderChainSpec for TempoChainSpec {
    const MIN_ETHASH_DIFFICULTY: u64 = 0;

    fn chain_configs() -> &'static HashMap<u64, ChainConfig<Self::Hardfork>> {
        &TEMPO_CHAIN_CONFIGS
    }

    fn default_base_fee_params() -> &'static BaseFeeParams<Self::Hardfork> {
        &TEMPO_BASE_FEE_PARAMS
    }

    fn next_base_fee_per_gas(
        header: &BlockHeader,
        hardfork: Self::Hardfork,
        _default_base_fee_params: &BaseFeeParams<Self::Hardfork>,
    ) -> u128 {
        if hardfork.is_t7() {
            tempo_t7_next_block_base_fee(
                header
                    .base_fee_per_gas
                    .expect("Tempo blocks must have a base fee")
                    .try_into()
                    .expect("Tempo base fee must fit in u64"),
                header.gas_used,
            )
            .into()
        } else if hardfork.is_t1() {
            TEMPO_T1_BASE_FEE.into()
        } else {
            TEMPO_T0_BASE_FEE.into()
        }
    }

    fn default_schedulded_blob_params() -> Option<ScheduledBlobParams> {
        None
    }
}

impl ReceiptChainSpec for TempoChainSpec {
    type ExecutionReceiptBuilder = GenericExecutionReceiptBuilder;
    type Receipt = crate::receipt::TempoBlockReceipt<Self::ExecutionReceipt<FilterLog>>;
}

impl RpcBlockChainSpec for TempoChainSpec {
    type RpcBlock<DataT>
        = TempoRpcBlock<DataT>
    where
        DataT: serde::de::DeserializeOwned + serde::Serialize;
}

impl RpcChainSpec for TempoChainSpec {
    type RpcCallRequest = TempoCallRequest;
    type RpcReceipt = TempoRpcTransactionReceipt;
    type RpcTransaction = TempoRpcTransaction;
    type RpcTransactionRequest = L1RpcTransactionRequest;
}

impl<TimerT: Clone + TimeSinceEpoch> ProviderSpec<TimerT> for TempoChainSpec {
    type PooledTransaction = TempoSignedTransaction;
    type TransactionRequest = TempoTransactionRequest;

    fn cast_halt_reason(reason: Self::HaltReason) -> TransactionFailureReason<Self::HaltReason> {
        match reason {
            TempoHaltReason::Ethereum(edr_chain_spec::EvmHaltReason::CreateContractSizeLimit) => {
                TransactionFailureReason::CreateContractSizeLimit
            }
            TempoHaltReason::Ethereum(
                edr_chain_spec::EvmHaltReason::OpcodeNotFound
                | edr_chain_spec::EvmHaltReason::InvalidFEOpcode,
            ) => TransactionFailureReason::OpcodeNotFound,
            TempoHaltReason::Ethereum(edr_chain_spec::EvmHaltReason::OutOfGas(error)) => {
                TransactionFailureReason::OutOfGas(error)
            }
            reason => TransactionFailureReason::Inner(reason),
        }
    }
}
