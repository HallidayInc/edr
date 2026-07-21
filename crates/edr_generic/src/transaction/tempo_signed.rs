use alloy_consensus::transaction::{Recovered, SignerRecoverable as _, TxHashRef as _};
use alloy_eips::eip2718::{Decodable2718 as _, Encodable2718 as _};
use alloy_evm::FromRecoveredTx as _;
use edr_chain_spec::{ExecutableTransaction, TransactionValidation};
use edr_primitives::{Address, Bytes, B256, U256};
use edr_provider::spec::HardforkValidationData;
use edr_transaction::{
    IsEip155, IsEip4844, IsLegacy, IsSupported, TransactionMut, TransactionType, TxKind,
};
use revm_context_interface::Transaction;
use tempo_primitives::{TempoSignature, TempoTransaction, TempoTxEnvelope};
use tempo_revm::{TempoInvalidTransaction, TempoTxEnv};

use super::{SignedTransactionWithFallbackToPostEip155, Type};

/// Error decoding an EDR transaction as a Tempo transaction envelope.
#[derive(Debug, thiserror::Error)]
pub enum TempoSignedTransactionError {
    /// The transaction envelope is not supported by Tempo.
    #[error("invalid Tempo transaction envelope: {0}")]
    Decode(#[from] alloy_eips::eip2718::Eip2718Error),
}

/// A recovered transaction backed by Tempo's canonical transaction envelope.
///
/// The cached [`TempoTxEnv`] is the exact transaction environment consumed by
/// `tempo-revm`. EDR's transaction traits delegate to it for block-building and
/// request plumbing.
#[derive(Clone, Debug)]
pub struct TempoSignedTransaction {
    recovered: Recovered<TempoTxEnvelope>,
    env: TempoTxEnv,
    to: Option<Address>,
    rlp_encoding: Bytes,
    authorization_list: Vec<edr_eip7702::SignedAuthorization>,
}

impl TempoSignedTransaction {
    /// Creates a transaction from Tempo's canonical recovered envelope.
    pub fn new(recovered: Recovered<TempoTxEnvelope>) -> Self {
        let env = TempoTxEnv::from_recovered_tx(recovered.inner(), recovered.signer());
        let to = match env.inner.kind {
            TxKind::Call(to) => Some(to),
            TxKind::Create => None,
        };
        let rlp_encoding = recovered.inner().encoded_2718();
        let authorization_list = Transaction::authorization_list(&env)
            .filter_map(|authorization| authorization.as_ref().left().cloned())
            .collect();

        Self {
            recovered,
            env,
            to,
            rlp_encoding: rlp_encoding.into(),
            authorization_list,
        }
    }

    pub(crate) fn new_for_call(
        transaction: TempoTransaction,
        signature: TempoSignature,
        caller: Address,
        key_id: Option<Address>,
    ) -> Self {
        let envelope = transaction.into_signed(signature).into();
        let mut transaction = Self::new(Recovered::new_unchecked(envelope, caller));
        if let Some(aa_env) = transaction.env.tempo_tx_env.as_mut() {
            aa_env.override_key_id = key_id;
        }
        transaction
    }

    /// Decodes an EDR Ethereum transaction into Tempo's canonical envelope.
    pub fn try_from_edr(
        transaction: SignedTransactionWithFallbackToPostEip155,
    ) -> Result<Self, TempoSignedTransactionError> {
        let caller = *ExecutableTransaction::caller(&transaction);
        let rlp_encoding = transaction.rlp_encoding().clone();
        let envelope = TempoTxEnvelope::decode_2718(&mut rlp_encoding.as_ref())?;
        Ok(Self::new(Recovered::new_unchecked(envelope, caller)))
    }

    /// Returns the native Tempo transaction environment.
    pub fn tempo_tx_env(&self) -> TempoTxEnv {
        self.env.clone()
    }

    /// Returns the recovered Tempo envelope.
    pub fn recovered(&self) -> &Recovered<TempoTxEnvelope> {
        &self.recovered
    }
}

impl Default for TempoSignedTransaction {
    fn default() -> Self {
        Self::try_from_edr(SignedTransactionWithFallbackToPostEip155::default())
            .expect("the default Ethereum transaction must be a valid Tempo envelope")
    }
}

impl From<SignedTransactionWithFallbackToPostEip155> for TempoSignedTransaction {
    fn from(value: SignedTransactionWithFallbackToPostEip155) -> Self {
        Self::try_from_edr(value)
            .expect("Tempo does not support this Ethereum transaction envelope")
    }
}

impl alloy_rlp::Decodable for TempoSignedTransaction {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let envelope = TempoTxEnvelope::decode_2718(buf).map_err(alloy_rlp::Error::from)?;
        let signer = envelope
            .recover_signer()
            .map_err(|_| alloy_rlp::Error::Custom("invalid Tempo transaction signature"))?;

        Ok(Self::new(Recovered::new_unchecked(envelope, signer)))
    }
}

impl alloy_rlp::Encodable for TempoSignedTransaction {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        out.put_slice(&self.rlp_encoding);
    }

    fn length(&self) -> usize {
        self.rlp_encoding.len()
    }
}

impl TransactionValidation for TempoSignedTransaction {
    type ValidationError = TempoInvalidTransaction;
}

impl ExecutableTransaction for TempoSignedTransaction {
    fn caller(&self) -> &Address {
        self.recovered.signer_ref()
    }

    fn gas_limit(&self) -> u64 {
        self.env.inner.gas_limit
    }

    fn gas_price(&self) -> &u128 {
        &self.env.inner.gas_price
    }

    fn kind(&self) -> TxKind {
        self.env.inner.kind
    }

    fn value(&self) -> &U256 {
        &self.env.inner.value
    }

