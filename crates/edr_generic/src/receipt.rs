use edr_chain_spec::{EvmSpecId, ExecutableTransaction as _};
use edr_chain_spec_evm::result::ExecutionResult;
use edr_primitives::B256;
use edr_receipt::log::{logs_to_bloom, ExecutionLog, FilterLog};
use edr_receipt_builder_api::ExecutionReceiptBuilder;
use edr_state_api::State;
use edr_transaction::TransactionType;
use std::ops::Deref;

use edr_chain_spec_receipt::ReceiptConstructor;
use edr_primitives::{Address, Bloom};
use edr_receipt::{
    AsExecutionReceipt, ExecutionReceipt, ReceiptTrait, RootOrStatus, TransactionReceipt,
};

use crate::{eip2718::TypedEnvelope, transaction};

pub struct GenericExecutionReceiptBuilder;

/// A block receipt whose construction follows Tempo's hardfork and context
/// types while reusing EDR's common receipt data model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TempoBlockReceipt<ExecutionReceiptT: ExecutionReceipt<Log = FilterLog>> {
    pub inner: TransactionReceipt<ExecutionReceiptT>,
    pub block_hash: B256,
    pub block_number: u64,
    pub fee_token: Option<Address>,
    pub fee_payer: Address,
}

impl<ExecutionReceiptT: ExecutionReceipt<Log = FilterLog>> AsExecutionReceipt
    for TempoBlockReceipt<ExecutionReceiptT>
{
    type ExecutionReceipt = ExecutionReceiptT;

    fn as_execution_receipt(&self) -> &Self::ExecutionReceipt {
        self.inner.as_execution_receipt()
    }
}

impl<ExecutionReceiptT: ExecutionReceipt<Log = FilterLog>> Deref
    for TempoBlockReceipt<ExecutionReceiptT>
{
    type Target = TransactionReceipt<ExecutionReceiptT>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<ExecutionReceiptT> alloy_rlp::Encodable for TempoBlockReceipt<ExecutionReceiptT>
where
    ExecutionReceiptT: ExecutionReceipt<Log = FilterLog> + alloy_rlp::Encodable,
{
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        self.inner.encode(out);
    }

    fn length(&self) -> usize {
        self.inner.length()
    }
}

impl<ExecutionReceiptT: ExecutionReceipt<Log = FilterLog>> ExecutionReceipt
    for TempoBlockReceipt<ExecutionReceiptT>
{
    type Log = FilterLog;

    fn cumulative_gas_used(&self) -> u64 {
        self.inner.cumulative_gas_used()
    }

    fn logs_bloom(&self) -> &Bloom {
        self.inner.logs_bloom()
    }

    fn transaction_logs(&self) -> &[Self::Log] {
        self.inner.transaction_logs()
    }

    fn root_or_status(&self) -> RootOrStatus<'_> {
        self.inner.root_or_status()
    }
}

impl<ExecutionReceiptT: ExecutionReceipt<Log = FilterLog>>
    ReceiptConstructor<transaction::TempoSignedTransaction>
    for TempoBlockReceipt<ExecutionReceiptT>
{
    type Context = ();
    type ExecutionReceipt = ExecutionReceiptT;
    type Hardfork = tempo_hardfork::TempoHardfork;

    fn new_receipt(
        _context: &Self::Context,
        _hardfork: Self::Hardfork,
        transaction: &transaction::TempoSignedTransaction,
        transaction_receipt: TransactionReceipt<Self::ExecutionReceipt>,
        block_hash: &B256,
        block_number: u64,
    ) -> Self {
        let fee_payer = transaction
            .recovered()
            .inner()
            .fee_payer(*transaction.caller())
            .unwrap_or(*transaction.caller());
        let fee_token = (transaction_receipt
            .effective_gas_price
            .is_some_and(|price| price != 0)
            && transaction_receipt.gas_used != 0)
            .then(|| {
                transaction_receipt
                    .transaction_logs()
                    .last()
                    .map(|log| log.address)
            })
            .flatten();

        Self {
            inner: transaction_receipt,
            block_hash: *block_hash,
            block_number,
            fee_token,
            fee_payer,
        }
    }
}

