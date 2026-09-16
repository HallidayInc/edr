use core::fmt::Debug;

use edr_block_api::Block as _;
use edr_block_builder_api::{
    BlockBuilder, BlockBuilderCreationError, BlockFinalizeError, BlockInputs,
    BlockTransactionError, BuiltBlockAndStateWithMetadata, DatabaseComponents, PrecompileFn,
    WrapDatabaseRef,
};
use edr_block_header::{BlockConfig, HeaderOverrides, PartialHeader};
use edr_chain_config::NativeTokenMirror;
use edr_chain_l1::block::EthBlockBuilder;
use edr_chain_spec::TransactionValidation;
use edr_chain_spec_block::BlockChainSpec;
use edr_chain_spec_evm::{
    config::EvmConfig, ContextForChainSpec, DatabaseComponentError, Inspector,
};
use edr_primitives::{Address, Bytes, HashMap, HashSet, U256};
use edr_state_api::{DynState, State, StateError};

use crate::{
    precompiles::{
        arc_blocklist_slot, arc_gas_values_slot, arc_pack_gas_values_parts,
        ARC_NATIVE_COIN_CONTROL_ADDRESS, ARC_PROTOCOL_CONFIG_ADDRESS,
        ARC_PROTOCOL_CONFIG_FEE_PARAMS_SLOT, ARC_SYSTEM_ACCOUNTING_ADDRESS,
    },
    receipt::{ArcBlockReceipt, GenericExecutionReceiptBuilder},
    transaction::SignedTransactionWithFallbackToPostEip155,
    ArcChainSpec, ArcHardfork,
};

type ArcInnerBlockBuilder<'builder, BlockchainErrorT> = EthBlockBuilder<
    'builder,
    <ArcChainSpec as edr_chain_spec_receipt::ReceiptChainSpec>::Receipt,
    <ArcChainSpec as BlockChainSpec>::Block,
    BlockchainErrorT,
    ArcChainSpec,
    GenericExecutionReceiptBuilder,
    ArcChainSpec,
    <ArcChainSpec as edr_block_api::GenesisBlockFactory>::LocalBlock,
>;

/// Arc block builder with ADR-0004 fee accounting and protocol pre-block checks.
pub struct ArcBlockBuilder<'builder, BlockchainErrorT: Debug + Send + Sync + 'static> {
    eth: ArcInnerBlockBuilder<'builder, BlockchainErrorT>,
    chain_id: u64,
}

