#![cfg(feature = "test-remote")]

use std::sync::Arc;

use edr_chain_config::{ChainOverride, HardforkActivations};
use edr_chain_l1::{
    rpc::{call::L1CallRequest, transaction::L1RpcTransactionRequest},
    Hardfork,
};
use edr_eth::BlockSpec;
use edr_generic::InjectiveChainSpec;
use edr_primitives::{address, keccak256, Address, Bytes, B256, U256};
use edr_provider::{MethodInvocation, ProviderRequest};
use edr_rpc_eth::client::EthRpcClientForChainSpec;

use crate::integration::helpers::get_chain_fork_provider;

const INJECTIVE_RPC_URL: &str = "https://sentry.evm-rpc.injective.network/";

fn calldata(signature: &str, words: impl IntoIterator<Item = [u8; 32]>) -> Bytes {
    let mut data = keccak256(signature).as_slice()[..4].to_vec();
    for word in words {
        data.extend(word);
    }
    data.into()
}

fn address_word(address: Address) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(address.as_slice());
    word
}

#[tokio::test(flavor = "multi_thread")]
async fn native_usdc_supply_matches_the_fork_block() -> anyhow::Result<()> {
    let client = Arc::new(EthRpcClientForChainSpec::<InjectiveChainSpec>::new(
        INJECTIVE_RPC_URL,
        edr_defaults::CACHE_DIR.into(),
        None,
    )?);
    let block_number = client.block_number().await?.saturating_sub(10);
    let chain_override = ChainOverride {
        name: "Injective".to_owned(),
        hardfork_activation_overrides: Some(HardforkActivations::with_spec_id(Hardfork::OSAKA)),
        native_token_mirror: None,
    };
    let provider = get_chain_fork_provider::<InjectiveChainSpec>(
        1776,
        block_number,
        chain_override,
        INJECTIVE_RPC_URL.to_owned(),
        None,
    )?;

    let usdc = address!("a00C59fF5a080D2b954d0c75e46E22a0c371235a");
    let total_supply_calldata = Bytes::from_static(&[0x18, 0x16, 0x0d, 0xdd]);
    let result = provider.handle_request(ProviderRequest::with_single(MethodInvocation::Call(
        L1CallRequest {
            to: Some(usdc),
            data: Some(total_supply_calldata.clone()),
            ..L1CallRequest::default()
        },
        Some(BlockSpec::Number(block_number)),
        None,
    )))?;
    let actual: Bytes = serde_json::from_value(result.result)?;
    let expected = client
        .call(usdc, total_supply_calldata, BlockSpec::Number(block_number))
        .await?;

    assert_eq!(actual, expected);
    assert!(U256::from_be_slice(&actual) > U256::ZERO);

    let sender = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
    let recipient = address!("000000000000000000000000000000000000bEEF");
    let allowance = U256::from(42);

    let send = |data| -> anyhow::Result<B256> {
        let response = provider.handle_request(ProviderRequest::with_single(
            MethodInvocation::SendTransaction(L1RpcTransactionRequest {
                from: sender,
                to: Some(usdc),
                gas: Some(500_000),
                data: Some(data),
                ..L1RpcTransactionRequest::default()
            }),
        ))?;
        Ok(serde_json::from_value(response.result)?)
    };
    let assert_success = |transaction_hash| -> anyhow::Result<()> {
        let response = provider.handle_request(ProviderRequest::with_single(
            MethodInvocation::GetTransactionReceipt(transaction_hash),
        ))?;
        assert_eq!(response.result["status"], "0x1");
        Ok(())
    };
    let call = |data| -> anyhow::Result<Bytes> {
        let response =
            provider.handle_request(ProviderRequest::with_single(MethodInvocation::Call(
                L1CallRequest {
                    to: Some(usdc),
                    data: Some(data),
                    ..L1CallRequest::default()
                },
                None,
                None,
            )))?;
        Ok(serde_json::from_value(response.result)?)
    };

    let balance_of_calldata = calldata("balanceOf(address)", [address_word(sender)]);
    let expected_balance = client
        .call(
            usdc,
            balance_of_calldata.clone(),
            BlockSpec::Number(block_number),
        )
        .await?;
    assert_eq!(call(balance_of_calldata)?, expected_balance);

    assert_success(send(calldata(
        "approve(address,uint256)",
        [address_word(sender), allowance.to_be_bytes()],
    ))?)?;
    let approved = call(calldata(
        "allowance(address,address)",
        [address_word(sender), address_word(sender)],
    ))?;
    assert_eq!(U256::from_be_slice(&approved), allowance);

    assert_success(send(calldata(
        "transfer(address,uint256)",
        [address_word(recipient), U256::ZERO.to_be_bytes()],
    ))?)?;
    assert_success(send(calldata(
        "transferFrom(address,address,uint256)",
        [
            address_word(sender),
            address_word(recipient),
            U256::ZERO.to_be_bytes(),
        ],
    ))?)?;

    Ok(())
}