    fn data(&self) -> &Bytes {
        &self.env.inner.data
    }

    fn nonce(&self) -> u64 {
        self.env.inner.nonce
    }

    fn chain_id(&self) -> Option<u64> {
        self.env.inner.chain_id
    }

    fn access_list(&self) -> Option<&[edr_eip2930::AccessListItem]> {
        (self.env.inner.tx_type != 0).then_some(self.env.inner.access_list.as_slice())
    }

    fn effective_gas_price(&self, block_base_fee: u128) -> Option<u128> {
        (self.env.inner.tx_type != 0)
            .then(|| Transaction::effective_gas_price(&self.env, block_base_fee))
    }

    fn max_fee_per_gas(&self) -> Option<&u128> {
        (self.env.inner.tx_type != 0).then_some(&self.env.inner.gas_price)
    }

    fn max_priority_fee_per_gas(&self) -> Option<&u128> {
        self.env.inner.gas_priority_fee.as_ref()
    }

    fn blob_hashes(&self) -> &[B256] {
        &self.env.inner.blob_hashes
    }

    fn max_fee_per_blob_gas(&self) -> Option<&u128> {
        None
    }

    fn total_blob_gas(&self) -> Option<u64> {
        None
    }

    fn authorization_list(&self) -> Option<&[edr_eip7702::SignedAuthorization]> {
        (!self.authorization_list.is_empty()).then_some(&self.authorization_list)
    }

    fn rlp_encoding(&self) -> &Bytes {
        &self.rlp_encoding
    }

    fn transaction_hash(&self) -> &B256 {
        self.recovered.inner().tx_hash()
    }
}

impl TransactionMut for TempoSignedTransaction {
    fn set_gas_limit(&mut self, gas_limit: u64) {
        self.env.inner.gas_limit = gas_limit;
    }
}

impl HardforkValidationData for TempoSignedTransaction {
    fn to(&self) -> Option<&Address> {
        // Tempo has no blob transaction type, so this is only used by the
        // common validation path to distinguish calls from deployments.
        self.to.as_ref()
    }

    fn gas_price(&self) -> Option<&u128> {
        matches!(self.env.inner.tx_type, 0 | 1).then_some(&self.env.inner.gas_price)
    }

    fn max_fee_per_gas(&self) -> Option<&u128> {
        ExecutableTransaction::max_fee_per_gas(self)
    }

    fn max_priority_fee_per_gas(&self) -> Option<&u128> {
        ExecutableTransaction::max_priority_fee_per_gas(self)
    }

    fn access_list(&self) -> Option<&Vec<edr_eip2930::AccessListItem>> {
        (self.env.inner.tx_type != 0).then_some(&self.env.inner.access_list)
    }

    fn blobs(&self) -> Option<&Vec<edr_transaction::pooled::eip4844::Blob>> {
        None
    }

    fn blob_hashes(&self) -> Option<&Vec<B256>> {
        None
    }

    fn authorization_list(&self) -> Option<&Vec<edr_eip7702::SignedAuthorization>> {
        (!self.authorization_list.is_empty()).then_some(&self.authorization_list)
    }
}

impl TransactionType for TempoSignedTransaction {
    type Type = Type;

    fn transaction_type(&self) -> Self::Type {
        Type::from(self.env.inner.tx_type)
    }
}

impl IsSupported for TempoSignedTransaction {
    fn is_supported_transaction(&self) -> bool {
        matches!(self.env.inner.tx_type, 0 | 1 | 2 | 4 | 0x76)
    }
}

impl IsEip155 for TempoSignedTransaction {
    fn is_eip155(&self) -> bool {
        ExecutableTransaction::chain_id(self).is_some()
    }
}

impl IsEip4844 for TempoSignedTransaction {
    fn is_eip4844(&self) -> bool {
        false
    }
}

impl IsLegacy for TempoSignedTransaction {
    fn is_legacy(&self) -> bool {
        self.env.inner.tx_type == 0
    }
}

edr_transaction::impl_revm_transaction_trait!(TempoSignedTransaction);

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use alloy_rlp::Decodable as _;
    use edr_chain_spec::ExecutableTransaction as _;
    use edr_primitives::{Address, B256};
    use edr_transaction::{IsSupported as _, TransactionType as _};

    use super::{TempoSignedTransaction, Type};

    #[test]
    fn decodes_native_tempo_envelope() {
        // Real Moderato transaction fetched through eth_getRawTransactionByHash.
        let encoded = alloy_primitives::hex::decode("76f8e082a5bf8410f50aef84587b96ef8308bad2f85ef85c9420c000000000000000000000000000000000000080b84440c10f190000000000000000000000000c24bed95abf786fd4750ad9ec3c00105e60ec1f000000000000000000000000000000000000000000000000000000e8d4a51000c0a0ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff80846a5f09a2808080c0b84102883a94742034c2179ebec62e17cee8bb6e0a97c6875c7a22dd612a91fb1e0d0244c8b9d4f18a39387728e6b384c10908abc9357f861724616847b6293956e91c").unwrap();
        let mut encoded = encoded.as_slice();

        let transaction = TempoSignedTransaction::decode(&mut encoded).unwrap();

        assert!(encoded.is_empty());
        assert_eq!(transaction.transaction_type(), Type::Unrecognized(0x76));
        assert!(transaction.is_supported_transaction());
        assert_eq!(
            transaction.caller(),
            &Address::from_str("0x5bc1473610754a5ca10749552b119df90c1a1877").unwrap()
        );
        assert_eq!(
            transaction.transaction_hash(),
            &B256::from_str("0x9cc0f3be3c0853a94afdee8f50581c76877623baa24fdbfd472db30b2cf20cd2")
                .unwrap()
        );
    }
}
