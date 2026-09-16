use std::sync::Arc;

use alloy_sol_types::{sol, SolCall};
use edr_generic::{ArcChainSpec, ArcHardfork};
use edr_primitives::{address, keccak256, Address, Bytes, U256};
use edr_provider::{
    test_utils, time::CurrentTime, MethodInvocation, NoopLogger, Provider, ProviderRequest,
};
use edr_solidity::contract_decoder::ContractDecoder;
use parking_lot::RwLock;
use serde_json::json;
use slh_dsa::{
    signature::{Keypair, Signer},
    Sha2_128s, SigningKey,
};
use tokio::runtime;

const AUTHORITY: Address = address!("1800000000000000000000000000000000000000");
const CONTROL: Address = address!("1800000000000000000000000000000000000001");
const FIAT_TOKEN: Address = address!("3600000000000000000000000000000000000000");
const CALL_FROM: Address = address!("1800000000000000000000000000000000000003");
const MEMO: Address = address!("5294e9927c3306dcbadb03fe70b92e01ccede505");
const SYSTEM_ACCOUNTING: Address = address!("1800000000000000000000000000000000000002");
const PQ: Address = address!("1800000000000000000000000000000000000004");
const SYSTEM: Address = address!("fffffffffffffffffffffffffffffffffffffffe");

sol! {
    interface ArcCallFromTest {
        function callFrom(address sender, address target, bytes calldata data)
            external returns (bool success, bytes memory returnData);
    }

    struct ArcGasValuesTest {
        uint64 gasUsed;
        uint64 gasUsedSmoothed;
        uint64 nextBaseFee;
    }

    interface ArcSystemAccountingTest {
        function storeGasValues(uint64 blockNumber, ArcGasValuesTest calldata gasValues) external returns (bool);
        function getGasValues(uint64 blockNumber) external view returns (ArcGasValuesTest memory gasValues);
    }

    interface ArcPqTest {
        function verifySlhDsaSha2128s(bytes calldata vk, bytes calldata message, bytes calldata sig)
            external returns (bool isValid);
    }
}

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

fn quantity_u64(value: &serde_json::Value) -> anyhow::Result<u64> {
    let value = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("expected JSON-RPC quantity"))?;
    Ok(u64::from_str_radix(value.trim_start_matches("0x"), 16)?)
}

