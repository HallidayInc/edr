use std::{marker::PhantomData, num::NonZeroU64};

use alloy_rpc_types::{AccessList, AccessListItem, TransactionInput, TransactionRequest};
use edr_chain_l1::{
    rpc::{call::L1CallRequest, transaction::L1RpcTransactionRequest},
    L1TransactionRequest,
};
use edr_chain_spec::EvmSpecId;
use edr_primitives::{Address, Bytes, U256};
use edr_provider::{
    calculate_eip1559_fee_parameters,
    requests::validation::{validate_call_request, validate_send_transaction_request},
    spec::{CallContext, FromRpcType, MaybeSender, TransactionContext},
    time::TimeSinceEpoch,
    ProviderError, ProviderErrorForChainSpec,
};
use edr_signer::{FakeSign, SecretKey, Sign, SignatureError};
use edr_transaction::TxKind;
use tempo_primitives::{
    transaction::{
        key_authorization::serde_nonzero_quantity_opt,
        tt_signature::{
            KeychainSignature, P256SignatureWithPreHash, PrimitiveSignature, WebAuthnSignature,
        },
        Call, SignedKeyAuthorization, TempoSignedAuthorization,
    },
    SignatureType, TempoSignature, TempoTransaction,
};

use crate::{
    transaction::{SignedTransactionWithFallbackToPostEip155, TempoSignedTransaction},
    GenericChainSpec, TempoChainSpec,
};

/// Container type for various Ethereum transaction requests.
// NOTE: This is a newtype only because the default FromRpcType implementation
// provides an error of ProviderError<L1ChainSpec> specifically. Despite us
// wanting the same logic, we need to use our own type and copy the
// implementation.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GenericTransactionRequest<ChainSpecT = GenericChainSpec>(
    L1TransactionRequest,
    PhantomData<fn() -> ChainSpecT>,
);

/// A standard Ethereum-style request executed as a native Tempo transaction.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TempoTransactionRequest {
    /// A standard Ethereum transaction request.
    Ethereum(GenericTransactionRequest<TempoChainSpec>),
    /// A native Tempo AA request used by `eth_call` and `eth_estimateGas`.
    Tempo {
        transaction: TempoTransaction,
        signature: TempoSignature,
        key_id: Option<Address>,
    },
}

/// Tempo's JSON-RPC call request, including the fields that select native AA
/// transaction semantics.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoCallRequest {
    /// Standard Ethereum call fields.
    #[serde(flatten)]
    pub inner: L1CallRequest,
    /// Transaction nonce.
    #[serde(default, with = "alloy_serde::quantity::opt")]
    pub nonce: Option<u64>,
    /// Chain ID.
    #[serde(default, with = "alloy_serde::quantity::opt")]
    pub chain_id: Option<u64>,
    /// Optional fee token preference.
    #[serde(default)]
    pub fee_token: Option<Address>,
    /// Optional key for Tempo's two-dimensional nonce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce_key: Option<U256>,
    /// Calls for a Tempo batch transaction.
    #[serde(default)]
    pub calls: Vec<Call>,
    /// Signature type hint used to model signature verification gas.
    #[serde(default)]
    pub key_type: Option<SignatureType>,
    /// Signature-specific data used to model signature verification gas.
    #[serde(default)]
    pub key_data: Option<Bytes>,
    /// Access key ID used to model keychain validation and spending limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<Address>,
    /// Tempo authorization list.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        rename = "aaAuthorizationList"
    )]
    pub tempo_authorization_list: Vec<TempoSignedAuthorization>,
    /// Key authorization for provisioning an access key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_authorization: Option<SignedKeyAuthorization>,
    /// Expiration timestamp.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_nonzero_quantity_opt"
    )]
    pub valid_before: Option<NonZeroU64>,
    /// Earliest valid timestamp.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_nonzero_quantity_opt"
    )]
    pub valid_after: Option<NonZeroU64>,
    /// Optional sponsored transaction fee-payer signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_payer_signature: Option<alloy_primitives::Signature>,
}

