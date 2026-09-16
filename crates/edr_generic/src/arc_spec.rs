use std::sync::{Arc, LazyLock};

use edr_block_api::{sync::SyncBlock, GenesisBlockFactory, GenesisBlockOptions};
use edr_block_header::{
    calculate_next_base_fee_per_gas, BlockConfig, BlockHeader, HeaderAndEvmSpec,
};
use edr_block_local::EthLocalBlock;
use edr_block_remote::FetchRemoteReceiptError;
use edr_chain_config::{ChainConfig, ForkCondition, HardforkActivation, HardforkActivations};
use edr_chain_l1::{
    rpc::{call::L1CallRequest, transaction::L1RpcTransactionRequest},
    L1ChainSpec,
};
use edr_chain_spec::{
    BlockEnvChainSpec, BlockEnvExt, BlockEnvForHardfork, BlockEnvTrait, ChainSpec,
    ContextChainSpec, HardforkChainSpec, TransactionValidation,
};
use edr_chain_spec_block::BlockChainSpec;
use edr_chain_spec_evm::{
    CfgEnv, Context, ContextForChainSpec, Database, EvmChainSpec, ExecuteEvm as _,
    ExecutionResultAndState, InspectEvm as _, Inspector, InterpreterResult, Journal,
    JournalTrait as _, LocalContext, PrecompileProvider, TransactionError,
};
use edr_chain_spec_provider::ProviderChainSpec;
use edr_chain_spec_receipt::ReceiptChainSpec;
use edr_chain_spec_rpc::{RpcBlockChainSpec, RpcChainSpec};
use edr_eip1559::{BaseFeeParams, ConstantBaseFeeParams};
use edr_eip7892::ScheduledBlobParams;
use edr_primitives::HashMap;
use edr_provider::{time::TimeSinceEpoch, ProviderSpec, TransactionFailureReason};
use edr_receipt::{log::FilterLog, ExecutionReceiptChainSpec};
use edr_state_api::StateDiff;

use crate::{
    arc_block_builder::ArcBlockBuilder,
    arc_evm::{build_arc_instructions, ArcEvm},
    eip2718::TypedEnvelope,
    precompiles::ArcPrecompiles,
    receipt::{ArcBlockReceipt, GenericExecutionReceiptBuilder},
    rpc::{
        block::GenericRpcBlock, receipt::ArcRpcTransactionReceipt,
        transaction::ArcRpcTransactionWithSignature,
    },
    ArcChainSpec, ArcHardfork, GenericChainSpec,
};

const ARC_BASE_FEE_PARAMS: BaseFeeParams<ArcHardfork> =
    BaseFeeParams::Constant(ConstantBaseFeeParams {
        max_change_denominator: 50,
        elasticity_multiplier: 2,
    });
const ARC_MIN_BASE_FEE: u128 = 1;
const ARC_MAX_BASE_FEE: u128 = u64::MAX as u128 - 1;

impl BlockChainSpec for ArcChainSpec {
    type Block =
        dyn SyncBlock<Arc<Self::Receipt>, Self::SignedTransaction, Error = Self::FetchReceiptError>;

    type BlockBuilder<'builder, BlockchainErrorT: 'static + std::error::Error + Send + Sync> =
        ArcBlockBuilder<'builder, BlockchainErrorT>;

    type FetchReceiptError =
        FetchRemoteReceiptError<<Self::Receipt as TryFrom<Self::RpcReceipt>>::Error>;
}

impl BlockEnvChainSpec for ArcChainSpec {
    type BlockEnv<'header, BlockHeaderT>
        = HeaderAndEvmSpec<'header, BlockHeaderT, ArcHardfork>
    where
        BlockHeaderT: 'header + BlockEnvForHardfork<Self::Hardfork>;
}

impl ChainSpec for ArcChainSpec {
    type HaltReason = edr_chain_l1::HaltReason;
    type SignedTransaction = crate::transaction::SignedTransactionWithFallbackToPostEip155;
}

