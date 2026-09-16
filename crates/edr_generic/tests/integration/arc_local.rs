use super::arc_helpers::{quantity as quantity_u64, quantity_u256, ArcProvider};
use alloy_sol_types::{SolCall, SolValue};
use arc_precompiles::system_accounting::{
    GasValues as ArcGasValuesTest, ISystemAccounting as ArcSystemAccountingTest,
};
use arc_precompiles::{call_from::ICallFrom as ArcCallFromTest, pq::IPQ as ArcPqTest};
use edr_generic::ArcHardfork;
use edr_primitives::{address, keccak256, Address, Bytes, U256};
use serde_json::json;
use slh_dsa::{
    signature::{Keypair, Signer},
    Sha2_128s, SigningKey,
};

const AUTHORITY: Address = address!("1800000000000000000000000000000000000000");
const CONTROL: Address = address!("1800000000000000000000000000000000000001");
const FIAT_TOKEN: Address = address!("3600000000000000000000000000000000000000");
const CALL_FROM: Address = address!("1800000000000000000000000000000000000003");
const MEMO: Address = address!("5294e9927c3306dcbadb03fe70b92e01ccede505");
const SYSTEM_ACCOUNTING: Address = address!("1800000000000000000000000000000000000002");
const PQ: Address = address!("1800000000000000000000000000000000000004");
const SYSTEM: Address = address!("fffffffffffffffffffffffffffffffffffffffe");

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

fn word(value: serde_json::Value) -> anyhow::Result<U256> {
    let bytes: Bytes = serde_json::from_value(value)?;
    Ok(U256::from_be_slice(&bytes))
}