impl TempoCallRequest {
    fn has_aa_fields(&self) -> bool {
        !self.calls.is_empty()
            || self.nonce_key.is_some()
            || self.fee_token.is_some()
            || !self.tempo_authorization_list.is_empty()
            || self.key_authorization.is_some()
            || self.key_id.is_some()
            || self.key_type.is_some()
            || self.key_data.is_some()
            || self.valid_before.is_some()
            || self.valid_after.is_some()
            || self.fee_payer_signature.is_some()
            || self.inner.transaction_type == Some(0x76)
    }
}

impl MaybeSender for TempoCallRequest {
    fn maybe_sender(&self) -> Option<&Address> {
        self.inner.from.as_ref()
    }
}

impl<ChainSpecT> From<L1TransactionRequest> for GenericTransactionRequest<ChainSpecT> {
    fn from(value: L1TransactionRequest) -> Self {
        Self(value, PhantomData)
    }
}

impl<ChainSpecT> FakeSign for GenericTransactionRequest<ChainSpecT> {
    type Signed = SignedTransactionWithFallbackToPostEip155;

    fn fake_sign(self, sender: Address) -> SignedTransactionWithFallbackToPostEip155 {
        <L1TransactionRequest as FakeSign>::fake_sign(self.0, sender).into()
    }
}

impl<ChainSpecT> Sign for GenericTransactionRequest<ChainSpecT> {
    type Signed = SignedTransactionWithFallbackToPostEip155;

    unsafe fn sign_for_sender_unchecked(
        self,
        secret_key: &SecretKey,
        caller: Address,
    ) -> Result<SignedTransactionWithFallbackToPostEip155, SignatureError> {
        // SAFETY: The safety concern is propagated in the function signature.
        unsafe {
            <L1TransactionRequest as Sign>::sign_for_sender_unchecked(self.0, secret_key, caller)
        }
        .map(Into::into)
    }
}

impl FakeSign for TempoTransactionRequest {
    type Signed = TempoSignedTransaction;

    fn fake_sign(self, sender: Address) -> Self::Signed {
        match self {
            Self::Ethereum(request) => request.fake_sign(sender).into(),
            Self::Tempo {
                transaction,
                signature,
                key_id,
            } => TempoSignedTransaction::new_for_call(transaction, signature, sender, key_id),
        }
    }
}

impl Sign for TempoTransactionRequest {
    type Signed = TempoSignedTransaction;

    unsafe fn sign_for_sender_unchecked(
        self,
        secret_key: &SecretKey,
        caller: Address,
    ) -> Result<Self::Signed, SignatureError> {
        match self {
            Self::Ethereum(request) => {
                // SAFETY: The safety concern is propagated in the function signature.
                unsafe { request.sign_for_sender_unchecked(secret_key, caller) }.map(Into::into)
            }
            request @ Self::Tempo { .. } => Ok(request.fake_sign(caller)),
        }
    }
}

