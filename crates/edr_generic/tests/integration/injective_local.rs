use std::sync::Arc;

use edr_generic::InjectiveChainSpec;
use edr_primitives::{address, keccak256, Address, Bytes, U256};
use edr_provider::{
    test_utils, time::CurrentTime, MethodInvocation, NoopLogger, Provider, ProviderRequest,
};
use edr_solidity::contract_decoder::ContractDecoder;
use parking_lot::RwLock;
use serde_json::json;
use tokio::runtime;

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

fn balance_slot(account: Address) -> U256 {
    let mut slot = [0u8; 32];
    slot[..12].copy_from_slice(b"EDR_INJ_BAL_");
    slot[12..].copy_from_slice(account.as_slice());
    U256::from_be_bytes(slot)
}

fn request(
    provider: &Provider<InjectiveChainSpec>,
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let invocation: MethodInvocation<InjectiveChainSpec> =
        serde_json::from_value(json!({ "method": method, "params": params }))?;
    Ok(provider
        .handle_request(ProviderRequest::with_single(invocation))?
        .result)
}

#[tokio::test(flavor = "multi_thread")]
async fn bank_writes_are_journaled_and_update_supply() -> anyhow::Result<()> {
    let bank = address!("0000000000000000000000000000000000000064");
    let token = address!("0000000000000000000000000000000000001776");
    let sender = address!("00000000000000000000000000000000000000aa");
    let recipient = address!("00000000000000000000000000000000000000bb");
    let initial_balance = U256::from(100);
    let amount = U256::from(40);
    let total_supply = U256::from(1_000);
    let total_supply_slot =
        U256::from_be_slice(keccak256("edr.injective.bank.totalSupply").as_slice());

    let mut config = test_utils::create_test_config::<edr_chain_l1::Hardfork>();
    config.chain_id = 1776;
    let provider = Provider::new(
        runtime::Handle::current(),
        Box::new(NoopLogger::<InjectiveChainSpec>::default()),
        Box::new(|_event| {}),
        config,
        Arc::new(RwLock::<ContractDecoder>::default()),
        CurrentTime,
    )?;

    request(
        &provider,
        "hardhat_setStorageAt",
        json!([
            token,
            format!("{:#x}", balance_slot(sender)),
            format!("{initial_balance:#066x}")
        ]),
    )?;
    request(
        &provider,
        "hardhat_setStorageAt",
        json!([
            token,
            format!("{total_supply_slot:#x}"),
            format!("{total_supply:#066x}")
        ]),
    )?;

    let balance_of = |account| {
        request(
            &provider,
            "eth_call",
            json!([{
                "to": bank,
                "data": calldata(
                    "balanceOf(address,address)",
                    [address_word(token), address_word(account)]
                ),
            }, "latest"]),
        )
        .and_then(|value| serde_json::from_value::<Bytes>(value).map_err(Into::into))
        .map(|value| U256::from_be_slice(&value))
    };

    assert_eq!(balance_of(sender)?, initial_balance);
    assert_eq!(balance_of(recipient)?, U256::ZERO);

    request(&provider, "hardhat_impersonateAccount", json!([token]))?;
    request(
        &provider,
        "hardhat_setBalance",
        json!([token, "0xde0b6b3a7640000"]),
    )?;
    let send_bank_transaction = |data| {
        request(
            &provider,
            "eth_sendTransaction",
            json!([{
                "from": token,
                "to": bank,
                "data": data,
                "gas": "0x493e0",
            }]),
        )
    };
    send_bank_transaction(calldata(
        "transfer(address,address,uint256)",
        [
            address_word(sender),
            address_word(recipient),
            amount.to_be_bytes(),
        ],
    ))?;

    assert_eq!(balance_of(sender)?, initial_balance - amount);
    assert_eq!(balance_of(recipient)?, amount);

    let supply = || {
        request(
            &provider,
            "eth_call",
            json!([{
                "to": bank,
                "data": calldata("totalSupply(address)", [address_word(token)]),
            }, "latest"]),
        )
        .and_then(|value| serde_json::from_value::<Bytes>(value).map_err(Into::into))
        .map(|value| U256::from_be_slice(&value))
    };
    assert_eq!(supply()?, total_supply);

    let mint_amount = U256::from(25);
    send_bank_transaction(calldata(
        "mint(address,uint256)",
        [address_word(recipient), mint_amount.to_be_bytes()],
    ))?;
    assert_eq!(balance_of(recipient)?, amount + mint_amount);
    assert_eq!(supply()?, total_supply + mint_amount);

    let burn_amount = U256::from(15);
    send_bank_transaction(calldata(
        "burn(address,uint256)",
        [address_word(recipient), burn_amount.to_be_bytes()],
    ))?;
    assert_eq!(balance_of(recipient)?, amount + mint_amount - burn_amount);
    assert_eq!(supply()?, total_supply + mint_amount - burn_amount);

    let balance_before_failed_burn = balance_of(recipient)?;
    let supply_before_failed_burn = supply()?;
    let failed_burn = send_bank_transaction(calldata(
        "burn(address,uint256)",
        [
            address_word(recipient),
            (balance_before_failed_burn + U256::from(1)).to_be_bytes(),
        ],
    ))?;
    let failed_burn_receipt =
        request(&provider, "eth_getTransactionReceipt", json!([failed_burn]))?;
    assert_eq!(failed_burn_receipt["status"], "0x0");
    assert_eq!(balance_of(recipient)?, balance_before_failed_burn);
    assert_eq!(supply()?, supply_before_failed_burn);

    let snapshot = request(&provider, "evm_snapshot", json!([]))?;
    request(
        &provider,
        "hardhat_setStorageAt",
        json!([
            token,
            format!("{:#x}", balance_slot(recipient)),
            format!("{:#066x}", U256::MAX)
        ]),
    )?;
    let overflowing_mint = send_bank_transaction(calldata(
        "mint(address,uint256)",
        [address_word(recipient), U256::from(1).to_be_bytes()],
    ))?;
    let overflowing_mint_receipt = request(
        &provider,
        "eth_getTransactionReceipt",
        json!([overflowing_mint]),
    )?;
    assert_eq!(overflowing_mint_receipt["status"], "0x0");
    assert_eq!(balance_of(recipient)?, U256::MAX);
    assert_eq!(supply()?, supply_before_failed_burn);

    assert_eq!(
        request(&provider, "evm_revert", json!([snapshot]))?,
        serde_json::Value::Bool(true)
    );
    assert_eq!(balance_of(recipient)?, balance_before_failed_burn);
    assert_eq!(supply()?, supply_before_failed_burn);

    Ok(())
}
