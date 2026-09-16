use std::sync::{Arc, LazyLock};

use alloy_evm::Evm as _;
use edr_block_api::{sync::SyncBlock, GenesisBlockFactory, GenesisBlockOptions};
use edr_block_header::{BlockConfig, BlockHeader, HeaderAndEvmSpec};
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
    CfgEnv, ContextForChainSpec, Database, EvmChainSpec, ExecutionResultAndState, InspectEvm as _,
    Inspector, InterpreterResult, PrecompileProvider, TransactionError,
};
use edr_chain_spec_provider::ProviderChainSpec;
use edr_chain_spec_receipt::ReceiptChainSpec;
use edr_chain_spec_rpc::{RpcBlockChainSpec, RpcChainSpec};
use edr_eip1559::{BaseFeeParams, ConstantBaseFeeParams};
use edr_eip7892::ScheduledBlobParams;
use edr_primitives::HashMap;
use edr_provider::{time::TimeSinceEpoch, ProviderSpec, TransactionFailureReason};
use edr_receipt::{log::FilterLog, ExecutionReceiptChainSpec};
use edr_state_api::{StateCommit as _, StateDiff};

use crate::{
    arc_evm::{create_evm, transaction_env},
    eip2718::TypedEnvelope,
    receipt::GenericExecutionReceiptBuilder,
    rpc::{
        block::GenericRpcBlock, receipt::GenericRpcTransactionReceipt,
        transaction::GenericRpcTransactionWithSignature,
    },
    ArcChainSpec, ArcHardfork, GenericChainSpec,
};

// EDR's Ethereum-shaped configuration view; Arc computes actual block fees.
static ARC_BASE_FEE_PARAMS: LazyLock<BaseFeeParams<ArcHardfork>> = LazyLock::new(|| {
    let defaults =
        arc_execution_config::fee_config::base_fee_config(0, 0).resolve_calc_params(None);
    BaseFeeParams::Constant(ConstantBaseFeeParams {
        max_change_denominator: 10_000 / u128::from(defaults.k_rate),
        elasticity_multiplier: 10_000 / u128::from(defaults.inverse_elasticity_multiplier),
    })
});

impl BlockChainSpec for ArcChainSpec {
    type Block =
        dyn SyncBlock<Arc<Self::Receipt>, Self::SignedTransaction, Error = Self::FetchReceiptError>;

    type BlockBuilder<'builder, BlockchainErrorT: 'static + std::error::Error + Send + Sync> =
        edr_chain_l1::block::EthBlockBuilder<
            'builder,
            Self::Receipt,
            Self::Block,
            BlockchainErrorT,
            Self,
            GenericExecutionReceiptBuilder,
            Self,
            <Self as GenesisBlockFactory>::LocalBlock,
        >;

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
    type Context = ();
}

impl EvmChainSpec for ArcChainSpec {
    type EvmContext<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug> =
        alloy_evm::eth::EthEvmContext<DatabaseT>;

    type PrecompileProvider<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug> =
        alloy_evm::precompiles::PrecompilesMap;

    fn new_precompile_provider<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug>(
        hardfork: Self::Hardfork,
    ) -> Self::PrecompileProvider<BlockT, DatabaseT> {
        arc_precompiles::precompile_provider::ArcPrecompileProvider::create_precompiles_map(
            hardfork.into(),
            hardfork.flags(),
        )
    }