impl ContextChainSpec for ArcChainSpec {
    type Context = edr_mirror::MirrorContext;
}

impl EvmChainSpec for ArcChainSpec {
    type EvmContext<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug> = Context<
        BlockT,
        Self::SignedTransaction,
        CfgEnv<Self::Hardfork>,
        DatabaseT,
        Journal<DatabaseT>,
        Self::Context,
    >;

    type PrecompileProvider<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug> =
        ArcPrecompiles;

    fn new_precompile_provider<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug>(
        hardfork: Self::Hardfork,
    ) -> Self::PrecompileProvider<BlockT, DatabaseT> {
        ArcPrecompiles::new(hardfork)
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
        let hardfork = cfg.spec;
        let context = Context {
            block,
            tx: transaction,
            journaled_state: Journal::new(database),
            cfg,
            chain: edr_mirror::MirrorContext::new(mirror_config),
            local: LocalContext::default(),
            error: Ok(()),
        };
        let mut evm = ArcEvm::new(
            context,
            hardfork,
            build_arc_instructions(hardfork),
            precompile_provider,
        );

        evm.replay().map_err(TransactionError::from)
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
        let hardfork = cfg.spec;
        let context = Context {
            block,
            tx: Self::SignedTransaction::default(),
            journaled_state: Journal::new(database),
            cfg,
            chain: edr_mirror::MirrorContext::new(mirror_config),
            local: LocalContext::default(),
            error: Ok(()),
        };
        let mut evm = ArcEvm::new_with_inspector(
            context,
            inspector,
            hardfork,
            build_arc_instructions(hardfork),
            precompile_provider,
        );

        evm.inspect_tx(transaction).map_err(TransactionError::from)
    }
}

impl ExecutionReceiptChainSpec for ArcChainSpec {
    type ExecutionReceipt<LogT> = TypedEnvelope<edr_receipt::Execution<LogT>>;
}

impl GenesisBlockFactory for ArcChainSpec {
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
        EthLocalBlock::with_genesis_state(genesis_diff.into(), block_config, options)
    }
}

impl HardforkChainSpec for ArcChainSpec {
    type Hardfork = ArcHardfork;
}

const ARC_MAINNET_CHAIN_ID: u64 = 5_042;
const ARC_DEVNET_CHAIN_ID: u64 = 5_042_001;
const ARC_TESTNET_CHAIN_ID: u64 = 5_042_002;
const ARC_LOCAL_CHAIN_ID: u64 = 1_337;

fn arc_chain_config(
    name: &str,
    activations: Vec<HardforkActivation<ArcHardfork>>,
) -> ChainConfig<ArcHardfork> {
    ChainConfig {
        name: name.to_owned(),
        hardfork_activations: HardforkActivations::new(activations),
        base_fee_params: ARC_BASE_FEE_PARAMS.clone(),
        bpo_hardfork_schedule: None,
        native_token_mirror: None,
    }
}

fn arc_hardfork_at(
    activations: &[HardforkActivation<ArcHardfork>],
    block_number: u64,
    block_timestamp: u64,
) -> Option<ArcHardfork> {
    Some(ArcHardfork::combine(activations.iter().filter_map(
        |activation| {
            let active = match activation.condition {
                ForkCondition::Block(block) => block_number >= block,
                ForkCondition::Timestamp(timestamp) => block_timestamp >= timestamp,
            };
            active.then_some(activation.hardfork)
        },
    )))
}

impl ArcChainSpec {
    pub fn resolve_hardfork(
        activations: &HardforkActivations<ArcHardfork>,
        block_number: u64,
        timestamp: u64,
    ) -> Option<ArcHardfork> {
        arc_hardfork_at(activations.as_slice(), block_number, timestamp)
    }
}

fn block(hardfork: ArcHardfork, activation: u64) -> HardforkActivation<ArcHardfork> {
    HardforkActivation {
        condition: ForkCondition::Block(activation),
        hardfork,
    }
}