#[tokio::test(flavor = "multi_thread")]
async fn mined_blocks_commit_arc_fee_accounting() -> anyhow::Result<()> {
    let mut config = test_utils::create_test_config::<ArcHardfork>();
    config.chain_id = 1_337;
    let provider = Provider::new(
        runtime::Handle::current(),
        Box::new(NoopLogger::<ArcChainSpec>::default()),
        Box::new(|_event| {}),
        config,
        Arc::new(RwLock::<ContractDecoder>::default()),
        CurrentTime,
    )?;

    request(&provider, "evm_mine", json!([]))?;
    request(&provider, "evm_mine", json!([]))?;

    let block_one = request(&provider, "eth_getBlockByNumber", json!(["0x1", false]))?;
    let block_two = request(&provider, "eth_getBlockByNumber", json!(["0x2", false]))?;
    let extra_one: Bytes = serde_json::from_value(block_one["extraData"].clone())?;
    let extra_two: Bytes = serde_json::from_value(block_two["extraData"].clone())?;
    assert_eq!(extra_one.len(), 8);
    assert_eq!(extra_two.len(), 8);
    assert_eq!(
        quantity_u64(&block_two["baseFeePerGas"])?,
        u64::from_be_bytes(extra_one.as_ref().try_into()?),
    );

    for (number, block) in [(1u64, block_one), (2u64, block_two)] {
        let get = ArcSystemAccountingTest::getGasValuesCall {
            blockNumber: number,
        };
        let output: Bytes = serde_json::from_value(request(
            &provider,
            "eth_call",
            json!([{
                "to": SYSTEM_ACCOUNTING,
                "data": Bytes::from(get.abi_encode()),
            }, "latest"]),
        )?)?;
        let values = ArcSystemAccountingTest::getGasValuesCall::abi_decode_returns(&output)?;
        let extra: Bytes = serde_json::from_value(block["extraData"].clone())?;
        assert_eq!(values.gasUsed, quantity_u64(&block["gasUsed"])?);
        assert_eq!(
            values.nextBaseFee,
            u64::from_be_bytes(extra.as_ref().try_into()?),
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn blocklisted_beneficiary_rejects_an_empty_block() -> anyhow::Result<()> {
    let mut config = test_utils::create_test_config::<ArcHardfork>();
    config.chain_id = 1_337;
    let provider = Provider::new(
        runtime::Handle::current(),
        Box::new(NoopLogger::<ArcChainSpec>::default()),
        Box::new(|_event| {}),
        config,
        Arc::new(RwLock::<ContractDecoder>::default()),
        CurrentTime,
    )?;
    let beneficiary: Address =
        serde_json::from_value(request(&provider, "eth_coinbase", json!([]))?)?;
    let mut slot_input = [0u8; 64];
    slot_input[12..32].copy_from_slice(beneficiary.as_slice());
    slot_input[63] = 2;
    let slot = U256::from_be_slice(keccak256(slot_input).as_slice());
    request(
        &provider,
        "hardhat_setStorageAt",
        json!([
            CONTROL,
            format!("{slot:#066x}"),
            format!("{:#066x}", U256::from(1))
        ]),
    )?;

    assert!(request(&provider, "evm_mine", json!([])).is_err());
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
        let mut config = test_utils::create_test_config::<ArcHardfork>();
        config.chain_id = 1_337;
        config.hardfork = hardfork;
        let provider = Provider::new(
            runtime::Handle::current(),
            Box::new(NoopLogger::<ArcChainSpec>::default()),
            Box::new(|_event| {}),
            config,
            Arc::new(RwLock::<ContractDecoder>::default()),
            CurrentTime,
        )?;
        request(&provider, "hardhat_impersonateAccount", json!([sender]))?;
        request(
            &provider,
            "hardhat_setBalance",
            json!([sender, "0x8ac7230489e80000"]),
        )?;
        request(&provider, "hardhat_setBalance", json!([forwarder, "0x1"]))?;
        request(
            &provider,
            "hardhat_setCode",
            json!([forwarder, Bytes::from(forwarder_code.clone())]),
        )?;
        request(
            &provider,
            "hardhat_setStorageAt",
            json!([
                CONTROL,
                format!("{blocklist_slot:#066x}"),
                format!("{:#066x}", U256::from(1)),
            ]),
        )?;

        let hash = request(
            &provider,
            "eth_sendTransaction",
            json!([{
                "from": sender,
                "to": forwarder,
                "gas": "0x493e0",
            }]),
        )?;
        let receipt = request(&provider, "eth_getTransactionReceipt", json!([hash]))?;
        assert_eq!(receipt["status"], "0x1", "{hardfork:?}");
        assert_eq!(
            request(&provider, "eth_getBalance", json!([forwarder, "latest"]))?,
            "0x1",
            "{hardfork:?}",
        );
        assert_eq!(
            request(&provider, "eth_getBalance", json!([recipient, "latest"]))?,
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

    let mut config = test_utils::create_test_config::<ArcHardfork>();
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
    let send_to = |from: Address, to: Address, data: Bytes| -> anyhow::Result<serde_json::Value> {
        let hash = request(
            &provider,
            "eth_sendTransaction",
            json!([{ "from": from, "to": to, "data": data, "gas": "0x493e0" }]),
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

    let blocklist = send_to(
        FIAT_TOKEN,
        CONTROL,
        calldata("blocklist(address)", [address_word(sender)]),
    )?;
    assert_eq!(blocklist["status"], "0x1");
    assert_eq!(blocklist["logs"].as_array().map(Vec::len), Some(1));

    let blocklisted = request(
        &provider,
        "eth_call",
        json!([{
            "to": CONTROL,
            "data": calldata("isBlocklisted(address)", [address_word(sender)]),
        }, "latest"]),
    )
    .and_then(word)?;
    assert_eq!(blocklisted, U256::from(1));

    let sender_balance = balance(sender)?;
    let recipient_balance = balance(recipient)?;
    let blocked_transfer = send(
        FIAT_TOKEN,
        transfer_calldata(sender, recipient, U256::from(1)),
    )?;
    assert_eq!(blocked_transfer["status"], "0x0");
    assert_eq!(balance(sender)?, sender_balance);
    assert_eq!(balance(recipient)?, recipient_balance);

    let unblock = send_to(
        FIAT_TOKEN,
        CONTROL,
        calldata("unBlocklist(address)", [address_word(sender)]),
    )?;
    assert_eq!(unblock["status"], "0x1");
    assert_eq!(
        request(
            &provider,
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
    request(
        &provider,
        "hardhat_setCode",
        json!([delegate, Bytes::from(delegate_code)]),
    )?;

    let delegated_transfer = send_to(
        FIAT_TOKEN,
        delegate,
        transfer_calldata(sender, recipient, U256::from(1)),
    )?;
    assert_eq!(delegated_transfer["status"], "0x0");
    assert_eq!(balance(sender)?, sender_balance);
    assert_eq!(balance(recipient)?, recipient_balance);

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

    let mut config = test_utils::create_test_config::<ArcHardfork>();
    config.chain_id = 5042;
    let provider = Provider::new(
        runtime::Handle::current(),
        Box::new(NoopLogger::<ArcChainSpec>::default()),
        Box::new(|_event| {}),
        config,
        Arc::new(RwLock::<ContractDecoder>::default()),
        CurrentTime,
    )?;

    for account in [sender, FIAT_TOKEN] {
        request(&provider, "hardhat_impersonateAccount", json!([account]))?;
        request(
            &provider,
            "hardhat_setBalance",
            json!([account, format!("{gas_reserve:#x}")]),
        )?;
    }

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
    let send = |from: Address, to: Address, value: U256| -> anyhow::Result<serde_json::Value> {
        let hash = request(
            &provider,
            "eth_sendTransaction",
            json!([{
                "from": from,
                "to": to,
                "value": format!("{value:#x}"),
                "gas": "0x493e0",
            }]),
        )?;
        request(&provider, "eth_getTransactionReceipt", json!([hash]))
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

    let coinbase: Address = serde_json::from_value(request(&provider, "eth_coinbase", json!([]))?)?;
    assert_ne!(coinbase, sender);
    let coinbase_before = balance(coinbase)?;
    let amount = U256::from(123_456);
    let transfer = send(sender, recipient, amount)?;
    assert_eq!(transfer["status"], "0x1");
    assert_transfer_log(&transfer)?;
    assert_eq!(balance(recipient)?, amount);
    let gas_used = U256::from_str_radix(
        transfer["gasUsed"]
            .as_str()
            .expect("gas used quantity")
            .trim_start_matches("0x"),
        16,
    )?;
    let effective_gas_price = U256::from_str_radix(
        transfer["effectiveGasPrice"]
            .as_str()
            .expect("effective gas price quantity")
            .trim_start_matches("0x"),
        16,
    )?;
    assert_eq!(
        balance(coinbase)? - coinbase_before,
        gas_used * effective_gas_price,
        "Arc pays the beneficiary both the base fee and priority fee"
    );

    let blocklist_hash = request(
        &provider,
        "eth_sendTransaction",
        json!([{
            "from": FIAT_TOKEN,
            "to": CONTROL,
            "data": calldata("blocklist(address)", [address_word(recipient)]),
            "gas": "0x493e0",
        }]),
    )?;
    let blocklist_receipt = request(
        &provider,
        "eth_getTransactionReceipt",
        json!([blocklist_hash]),
    )?;
    assert_eq!(blocklist_receipt["status"], "0x1");
    assert!(send(sender, recipient, U256::from(1)).is_err());

    let mut forwarder_code = alloy_primitives::hex!("60006000600060006001").to_vec();
    forwarder_code.push(0x73);
    forwarder_code.extend_from_slice(recipient.as_slice());
    forwarder_code.extend_from_slice(&alloy_primitives::hex!("5af100"));
    request(
        &provider,
        "hardhat_setCode",
        json!([forwarder, Bytes::from(forwarder_code)]),
    )?;
    request(&provider, "hardhat_setBalance", json!([forwarder, "0x1"]))?;

    let blocked = send(sender, forwarder, U256::ZERO)?;
    assert_eq!(blocked["status"], "0x1");
    assert_eq!(blocked["logs"].as_array().map(Vec::len), Some(0));
    assert_eq!(balance(forwarder)?, U256::from(1));
    assert_eq!(balance(recipient)?, amount);

    let mut selfdestruct_code = vec![0x73];
    selfdestruct_code.extend_from_slice(beneficiary.as_slice());
    selfdestruct_code.push(0xff);
    request(
        &provider,
        "hardhat_setCode",
        json!([destructing, Bytes::from(selfdestruct_code)]),
    )?;
    request(
        &provider,
        "hardhat_setBalance",
        json!([destructing, "0x2a"]),
    )?;

    let selfdestruct = send(sender, destructing, U256::ZERO)?;
    assert_eq!(selfdestruct["status"], "0x1");
    assert_transfer_log(&selfdestruct)?;
    assert_eq!(balance(beneficiary)?, U256::from(42));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_rust_zero8_selection_executes_all_prior_features() -> anyhow::Result<()> {
    let sender = address!("0000000000000000000000000000000000000201");
    let target = address!("0000000000000000000000000000000000000202");
    let gas_reserve = U256::from(10_000_000_000_000_000_000u128);

    let mut config = test_utils::create_test_config::<ArcHardfork>();
    config.chain_id = 5042;
    config.hardfork = ArcHardfork::ZERO8;
    let provider = Provider::new(
        runtime::Handle::current(),
        Box::new(NoopLogger::<ArcChainSpec>::default()),
        Box::new(|_event| {}),
        config,
        Arc::new(RwLock::<ContractDecoder>::default()),
        CurrentTime,
    )?;
    request(&provider, "hardhat_impersonateAccount", json!([sender]))?;
    request(
        &provider,
        "hardhat_setBalance",
        json!([sender, format!("{gas_reserve:#x}")]),
    )?;

    let mut memo_code = alloy_primitives::hex!("363d3d37600060003660006000").to_vec();
    memo_code.push(0x73);
    memo_code.extend_from_slice(CALL_FROM.as_slice());
    memo_code.extend_from_slice(&alloy_primitives::hex!("5af13d600060003e3d6000f3"));
    request(
        &provider,
        "hardhat_setCode",
        json!([MEMO, Bytes::from(memo_code)]),
    )?;
    request(
        &provider,
        "hardhat_setCode",
        json!([target, "0x3360005260206000f3"]),
    )?;

    let call = ArcCallFromTest::callFromCall {
        sender,
        target,
        data: Bytes::new(),
    };
    let output: Bytes = serde_json::from_value(request(
        &provider,
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
    request(
        &provider,
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
    let mut config = test_utils::create_test_config::<ArcHardfork>();
    config.chain_id = 5042;
    let provider = Provider::new(
        runtime::Handle::current(),
        Box::new(NoopLogger::<ArcChainSpec>::default()),
        Box::new(|_event| {}),
        config,
        Arc::new(RwLock::<ContractDecoder>::default()),
        CurrentTime,
    )?;
    request(&provider, "hardhat_impersonateAccount", json!([SYSTEM]))?;
    request(
        &provider,
        "hardhat_setBalance",
        json!([SYSTEM, "0x8ac7230489e80000"]),
    )?;

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
    let hash = request(
        &provider,
        "eth_sendTransaction",
        json!([{
            "from": SYSTEM,
            "to": SYSTEM_ACCOUNTING,
            "data": Bytes::from(store.abi_encode()),
            "gas": "0x493e0",
        }]),
    )?;
    let receipt = request(&provider, "eth_getTransactionReceipt", json!([hash]))?;
    assert_eq!(receipt["status"], "0x1");

    let get = ArcSystemAccountingTest::getGasValuesCall { blockNumber: 66 };
    let output: Bytes = serde_json::from_value(request(
        &provider,
        "eth_call",
        json!([{
            "to": SYSTEM_ACCOUNTING,
            "data": Bytes::from(get.abi_encode()),
        }, "latest"]),
    )?)?;
    let stored = ArcSystemAccountingTest::getGasValuesCall::abi_decode_returns(&output)?;
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
    let output: Bytes = serde_json::from_value(request(
        &provider,
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
