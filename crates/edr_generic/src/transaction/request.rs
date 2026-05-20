use std::marker::PhantomData;

use edr_chain_l1::{
    rpc::{call::L1CallRequest, transaction::L1RpcTransactionRequest},
    L1ChainSpec, L1TransactionRequest,
};
use edr_chain_spec::{ChainSpec, HardforkChainSpec};
use edr_chain_spec_evm::EvmChainSpec;
use edr_chain_spec_provider::ProviderChainSpec;
use edr_primitives::Address;
use edr_provider::{
    spec::{CallContext, FromRpcType, TransactionContext},
    time::TimeSinceEpoch,
    ProviderError, ProviderErrorForChainSpec,
};
use edr_signer::{FakeSign, SecretKey, Sign, SignatureError};

use crate::transaction::SignedTransactionWithFallbackToPostEip155;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GenericTransactionRequest<ChainSpecT>(
    pub(crate) L1TransactionRequest,
    PhantomData<ChainSpecT>,
);

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
        unsafe {
            <L1TransactionRequest as Sign>::sign_for_sender_unchecked(self.0, secret_key, caller)
        }
        .map(Into::into)
    }
}

impl<ChainSpecT, TimerT> FromRpcType<L1CallRequest, TimerT>
    for GenericTransactionRequest<ChainSpecT>
where
    ChainSpecT: 'static
        + ChainSpec
        + EvmChainSpec
        + HardforkChainSpec
        + ProviderChainSpec
        + edr_provider::ProviderSpec<TimerT>,
    TimerT: Clone + TimeSinceEpoch,
{
    type Context<'context> = CallContext<'context, ChainSpecT, TimerT>;
    type Error = ProviderErrorForChainSpec<ChainSpecT>;

    fn from_rpc_type(
        value: L1CallRequest,
        context: Self::Context<'_>,
    ) -> Result<Self, Self::Error> {
        // SAFETY: CallContext has the same memory layout regardless of ChainSpecT
        // parameter GenericChainSpec and L1ChainSpec have compatible runtime
        // representations
        let l1_context: CallContext<'_, L1ChainSpec, TimerT> =
            unsafe { std::mem::transmute(context) };

        match <L1TransactionRequest as FromRpcType<L1CallRequest, TimerT>>::from_rpc_type(
            value, l1_context,
        ) {
            Ok(req) => Ok(Self(req, PhantomData)),
            Err(edr_provider::ProviderError::InvalidArgument(msg)) => {
                Err(ProviderError::InvalidArgument(msg))
            }
            Err(_) => Err(ProviderError::InvalidArgument(
                "Failed to convert RPC call request".to_string(),
            )),
        }
    }
}

impl<ChainSpecT, TimerT> FromRpcType<L1RpcTransactionRequest, TimerT>
    for GenericTransactionRequest<ChainSpecT>
where
    ChainSpecT: 'static
        + ChainSpec
        + EvmChainSpec
        + HardforkChainSpec
        + ProviderChainSpec
        + edr_provider::ProviderSpec<TimerT>,
    TimerT: Clone + TimeSinceEpoch,
{
    type Context<'context> = TransactionContext<'context, ChainSpecT, TimerT>;
    type Error = ProviderErrorForChainSpec<ChainSpecT>;

    fn from_rpc_type(
        value: L1RpcTransactionRequest,
        context: Self::Context<'_>,
    ) -> Result<Self, Self::Error> {
        // SAFETY: TransactionContext has the same memory layout regardless of
        // ChainSpecT parameter GenericChainSpec and L1ChainSpec have compatible
        // runtime representations
        let l1_context: TransactionContext<'_, L1ChainSpec, TimerT> =
            unsafe { std::mem::transmute(context) };

        match <L1TransactionRequest as FromRpcType<L1RpcTransactionRequest, TimerT>>::from_rpc_type(
            value, l1_context,
        ) {
            Ok(req) => Ok(Self(req, PhantomData)),
            Err(edr_provider::ProviderError::InvalidArgument(msg)) => {
                Err(ProviderError::InvalidArgument(msg))
            }
            Err(_) => Err(ProviderError::InvalidArgument(
                "Failed to convert RPC transaction request".to_string(),
            )),
        }
    }
}
