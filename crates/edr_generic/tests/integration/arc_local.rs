use std::sync::Arc;

use edr_generic::ArcChainSpec;
use edr_primitives::{address, keccak256, Address, Bytes, U256};
use edr_provider::{
    test_utils, time::CurrentTime, MethodInvocation, NoopLogger, Provider, ProviderRequest,
};
use edr_solidity::contract_decoder::ContractDecoder;
use parking_lot::RwLock;
use serde_json::json;
use tokio::runtime;

const AUTHORITY: Address = address!("1800000000000000000000000000000000000000");
const CONTROL: Address = address!("1800000000000000000000000000000000000001");
const FIAT_TOKEN: Address = address!("3600000000000000000000000000000000000000");

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

fn word(value: serde_json::Value) -> anyhow::Result<U256> {
    let bytes: Bytes = serde_json::from_value(value)?;
    Ok(U256::from_be_slice(&bytes))
}

#[tokio::test(flavor = "multi_thread")]
async fn native_coin_authority_moves_native_balances_for_the_fiat_token() -> anyhow::Result<()> {
    let sender = address!("00000000000000000000000000000000000000aa");
    let recipient = address!("00000000000000000000000000000000000000bb");
    let stranger = address!("00000000000000000000000000000000000000cc");
    let initial_balance = U256::from(1_000_000_000_000_000_000u128);
    let amount = U256::from(400_000_000_000_000_000u128);
    let total_supply = U256::from(5_000_000_000_000_000_000u128);
    let gas_reserve = U256::from(10_000_000_000_000_000_000u128);

    let mut config = test_utils::create_test_config::<edr_chain_l1::Hardfork>();
    config.chain_id = 5042;
    let provider = Provider::new(
        runtime::Handle::current(),
        Box::new(NoopLogger::<ArcChainSpec>::default()),
        Box::new(|_event| {}),
        config,
        Arc::new(RwLock::<ContractDecoder>::default()),
        CurrentTime,
    )?;

    for account in [FIAT_TOKEN, stranger] {
        request(&provider, "hardhat_impersonateAccount", json!([account]))?;
        request(
            &provider,
            "hardhat_setBalance",
            json!([account, format!("{gas_reserve:#x}")]),
        )?;
    }
    request(
        &provider,
        "hardhat_setBalance",
        json!([sender, format!("{initial_balance:#x}")]),
    )?;
    request(
        &provider,
        "hardhat_setStorageAt",
        json!([AUTHORITY, "0x2", format!("{total_supply:#066x}")]),
    )?;

    let balance = |account| {
        request(&provider, "eth_getBalance", json!([account, "latest"])).and_then(|value| {
            Ok(U256::from_str_radix(
                value
                    .as_str()
                    .expect("hex quantity")
                    .trim_start_matches("0x"),
                16,
            )?)
        })
    };
    let supply = || {
        request(
            &provider,
            "eth_call",
            json!([{ "to": AUTHORITY, "data": calldata("totalSupply()", []) }, "latest"]),
        )
        .and_then(word)
    };
    let send = |from: Address, data: Bytes| -> anyhow::Result<serde_json::Value> {
        let hash = request(
            &provider,
            "eth_sendTransaction",
            json!([{ "from": from, "to": AUTHORITY, "data": data, "gas": "0x493e0" }]),
        )?;
        request(&provider, "eth_getTransactionReceipt", json!([hash]))
    };
    let transfer_calldata = |from, to, value: U256| {
        calldata(
            "transfer(address,address,uint256)",
            [address_word(from), address_word(to), value.to_be_bytes()],
        )
    };

    assert_eq!(supply()?, total_supply);
    assert_eq!(balance(sender)?, initial_balance);
    assert_eq!(balance(recipient)?, U256::ZERO);

    let unauthorized = send(stranger, transfer_calldata(sender, recipient, amount))?;
    assert_eq!(unauthorized["status"], "0x0");
    assert_eq!(balance(sender)?, initial_balance);
    assert_eq!(balance(recipient)?, U256::ZERO);

    let transfer = send(FIAT_TOKEN, transfer_calldata(sender, recipient, amount))?;
    assert_eq!(transfer["status"], "0x1");
    assert_eq!(balance(sender)?, initial_balance - amount);
    assert_eq!(balance(recipient)?, amount);
    assert_eq!(
        transfer["logs"].as_array().map(Vec::len),
        Some(1),
        "transfer emits one EIP-7708 Transfer log"
    );
    assert_eq!(supply()?, total_supply);

    let mint_amount = U256::from(250);
    let mint = send(
        FIAT_TOKEN,
        calldata(
            "mint(address,uint256)",
            [address_word(recipient), mint_amount.to_be_bytes()],
        ),
    )?;
    assert_eq!(mint["status"], "0x1");
    assert_eq!(balance(recipient)?, amount + mint_amount);
    assert_eq!(supply()?, total_supply + mint_amount);

    let burn_amount = U256::from(100);
    let burn = send(
        FIAT_TOKEN,
        calldata(
            "burn(address,uint256)",
            [address_word(recipient), burn_amount.to_be_bytes()],
        ),
    )?;
    assert_eq!(burn["status"], "0x1");
    assert_eq!(balance(recipient)?, amount + mint_amount - burn_amount);
    assert_eq!(supply()?, total_supply + mint_amount - burn_amount);

    let balance_before = balance(recipient)?;
    let supply_before = supply()?;
    let failed_burn = send(
        FIAT_TOKEN,
        calldata(
            "burn(address,uint256)",
            [
                address_word(recipient),
                (balance_before + U256::from(1)).to_be_bytes(),
            ],
        ),
    )?;
    assert_eq!(failed_burn["status"], "0x0");
    assert_eq!(balance(recipient)?, balance_before);
    assert_eq!(supply()?, supply_before);

    let snapshot = request(&provider, "evm_snapshot", json!([]))?;
    let snapshot_transfer = send(
        FIAT_TOKEN,
        transfer_calldata(recipient, sender, U256::from(7)),
    )?;
    assert_eq!(snapshot_transfer["status"], "0x1");
    assert_eq!(balance(recipient)?, balance_before - U256::from(7));
    assert_eq!(
        request(&provider, "evm_revert", json!([snapshot]))?,
        json!(true)
    );
    assert_eq!(balance(recipient)?, balance_before);

    let blocklisted = request(
        &provider,
        "eth_call",
        json!([{
            "to": CONTROL,
            "data": calldata("isBlocklisted(address)", [address_word(sender)]),
        }, "latest"]),
    )
    .and_then(word)?;
    assert_eq!(blocklisted, U256::ZERO);

    Ok(())
}