macro_rules! impl_from_rpc_type {
    ($chain_spec:ty) => {
        impl<TimerT: Clone + TimeSinceEpoch> FromRpcType<L1CallRequest, TimerT>
            for GenericTransactionRequest<$chain_spec>
        {
            type Context<'context> = CallContext<'context, $chain_spec, TimerT>;

            type Error = ProviderErrorForChainSpec<$chain_spec>;

            fn from_rpc_type(
                value: L1CallRequest,
                context: Self::Context<'_>,
            ) -> Result<Self, Self::Error> {
                let CallContext {
                    data,
                    block_spec,
                    state_overrides,
                    default_gas_price_fn,
                    max_fees_fn,
                } = context;

                validate_call_request::<$chain_spec, TimerT>(data.hardfork(), &value, block_spec)?;

                let L1CallRequest {
                    from,
                    to,
                    gas,
                    gas_price,
                    max_fee_per_gas,
                    max_priority_fee_per_gas,
                    value,
                    data: input,
                    access_list,
                    // We ignore the transaction type
                    transaction_type: _transaction_type,
                    blobs: _blobs,
                    blob_hashes: _blob_hashes,
                    authorization_list,
                } = value;

                let chain_id = data.chain_id_at_block_spec(block_spec)?;
                let sender = from.unwrap_or_else(|| data.default_caller());
                let gas_limit = gas.unwrap_or_else(|| data.default_transaction_gas_limit());
                let input = input.map_or(Bytes::new(), Bytes::from);
                let nonce = data.nonce(&sender, Some(block_spec), state_overrides)?;
                let value = value.unwrap_or(U256::ZERO);

                let evm_spec_id = data.evm_spec_id();
                let request = if evm_spec_id < EvmSpecId::LONDON || gas_price.is_some() {
                    let gas_price = gas_price.map_or_else(|| default_gas_price_fn(data), Ok)?;
                    match access_list {
                        Some(access_list) if evm_spec_id >= EvmSpecId::BERLIN => {
                            L1TransactionRequest::Eip2930(edr_chain_l1::request::Eip2930 {
                                nonce,
                                gas_price,
                                gas_limit,
                                value,
                                input,
                                kind: to.into(),
                                chain_id,
                                access_list,
                            })
                        }
                        _ => L1TransactionRequest::Eip155(edr_chain_l1::request::Eip155 {
                            nonce,
                            gas_price,
                            gas_limit,
                            kind: to.into(),
                            value,
                            input,
                            chain_id,
                        }),
                    }
                } else {
                    let (max_fee_per_gas, max_priority_fee_per_gas) =
                        max_fees_fn(data, block_spec, max_fee_per_gas, max_priority_fee_per_gas)?;

                    if let Some(authorization_list) = authorization_list {
                        L1TransactionRequest::Eip7702(edr_chain_l1::request::Eip7702 {
                            chain_id,
                            nonce,
                            max_fee_per_gas,
                            max_priority_fee_per_gas,
                            gas_limit,
                            to: to.ok_or(ProviderError::Eip7702TransactionMissingReceiver)?,
                            value,
                            input,
                            access_list: access_list.unwrap_or_default(),
                            authorization_list,
                        })
                    } else {
                        L1TransactionRequest::Eip1559(edr_chain_l1::request::Eip1559 {
                            chain_id,
                            nonce,
                            max_fee_per_gas,
                            max_priority_fee_per_gas,
                            gas_limit,
                            kind: to.into(),
                            value,
                            input,
                            access_list: access_list.unwrap_or_default(),
                        })
                    }
                };

                Ok(request.into())
            }
        }

        impl<TimerT: Clone + TimeSinceEpoch> FromRpcType<L1RpcTransactionRequest, TimerT>
            for GenericTransactionRequest<$chain_spec>
        {
            type Context<'context> = TransactionContext<'context, $chain_spec, TimerT>;

            type Error = ProviderErrorForChainSpec<$chain_spec>;

            fn from_rpc_type(
                value: L1RpcTransactionRequest,
                context: Self::Context<'_>,
            ) -> Result<Self, Self::Error> {
                let TransactionContext { data } = context;

                validate_send_transaction_request(data, &value)?;

                let L1RpcTransactionRequest {
                    from,
                    to,
                    gas_price,
                    max_fee_per_gas,
                    max_priority_fee_per_gas,
                    gas,
                    value,
                    data: input,
                    nonce,
                    chain_id,
                    access_list,
                    // We ignore the transaction type
                    transaction_type: _transaction_type,
                    blobs: _blobs,
                    blob_hashes: _blob_hashes,
                    authorization_list,
                } = value;

                let chain_id = chain_id.unwrap_or_else(|| data.chain_id());
                let gas_limit = gas.unwrap_or_else(|| data.default_transaction_gas_limit());
                let input = input.map_or(Bytes::new(), Into::into);
                let nonce = nonce.map_or_else(|| data.account_next_nonce(&from), Ok)?;
                let value = value.unwrap_or(U256::ZERO);

                let current_hardfork = data.evm_spec_id();
                let request = if let Some(authorization_list) = authorization_list {
                    let (max_fee_per_gas, max_priority_fee_per_gas) =
                        calculate_eip1559_fee_parameters(
                            data,
                            max_fee_per_gas,
                            max_priority_fee_per_gas,
                        )?;

                    L1TransactionRequest::Eip7702(edr_chain_l1::request::Eip7702 {
                        nonce,
                        max_fee_per_gas,
                        max_priority_fee_per_gas,
                        gas_limit,
                        value,
                        input,
                        to: to.ok_or(ProviderError::Eip7702TransactionMissingReceiver)?,
                        chain_id,
                        access_list: access_list.unwrap_or_default(),
                        authorization_list,
                    })
                } else if current_hardfork >= EvmSpecId::LONDON
                    && (gas_price.is_none()
                        || max_fee_per_gas.is_some()
                        || max_priority_fee_per_gas.is_some())
                {
                    let (max_fee_per_gas, max_priority_fee_per_gas) =
                        calculate_eip1559_fee_parameters(
                            data,
                            max_fee_per_gas,
                            max_priority_fee_per_gas,
                        )?;

                    L1TransactionRequest::Eip1559(edr_chain_l1::request::Eip1559 {
                        nonce,
                        max_fee_per_gas,
                        max_priority_fee_per_gas,
                        gas_limit,
                        value,
                        input,
                        kind: match to {
                            Some(to) => TxKind::Call(to),
                            None => TxKind::Create,
                        },
                        chain_id,
                        access_list: access_list.unwrap_or_default(),
                    })
                } else if let Some(access_list) = access_list {
                    L1TransactionRequest::Eip2930(edr_chain_l1::request::Eip2930 {
                        nonce,
                        gas_price: gas_price.map_or_else(|| data.next_gas_price(), Ok)?,
                        gas_limit,
                        value,
                        input,
                        kind: match to {
                            Some(to) => TxKind::Call(to),
                            None => TxKind::Create,
                        },
                        chain_id,
                        access_list,
                    })
                } else {
                    L1TransactionRequest::Eip155(edr_chain_l1::request::Eip155 {
                        nonce,
                        gas_price: gas_price.map_or_else(|| data.next_gas_price(), Ok)?,
                        gas_limit,
                        value,
                        input,
                        kind: match to {
                            Some(to) => TxKind::Call(to),
                            None => TxKind::Create,
                        },
                        chain_id,
                    })
                };

                Ok(request.into())
            }
        }
    };
}