fn timestamp(hardfork: ArcHardfork, activation: u64) -> HardforkActivation<ArcHardfork> {
    HardforkActivation {
        condition: ForkCondition::Timestamp(activation),
        hardfork,
    }
}

static ARC_CHAIN_CONFIGS: LazyLock<HashMap<u64, ChainConfig<ArcHardfork>>> = LazyLock::new(|| {
    [
        (
            ARC_LOCAL_CHAIN_ID,
            arc_chain_config(
                "Arc Local",
                vec![
                    block(ArcHardfork::ACTIVATE_ZERO3, 0),
                    block(ArcHardfork::ACTIVATE_ZERO4, 0),
                    block(ArcHardfork::ACTIVATE_ZERO5, 0),
                    block(ArcHardfork::ACTIVATE_ZERO6, 0),
                    timestamp(ArcHardfork::OSAKA, 0),
                    timestamp(ArcHardfork::ACTIVATE_ZERO7, 0),
                    timestamp(ArcHardfork::ACTIVATE_ZERO8, 0),
                ],
            ),
        ),
        (
            ARC_MAINNET_CHAIN_ID,
            arc_chain_config(
                "Arc Mainnet",
                vec![
                    block(ArcHardfork::ACTIVATE_ZERO3, 0),
                    block(ArcHardfork::ACTIVATE_ZERO4, 0),
                    timestamp(ArcHardfork::OSAKA, 0),
                    block(ArcHardfork::ACTIVATE_ZERO5, 0),
                    block(ArcHardfork::ACTIVATE_ZERO6, 0),
                    timestamp(ArcHardfork::ACTIVATE_ZERO7, 1_789_052_400),
                    timestamp(ArcHardfork::ACTIVATE_ZERO8, 1_789_052_400),
                ],
            ),
        ),
        (
            ARC_DEVNET_CHAIN_ID,
            arc_chain_config(
                "Arc Devnet",
                vec![
                    block(ArcHardfork::ACTIVATE_ZERO3, 7_437_594),
                    block(ArcHardfork::ACTIVATE_ZERO4, 19_491_165),
                    timestamp(ArcHardfork::OSAKA, 1_775_483_400),
                    block(ArcHardfork::ACTIVATE_ZERO5, 32_371_192),
                    block(ArcHardfork::ACTIVATE_ZERO6, 40_033_853),
                    timestamp(ArcHardfork::ACTIVATE_ZERO7, 1_780_495_200),
                    timestamp(ArcHardfork::ACTIVATE_ZERO8, 1_787_756_400),
                ],
            ),
        ),
        (
            ARC_TESTNET_CHAIN_ID,
            arc_chain_config(
                "Arc Testnet",
                vec![
                    block(ArcHardfork::ACTIVATE_ZERO3, 11_172_019),
                    block(ArcHardfork::ACTIVATE_ZERO4, 26_148_086),
                    timestamp(ArcHardfork::OSAKA, 1_779_890_400),
                    timestamp(ArcHardfork::ACTIVATE_ZERO5, 1_779_894_517),
                    timestamp(ArcHardfork::ACTIVATE_ZERO6, 1_779_894_517),
                    timestamp(ArcHardfork::ACTIVATE_ZERO7, 1_781_791_200),
                    timestamp(ArcHardfork::ACTIVATE_ZERO8, 1_788_447_600),
                ],
            ),
        ),
    ]
    .into_iter()
    .collect()
});

impl ProviderChainSpec for ArcChainSpec {
    const MIN_ETHASH_DIFFICULTY: u64 = L1ChainSpec::MIN_ETHASH_DIFFICULTY;

