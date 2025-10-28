use edr_chain_l1::rpc::{call::L1CallRequest, TransactionRequest};
use edr_rpc_spec::RpcSpec;
use serde::{de::DeserializeOwned, Serialize};

use crate::{eip2718::TypedEnvelope, ArbChainSpec, GenericChainSpec};

pub mod block;
pub mod receipt;
pub mod transaction;

macro_rules! impl_rpc_spec_for_chain {
    ($chain:ty) => {
        impl RpcSpec for $chain {
            type ExecutionReceipt<Log> = TypedEnvelope<edr_receipt::execution::Eip658<Log>>;
            type RpcBlock<Data>
                = self::block::GenericRpcBlock<Data>
            where
                Data: Default + DeserializeOwned + Serialize;
            type RpcCallRequest = L1CallRequest;
            type RpcReceipt = self::receipt::BlockReceipt;
            type RpcTransaction = self::transaction::TransactionWithSignature<$chain>;
            type RpcTransactionRequest = TransactionRequest;
        }
    };
}

impl_rpc_spec_for_chain!(GenericChainSpec);
impl_rpc_spec_for_chain!(ArbChainSpec);