impl_from_rpc_type!(crate::GenericChainSpec);
impl_from_rpc_type!(crate::ArbChainSpec);
impl_from_rpc_type!(crate::ApeChainSpec);
impl_from_rpc_type!(crate::TempoChainSpec);

impl<TimerT: Clone + TimeSinceEpoch> FromRpcType<TempoCallRequest, TimerT>
    for TempoTransactionRequest
{
    type Context<'context> = CallContext<'context, TempoChainSpec, TimerT>;
    type Error = ProviderErrorForChainSpec<TempoChainSpec>;

    fn from_rpc_type(
        value: TempoCallRequest,
        context: Self::Context<'_>,
    ) -> Result<Self, Self::Error> {
        if !value.has_aa_fields() {
            return GenericTransactionRequest::from_rpc_type(value.inner, context)
                .map(Self::Ethereum);
        }

        let CallContext {
            data,
            block_spec,
            state_overrides,
            default_gas_price_fn: _,
            max_fees_fn,
        } = context;

        validate_call_request::<TempoChainSpec, TimerT>(data.hardfork(), &value.inner, block_spec)?;

        let TempoCallRequest {
            inner,
            nonce,
            chain_id,
            fee_token,
            nonce_key,
            calls,
            key_type,
            key_data,
            key_id,
            tempo_authorization_list,
            key_authorization,
            valid_before,
            valid_after,
            fee_payer_signature,
        } = value;

        let expected_chain_id = data.chain_id_at_block_spec(block_spec)?;
        if let Some(chain_id) = chain_id
            && chain_id != expected_chain_id
        {
            return Err(ProviderError::InvalidChainId {
                expected: expected_chain_id,
                actual: chain_id,
            });
        }

        let sender = inner.from.unwrap_or_else(|| data.default_caller());
        let nonce = nonce.map_or_else(
            || data.nonce(&sender, Some(block_spec), state_overrides),
            Ok,
        )?;
        let gas_limit = inner
            .gas
            .unwrap_or_else(|| data.default_transaction_gas_limit());
        let (max_fee_per_gas, max_priority_fee_per_gas) = max_fees_fn(
            data,
            block_spec,
            inner.max_fee_per_gas,
            inner.max_priority_fee_per_gas,
        )?;

        let access_list = inner.access_list.map(|access_list| {
            AccessList(
                access_list
                    .into_iter()
                    .map(|item| AccessListItem {
                        address: item.address,
                        storage_keys: item.storage_keys,
                    })
                    .collect(),
            )
        });

        let request = tempo_alloy::rpc::TempoTransactionRequest {
            inner: TransactionRequest {
                from: Some(sender),
                to: inner.to.map(TxKind::Call),
                gas_price: inner.gas_price,
                max_fee_per_gas: Some(max_fee_per_gas),
                max_priority_fee_per_gas: Some(max_priority_fee_per_gas),
                gas: Some(gas_limit),
                value: inner.value,
                input: TransactionInput::new(inner.data.unwrap_or_default()),
                nonce: Some(nonce),
                chain_id: Some(expected_chain_id),
                access_list,
                transaction_type: Some(0x76),
                ..Default::default()
            },
            fee_token,
            nonce_key,
            calls,
            key_type,
            key_data,
            key_id,
            tempo_authorization_list,
            key_authorization,
            valid_before,
            valid_after,
            fee_payer_signature,
        };

        let signature = mock_tempo_signature(
            request
                .key_type
                .as_ref()
                .unwrap_or(&SignatureType::Secp256k1),
            request.key_data.as_ref(),
            request.key_id,
            sender,
            data.hardfork().is_t1c(),
        );
        let transaction = request
            .build_aa()
            .map_err(|error| ProviderError::InvalidArgument(error.to_string()))?;

        Ok(Self::Tempo {
            transaction,
            signature,
            key_id,
        })
    }
}

