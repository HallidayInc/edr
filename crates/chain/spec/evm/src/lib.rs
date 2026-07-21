pub mod config;
mod error;
pub mod handler;
pub mod interpreter;
pub mod result;

use edr_chain_spec::{
    BlockEnvExt, ChainSpec, ContextChainSpec, EvmTransactionValidationError, HardforkChainSpec,
    TransactionValidation,
};
pub use edr_database_components::DatabaseComponentError;
pub use revm_context::{
    Block as BlockEnvTrait, CfgEnv, Context, ContextError, ContextTr as ContextTrait, Database,
    Evm, Journal, JournalEntry, JournalTr as JournalTrait, LocalContext,
};
pub use revm_handler::{ExecuteEvm, PrecompileProvider};
pub use revm_inspector::{InspectEvm, Inspector, JournalExt, NoOpInspector};

pub use self::error::{TransactionError, TransactionErrorForChainSpec};
pub use crate::{interpreter::InterpreterResult, result::ExecutionResultAndState};

/// Helper type for a chain-specific [`Context`].
pub type ContextForChainSpec<ChainSpecT, BlockEnvT, DatabaseT> =
    <ChainSpecT as EvmChainSpec>::EvmContext<BlockEnvT, DatabaseT>;

/// Trait for specifying the types for running a transaction in a chain's
/// associated EVM.
pub trait EvmChainSpec:
    ChainSpec<
        SignedTransaction: TransactionValidation<
            ValidationError: From<EvmTransactionValidationError>,
        >,
    > + ContextChainSpec
    + HardforkChainSpec
{
    /// Concrete REVM context used by this chain.
    ///
    /// Most chains use the standard Ethereum context. Chains with a custom
    /// execution engine, such as Tempo, can select their native context here.
    type EvmContext<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug>: ContextTrait<
        Db = DatabaseT,
        Journal: JournalExt,
    >;

    /// Type representing a precompile provider.
    type PrecompileProvider<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug>: PrecompileProvider<
        ContextForChainSpec<Self, BlockT, DatabaseT>,
        Output = InterpreterResult,
    >;

    /// Constructs the precompile provider for the given hardfork.
    fn new_precompile_provider<BlockT: BlockEnvTrait, DatabaseT: Database + core::fmt::Debug>(
        hardfork: Self::Hardfork,
    ) -> Self::PrecompileProvider<BlockT, DatabaseT>;

    /// Runs a transaction inside the chain's EVM without committing the
    /// changes.
    #[allow(clippy::type_complexity)]
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
        Self::dry_run_with_inspector(
            block,
            cfg,
            transaction,
            database,
            precompile_provider,
            NoOpInspector,
            mirror_config,
        )
    }

    /// Runs a transaction inside the chain's EVM without committing the
    /// changes, while an inspector is observing the execution.
    #[allow(clippy::type_complexity)]
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
    >;
}