    fn cold_precompile_addresses(hardfork: Self::Hardfork) -> &'static [edr_primitives::Address] {
        arc_precompiles::precompile_provider::ArcPrecompileProvider::cold_addresses(
            hardfork.flags(),
        )
    }

    fn prepare_block<
        BlockT: BlockEnvTrait,
        DatabaseT: Database + edr_state_api::StateCommit + core::fmt::Debug,
    >(
        block: BlockT,
        cfg: CfgEnv<Self::Hardfork>,
        parent: &BlockHeader,
        database: DatabaseT,
    ) -> Result<edr_chain_spec_evm::BlockChanges, String> {
        use arc_execution_config::{fee_config, gas_fee};
        let number = u64::try_from(block.number()).map_err(|e| e.to_string())?;
        let prague = edr_chain_spec::EvmSpecId::from(cfg.spec) >= edr_chain_spec::EvmSpecId::PRAGUE;
        let gas_config = fee_config::block_gas_limit_config(cfg.chain_id, number);
        let fee_config = fee_config::base_fee_config(cfg.chain_id, number);
        let precompiles = Self::new_precompile_provider::<BlockT, DatabaseT>(cfg.spec);
        let mut evm = create_evm(
            block,
            cfg,
            database,
            edr_chain_spec_evm::NoOpInspector,
            precompiles,
        );
        // EIP-2935 precedes Arc's protocol queries. Commit once, and return the
        // same writes to EDR for historical state and snapshot bookkeeping.
        let state = if prague && number != 0 {
            let changes = evm
                .transact_system_call(
                    alloy_eips::eip4788::SYSTEM_ADDRESS,
                    alloy_eips::eip2935::HISTORY_STORAGE_ADDRESS,
                    parent.hash().0.into(),
                )
                .map_err(|e| e.to_string())?
                .state;
            evm.db_mut().commit(changes.clone());
            changes
        } else {
            Default::default()
        };
        let gas_limit = arc_evm::executor::prepare_block(&mut evm, number, &gas_config)
            .map_err(|e| e.to_string())?;
        Ok(edr_chain_spec_evm::BlockChanges {
            state,
            gas_limit: Some(gas_limit),
            base_fee: Some(u128::from(gas_fee::next_block_base_fee(
                &parent.extra_data,
                parent.gas_used,
                parent.gas_limit,
                parent
                    .base_fee_per_gas
                    .unwrap_or_default()
                    .try_into()
                    .map_err(|e: std::num::TryFromIntError| e.to_string())?,
                &fee_config,
            ))),
            ..Default::default()
        })
    }

    fn finish_block<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug>(
        block: BlockT,
        cfg: CfgEnv<Self::Hardfork>,
        gas_used: u64,
        database: DatabaseT,
    ) -> Result<edr_chain_spec_evm::BlockChanges, String> {
        let number = u64::try_from(block.number()).map_err(|e| e.to_string())?;
        let next_number = number.checked_add(1).ok_or("Arc block number overflow")?;
        let config = arc_execution_config::fee_config::base_fee_config(cfg.chain_id, next_number);
        let precompiles = Self::new_precompile_provider::<BlockT, DatabaseT>(cfg.spec);
        let mut evm = create_evm(
            block,
            cfg,
            database,
            edr_chain_spec_evm::NoOpInspector,
            precompiles,
        );
        let (values, state) =
            arc_evm::executor::finish_block(&mut evm, number, gas_used, &config, None)
                .map_err(|e| e.to_string())?;
        Ok(edr_chain_spec_evm::BlockChanges {
            state,
            extra_data: Some(arc_execution_config::gas_fee::encode_base_fee_to_bytes(
                values.nextBaseFee,
            )),
            ..Default::default()
        })
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
        _mirror_config: Option<edr_chain_config::NativeTokenMirror>,
    ) -> Result<
        ExecutionResultAndState<Self::HaltReason>,
        TransactionError<
            DatabaseT::Error,
            <Self::SignedTransaction as TransactionValidation>::ValidationError,
        >,
    > {
        create_evm(block, cfg, database, inspector, precompile_provider)
            .inspect_tx(transaction_env(&transaction))
            .map_err(TransactionError::from)
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

use arc_execution_config::chain_ids::{
    DEVNET_CHAIN_ID as ARC_DEVNET_CHAIN_ID, LOCALDEV_CHAIN_ID as ARC_LOCAL_CHAIN_ID,
    MAINNET_CHAIN_ID as ARC_MAINNET_CHAIN_ID, TESTNET_CHAIN_ID as ARC_TESTNET_CHAIN_ID,
};

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

impl ArcChainSpec {
    pub fn resolve_hardfork(
        activations: &HardforkActivations<ArcHardfork>,
        block_number: u64,
        timestamp: u64,
    ) -> Option<ArcHardfork> {
        Some(ArcHardfork::combine(
            activations.as_slice().iter().filter_map(|activation| {
                let active = match activation.condition {
                    ForkCondition::Block(block) => block_number >= block,
                    ForkCondition::Timestamp(time) => timestamp >= time,
                };
                active.then_some(activation.hardfork)
            }),
        ))
    }
}

/// EDR owns hardfork selection/serialization; activation dates come from Arc.
fn network_activations(
    schedule: &[(
        arc_execution_config::hardforks::ArcHardfork,
        alloy_hardforks::ForkCondition,
    )],
    osaka_timestamp: u64,
) -> Vec<HardforkActivation<ArcHardfork>> {
    let mut activations = vec![HardforkActivation {
        condition: ForkCondition::Timestamp(osaka_timestamp),
        hardfork: ArcHardfork::OSAKA,
    }];
    activations.extend(
        schedule
            .iter()
            .map(|&(fork, condition)| HardforkActivation {
                condition: match condition {
                    alloy_hardforks::ForkCondition::Block(block) => ForkCondition::Block(block),
                    alloy_hardforks::ForkCondition::Timestamp(time) => {
                        ForkCondition::Timestamp(time)
                    }
                    _ => unreachable!("Arc schedules activate by block or timestamp"),
                },
                hardfork: ArcHardfork::Activation(fork),
            }),
    );
    activations
}

static ARC_CHAIN_CONFIGS: LazyLock<HashMap<u64, ChainConfig<ArcHardfork>>> = LazyLock::new(|| {
    use arc_execution_config::hardforks::{
        ARC_DEVNET_SCHEDULE, ARC_LOCALDEV_SCHEDULE, ARC_MAINNET_SCHEDULE,
        ARC_OSAKA_HARDFORK_TIMESTAMP_ACTIVATION_DEVNET,
        ARC_OSAKA_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET, ARC_TESTNET_SCHEDULE,
    };
    [
        (ARC_LOCAL_CHAIN_ID, "Arc Local", ARC_LOCALDEV_SCHEDULE, 0),
        (ARC_MAINNET_CHAIN_ID, "Arc Mainnet", ARC_MAINNET_SCHEDULE, 0),
        (
            ARC_DEVNET_CHAIN_ID,
            "Arc Devnet",
            ARC_DEVNET_SCHEDULE,
            ARC_OSAKA_HARDFORK_TIMESTAMP_ACTIVATION_DEVNET,
        ),
        (
            ARC_TESTNET_CHAIN_ID,
            "Arc Testnet",
            ARC_TESTNET_SCHEDULE,
            ARC_OSAKA_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET,
        ),
    ]
    .into_iter()
    .map(|(id, name, schedule, osaka)| {
        (
            id,
            arc_chain_config(name, network_activations(&schedule, osaka)),
        )
    })
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
        let _ = (hardfork, default_base_fee_params);
        u128::from(arc_execution_config::gas_fee::next_block_base_fee(
            &header.extra_data,
            header.gas_used,
            header.gas_limit,
            header
                .base_fee_per_gas
                .unwrap_or_default()
                .try_into()
                .unwrap_or(u64::MAX),
            &arc_execution_config::fee_config::base_fee_config(0, header.number.saturating_add(1)),
        ))
    }

    fn default_schedulded_blob_params() -> Option<ScheduledBlobParams> {
        GenericChainSpec::default_schedulded_blob_params()
    }
}

impl ReceiptChainSpec for ArcChainSpec {
    type ExecutionReceiptBuilder = GenericExecutionReceiptBuilder;
    type Receipt = edr_chain_l1::receipt::L1BlockReceipt<
        <Self as ExecutionReceiptChainSpec>::ExecutionReceipt<FilterLog>,
    >;
}

impl RpcBlockChainSpec for ArcChainSpec {
    type RpcBlock<DataT>
        = GenericRpcBlock<DataT>
    where
        DataT: serde::de::DeserializeOwned + serde::Serialize;
}

impl RpcChainSpec for ArcChainSpec {
    type RpcCallRequest = L1CallRequest;
    type RpcReceipt = GenericRpcTransactionReceipt;
    type RpcTransaction = GenericRpcTransactionWithSignature;
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
    use arc_execution_config::hardforks::ArcHardfork as NativeHardfork;
    use edr_chain_config::{
        ChainConfig, ChainOverride, ForkCondition, HardforkActivation, HardforkActivations,
    };
    use edr_chain_spec_provider::ProviderChainSpec;
    use edr_primitives::{Bytes, HashMap};

    use super::{ArcChainSpec, ArcHardfork};
    use crate::ArcFeatures;

    #[test]
    fn arc_dynamic_precompiles_survive_edr_overrides_without_becoming_warm() {
        use alloy_evm::eth::EthEvmContext;
        use arc_precompiles::native_coin_authority::NATIVE_COIN_AUTHORITY_ADDRESS;
        use edr_chain_spec_evm::{EvmChainSpec, PrecompileProvider};
        use revm_context::{database_interface::EmptyDB, BlockEnv};

        let base = ArcChainSpec::new_precompile_provider::<BlockEnv, EmptyDB>(ArcHardfork::LATEST);
        let provider =
            edr_precompile::OverriddenPrecompileProvider::<_, EthEvmContext<EmptyDB>>::new(base);
        assert!(provider.contains(&NATIVE_COIN_AUTHORITY_ADDRESS));
        assert!(!provider
            .warm_addresses()
            .contains(&NATIVE_COIN_AUTHORITY_ADDRESS));
        assert!(ArcChainSpec::cold_precompile_addresses(ArcHardfork::LATEST)
            .contains(&NATIVE_COIN_AUTHORITY_ADDRESS));
    }

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
        use edr_chain_spec::EvmSpecId;
        let osaka_zero4 = ArcHardfork::new(EvmSpecId::OSAKA, ArcHardfork::ZERO4.flags());
        for (chain_id, block, timestamp, expected) in [
            (5_042_001, 7_437_593, 0, ArcHardfork::PRAGUE),
            (5_042_001, 7_437_594, 0, ArcHardfork::ZERO3),
            (5_042_001, 32_371_192, 0, ArcHardfork::ZERO5_PRAGUE),
            (5_042_001, 40_033_853, 0, ArcHardfork::ZERO6_PRAGUE),
            (5_042_001, 40_033_853, 1_780_495_199, ArcHardfork::ZERO6),
            (5_042_001, 40_033_853, 1_780_495_200, ArcHardfork::ZERO7),
            (5_042_002, 11_172_018, 0, ArcHardfork::PRAGUE),
            (5_042_002, 11_172_019, 0, ArcHardfork::ZERO3),
            (5_042_002, 26_148_086, 1_779_890_399, ArcHardfork::ZERO4),
            (5_042_002, 26_148_086, 1_779_890_400, osaka_zero4),
            (5_042_002, 26_148_086, 1_779_894_517, ArcHardfork::ZERO6),
        ] {
            let config = &ArcChainSpec::chain_configs()[&chain_id];
            let actual =
                ArcChainSpec::resolve_hardfork(&config.hardfork_activations, block, timestamp)
                    .unwrap();
            assert_eq!(
                actual, expected,
                "chain {chain_id}, block {block}, timestamp {timestamp}"
            );
        }
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
        assert!(hardfork.flags().is_active(NativeHardfork::Zero5));
        assert!(!hardfork.flags().is_active(NativeHardfork::Zero6));
    }

    #[test]
    fn arc_features_are_independent() {
        let activations = HardforkActivations::new(vec![HardforkActivation {
            condition: ForkCondition::Block(0),
            hardfork: ArcHardfork::ACTIVATE_ZERO6,
        }]);
        let hardfork = ArcChainSpec::resolve_hardfork(&activations, 0, 0).unwrap();

        assert!(hardfork.flags().is_active(NativeHardfork::Zero6));
        assert!(!hardfork.flags().is_active(NativeHardfork::Zero5));
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
        assert!(ArcHardfork::ZERO8.flags().is_active(NativeHardfork::Zero5));
        assert!(ArcHardfork::ZERO8.flags().is_active(NativeHardfork::Zero6));
        assert!(ArcHardfork::ZERO8.flags().is_active(NativeHardfork::Zero7));
        assert!(ArcHardfork::ZERO8.flags().is_active(NativeHardfork::Zero8));
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
        assert!(custom.flags().is_active(NativeHardfork::Zero6));
        assert!(custom.flags().is_active(NativeHardfork::Zero8));
        assert!(!custom.flags().is_active(NativeHardfork::Zero5));
        assert!(!custom.flags().is_active(NativeHardfork::Zero7));
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