impl<'builder, BlockchainErrorT: 'static + std::error::Error + Send + Sync>
    BlockBuilder<
        'builder,
        ArcChainSpec,
        ArcBlockReceipt<
            crate::eip2718::TypedEnvelope<edr_receipt::Execution<edr_receipt::log::FilterLog>>,
        >,
        <ArcChainSpec as BlockChainSpec>::Block,
    > for ArcBlockBuilder<'builder, BlockchainErrorT>
{
    type BlockchainError = BlockchainErrorT;
    type LocalBlock = <ArcChainSpec as edr_block_api::GenesisBlockFactory>::LocalBlock;

    fn new_block_builder(
        blockchain: &'builder dyn edr_blockchain_api::Blockchain<
            <ArcChainSpec as edr_chain_spec_receipt::ReceiptChainSpec>::Receipt,
            <ArcChainSpec as BlockChainSpec>::Block,
            Self::BlockchainError,
            ArcHardfork,
            Self::LocalBlock,
            SignedTransactionWithFallbackToPostEip155,
        >,
        block_config: &'builder BlockConfig<ArcHardfork>,
        state: Box<dyn DynState>,
        evm_config: &EvmConfig,
        inputs: BlockInputs,
        mut overrides: HeaderOverrides<ArcHardfork>,
        custom_precompiles: &'builder HashMap<Address, PrecompileFn>,
        native_token_mirror: Option<&'builder NativeTokenMirror>,
    ) -> Result<
        Self,
        BlockBuilderCreationError<
            DatabaseComponentError<Self::BlockchainError, StateError>,
            ArcHardfork,
        >,
    > {
        let parent = blockchain.last_block().map_err(|error| {
            BlockBuilderCreationError::Database(DatabaseComponentError::Blockchain(error))
        })?;
        let parent_header = parent.block_header();
        let chain_id = blockchain.chain_id();

        overrides.gas_limit = Some(expected_gas_limit(state.as_ref(), chain_id).map_err(
            |error| BlockBuilderCreationError::Database(DatabaseComponentError::State(error)),
        )?);

        if overrides.base_fee.is_none() {
            overrides.base_fee = Some(u128::from(
                decode_base_fee(&parent_header.extra_data).unwrap_or_else(|| {
                    arc_next_base_fee(
                        parent_header.gas_used,
                        parent_header.gas_limit,
                        parent_header
                            .base_fee_per_gas
                            .unwrap_or_default()
                            .try_into()
                            .unwrap_or(u64::MAX),
                        200,
                        5_000,
                    )
                }),
            ));
        }

        let beneficiary = overrides.beneficiary.unwrap_or_default();
        let blocked = state
            .storage(
                ARC_NATIVE_COIN_CONTROL_ADDRESS,
                arc_blocklist_slot(beneficiary),
            )
            .map_err(|error| {
                BlockBuilderCreationError::Database(DatabaseComponentError::State(error))
            })?
            != U256::ZERO;
        if blocked {
            return Err(BlockBuilderCreationError::InvalidBlock(
                "Arc block beneficiary is blocklisted".to_owned(),
            ));
        }

        let eth = EthBlockBuilder::new(
            edr_mirror::MirrorContext::new(native_token_mirror.cloned()),
            blockchain,
            block_config,
            state,
            evm_config,
            inputs,
            overrides,
            custom_precompiles,
            native_token_mirror,
        )?;

        Ok(Self { eth, chain_id })
    }

    fn header(&self) -> &PartialHeader {
        self.eth.header()
    }

    fn precompile_addresses(&self) -> &HashSet<Address> {
        self.eth.precompile_addresses()
    }

    fn add_transaction(
        &mut self,
        transaction: SignedTransactionWithFallbackToPostEip155,
    ) -> Result<
        (),
        BlockTransactionError<
            DatabaseComponentError<Self::BlockchainError, StateError>,
            <SignedTransactionWithFallbackToPostEip155 as TransactionValidation>::ValidationError,
        >,
    > {
        self.eth.add_transaction(transaction)
    }

    fn add_transaction_with_inspector<InspectorT>(
        &mut self,
        transaction: SignedTransactionWithFallbackToPostEip155,
        inspector: &mut InspectorT,
    ) -> Result<
        (),
        BlockTransactionError<
            DatabaseComponentError<Self::BlockchainError, StateError>,
            <SignedTransactionWithFallbackToPostEip155 as TransactionValidation>::ValidationError,
        >,
    >
    where
        InspectorT: for<'inspector> Inspector<
            ContextForChainSpec<
                ArcChainSpec,
                <ArcChainSpec as edr_chain_spec::BlockEnvChainSpec>::BlockEnv<
                    'inspector,
                    PartialHeader,
                >,
                WrapDatabaseRef<
                    DatabaseComponents<
                        &'inspector dyn edr_blockchain_api::Blockchain<
                            <ArcChainSpec as edr_chain_spec_receipt::ReceiptChainSpec>::Receipt,
                            <ArcChainSpec as BlockChainSpec>::Block,
                            Self::BlockchainError,
                            ArcHardfork,
                            Self::LocalBlock,
                            SignedTransactionWithFallbackToPostEip155,
                        >,
                        &'inspector dyn DynState,
                    >,
                >,
            >,
        >,
    {
        self.eth
            .add_transaction_with_inspector(transaction, inspector)
    }

    fn finalize_block(
        mut self,
        rewards: Vec<(Address, u128)>,
    ) -> Result<
        BuiltBlockAndStateWithMetadata<Self::LocalBlock, edr_chain_l1::HaltReason>,
        BlockFinalizeError<StateError>,
    > {
        self.apply_post_block_accounting()
            .map_err(BlockFinalizeError::State)?;
        self.eth.finalize(rewards)
    }
}

