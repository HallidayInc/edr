//! Convert EDR environments to Arc's native REVM engine.

use std::sync::Arc;

use alloy_evm::{eth::EthEvmContext, EvmEnv, EvmFactory};
use arc_evm::{ArcEvm, ArcEvmFactory};
use edr_chain_spec_evm::{
    BlockEnvTrait, CfgEnv, Database, Inspector, InterpreterResult, PrecompileProvider,
};
use revm_context::{BlockEnv, TxEnv};
use revm_context_interface::Transaction;
use revm_handler::instructions::EthInstructions;
use revm_interpreter::interpreter::EthInterpreter;

use crate::{transaction::SignedTransactionWithFallbackToPostEip155, ArcHardfork};

/// Preserve EDR's precompile overrides while using Arc's factory-configured
/// context and instructions. Subcalls use Arc's implementation and caller list.
pub(crate) fn create_evm<DB, I, P>(
    block: impl BlockEnvTrait,
    cfg: CfgEnv<ArcHardfork>,
    database: DB,
    inspector: I,
    precompiles: P,
) -> ArcEvm<EthEvmContext<DB>, I, EthInstructions<EthInterpreter, EthEvmContext<DB>>, P>
where
    DB: Database + core::fmt::Debug,
    I: Inspector<EthEvmContext<DB>>,
    P: PrecompileProvider<EthEvmContext<DB>, Output = InterpreterResult>,
{
    let hardfork = cfg.spec;
    let flags = hardfork.flags();
    let gas_params = cfg.gas_params.clone();
    let env = EvmEnv {
        cfg_env: cfg.with_spec_and_gas_params(hardfork.into(), gas_params),
        block_env: BlockEnv {
            number: block.number(),
            beneficiary: block.beneficiary(),
            timestamp: block.timestamp(),
            gas_limit: block.gas_limit(),
            basefee: block.basefee(),
            difficulty: block.difficulty(),
            prevrandao: block.prevrandao(),
            blob_excess_gas_and_price: block.blob_excess_gas_and_price(),
            slot_num: block.slot_num(),
        },
    };
    ArcEvmFactory::new(Arc::new(flags))
        .create_evm_with_inspector(database, env, inspector)
        .with_precompiles(precompiles)
}

pub(crate) fn transaction_env(tx: &SignedTransactionWithFallbackToPostEip155) -> TxEnv {
    let mut env = TxEnv {
        tx_type: tx.tx_type(),
        caller: tx.caller(),
        gas_limit: tx.gas_limit(),
        gas_price: tx.gas_price(),
        kind: tx.kind(),
        value: tx.value(),
        data: tx.input().clone(),
        nonce: tx.nonce(),
        chain_id: tx.chain_id(),
        access_list: tx
            .access_list()
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        gas_priority_fee: tx.max_priority_fee_per_gas(),
        blob_hashes: tx.blob_versioned_hashes().to_vec(),
        max_fee_per_blob_gas: tx.max_fee_per_blob_gas(),
        authorization_list: Vec::new(),
    };
    env.set_signed_authorization(tx.authorization_list().cloned().collect());
    env
}