#[tokio::test(flavor = "multi_thread")]
async fn mined_blocks_commit_arc_protocol_state() -> anyhow::Result<()> {
    use alloy_eips::eip2935::{HISTORY_STORAGE_ADDRESS, HISTORY_STORAGE_CODE};
    let provider = ArcProvider::new(1_337, ArcHardfork::LATEST)?;

    provider.request(
        "hardhat_setCode",
        json!([HISTORY_STORAGE_ADDRESS, HISTORY_STORAGE_CODE]),
    )?;
    let snapshot = provider.request("evm_snapshot", json!([]))?;
    provider.request("evm_mine", json!([]))?;
    provider.request("evm_mine", json!([]))?;

    let block_one = provider.request("eth_getBlockByNumber", json!(["0x1", false]))?;
    let block_two = provider.request("eth_getBlockByNumber", json!(["0x2", false]))?;
    let extra_one: Bytes = serde_json::from_value(block_one["extraData"].clone())?;
    let extra_two: Bytes = serde_json::from_value(block_two["extraData"].clone())?;
    assert_eq!(extra_one.len(), 8);
    assert_eq!(extra_two.len(), 8);
    assert_eq!(
        quantity_u64(&block_two["baseFeePerGas"])?,
        u64::from_be_bytes(extra_one.as_ref().try_into()?),
    );

    assert_eq!(
        provider.request(
            "eth_getStorageAt",
            json!([HISTORY_STORAGE_ADDRESS, "0x1", "latest"])
        )?,
        block_one["hash"],
    );
    assert_eq!(
        word(provider.request(
            "eth_getStorageAt",
            json!([HISTORY_STORAGE_ADDRESS, "0x1", "0x1"])
        )?)?,
        U256::ZERO,
    );

    for (number, block) in [(1u64, block_one), (2u64, block_two)] {
        let get = ArcSystemAccountingTest::getGasValuesCall {
            blockNumber: number,
        };
        let values = provider.call(SYSTEM_ACCOUNTING, get)?;
        let extra: Bytes = serde_json::from_value(block["extraData"].clone())?;
        assert_eq!(values.gasUsed, quantity_u64(&block["gasUsed"])?);
        assert_eq!(
            values.nextBaseFee,
            u64::from_be_bytes(extra.as_ref().try_into()?),
        );
    }

    assert_eq!(
        provider.request("evm_revert", json!([snapshot]))?,
        json!(true)
    );
    let reverted = provider.call(
        SYSTEM_ACCOUNTING,
        ArcSystemAccountingTest::getGasValuesCall { blockNumber: 2 },
    )?;
    assert_eq!(reverted.nextBaseFee, 0);
    assert_eq!(
        word(provider.request(
            "eth_getStorageAt",
            json!([HISTORY_STORAGE_ADDRESS, "0x1", "latest"])
        )?)?,
        U256::ZERO,
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn block_parameters_follow_the_contract_api() -> anyhow::Result<()> {
    use arc_execution_config::protocol_config::{
        IProtocolConfig::FeeParams, PROTOCOL_CONFIG_ADDRESS,
    };

    let provider = ArcProvider::new(1_337, ArcHardfork::LATEST)?;
    let params = FeeParams {
        alpha: 20,
        kRate: 200,
        inverseElasticityMultiplier: 5_000,
        minBaseFee: U256::from(42),
        maxBaseFee: U256::from(42),
        blockGasLimit: U256::from(40_000_000),
    };
    // Return the ABI payload from bytecode, with no ProtocolConfig storage layout.
    // CODECOPY copies the 192-byte payload following this 12-byte program.
    let mut code = edr_primitives::hex!("60c0600c60003960c06000f3").to_vec();
    code.extend(params.abi_encode());
    provider.request(
        "hardhat_setCode",
        json!([PROTOCOL_CONFIG_ADDRESS, Bytes::from(code)]),
    )?;
    provider.request("evm_mine", json!([]))?;
    provider.request("evm_mine", json!([]))?;
    let block = provider.request("eth_getBlockByNumber", json!(["latest", false]))?;
    assert_eq!(quantity_u64(&block["gasLimit"])?, 40_000_000);
    assert_eq!(quantity_u64(&block["baseFeePerGas"])?, 42);

    // A failing contract query uses Arc's own network defaults.
    provider.request(
        "hardhat_setCode",
        json!([PROTOCOL_CONFIG_ADDRESS, "0x60006000fd"]),
    )?;
    provider.request("evm_mine", json!([]))?;
    let block = provider.request("eth_getBlockByNumber", json!(["latest", false]))?;
    assert_eq!(quantity_u64(&block["gasLimit"])?, 30_000_000);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn blocklisted_beneficiary_rejects_an_empty_block() -> anyhow::Result<()> {
    let provider = ArcProvider::new(1_337, ArcHardfork::LATEST)?;
    let beneficiary: Address =
        serde_json::from_value(provider.request("eth_coinbase", json!([]))?)?;
    let mut slot_input = [0u8; 64];
    slot_input[12..32].copy_from_slice(beneficiary.as_slice());
    slot_input[63] = 2;
    let slot = U256::from_be_slice(keccak256(slot_input).as_slice());
    provider.request(
        "hardhat_setStorageAt",
        json!([
            CONTROL,
            format!("{slot:#066x}"),
            format!("{:#066x}", U256::from(1))
        ]),
    )?;

    assert!(provider.request("evm_mine", json!([])).is_err());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn pre_zero7_nested_transfers_enforce_the_blocklist() -> anyhow::Result<()> {
    let sender = address!("0000000000000000000000000000000000000011");
    let forwarder = address!("0000000000000000000000000000000000000012");
    let recipient = address!("0000000000000000000000000000000000000013");
    let mut slot_input = [0u8; 64];
    slot_input[12..32].copy_from_slice(recipient.as_slice());
    slot_input[63] = 2;
    let blocklist_slot = U256::from_be_slice(keccak256(slot_input).as_slice());
    let mut forwarder_code = alloy_primitives::hex!("60006000600060006001").to_vec();
    forwarder_code.push(0x73);
    forwarder_code.extend_from_slice(recipient.as_slice());
    forwarder_code.extend_from_slice(&alloy_primitives::hex!("5af100"));

    for hardfork in [ArcHardfork::ZERO4, ArcHardfork::ZERO5, ArcHardfork::ZERO6] {
        let provider = ArcProvider::new(1_337, hardfork)?;
        provider.request("hardhat_impersonateAccount", json!([sender]))?;
        provider.request("hardhat_setBalance", json!([sender, "0x8ac7230489e80000"]))?;
        provider.request("hardhat_setBalance", json!([forwarder, "0x1"]))?;
        provider.request(
            "hardhat_setCode",
            json!([forwarder, Bytes::from(forwarder_code.clone())]),
        )?;
        provider.request(
            "hardhat_setStorageAt",
            json!([
                CONTROL,
                format!("{blocklist_slot:#066x}"),
                format!("{:#066x}", U256::from(1)),
            ]),
        )?;

        let receipt = provider.send(json!({
            "from": sender,
            "to": forwarder,
            "gas": "0x493e0",
        }))?;
        assert_eq!(receipt["status"], "0x1", "{hardfork:?}");
        assert_eq!(
            provider.request("eth_getBalance", json!([forwarder, "latest"]))?,
            "0x1",
            "{hardfork:?}",
        );
        assert_eq!(
            provider.request("eth_getBalance", json!([recipient, "latest"]))?,
            "0x0",
            "{hardfork:?}",
        );
    }

    Ok(())
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

    let provider = ArcProvider::new(5042, ArcHardfork::LATEST)?;

    for account in [FIAT_TOKEN, stranger] {
        provider.request("hardhat_impersonateAccount", json!([account]))?;
        provider.request(
            "hardhat_setBalance",
            json!([account, format!("{gas_reserve:#x}")]),
        )?;
    }
    provider.request(
        "hardhat_setBalance",
        json!([sender, format!("{initial_balance:#x}")]),
    )?;
    provider.request(
        "hardhat_setStorageAt",
        json!([AUTHORITY, "0x2", format!("{total_supply:#066x}")]),
    )?;

    let supply = || {
        provider
            .request(
                "eth_call",
                json!([{ "to": AUTHORITY, "data": calldata("totalSupply()", []) }, "latest"]),
            )
            .and_then(word)
    };
    let send_to = |from, to, data| {
        provider.send(json!({
            "from": from, "to": to, "data": data, "gas": "0x493e0"
        }))
    };
    let send = |from: Address, data: Bytes| send_to(from, AUTHORITY, data);
    let transfer_calldata = |from, to, value: U256| {
        calldata(
            "transfer(address,address,uint256)",
            [address_word(from), address_word(to), value.to_be_bytes()],
        )
    };

    assert_eq!(supply()?, total_supply);
    assert_eq!(provider.balance(sender)?, initial_balance);
    assert_eq!(provider.balance(recipient)?, U256::ZERO);

    let unauthorized = send(stranger, transfer_calldata(sender, recipient, amount))?;
    assert_eq!(unauthorized["status"], "0x0");
    assert_eq!(provider.balance(sender)?, initial_balance);
    assert_eq!(provider.balance(recipient)?, U256::ZERO);

    let transfer = send(FIAT_TOKEN, transfer_calldata(sender, recipient, amount))?;
    assert_eq!(transfer["status"], "0x1");
    assert_eq!(provider.balance(sender)?, initial_balance - amount);
    assert_eq!(provider.balance(recipient)?, amount);
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
    assert_eq!(provider.balance(recipient)?, amount + mint_amount);
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
    assert_eq!(
        provider.balance(recipient)?,
        amount + mint_amount - burn_amount
    );
    assert_eq!(supply()?, total_supply + mint_amount - burn_amount);

    let balance_before = provider.balance(recipient)?;
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
    assert_eq!(provider.balance(recipient)?, balance_before);
    assert_eq!(supply()?, supply_before);

    let snapshot = provider.request("evm_snapshot", json!([]))?;
    let snapshot_transfer = send(
        FIAT_TOKEN,
        transfer_calldata(recipient, sender, U256::from(7)),
    )?;
    assert_eq!(snapshot_transfer["status"], "0x1");
    assert_eq!(provider.balance(recipient)?, balance_before - U256::from(7));
    assert_eq!(
        provider.request("evm_revert", json!([snapshot]))?,
        json!(true)
    );
    assert_eq!(provider.balance(recipient)?, balance_before);

    let blocklist = send_to(
        FIAT_TOKEN,
        CONTROL,
        calldata("blocklist(address)", [address_word(sender)]),
    )?;
    assert_eq!(blocklist["status"], "0x1");
    assert_eq!(blocklist["logs"].as_array().map(Vec::len), Some(1));

    let blocklisted = provider
        .request(
            "eth_call",
            json!([{
            "to": CONTROL,
            "data": calldata("isBlocklisted(address)", [address_word(sender)]),
        }, "latest"]),
        )
        .and_then(word)?;
    assert_eq!(blocklisted, U256::from(1));

    let sender_balance = provider.balance(sender)?;
    let recipient_balance = provider.balance(recipient)?;
    let blocked_transfer = send(
        FIAT_TOKEN,
        transfer_calldata(sender, recipient, U256::from(1)),
    )?;
    assert_eq!(blocked_transfer["status"], "0x0");
    assert_eq!(provider.balance(sender)?, sender_balance);
    assert_eq!(provider.balance(recipient)?, recipient_balance);

    let unblock = send_to(
        FIAT_TOKEN,
        CONTROL,
        calldata("unBlocklist(address)", [address_word(sender)]),
    )?;
    assert_eq!(unblock["status"], "0x1");
    assert_eq!(
        provider
            .request(
                "eth_call",
                json!([{
                "to": CONTROL,
                "data": calldata("isBlocklisted(address)", [address_word(sender)]),
            }, "latest"]),
            )
            .and_then(word)?,
        U256::ZERO
    );

    let delegate = address!("00000000000000000000000000000000000000dd");
    let mut delegate_code = alloy_primitives::hex!("363d3d373d3d3d363d73").to_vec();
    delegate_code.extend_from_slice(AUTHORITY.as_slice());
    delegate_code.extend_from_slice(&alloy_primitives::hex!("5af43d82803e903d91602b57fd5bf3"));
    provider.request(
        "hardhat_setCode",
        json!([delegate, Bytes::from(delegate_code)]),
    )?;

    let delegated_transfer = send_to(
        FIAT_TOKEN,
        delegate,
        transfer_calldata(sender, recipient, U256::from(1)),
    )?;
    assert_eq!(delegated_transfer["status"], "0x0");
    assert_eq!(provider.balance(sender)?, sender_balance);
    assert_eq!(provider.balance(recipient)?, recipient_balance);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_evm_enforces_native_transfer_rules_and_emits_transfer_logs() -> anyhow::Result<()> {
    let sender = address!("0000000000000000000000000000000000000101");
    let recipient = address!("0000000000000000000000000000000000000102");
    let forwarder = address!("0000000000000000000000000000000000000103");
    let destructing = address!("0000000000000000000000000000000000000104");
    let beneficiary = address!("0000000000000000000000000000000000000105");
    let gas_reserve = U256::from(10_000_000_000_000_000_000u128);

    let provider = ArcProvider::new(5042, ArcHardfork::LATEST)?;

    for account in [sender, FIAT_TOKEN] {
        provider.request("hardhat_impersonateAccount", json!([account]))?;
        provider.request(
            "hardhat_setBalance",
            json!([account, format!("{gas_reserve:#x}")]),
        )?;
    }

    let send = |from: Address, to: Address, value: U256| {
        provider.send(json!({
            "from": from, "to": to, "value": format!("{value:#x}"), "gas": "0x493e0"
        }))
    };
    let assert_transfer_log = |receipt: &serde_json::Value| -> anyhow::Result<()> {
        let logs = receipt["logs"].as_array().expect("receipt logs");
        assert_eq!(logs.len(), 1);
        assert_eq!(
            serde_json::from_value::<Address>(logs[0]["address"].clone())?,
            address!("fffffffffffffffffffffffffffffffffffffffe")
        );
        Ok(())
    };

    let coinbase: Address = serde_json::from_value(provider.request("eth_coinbase", json!([]))?)?;
    assert_ne!(coinbase, sender);
    let coinbase_before = provider.balance(coinbase)?;
    let amount = U256::from(123_456);
    let transfer = send(sender, recipient, amount)?;
    assert_eq!(transfer["status"], "0x1");
    assert_transfer_log(&transfer)?;
    assert_eq!(provider.balance(recipient)?, amount);
    let gas_used = quantity_u256(&transfer["gasUsed"])?;
    let effective_gas_price = quantity_u256(&transfer["effectiveGasPrice"])?;
    assert_eq!(
        provider.balance(coinbase)? - coinbase_before,
        gas_used * effective_gas_price,
        "Arc pays the beneficiary both the base fee and priority fee"
    );

    let blocklist_receipt = provider.send(json!({
        "from": FIAT_TOKEN,
        "to": CONTROL,
        "data": calldata("blocklist(address)", [address_word(recipient)]),
        "gas": "0x493e0",
    }))?;
    assert_eq!(blocklist_receipt["status"], "0x1");
    assert!(send(sender, recipient, U256::from(1)).is_err());

    let mut forwarder_code = alloy_primitives::hex!("60006000600060006001").to_vec();
    forwarder_code.push(0x73);
    forwarder_code.extend_from_slice(recipient.as_slice());
    forwarder_code.extend_from_slice(&alloy_primitives::hex!("5af100"));
    provider.request(
        "hardhat_setCode",
        json!([forwarder, Bytes::from(forwarder_code)]),
    )?;
    provider.request("hardhat_setBalance", json!([forwarder, "0x1"]))?;

    let blocked = send(sender, forwarder, U256::ZERO)?;
    assert_eq!(blocked["status"], "0x1");
    assert_eq!(blocked["logs"].as_array().map(Vec::len), Some(0));
    assert_eq!(provider.balance(forwarder)?, U256::from(1));
    assert_eq!(provider.balance(recipient)?, amount);

    let mut selfdestruct_code = vec![0x73];
    selfdestruct_code.extend_from_slice(beneficiary.as_slice());
    selfdestruct_code.push(0xff);
    provider.request(
        "hardhat_setCode",
        json!([destructing, Bytes::from(selfdestruct_code)]),
    )?;
    provider.request("hardhat_setBalance", json!([destructing, "0x2a"]))?;

    let selfdestruct = send(sender, destructing, U256::ZERO)?;
    assert_eq!(selfdestruct["status"], "0x1");
    assert_transfer_log(&selfdestruct)?;
    assert_eq!(provider.balance(beneficiary)?, U256::from(42));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_rust_zero8_selection_executes_all_prior_features() -> anyhow::Result<()> {
    let sender = address!("0000000000000000000000000000000000000201");
    let target = address!("0000000000000000000000000000000000000202");
    let gas_reserve = U256::from(10_000_000_000_000_000_000u128);

    let provider = ArcProvider::new(5042, ArcHardfork::ZERO8)?;
    provider.request("hardhat_impersonateAccount", json!([sender]))?;
    provider.request(
        "hardhat_setBalance",
        json!([sender, format!("{gas_reserve:#x}")]),
    )?;

    let mut memo_code = alloy_primitives::hex!("363d3d37600060003660006000").to_vec();
    memo_code.push(0x73);
    memo_code.extend_from_slice(CALL_FROM.as_slice());
    memo_code.extend_from_slice(&alloy_primitives::hex!("5af13d600060003e3d6000f3"));
    provider.request("hardhat_setCode", json!([MEMO, Bytes::from(memo_code)]))?;
    provider.request("hardhat_setCode", json!([target, "0x3360005260206000f3"]))?;

    let call = ArcCallFromTest::callFromCall {
        sender,
        target,
        data: Bytes::new(),
    };
    let output: Bytes = serde_json::from_value(provider.request(
        "eth_call",
        json!([{
            "from": sender,
            "to": MEMO,
            "data": Bytes::from(call.abi_encode()),
            "gas": "0x493e0",
        }, "latest"]),
    )?)?;
    let decoded = ArcCallFromTest::callFromCall::abi_decode_returns(&output)?;
    assert!(decoded.success);
    assert_eq!(
        U256::from_be_slice(&decoded.returnData),
        U256::from_be_slice(sender.as_slice())
    );
    provider.request(
        "debug_traceCall",
        json!([{
            "from": sender,
            "to": MEMO,
            "data": Bytes::from(call.abi_encode()),
            "gas": "0x493e0",
        }, "latest", {}]),
    )?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_auxiliary_precompiles_match_the_network_interfaces() -> anyhow::Result<()> {
    let provider = ArcProvider::new(5042, ArcHardfork::LATEST)?;
    provider.request("hardhat_impersonateAccount", json!([SYSTEM]))?;
    provider.request("hardhat_setBalance", json!([SYSTEM, "0x8ac7230489e80000"]))?;

    let gas_values = ArcGasValuesTest {
        gasUsed: 10,
        gasUsedSmoothed: 20,
        nextBaseFee: 30,
    };
    let store = ArcSystemAccountingTest::storeGasValuesCall {
        // Avoid the slot automatically written for block 1 (ring size is 64).
        blockNumber: 66,
        gasValues: gas_values.clone(),
    };
    let receipt = provider.send(json!({
        "from": SYSTEM,
        "to": SYSTEM_ACCOUNTING,
        "data": Bytes::from(store.abi_encode()),
        "gas": "0x493e0",
    }))?;
    assert_eq!(receipt["status"], "0x1");

    let get = ArcSystemAccountingTest::getGasValuesCall { blockNumber: 66 };
    let stored = provider.call(SYSTEM_ACCOUNTING, get)?;
    assert_eq!(stored.gasUsed, gas_values.gasUsed);
    assert_eq!(stored.gasUsedSmoothed, gas_values.gasUsedSmoothed);
    assert_eq!(stored.nextBaseFee, gas_values.nextBaseFee);

    let signing_key = SigningKey::<Sha2_128s>::slh_keygen_internal(&[1; 16], &[2; 16], &[3; 16]);
    let message = b"Arc fork support";
    let signature = signing_key.sign(message);
    let verify = ArcPqTest::verifySlhDsaSha2128sCall {
        vk: signing_key.verifying_key().to_bytes().to_vec().into(),
        message: message.to_vec().into(),
        sig: signature.to_bytes().to_vec().into(),
    };
    let output: Bytes = serde_json::from_value(provider.request(
        "eth_call",
        json!([{
            "to": PQ,
            "data": Bytes::from(verify.abi_encode()),
            "gas": "0x7a120",
        }, "latest"]),
    )?)?;
    assert!(ArcPqTest::verifySlhDsaSha2128sCall::abi_decode_returns(
        &output
    )?);

    Ok(())
}