    fn chain_configs() -> &'static HashMap<u64, ChainConfig<Self::Hardfork>> {
        &ARC_CHAIN_CONFIGS
    }

    fn resolve_hardfork(
        activations: &HardforkActivations<Self::Hardfork>,
        block_number: u64,
        timestamp: u64,
    ) -> Option<Self::Hardfork> {
        Self::resolve_hardfork(activations, block_number, timestamp)
    }

    fn normalize_hardfork(hardfork: Self::Hardfork) -> Self::Hardfork {
        hardfork.execution()
    }

    fn default_base_fee_params() -> &'static BaseFeeParams<Self::Hardfork> {
        &ARC_BASE_FEE_PARAMS
    }

    fn next_base_fee_per_gas(
        header: &BlockHeader,
        hardfork: Self::Hardfork,
        default_base_fee_params: &BaseFeeParams<Self::Hardfork>,
    ) -> u128 {
        if header.extra_data.len() == 8 {
            let mut encoded_base_fee = [0u8; 8];
            encoded_base_fee.copy_from_slice(&header.extra_data);
            return u128::from(u64::from_be_bytes(encoded_base_fee));
        }

        calculate_next_base_fee_per_gas(
            header,
            u128::from(header.gas_used),
            default_base_fee_params,
            hardfork,
        )
        .clamp(ARC_MIN_BASE_FEE, ARC_MAX_BASE_FEE)
    }

    fn default_schedulded_blob_params() -> Option<ScheduledBlobParams> {
        GenericChainSpec::default_schedulded_blob_params()
    }
}

impl ReceiptChainSpec for ArcChainSpec {
    type ExecutionReceiptBuilder = GenericExecutionReceiptBuilder;
    type Receipt =
        ArcBlockReceipt<<Self as ExecutionReceiptChainSpec>::ExecutionReceipt<FilterLog>>;
}

impl RpcBlockChainSpec for ArcChainSpec {
    type RpcBlock<DataT>
        = GenericRpcBlock<DataT>
    where
        DataT: serde::de::DeserializeOwned + serde::Serialize;
}

impl RpcChainSpec for ArcChainSpec {
    type RpcCallRequest = L1CallRequest;
    type RpcReceipt = ArcRpcTransactionReceipt;
    type RpcTransaction = ArcRpcTransactionWithSignature;
    type RpcTransactionRequest = L1RpcTransactionRequest;
}

impl<TimerT: Clone + TimeSinceEpoch> ProviderSpec<TimerT> for ArcChainSpec {
    type PooledTransaction = edr_chain_l1::L1PooledTransaction;
    type TransactionRequest = crate::transaction::GenericTransactionRequest<Self>;

    fn cast_halt_reason(reason: Self::HaltReason) -> TransactionFailureReason<Self::HaltReason> {
        <L1ChainSpec as ProviderSpec<TimerT>>::cast_halt_reason(reason)
    }
}

#[cfg(test)]
mod tests {
    use edr_chain_config::{
        ChainConfig, ChainOverride, ForkCondition, HardforkActivation, HardforkActivations,
    };
    use edr_chain_spec_provider::ProviderChainSpec;
    use edr_primitives::{Bytes, HashMap};

    use super::{ArcChainSpec, ArcHardfork};
    use crate::ArcFeatures;

    #[test]
    fn next_base_fee_uses_extra_data_when_present() {
        let header = edr_block_header::BlockHeader {
            extra_data: Bytes::copy_from_slice(&42u64.to_be_bytes()),
            ..Default::default()
        };

        assert_eq!(
            ArcChainSpec::next_base_fee_per_gas(
                &header,
                ArcHardfork::LATEST,
                ArcChainSpec::default_base_fee_params(),
            ),
            42
        );
    }

    #[test]
    fn next_base_fee_uses_arc_fallback_parameters() {
        let header = edr_block_header::BlockHeader {
            gas_limit: 30_000_000,
            gas_used: 0,
            base_fee_per_gas: Some(1_000_000_000),
            ..Default::default()
        };

        assert_eq!(
            ArcChainSpec::next_base_fee_per_gas(
                &header,
                ArcHardfork::LATEST,
                ArcChainSpec::default_base_fee_params(),
            ),
            980_000_000
        );
    }