impl<BlockchainErrorT: Debug + Send + Sync + 'static> ArcBlockBuilder<'_, BlockchainErrorT> {
    fn apply_post_block_accounting(&mut self) -> Result<(), StateError> {
        let number = self.eth.header().number;
        let gas_used = self.eth.header().gas_used;
        let gas_limit = self.eth.header().gas_limit;
        let base_fee = self
            .eth
            .header()
            .base_fee
            .unwrap_or_default()
            .try_into()
            .unwrap_or(u64::MAX);
        let parent_values = if number == 0 {
            (0, 0, 0)
        } else {
            let packed = self.eth.state().storage(
                ARC_SYSTEM_ACCOUNTING_ADDRESS,
                arc_gas_values_slot(number - 1),
            )?;
            crate::precompiles::arc_unpack_gas_values_parts(packed)
        };
        let params = fee_params(self.eth.state(), self.chain_id)?;
        let smoothed = determine_ema(parent_values.1, gas_used, params.alpha).unwrap_or(gas_used);
        let raw_next = arc_next_base_fee(
            smoothed,
            gas_limit,
            base_fee,
            params.k_rate,
            params.inverse_elasticity_multiplier,
        );
        let next_base_fee = params.clamp(raw_next);
        self.eth.set_account_storage_slot(
            ARC_SYSTEM_ACCOUNTING_ADDRESS,
            arc_gas_values_slot(number),
            arc_pack_gas_values_parts(gas_used, smoothed, next_base_fee),
        )?;
        self.eth.header_mut().extra_data = Bytes::copy_from_slice(&next_base_fee.to_be_bytes());
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct FeeParams {
    alpha: u64,
    k_rate: u64,
    inverse_elasticity_multiplier: u64,
    min: u64,
    max: u64,
    absolute_max: u64,
}

impl FeeParams {
    fn clamp(self, value: u64) -> u64 {
        let configured = if self.max == 0 || self.max < self.min {
            value
        } else {
            value.clamp(self.min, self.max)
        };
        configured.clamp(1, self.absolute_max)
    }
}

fn fee_params(state: &dyn DynState, chain_id: u64) -> Result<FeeParams, StateError> {
    let packed = state.storage(
        ARC_PROTOCOL_CONFIG_ADDRESS,
        ARC_PROTOCOL_CONFIG_FEE_PARAMS_SLOT,
    )?;
    let words = packed.as_limbs();
    let alpha = bounded(words[0], 1, 20, 100);
    let k_rate = bounded(
        words[1],
        1,
        200,
        if chain_id == 5_042 || chain_id == 5_042_002 {
            1_000
        } else {
            10_000
        },
    );
    let inverse_elasticity_multiplier = bounded(
        words[2],
        1,
        5_000,
        if chain_id == 5_042 || chain_id == 5_042_002 {
            9_000
        } else {
            10_000
        },
    );
    let min = state
        .storage(
            ARC_PROTOCOL_CONFIG_ADDRESS,
            ARC_PROTOCOL_CONFIG_FEE_PARAMS_SLOT + U256::from(1),
        )?
        .try_into()
        .unwrap_or(u64::MAX);
    let max = state
        .storage(
            ARC_PROTOCOL_CONFIG_ADDRESS,
            ARC_PROTOCOL_CONFIG_FEE_PARAMS_SLOT + U256::from(2),
        )?
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(FeeParams {
        alpha,
        k_rate,
        inverse_elasticity_multiplier,
        min,
        max,
        absolute_max: if chain_id == 5_042 || chain_id == 5_042_002 {
            20_000_000_000_000
        } else {
            u64::MAX - 1
        },
    })
}

fn expected_gas_limit(state: &dyn DynState, chain_id: u64) -> Result<u64, StateError> {
    let configured = state.storage(
        ARC_PROTOCOL_CONFIG_ADDRESS,
        ARC_PROTOCOL_CONFIG_FEE_PARAMS_SLOT + U256::from(3),
    )?;
    let configured = u64::try_from(configured).ok();
    let (min, max) = if chain_id == 5_042 || chain_id == 5_042_002 {
        (10_000_000, 200_000_000)
    } else {
        (1_000_000, 1_000_000_000)
    };
    Ok(configured
        .filter(|value| (min..=max).contains(value))
        .unwrap_or(30_000_000))
}

fn bounded(value: u64, min: u64, default: u64, max: u64) -> u64 {
    if (min..=max).contains(&value) {
        value
    } else {
        default
    }
}

fn determine_ema(parent: u64, current: u64, alpha: u64) -> Option<u64> {
    let alpha = u128::from(alpha);
    if alpha > 100 {
        return None;
    }
    let total = (100 - alpha)
        .checked_mul(u128::from(parent))?
        .checked_add(alpha.checked_mul(u128::from(current))?)?;
    u64::try_from(total / 100).ok()
}

pub(crate) fn arc_next_base_fee(
    gas_used: u64,
    gas_limit: u64,
    base_fee: u64,
    k_rate: u64,
    inverse_elasticity_multiplier: u64,
) -> u64 {
    let target =
        u64::try_from(u128::from(gas_limit) * u128::from(inverse_elasticity_multiplier) / 10_000)
            .unwrap_or(u64::MAX);
    if target == 0 || k_rate == 0 {
        return base_fee;
    }
    let denominator = u128::from(target) * 10_000 / u128::from(k_rate);
    if denominator == 0 {
        return base_fee;
    }
    match gas_used.cmp(&target) {
        core::cmp::Ordering::Equal => base_fee,
        core::cmp::Ordering::Greater => {
            let change = u128::from(base_fee) * u128::from(gas_used - target) / denominator;
            base_fee.saturating_add(u64::try_from(change).unwrap_or(u64::MAX).max(1))
        }
        core::cmp::Ordering::Less => {
            let change = u128::from(base_fee) * u128::from(target - gas_used) / denominator;
            base_fee.saturating_sub(u64::try_from(change).unwrap_or(u64::MAX))
        }
    }
}

fn decode_base_fee(extra_data: &Bytes) -> Option<u64> {
    let bytes: [u8; 8] = extra_data.as_ref().try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
}
