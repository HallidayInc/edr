#![cfg(feature = "test-remote")]

use std::sync::Arc;

use alloy_sol_types::{sol, SolCall};
use edr_chain_config::ChainOverride;
use edr_eth::BlockSpec;
use edr_generic::{ArcChainSpec, ArcHardfork};
use edr_primitives::{address, Address, Bytes, U256};
use edr_provider::{MethodInvocation, Provider, ProviderRequest};
use edr_rpc_eth::client::EthRpcClientForChainSpec;
use serde_json::json;

use crate::integration::helpers::{get_chain_fork_provider, get_chain_fork_provider_with_hardfork};

const ARC_TESTNET_CHAIN_ID: u64 = 5_042_002;
const ARC_TESTNET_RPC_URL: &str = "https://rpc.testnet.arc.io/";
const POST_ZERO8_BLOCK: u64 = 60_260_900;
const PRE_OSAKA_BLOCK: u64 = 26_148_086;
const SYSTEM_ACCOUNTING: Address = address!("1800000000000000000000000000000000000002");
const NATIVE_COIN_AUTHORITY: Address = address!("1800000000000000000000000000000000000000");
const NATIVE_TRANSFER_LOG: Address = address!("fffffffffffffffffffffffffffffffffffffffe");

sol! {
    struct ArcGasValuesRemote {
        uint64 gasUsed;
        uint64 gasUsedSmoothed;
        uint64 nextBaseFee;
    }

    interface ArcSystemAccountingRemote {
        function getGasValues(uint64 blockNumber) external view returns (ArcGasValuesRemote memory gasValues);
    }
}

fn testnet_override() -> ChainOverride<ArcHardfork> {
    ChainOverride {
        name: "Arc Testnet".to_owned(),
        hardfork_activation_overrides: None,
        native_token_mirror: None,
    }
}

fn request(
    provider: &Provider<ArcChainSpec>,
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let invocation: MethodInvocation<ArcChainSpec> =
        serde_json::from_value(json!({ "method": method, "params": params }))?;
    Ok(provider
        .handle_request(ProviderRequest::with_single(invocation))?
        .result)
}

fn quantity(value: &serde_json::Value) -> anyhow::Result<u64> {
    Ok(u64::from_str_radix(
        value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("expected JSON-RPC quantity"))?
            .trim_start_matches("0x"),
        16,
    )?)
}