    #[test]
    fn arc_network_hardfork_boundaries_use_block_and_timestamp() {
        let devnet = ArcChainSpec::chain_configs().get(&5_042_001).unwrap();
        let before_zero3 =
            ArcChainSpec::resolve_hardfork(&devnet.hardfork_activations, 7_437_593, 0).unwrap();
        assert_eq!(before_zero3, ArcHardfork::PRAGUE);
        let at_zero3 =
            ArcChainSpec::resolve_hardfork(&devnet.hardfork_activations, 7_437_594, 0).unwrap();
        assert_eq!(at_zero3, ArcHardfork::ZERO3);

        let zero5_prague =
            ArcChainSpec::resolve_hardfork(&devnet.hardfork_activations, 32_371_192, 0).unwrap();
        assert_eq!(zero5_prague, ArcHardfork::ZERO5_PRAGUE);
        let zero6_prague =
            ArcChainSpec::resolve_hardfork(&devnet.hardfork_activations, 40_033_853, 0).unwrap();
        assert_eq!(zero6_prague, ArcHardfork::ZERO6_PRAGUE);
        assert!(zero6_prague.is_zero5());
        assert!(zero6_prague.is_zero6());

        let zero6_osaka =
            ArcChainSpec::resolve_hardfork(&devnet.hardfork_activations, 40_033_853, 1_780_495_199)
                .unwrap();
        assert_eq!(
            edr_chain_spec::EvmSpecId::from(zero6_osaka),
            edr_chain_spec::EvmSpecId::OSAKA,
        );
        assert!(!zero6_osaka.is_zero7());
        let zero7 =
            ArcChainSpec::resolve_hardfork(&devnet.hardfork_activations, 40_033_853, 1_780_495_200)
                .unwrap();
        assert!(zero7.is_zero7());

        let testnet = ArcChainSpec::chain_configs().get(&5_042_002).unwrap();
        let before_zero3 =
            ArcChainSpec::resolve_hardfork(&testnet.hardfork_activations, 11_172_018, 0).unwrap();
        assert_eq!(before_zero3, ArcHardfork::PRAGUE);
        let at_zero3 =
            ArcChainSpec::resolve_hardfork(&testnet.hardfork_activations, 11_172_019, 0).unwrap();
        assert_eq!(at_zero3, ArcHardfork::ZERO3);

        let before_osaka = ArcChainSpec::resolve_hardfork(
            &testnet.hardfork_activations,
            26_148_086,
            1_779_890_399,
        )
        .unwrap();
        assert_eq!(
            edr_chain_spec::EvmSpecId::from(before_osaka),
            edr_chain_spec::EvmSpecId::PRAGUE,
        );
        let osaka = ArcChainSpec::resolve_hardfork(
            &testnet.hardfork_activations,
            26_148_086,
            1_779_890_400,
        )
        .unwrap();
        assert_eq!(
            edr_chain_spec::EvmSpecId::from(osaka),
            edr_chain_spec::EvmSpecId::OSAKA,
        );
        assert!(!osaka.is_zero5());
        let zero6 = ArcChainSpec::resolve_hardfork(
            &testnet.hardfork_activations,
            26_148_086,
            1_779_894_517,
        )
        .unwrap();
        assert!(zero6.is_zero5());
        assert!(zero6.is_zero6());
    }

    #[test]
    fn arc_hardfork_resolution_survives_serde_and_unknown_chain_overrides() {
        let chain_override = ChainOverride {
            name: "Arc override".to_owned(),
            hardfork_activation_overrides: Some(HardforkActivations::new(vec![
                HardforkActivation {
                    condition: ForkCondition::Timestamp(200),
                    hardfork: "zero5".parse().unwrap(),
                },
                HardforkActivation {
                    condition: ForkCondition::Timestamp(100),
                    hardfork: ArcHardfork::OSAKA,
                },
            ])),
            native_token_mirror: None,
        };
        let encoded = serde_json::to_value(&chain_override).unwrap();
        let decoded: ChainOverride<ArcHardfork> = serde_json::from_value(encoded).unwrap();
        let defaults = ArcChainSpec::chain_configs().get(&1_337).unwrap();
        let mut chain_configs: HashMap<u64, ChainConfig<ArcHardfork>> = HashMap::default();
        chain_configs.insert(
            9_999_999,
            ChainConfig {
                name: decoded.name,
                hardfork_activations: decoded.hardfork_activation_overrides.unwrap(),
                base_fee_params: defaults.base_fee_params.clone(),
                bpo_hardfork_schedule: None,
                native_token_mirror: decoded.native_token_mirror,
            },
        );
        let config = chain_configs.get(&9_999_999).unwrap();

        let hardfork =
            ArcChainSpec::resolve_hardfork(&config.hardfork_activations, 0, 200).unwrap();
        assert_eq!(
            edr_chain_spec::EvmSpecId::from(hardfork),
            edr_chain_spec::EvmSpecId::OSAKA,
        );
        assert!(hardfork.is_zero5());
        assert!(!hardfork.is_zero6());
    }