impl<ExecutionReceiptT: ExecutionReceipt<Log = FilterLog>> ReceiptTrait
    for TempoBlockReceipt<ExecutionReceiptT>
{
    fn block_number(&self) -> u64 {
        self.block_number
    }

    fn block_hash(&self) -> &B256 {
        &self.block_hash
    }

    fn contract_address(&self) -> Option<&Address> {
        self.inner.contract_address.as_ref()
    }

    fn effective_gas_price(&self) -> Option<&u128> {
        self.inner.effective_gas_price.as_ref()
    }

    fn from(&self) -> &Address {
        &self.inner.from
    }

    fn gas_used(&self) -> u64 {
        self.inner.gas_used
    }

    fn to(&self) -> Option<&Address> {
        self.inner.to.as_ref()
    }

    fn transaction_hash(&self) -> &B256 {
        &self.inner.transaction_hash
    }

    fn transaction_index(&self) -> u64 {
        self.inner.transaction_index
    }
}

impl
    ExecutionReceiptBuilder<
        edr_chain_l1::HaltReason,
        edr_chain_l1::Hardfork,
        transaction::SignedTransactionWithFallbackToPostEip155,
    > for GenericExecutionReceiptBuilder
{
    type Receipt = TypedEnvelope<edr_receipt::Execution<ExecutionLog>>;

    fn new_receipt_builder<StateT: State>(
        _pre_execution_state: StateT,
        _transaction: &transaction::SignedTransactionWithFallbackToPostEip155,
    ) -> Result<Self, StateT::Error> {
        Ok(Self)
    }

    fn build_receipt(
        self,
        transaction: &crate::transaction::SignedTransactionWithFallbackToPostEip155,
        result: &ExecutionResult<edr_chain_l1::HaltReason>,
        hardfork: edr_chain_l1::Hardfork,
        cumulative_gas_used: u64,
        state_root: B256,
    ) -> Self::Receipt {
        let logs = result.logs().to_vec();
        let logs_bloom = logs_to_bloom(&logs);

        let receipt = if hardfork >= EvmSpecId::BYZANTIUM {
            edr_receipt::execution::Eip658 {
                status: result.is_success(),
                cumulative_gas_used,
                logs_bloom,
                logs,
            }
            .into()
        } else {
            edr_receipt::execution::Legacy {
                root: state_root,
                cumulative_gas_used,
                logs_bloom,
                logs,
            }
            .into()
        };

        TypedEnvelope::new(receipt, transaction.transaction_type())
    }
}

impl
    ExecutionReceiptBuilder<
        crate::tempo::TempoHaltReason,
        tempo_hardfork::TempoHardfork,
        transaction::TempoSignedTransaction,
    > for GenericExecutionReceiptBuilder
{
    type Receipt = TypedEnvelope<edr_receipt::Execution<ExecutionLog>>;

    fn new_receipt_builder<StateT: State>(
        _pre_execution_state: StateT,
        _transaction: &transaction::TempoSignedTransaction,
    ) -> Result<Self, StateT::Error> {
        Ok(Self)
    }

    fn build_receipt(
        self,
        transaction: &transaction::TempoSignedTransaction,
        result: &ExecutionResult<crate::tempo::TempoHaltReason>,
        _hardfork: tempo_hardfork::TempoHardfork,
        cumulative_gas_used: u64,
        _state_root: B256,
    ) -> Self::Receipt {
        let logs = result.logs().to_vec();
        let receipt = edr_receipt::execution::Eip658 {
            status: result.is_success(),
            cumulative_gas_used,
            logs_bloom: logs_to_bloom(&logs),
            logs,
        }
        .into();

        TypedEnvelope::new(receipt, transaction.transaction_type())
    }
}