fn quantity_u256(value: &serde_json::Value) -> anyhow::Result<U256> {
    Ok(U256::from_str_radix(
        value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("expected JSON-RPC quantity"))?
            .trim_start_matches("0x"),
        16,
    )?)
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_and_mine_with_prague_zero5_and_zero6() -> anyhow::Result<()> {
    for hardfork in [ArcHardfork::ZERO5_PRAGUE, ArcHardfork::ZERO6_PRAGUE] {
        let provider = get_chain_fork_provider_with_hardfork::<ArcChainSpec>(
            ARC_TESTNET_CHAIN_ID,
            PRE_OSAKA_BLOCK,
            testnet_override(),
            ARC_TESTNET_RPC_URL.to_owned(),
            hardfork,
        )?;
        request(&provider, "evm_mine", json!([]))?;
        let block = request(
            &provider,
            "eth_getBlockByNumber",
            json!([format!("{:#x}", PRE_OSAKA_BLOCK + 1), false]),
        )?;
        let extra_data: Bytes = serde_json::from_value(block["extraData"].clone())?;
        assert_eq!(extra_data.len(), 8, "{hardfork:?}");
        assert_eq!(quantity(&block["number"])?, PRE_OSAKA_BLOCK + 1);
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_preserves_arc_state_and_fee_accounting() -> anyhow::Result<()> {
    let client = Arc::new(EthRpcClientForChainSpec::<ArcChainSpec>::new(
        ARC_TESTNET_RPC_URL,
        edr_defaults::CACHE_DIR.into(),
        None,
    )?);
    let provider = get_chain_fork_provider::<ArcChainSpec>(
        ARC_TESTNET_CHAIN_ID,
        POST_ZERO8_BLOCK,
        testnet_override(),
        ARC_TESTNET_RPC_URL.to_owned(),
        None,
    )?;

    let remote_balance = client
        .get_balance(
            NATIVE_COIN_AUTHORITY,
            Some(BlockSpec::Number(POST_ZERO8_BLOCK)),
        )
        .await?;
    let local_balance = quantity_u256(&request(
        &provider,
        "eth_getBalance",
        json!([NATIVE_COIN_AUTHORITY, format!("{POST_ZERO8_BLOCK:#x}")]),
    )?)?;
    assert_eq!(local_balance, remote_balance);

    let accounting_call = ArcSystemAccountingRemote::getGasValuesCall {
        blockNumber: POST_ZERO8_BLOCK,
    };
    let calldata = Bytes::from(accounting_call.abi_encode());
    let remote_accounting = client
        .call(
            SYSTEM_ACCOUNTING,
            calldata.clone(),
            BlockSpec::Number(POST_ZERO8_BLOCK),
        )
        .await?;
    let local_accounting: Bytes = serde_json::from_value(request(
        &provider,
        "eth_call",
        json!([{
            "to": SYSTEM_ACCOUNTING,
            "data": calldata,
        }, format!("{POST_ZERO8_BLOCK:#x}")]),
    )?)?;
    assert_eq!(local_accounting, remote_accounting);

    request(&provider, "evm_mine", json!([]))?;
    request(&provider, "evm_mine", json!([]))?;
    let first_number = POST_ZERO8_BLOCK + 1;
    let second_number = POST_ZERO8_BLOCK + 2;
    let first = request(
        &provider,
        "eth_getBlockByNumber",
        json!([format!("{first_number:#x}"), false]),
    )?;
    let second = request(
        &provider,
        "eth_getBlockByNumber",
        json!([format!("{second_number:#x}"), false]),
    )?;
    let first_extra: Bytes = serde_json::from_value(first["extraData"].clone())?;
    let second_extra: Bytes = serde_json::from_value(second["extraData"].clone())?;
    assert_eq!(first_extra.len(), 8);
    assert_eq!(second_extra.len(), 8);
    assert_eq!(
        u128::from(quantity(&second["baseFeePerGas"])?),
        u128::from(u64::from_be_bytes(first_extra.as_ref().try_into()?)),
    );

    for (number, block) in [(first_number, first), (second_number, second)] {
        let call = ArcSystemAccountingRemote::getGasValuesCall {
            blockNumber: number,
        };
        let output: Bytes = serde_json::from_value(request(
            &provider,
            "eth_call",
            json!([{
                "to": SYSTEM_ACCOUNTING,
                "data": Bytes::from(call.abi_encode()),
            }, "latest"]),
        )?)?;
        let values = ArcSystemAccountingRemote::getGasValuesCall::abi_decode_returns(&output)?;
        let extra: Bytes = serde_json::from_value(block["extraData"].clone())?;
        assert_eq!(values.gasUsed, quantity(&block["gasUsed"])?);
        assert_eq!(
            values.nextBaseFee,
            u64::from_be_bytes(extra.as_ref().try_into()?),
        );
    }

    let accounts: Vec<Address> =
        serde_json::from_value(request(&provider, "eth_accounts", json!([]))?)?;
    let sender = accounts[0];
    let recipient = accounts[1];
    let recipient_before = quantity_u256(&request(
        &provider,
        "eth_getBalance",
        json!([recipient, "latest"]),
    )?)?;
    let transaction_hash = request(
        &provider,
        "eth_sendTransaction",
        json!([{
            "from": sender,
            "to": recipient,
            "value": "0x1",
            "gas": "0x186a0",
        }]),
    )?;
    let receipt = request(
        &provider,
        "eth_getTransactionReceipt",
        json!([transaction_hash]),
    )?;
    assert_eq!(receipt["status"], "0x1");
    assert_eq!(
        quantity_u256(&request(
            &provider,
            "eth_getBalance",
            json!([recipient, "latest"]),
        )?)?,
        recipient_before + U256::from(1),
    );
    let receipt_logs = receipt["logs"].as_array().expect("receipt logs");
    assert_eq!(receipt_logs.len(), 1);
    assert_eq!(
        serde_json::from_value::<Address>(receipt_logs[0]["address"].clone())?,
        NATIVE_TRANSFER_LOG,
    );
    let filtered_logs = request(
        &provider,
        "eth_getLogs",
        json!([{
            "address": NATIVE_TRANSFER_LOG,
            "blockHash": receipt["blockHash"],
        }]),
    )?;
    assert_eq!(filtered_logs, receipt["logs"]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn forks_on_both_sides_of_arc_protocol_boundaries() -> anyhow::Result<()> {
    // These are the last and first blocks at the canonical timestamp boundaries.
    for block_number in [
        11_172_018, 11_172_019, 44_287_066, 44_287_067, 44_295_020, 44_295_021, 47_591_459,
        47_591_460, 60_260_873, 60_260_874,
    ] {
        let provider = get_chain_fork_provider::<ArcChainSpec>(
            ARC_TESTNET_CHAIN_ID,
            block_number,
            testnet_override(),
            ARC_TESTNET_RPC_URL.to_owned(),
            None,
        )?;
        let block = request(
            &provider,
            "eth_getBlockByNumber",
            json!([format!("{block_number:#x}"), false]),
        )?;
        assert_eq!(quantity(&block["number"])?, block_number);
        let extra: Bytes = serde_json::from_value(block["extraData"].clone())?;
        assert_eq!(extra.len(), if block_number < 44_287_066 { 17 } else { 8 });
    }

    Ok(())
}