    #[test]
    fn arc_features_are_independent() {
        let activations = HardforkActivations::new(vec![HardforkActivation {
            condition: ForkCondition::Block(0),
            hardfork: ArcHardfork::ACTIVATE_ZERO6,
        }]);
        let hardfork = ArcChainSpec::resolve_hardfork(&activations, 0, 0).unwrap();

        assert!(hardfork.is_zero6());
        assert!(!hardfork.is_zero5());
        assert_eq!(
            edr_chain_spec::EvmSpecId::from(hardfork),
            edr_chain_spec::EvmSpecId::PRAGUE,
        );
    }

    #[test]
    fn arc_selected_hardforks_are_executable() {
        assert_eq!(
            ArcChainSpec::normalize_hardfork("zero8".parse().unwrap()),
            ArcHardfork::ZERO8,
        );
        assert_eq!(ArcHardfork::ZERO8.execution(), ArcHardfork::ZERO8);
        assert!(ArcHardfork::ZERO8.is_zero5());
        assert!(ArcHardfork::ZERO8.is_zero6());
        assert!(ArcHardfork::ZERO8.is_zero7());
        assert!(ArcHardfork::ZERO8.is_zero8());
        assert_eq!(
            edr_chain_spec::EvmSpecId::from(ArcHardfork::ZERO8),
            edr_chain_spec::EvmSpecId::OSAKA,
        );
        assert_eq!(
            "zero5-prague".parse::<ArcHardfork>().unwrap(),
            ArcHardfork::ZERO5_PRAGUE,
        );
        assert_eq!(
            "zero6-prague".parse::<ArcHardfork>().unwrap(),
            ArcHardfork::ZERO6_PRAGUE,
        );
        let custom = ArcHardfork::new(
            edr_chain_spec::EvmSpecId::PRAGUE,
            ArcFeatures::ZERO6.union(ArcFeatures::ZERO8),
        );
        assert!(custom.is_zero6());
        assert!(custom.is_zero8());
        assert!(!custom.is_zero5());
        assert!(!custom.is_zero7());
    }

    #[test]
    fn arc_hardfork_equality_matches_ordering() {
        let hardforks = [
            ArcHardfork::PRAGUE,
            ArcHardfork::OSAKA,
            ArcHardfork::ACTIVATE_ZERO3,
            ArcHardfork::ACTIVATE_ZERO4,
            ArcHardfork::ACTIVATE_ZERO5,
            ArcHardfork::ACTIVATE_ZERO6,
            ArcHardfork::ACTIVATE_ZERO7,
            ArcHardfork::ACTIVATE_ZERO8,
            ArcHardfork::ZERO3,
            ArcHardfork::ZERO4,
            ArcHardfork::ZERO5_PRAGUE,
            ArcHardfork::ZERO6_PRAGUE,
            ArcHardfork::ZERO5,
            ArcHardfork::ZERO6,
            ArcHardfork::ZERO7,
            ArcHardfork::ZERO8,
        ];

        for left in hardforks {
            for right in hardforks {
                assert_eq!(
                    left == right,
                    left.partial_cmp(&right) == Some(core::cmp::Ordering::Equal),
                    "{left:?} {right:?}",
                );
            }
        }
    }
}