impl<TimerT: Clone + TimeSinceEpoch> FromRpcType<L1RpcTransactionRequest, TimerT>
    for TempoTransactionRequest
{
    type Context<'context> = TransactionContext<'context, TempoChainSpec, TimerT>;
    type Error = ProviderErrorForChainSpec<TempoChainSpec>;

    fn from_rpc_type(
        value: L1RpcTransactionRequest,
        context: Self::Context<'_>,
    ) -> Result<Self, Self::Error> {
        GenericTransactionRequest::from_rpc_type(value, context).map(Self::Ethereum)
    }
}

fn mock_tempo_signature(
    key_type: &SignatureType,
    key_data: Option<&alloy_primitives::Bytes>,
    key_id: Option<Address>,
    caller: Address,
    is_t1c: bool,
) -> TempoSignature {
    let signature = mock_primitive_signature(key_type, key_data.cloned());

    if key_id.is_some() {
        let signature = if is_t1c {
            KeychainSignature::new(caller, signature)
        } else {
            KeychainSignature::new_v1(caller, signature)
        };
        TempoSignature::Keychain(signature)
    } else {
        TempoSignature::Primitive(signature)
    }
}

fn mock_primitive_signature(
    signature_type: &SignatureType,
    key_data: Option<alloy_primitives::Bytes>,
) -> PrimitiveSignature {
    match signature_type {
        SignatureType::Secp256k1 => PrimitiveSignature::Secp256k1(
            alloy_primitives::Signature::new(U256::ZERO, U256::ZERO, false),
        ),
        SignatureType::P256 => PrimitiveSignature::P256(P256SignatureWithPreHash {
            r: alloy_primitives::B256::ZERO,
            s: alloy_primitives::B256::ZERO,
            pub_key_x: alloy_primitives::B256::ZERO,
            pub_key_y: alloy_primitives::B256::ZERO,
            pre_hash: false,
        }),
        SignatureType::WebAuthn => {
            const BASE_CLIENT_JSON: &str = r#"{"type":"webauthn.get","challenge":"","origin":""}"#;
            const AUTH_DATA_SIZE: usize = 37;
            const MIN_WEBAUTHN_SIZE: usize = AUTH_DATA_SIZE + BASE_CLIENT_JSON.len();
            const DEFAULT_WEBAUTHN_SIZE: usize = 800;
            const MAX_WEBAUTHN_SIZE: usize = 8192;

            let size = key_data
                .as_ref()
                .map_or(DEFAULT_WEBAUTHN_SIZE, |data| match data.len() {
                    1 => data[0] as usize,
                    2 => u16::from_be_bytes([data[0], data[1]]) as usize,
                    4 => u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize,
                    _ => DEFAULT_WEBAUTHN_SIZE,
                });
            let size = size.clamp(MIN_WEBAUTHN_SIZE, MAX_WEBAUTHN_SIZE);

            let mut webauthn_data = vec![0u8; AUTH_DATA_SIZE];
            webauthn_data[32] = 0x01;
            let additional_bytes = size - MIN_WEBAUTHN_SIZE;
            let client_json = if additional_bytes == 0 {
                BASE_CLIENT_JSON.to_owned()
            } else {
                let padding = "x".repeat(additional_bytes);
                format!(r#"{{"type":"webauthn.get","challenge":"","origin":"{padding}"}}"#)
            };
            webauthn_data.extend_from_slice(client_json.as_bytes());

            PrimitiveSignature::WebAuthn(WebAuthnSignature {
                webauthn_data: webauthn_data.into(),
                r: alloy_primitives::B256::ZERO,
                s: alloy_primitives::B256::ZERO,
                pub_key_x: alloy_primitives::B256::ZERO,
                pub_key_y: alloy_primitives::B256::ZERO,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use edr_primitives::Address;

    use super::TempoCallRequest;

    #[test]
    fn tempo_call_request_preserves_aa_fields() {
        let request: TempoCallRequest = serde_json::from_str(
            r#"{
                "from": "0x0000000000000000000000000000000000000001",
                "to": "0x0000000000000000000000000000000000000002",
                "type": "0x76",
                "feeToken": "0x20c0000000000000000000000000000000000000",
                "nonceKey": "0x1"
            }"#,
        )
        .unwrap();

        assert!(request.has_aa_fields());
        assert_eq!(request.inner.transaction_type, Some(0x76));
        assert_eq!(
            request.fee_token,
            Some(Address::from_str("0x20c0000000000000000000000000000000000000").unwrap())
        );
        assert_eq!(request.nonce_key, Some(edr_primitives::U256::from(1)));
    }

    #[test]
    fn ethereum_call_request_does_not_select_aa_semantics() {
        let request: TempoCallRequest = serde_json::from_str(
            r#"{
                "from": "0x0000000000000000000000000000000000000001",
                "to": "0x0000000000000000000000000000000000000002"
            }"#,
        )
        .unwrap();

        assert!(!request.has_aa_fields());
    }
}
