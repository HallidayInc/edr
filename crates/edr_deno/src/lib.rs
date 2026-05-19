#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use core::str::FromStr;
use std::{
    collections::HashSet,
    num::NonZeroU64,
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
};

use ansi_term::Color;
use deno_bindgen::deno_bindgen;
use edr_block_api::Block as _;
use edr_chain_config::{
    ChainOverride, ForkCondition, HardforkActivation, HardforkActivations, NativeTokenMirror,
};
use edr_chain_l1::{self as l1, L1ChainSpec};
use edr_chain_spec::ExecutableTransaction;
use edr_chain_spec_provider::ProviderChainSpec;
use edr_generic::{ApeChainSpec, ArbChainSpec, GenericChainSpec, APE_PRECOMPILE_STATE_ADDRESS};
use edr_op::{self, OpChainSpec};
use edr_primitives::{Address, Bytecode, Bytes, HashMap, U256, U64};
use edr_provider::{
    test_utils, time::CurrentTime, AccountOverride, InvalidRequestReason, Provider,
};
use edr_rpc_client::{
    cache::{
        key::{ReadCacheKey, WriteCacheKey},
        CacheableMethod,
    },
    header::{self, HeaderValue},
    jsonrpc, HeaderMap, RpcClient, RpcMethod,
};
use edr_signer::{public_key_to_address, SecretKey, SignatureError};
use edr_solidity::{artifacts::BuildInfoConfig, contract_decoder::ContractDecoder};
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::RwLock;
use serde::{ser::SerializeSeq, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value as JsonValue;
use tokio::runtime::Runtime;

const APE_ARBOWNERPUBLIC_ADDRESS: &str = "0x000000000000000000000000000000000000006b";
const APE_GET_SHARE_PRICE_SELECTOR: &str = "0x5b1dac60";
const APE_GET_SHARE_COUNT_SELECTOR: &str = "0x1c0b915b";
const APE_GET_APY_SELECTOR: &str = "0x1fb922e0";

fn parse_u64<E: serde::de::Error>(val: JsonValue) -> Result<u64, E> {
    match val {
        JsonValue::Number(n) => n.as_u64().ok_or_else(|| E::custom("invalid u64 number")),
        JsonValue::String(s) => s.parse::<u64>().map_err(E::custom),
        _ => Err(E::custom("expected number or string")),
    }
}

fn deserialize_u64_from_str_or_int<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let val = JsonValue::deserialize(deserializer)?;
    parse_u64::<D::Error>(val)
}

fn deserialize_opt_u64_from_str_or_int<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<JsonValue>::deserialize(deserializer)? {
        None => Ok(None),
        Some(v) => parse_u64::<D::Error>(v).map(Some),
    }
}

fn parse_u128<E: serde::de::Error>(val: JsonValue) -> Result<u128, E> {
    match val {
        JsonValue::Number(n) => n
            .as_u64()
            .map(|n| n as u128)
            .ok_or_else(|| E::custom("invalid u128 number")),
        JsonValue::String(s) => s.parse::<u128>().map_err(E::custom),
        _ => Err(E::custom("expected number or string")),
    }
}

fn parse_u256<E: serde::de::Error>(val: JsonValue) -> Result<U256, E> {
    match val {
        JsonValue::Number(n) => n
            .as_u64()
            .map(U256::from)
            .ok_or_else(|| E::custom("invalid u256 number")),
        JsonValue::String(s) => U256::from_str(&s).map_err(E::custom),
        _ => Err(E::custom("expected number or string")),
    }
}

fn deserialize_u256_from_str_or_int<'de, D>(deserializer: D) -> Result<U256, D::Error>
where
    D: Deserializer<'de>,
{
    let val = JsonValue::deserialize(deserializer)?;
    parse_u256::<D::Error>(val)
}

fn parse_address<E: serde::de::Error>(val: JsonValue) -> Result<Address, E> {
    match val {
        JsonValue::String(s) => Address::from_str(&s).map_err(E::custom),
        JsonValue::Array(bytes) => {
            let bytes = bytes
                .into_iter()
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or_else(|| E::custom("invalid address byte"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if bytes.len() != 20 {
                return Err(E::custom("address must be 20 bytes"));
            }
            Ok(Address::from_slice(&bytes))
        }
        JsonValue::Object(bytes) => {
            let bytes = (0..bytes.len())
                .map(|index| {
                    bytes
                        .get(&index.to_string())
                        .and_then(JsonValue::as_u64)
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or_else(|| E::custom("invalid address byte"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if bytes.len() != 20 {
                return Err(E::custom("address must be 20 bytes"));
            }
            Ok(Address::from_slice(&bytes))
        }
        _ => Err(E::custom("expected address string or bytes")),
    }
}

fn deserialize_address_from_str_or_bytes<'de, D>(deserializer: D) -> Result<Address, D::Error>
where
    D: Deserializer<'de>,
{
    let val = JsonValue::deserialize(deserializer)?;
    parse_address::<D::Error>(val)
}

fn deserialize_opt_u128_from_str_or_int<'de, D>(deserializer: D) -> Result<Option<u128>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<JsonValue>::deserialize(deserializer)? {
        None => Ok(None),
        Some(v) => parse_u128::<D::Error>(v).map(Some),
    }
}

fn secret_key_from_hex(s: &str) -> Result<SecretKey, SignatureError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).map_err(|_| SignatureError::InvalidSecretKeyHex)?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| SignatureError::InvalidSecretKeyLength)?;
    SecretKey::from_slice(&arr).map_err(SignatureError::EllipticCurveError)
}

#[derive(Default)]
struct ApePrecompileState {
    share_price: Option<U256>,
    share_count: Option<U256>,
    apy: Option<U256>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "method", content = "params")]
enum ApeRequestMethod {
    #[serde(rename = "eth_call")]
    Call(ApeCallRequest, ApeBlockParam),
    #[serde(rename = "eth_blockNumber", with = "empty_params")]
    BlockNumber(()),
    #[serde(rename = "eth_chainId", with = "empty_params")]
    ChainId(()),
}

#[derive(Clone, Debug, Serialize)]
struct ApeCallRequest {
    to: Address,
    data: Bytes,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum ApeBlockParam {
    Tag(&'static str),
    Number(U64),
}

#[derive(Clone, Debug)]
struct ApeUncacheableMethod;

impl CacheableMethod for ApeUncacheableMethod {
    type MethodWithResolvableBlockTag = ApeUncacheableMethod;

    fn resolve_block_tag(method: Self::MethodWithResolvableBlockTag, _block_number: u64) -> Self {
        method
    }

    fn read_cache_key(self) -> Option<ReadCacheKey> {
        None
    }

    fn write_cache_key(self) -> Option<WriteCacheKey<Self>> {
        None
    }
}

impl TryFrom<&ApeRequestMethod> for ApeUncacheableMethod {
    type Error = ();

    fn try_from(_value: &ApeRequestMethod) -> Result<Self, Self::Error> {
        Err(())
    }
}

impl RpcMethod for ApeRequestMethod {
    type Cacheable<'method> = ApeUncacheableMethod;

    fn block_number_request() -> Self {
        Self::BlockNumber(())
    }

    fn chain_id_request() -> Self {
        Self::ChainId(())
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Call(_, _) => "eth_call",
            Self::BlockNumber(_) => "eth_blockNumber",
            Self::ChainId(_) => "eth_chainId",
        }
    }
}

fn rpc_headers(headers: Option<&[HttpHeader]>) -> Option<HeaderMap> {
    let mut rpc_headers = HeaderMap::new();
    for header in headers.unwrap_or_default() {
        let name = header::HeaderName::from_bytes(header.name.as_bytes()).ok()?;
        let value = HeaderValue::from_str(&header.value).ok()?;
        rpc_headers.insert(name, value);
    }
    Some(rpc_headers)
}

fn fetch_ape_precompile_word(
    client: &RpcClient<ApeRequestMethod>,
    selector: &str,
    block_param: ApeBlockParam,
) -> Option<U256> {
    let call = ApeRequestMethod::Call(
        ApeCallRequest {
            to: Address::from_str(APE_ARBOWNERPUBLIC_ADDRESS).ok()?,
            data: Bytes::from_str(selector).ok()?,
        },
        block_param,
    );

    runtime()
        .block_on(client.call::<String>(call))
        .ok()
        .and_then(|result| U256::from_str(&result).ok())
}

fn fetch_ape_precompile_state(fork: Option<&ForkConfig>) -> ApePrecompileState {
    let Some(fork) = fork else {
        return ApePrecompileState::default();
    };

    let block_param = fork
        .block_number
        .map(|block_number| ApeBlockParam::Number(U64::from(block_number)))
        .unwrap_or(ApeBlockParam::Tag("latest"));

    let Ok(client) = RpcClient::<ApeRequestMethod>::new(
        &fork.json_rpc_url,
        PathBuf::new(),
        rpc_headers(fork.http_headers.as_deref()),
    ) else {
        return ApePrecompileState::default();
    };

    ApePrecompileState {
        share_price: fetch_ape_precompile_word(
            &client,
            APE_GET_SHARE_PRICE_SELECTOR,
            block_param.clone(),
        ),
        share_count: fetch_ape_precompile_word(
            &client,
            APE_GET_SHARE_COUNT_SELECTOR,
            block_param.clone(),
        ),
        apy: fetch_ape_precompile_word(&client, APE_GET_APY_SELECTOR, block_param),
    }
}

fn encode_ape_precompile_code(state: ApePrecompileState) -> Option<Bytecode> {
    if state.share_price.is_none() && state.share_count.is_none() && state.apy.is_none() {
        return None;
    }

    let mut metadata = Vec::with_capacity(96);
    for word in [
        state.share_price.unwrap_or(U256::ZERO),
        state.share_count.unwrap_or(U256::ZERO),
        state.apy.unwrap_or(U256::ZERO),
    ] {
        metadata.extend_from_slice(&word.to_be_bytes::<32>());
    }

    Some(Bytecode::new_raw(Bytes::from(metadata)))
}

fn seed_ape_precompile_state(
    genesis_state: &mut HashMap<edr_primitives::Address, AccountOverride>,
    fork: Option<&ForkConfig>,
) {
    let Some(code) = encode_ape_precompile_code(fetch_ape_precompile_state(fork)) else {
        return;
    };

    genesis_state.insert(
        APE_PRECOMPILE_STATE_ADDRESS,
        AccountOverride {
            balance: Some(U256::ZERO),
            nonce: Some(1),
            code: Some(code),
            storage: None,
        },
    );
}

fn parse_l1_spec_id(name: &str) -> Option<l1::Hardfork> {
    match name.to_ascii_lowercase().as_str() {
        "frontier" => Some(l1::Hardfork::FRONTIER),
        "frontierthawing" | "frontier_thawing" => Some(l1::Hardfork::FRONTIER_THAWING),
        "homestead" => Some(l1::Hardfork::HOMESTEAD),
        "daofork" | "dao_fork" => Some(l1::Hardfork::DAO_FORK),
        "tangerine" => Some(l1::Hardfork::TANGERINE),
        "spuriousdragon" | "spurious_dragon" => Some(l1::Hardfork::SPURIOUS_DRAGON),
        "byzantium" => Some(l1::Hardfork::BYZANTIUM),
        "constantinople" => Some(l1::Hardfork::CONSTANTINOPLE),
        "petersburg" => Some(l1::Hardfork::PETERSBURG),
        "istanbul" => Some(l1::Hardfork::ISTANBUL),
        "muirglacier" | "muir_glacier" => Some(l1::Hardfork::MUIR_GLACIER),
        "berlin" => Some(l1::Hardfork::BERLIN),
        "london" => Some(l1::Hardfork::LONDON),
        "arrowglacier" | "arrow_glacier" => Some(l1::Hardfork::ARROW_GLACIER),
        "grayglacier" | "gray_glacier" => Some(l1::Hardfork::GRAY_GLACIER),
        "merge" => Some(l1::Hardfork::MERGE),
        "shanghai" => Some(l1::Hardfork::SHANGHAI),
        "cancun" => Some(l1::Hardfork::CANCUN),
        "prague" => Some(l1::Hardfork::PRAGUE),
        _ => None,
    }
}

mod empty_params {
    use super::{Serialize, SerializeSeq, Serializer};

    pub fn serialize<SerializerT, T>(
        _value: &T,
        serializer: SerializerT,
    ) -> Result<SerializerT::Ok, SerializerT::Error>
    where
        SerializerT: Serializer,
        T: Serialize,
    {
        let seq = serializer.serialize_seq(Some(0))?;
        seq.end()
    }
}

fn parse_op_spec_id(name: &str) -> Option<edr_op::Hardfork> {
    match name.to_ascii_lowercase().as_str() {
        "bedrock" => Some(edr_op::Hardfork::BEDROCK),
        "regolith" => Some(edr_op::Hardfork::REGOLITH),
        "canyon" => Some(edr_op::Hardfork::CANYON),
        "ecotone" => Some(edr_op::Hardfork::ECOTONE),
        "fjord" => Some(edr_op::Hardfork::FJORD),
        "granite" => Some(edr_op::Hardfork::GRANITE),
        "holocene" => Some(edr_op::Hardfork::HOLOCENE),
        _ => None,
    }
}

fn default_l1_activations() -> Option<HardforkActivations<l1::Hardfork>> {
    L1ChainSpec::chain_configs()
        .get(&1)
        .map(|config| config.hardfork_activations.clone())
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Chain {
    L1,
    Op,
    Generic,
    Arb,
    Ape,
}

impl Default for Chain {
    fn default() -> Self {
        Chain::L1
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForkConfig {
    json_rpc_url: String,
    #[serde(default, deserialize_with = "deserialize_opt_u64_from_str_or_int")]
    block_number: Option<u64>,
    #[serde(default)]
    http_headers: Option<Vec<HttpHeader>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProviderOptions {
    #[serde(default)]
    fork: Option<ForkConfig>,
    #[serde(default, deserialize_with = "deserialize_opt_u64_from_str_or_int")]
    chain_id: Option<u64>,
    #[serde(default)]
    hardfork: Option<String>,
    #[serde(default)]
    chains: Option<Vec<ChainConfig>>,
    #[serde(default)]
    native_token_mirror: Option<NativeTokenMirrorConfig>,
    #[serde(default)]
    allow_unlimited_contract_size: Option<bool>,
    #[serde(default)]
    allow_blocks_with_same_timestamp: Option<bool>,
    #[serde(default)]
    bail_on_call_failure: Option<bool>,
    #[serde(default)]
    bail_on_transaction_failure: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_opt_u64_from_str_or_int")]
    block_gas_limit: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_opt_u128_from_str_or_int")]
    min_gas_price: Option<u128>,
    #[serde(default, deserialize_with = "deserialize_opt_u64_from_str_or_int")]
    network_id: Option<u64>,
    #[serde(default)]
    cache_dir: Option<String>,
    #[serde(default)]
    owned_accounts: Option<Vec<OwnedAccount>>,
    #[serde(default)]
    chain: Chain,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HardforkActivationConfig {
    #[serde(deserialize_with = "deserialize_u64_from_str_or_int")]
    block_number: u64,
    spec_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChainConfig {
    #[serde(deserialize_with = "deserialize_u64_from_str_or_int")]
    chain_id: u64,
    #[serde(default)]
    hardforks: Vec<HardforkActivationConfig>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeTokenMirrorConfig {
    #[serde(deserialize_with = "deserialize_address_from_str_or_bytes")]
    token: Address,
    #[serde(default)]
    decimals: Option<u8>,
    #[serde(deserialize_with = "deserialize_u256_from_str_or_int")]
    balance_slot: U256,
}

impl From<&NativeTokenMirrorConfig> for NativeTokenMirror {
    fn from(value: &NativeTokenMirrorConfig) -> Self {
        Self {
            token: value.token,
            decimals: value.decimals,
            balance_slot: value.balance_slot,
        }
    }
}

fn chain_overrides<HardforkT: Clone>(
    chains: &[ChainConfig],
    parse_spec_id: impl Fn(&str) -> Option<HardforkT>,
) -> HashMap<u64, ChainOverride<HardforkT>> {
    let mut chain_overrides = HashMap::default();
    for c in chains {
        let mut activations = Vec::new();
        for hf in &c.hardforks {
            if let Some(spec) = parse_spec_id(&hf.spec_id) {
                activations.push(HardforkActivation {
                    condition: ForkCondition::Block(hf.block_number),
                    hardfork: spec,
                });
            }
        }

        if !activations.is_empty() {
            chain_overrides.insert(
                c.chain_id,
                ChainOverride {
                    name: String::new(),
                    native_token_mirror: None,
                    hardfork_activation_overrides: (!activations.is_empty())
                        .then(|| HardforkActivations::new(activations)),
                },
            );
        }
    }

    chain_overrides
}

fn insert_native_token_mirror_override<HardforkT>(
    chain_overrides: &mut HashMap<u64, ChainOverride<HardforkT>>,
    chain_id: u64,
    native_token_mirror: Option<&NativeTokenMirrorConfig>,
) {
    let Some(native_token_mirror) = native_token_mirror else {
        return;
    };

    chain_overrides
        .entry(chain_id)
        .and_modify(|chain_override| {
            chain_override.native_token_mirror = Some(native_token_mirror.into());
        })
        .or_insert_with(|| ChainOverride {
            name: String::new(),
            hardfork_activation_overrides: None,
            native_token_mirror: Some(native_token_mirror.into()),
        });
}

fn insert_default_l1_activations<HardforkT: Clone>(
    chain_overrides: &mut HashMap<u64, ChainOverride<HardforkT>>,
    chain_id: u64,
    activations: &HardforkActivations<HardforkT>,
) {
    chain_overrides
        .entry(chain_id)
        .and_modify(|chain_override| {
            if chain_override.hardfork_activation_overrides.is_none() {
                chain_override.hardfork_activation_overrides = Some(activations.clone());
            }
        })
        .or_insert_with(|| ChainOverride {
            name: String::new(),
            native_token_mirror: None,
            hardfork_activation_overrides: Some(activations.clone()),
        });
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpHeader {
    name: String,
    value: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnedAccount {
    secret_key: String,
    balance: String,
}

type LogCallback = extern "C" fn(u32, *const u8, usize, u8);
type DecodeLogCallback = extern "C" fn(u32, *const u8, usize);

#[derive(Clone)]

struct FfiLogger {
    id: u32,
    enabled: bool,
    print_cb: Option<LogCallback>,
    decode_cb: Option<DecodeLogCallback>,
}

impl FfiLogger {
    fn new(id: u32, print_ptr: usize, decode_ptr: usize, enabled: bool) -> Self {
        let print_cb = if print_ptr == 0 {
            None
        } else {
            Some(unsafe { std::mem::transmute::<usize, LogCallback>(print_ptr) })
        };
        let decode_cb = if decode_ptr == 0 {
            None
        } else {
            Some(unsafe { std::mem::transmute::<usize, DecodeLogCallback>(decode_ptr) })
        };
        Self {
            id,
            enabled,
            print_cb,
            decode_cb,
        }
    }

    fn send(&self, message: &str, replace: bool) {
        if self.enabled {
            if let Some(cb) = self.print_cb {
                let bytes = message.as_bytes();
                cb(self.id, bytes.as_ptr(), bytes.len(), replace as u8);
            }
        }
    }

    fn send_inputs(&self, inputs: &[Bytes]) {
        if let Some(cb) = self.decode_cb {
            let mut data = Vec::new();
            data.extend_from_slice(&(inputs.len() as u32).to_le_bytes());
            for input in inputs {
                data.extend_from_slice(&(input.len() as u32).to_le_bytes());
                data.extend_from_slice(input);
            }
            cb(self.id, data.as_ptr(), data.len());
        }
    }

    fn log_inputs(&self, inputs: &[Bytes]) {
        for input in inputs {
            if let Ok(msg) = std::str::from_utf8(input) {
                self.send(msg, false);
            }
        }
        self.send_inputs(inputs);
    }

    fn log_tx_common<ChainSpecT>(&self, tx: &ChainSpecT::SignedTransaction)
    where
        ChainSpecT: edr_provider::ProviderSpec<CurrentTime>,
    {
        self.send("  Contract call:       <UnrecognizedContract>", false);
        self.send(
            &format!("  From:                0x{:x}", tx.caller()),
            false,
        );
        if let Some(to) = tx.kind().to() {
            self.send(&format!("  To:                  0x{to:x}"), false);
        }
        self.send(&format!("  Gas limit:           {}", tx.gas_limit()), false);
        if *tx.value() > U256::ZERO {
            self.send(
                &format!(
                    "  Value:               {}",
                    wei_to_human_readable(tx.value())
                ),
                false,
            );
        }
    }
}

fn wei_to_human_readable(wei: &U256) -> String {
    if *wei == U256::ZERO {
        "0 ETH".to_string()
    } else if *wei < U256::from(100_000u64) {
        format!("{wei} wei")
    } else if *wei < U256::from(100_000_000_000_000u64) {
        let mut decimal = to_decimal_string(wei, 9);
        decimal.push_str(" gwei");
        decimal
    } else {
        let mut decimal = to_decimal_string(wei, 18);
        decimal.push_str(" ETH");
        decimal
    }
}

fn to_decimal_string(value: &U256, exponent: u8) -> String {
    const MAX_DECIMALS: u8 = 4;

    let (integer, remainder) = value.div_rem(U256::from(10).pow(U256::from(exponent)));
    let decimal = remainder / U256::from(10).pow(U256::from(exponent - MAX_DECIMALS));

    let decimal = decimal.to_string().trim_end_matches('0').to_string();

    format!("{integer}.{decimal}")
}

impl<ChainSpecT> edr_provider::Logger<ChainSpecT, CurrentTime> for FfiLogger
where
    ChainSpecT: edr_provider::ProviderSpec<CurrentTime>,
{
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn set_is_enabled(&mut self, is_enabled: bool) {
        self.enabled = is_enabled;
    }

    fn print_method_logs(
        &mut self,
        method: &str,
        error: Option<&edr_provider::ProviderErrorForChainSpec<ChainSpecT>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(err) = error {
            use edr_provider::ProviderError;
            if matches!(err, ProviderError::UnsupportedMethod { .. }) {
                self.send(&Color::Red.paint(err.to_string()).to_string(), false);
            } else {
                self.send(&Color::Red.paint(method).to_string(), false);
                self.send(&err.to_string(), false);
            }
        } else {
            self.send(&Color::Green.paint(method).to_string(), false);
        }
        Ok(())
    }

    fn log_call(
        &mut self,
        transaction: &ChainSpecT::SignedTransaction,
        result: &edr_provider::CallResultWithMetadata<ChainSpecT::HaltReason>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.log_tx_common::<ChainSpecT>(transaction);
        self.log_inputs(&result.console_log_inputs);

        if !result.execution_result.is_success() {
            self.send("  Error: Transaction failed", false);
        }
        Ok(())
    }

    fn log_estimate_gas_failure(
        &mut self,
        transaction: &ChainSpecT::SignedTransaction,
        result: &edr_provider::EstimateGasFailure<ChainSpecT::HaltReason>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.log_tx_common::<ChainSpecT>(transaction);
        self.log_inputs(&result.encoded_console_logs);

        let failure = &result.transaction_failure;
        self.send(&format!("  Error: {failure}"), false);

        Ok(())
    }

    fn log_send_transaction(
        &mut self,
        tx: &ChainSpecT::SignedTransaction,
        results: &[edr_provider::MineBlockResultWithMetadataForChainSpec<
            ChainSpecT,
            edr_provider::observability::EvmObservedData,
        >],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("  Contract call:       <UnrecognizedContract>", false);
        self.send(
            &format!("  Transaction:         0x{:x}", tx.transaction_hash()),
            false,
        );
        self.log_tx_common::<ChainSpecT>(tx);

        if let Some(result) = results.first() {
            if let Some(res) = result.transaction_results.first() {
                self.send(
                    &format!(
                        "  Gas used:            {} of {}",
                        res.gas_used(),
                        tx.gas_limit()
                    ),
                    false,
                );
            }
            self.send(
                &format!(
                    "  Block #{}:     0x{:x}",
                    result.block.block_header().number,
                    result.block.block_hash()
                ),
                false,
            );
            for observed_data in &result.transaction_inspector_data {
                self.log_inputs(&observed_data.encoded_console_logs);
            }
        }
        Ok(())
    }

    fn log_interval_mined(
        &mut self,
        result: &edr_provider::MineBlockResultWithMetadataForChainSpec<
            ChainSpecT,
            edr_provider::observability::EvmObservedData,
        >,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for observed_data in &result.transaction_inspector_data {
            self.send_inputs(&observed_data.encoded_console_logs);
        }
        Ok(())
    }

    fn log_mined_block(
        &mut self,
        results: &[edr_provider::MineBlockResultWithMetadataForChainSpec<
            ChainSpecT,
            edr_provider::observability::EvmObservedData,
        >],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for r in results {
            for observed_data in &r.transaction_inspector_data {
                self.send_inputs(&observed_data.encoded_console_logs);
            }
        }
        Ok(())
    }
}

enum ProviderEntry {
    L1(Arc<Provider<L1ChainSpec>>),
    Op(Arc<Provider<OpChainSpec>>),
    Generic(Arc<Provider<GenericChainSpec>>),
    Arb(Arc<Provider<ArbChainSpec>>),
    Ape(Arc<Provider<ApeChainSpec>>),
}

impl Clone for ProviderEntry {
    fn clone(&self) -> Self {
        match self {
            Self::L1(p) => Self::L1(Arc::clone(p)),
            Self::Op(p) => Self::Op(Arc::clone(p)),
            Self::Generic(p) => Self::Generic(Arc::clone(p)),
            Self::Arb(p) => Self::Arb(Arc::clone(p)),
            Self::Ape(p) => Self::Ape(Arc::clone(p)),
        }
    }
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);
static PROVIDERS: Lazy<Mutex<HashMap<u32, ProviderEntry>>> =
    Lazy::new(|| Mutex::new(HashMap::default()));
static NEXT_CTX_ID: AtomicU32 = AtomicU32::new(1);
static CONTEXTS: Lazy<Mutex<HashSet<u32>>> = Lazy::new(|| Mutex::new(HashSet::new()));
static RUNTIME: OnceCell<Runtime> = OnceCell::new();

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new().expect("create runtime"))
}

/// Creates a new context instance. A context is required to create providers.
#[deno_bindgen]
pub fn context_new() -> u32 {
    let id = NEXT_CTX_ID.fetch_add(1, Ordering::Relaxed);
    CONTEXTS.lock().unwrap().insert(id);
    id
}

/// Drops a previously created context.
#[deno_bindgen]
pub fn context_drop(id: u32) {
    CONTEXTS.lock().unwrap().remove(&id);
}

/// Returns the current version of the crate.
#[deno_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Creates a new provider within the provided context using the given JSON
/// configuration.
#[deno_bindgen]
pub fn provider_new(
    context_id: u32,
    config_json: &str,
    log_cb: usize,
    decode_cb: usize,
    log_enabled: u8,
) -> u32 {
    if !CONTEXTS.lock().unwrap().contains(&context_id) {
        return 0;
    }

    let opts: ProviderOptions = if config_json.trim().is_empty() {
        ProviderOptions::default()
    } else {
        match serde_json::from_str(config_json) {
            Ok(c) => c,
            Err(_) => return 0,
        }
    };

    let (owned_accounts, genesis_state) = if let Some(list) = opts.owned_accounts.clone() {
        let mut keys = Vec::new();
        let mut genesis = HashMap::new();
        for acc in list {
            let key = match secret_key_from_hex(&acc.secret_key) {
                Ok(k) => k,
                Err(_) => return 0,
            };
            let balance = match U256::from_str(&acc.balance) {
                Ok(b) => b,
                Err(_) => return 0,
            };
            let address = public_key_to_address(key.public_key());
            genesis.insert(
                address,
                AccountOverride {
                    balance: Some(balance),
                    nonce: None,
                    code: None,
                    storage: None,
                },
            );
            keys.push(key);
        }
        (keys, genesis)
    } else {
        (Vec::new(), HashMap::new())
    };

    let fork_opts = opts.fork.clone();
    let runtime = runtime();
    let contract_decoder = match ContractDecoder::new(&BuildInfoConfig::default()) {
        Ok(d) => Arc::new(RwLock::new(d)),
        Err(_) => return 0,
    };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    let entry = match opts.chain {
        Chain::L1 => {
            let fork = fork_opts.as_ref().map(|f| {
                let mut chain_overrides: HashMap<u64, ChainOverride<l1::Hardfork>> =
                    HashMap::default();
                if let Some(chains) = &opts.chains {
                    chain_overrides = self::chain_overrides(chains, parse_l1_spec_id);
                }
                edr_provider::ForkConfig {
                    block_number: f.block_number,
                    cache_dir: opts
                        .cache_dir
                        .clone()
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                    chain_overrides,
                    http_headers: f.http_headers.as_ref().map(|h| {
                        h.iter()
                            .map(|h| (h.name.clone(), h.value.clone()))
                            .collect::<std::collections::HashMap<_, _>>()
                    }),
                    url: f.json_rpc_url.clone(),
                }
            });
            let mut cfg = if fork.is_some() {
                test_utils::create_test_config_with_fork::<l1::Hardfork>(fork)
            } else {
                test_utils::create_test_config::<l1::Hardfork>()
            };
            if let Some(v) = opts.allow_unlimited_contract_size {
                cfg.allow_unlimited_contract_size = v;
            }
            if let Some(v) = opts.allow_blocks_with_same_timestamp {
                cfg.allow_blocks_with_same_timestamp = v;
            }
            if let Some(v) = opts.bail_on_call_failure {
                cfg.bail_on_call_failure = v;
            }
            if let Some(v) = opts.bail_on_transaction_failure {
                cfg.bail_on_transaction_failure = v;
            }
            if let Some(v) = opts.block_gas_limit {
                if let Some(nz) = NonZeroU64::new(v) {
                    cfg.block_gas_limit = nz;
                }
            }
            if let Some(v) = opts.min_gas_price {
                cfg.min_gas_price = v;
            }
            if let Some(id) = opts.chain_id {
                cfg.chain_id = id;
                if opts.network_id.is_none() {
                    cfg.network_id = id;
                }
            }
            if let Some(v) = opts.network_id {
                cfg.network_id = v;
            }
            if let Some(ref name) = opts.hardfork {
                if let Some(spec) = parse_l1_spec_id(name) {
                    cfg.hardfork = spec;
                }
            }
            if let Some(chains) = &opts.chains
                && !chains.is_empty()
            {
                cfg.chain_overrides
                    .extend(self::chain_overrides(chains, parse_l1_spec_id));
            }
            insert_native_token_mirror_override(
                &mut cfg.chain_overrides,
                cfg.chain_id,
                opts.native_token_mirror.as_ref(),
            );
            if !owned_accounts.is_empty() {
                cfg.owned_accounts = owned_accounts.clone();
            }
            if !genesis_state.is_empty() {
                cfg.genesis_state.extend(genesis_state.clone());
            }
            match Provider::<L1ChainSpec>::new(
                runtime.handle().clone(),
                Box::new(FfiLogger::new(id, log_cb, decode_cb, log_enabled != 0)),
                Box::new(|_event| {}),
                cfg,
                contract_decoder,
                CurrentTime,
            ) {
                Ok(p) => ProviderEntry::L1(Arc::new(p)),
                Err(_) => return 0,
            }
        }
        Chain::Op => {
            let fork = fork_opts.as_ref().map(|f| {
                let mut chain_overrides: HashMap<u64, ChainOverride<edr_op::Hardfork>> =
                    HashMap::default();
                if let Some(chains) = &opts.chains {
                    chain_overrides = self::chain_overrides(chains, parse_op_spec_id);
                }
                edr_provider::ForkConfig {
                    block_number: f.block_number,
                    cache_dir: opts
                        .cache_dir
                        .clone()
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                    chain_overrides,
                    http_headers: f.http_headers.as_ref().map(|h| {
                        h.iter()
                            .map(|h| (h.name.clone(), h.value.clone()))
                            .collect::<std::collections::HashMap<_, _>>()
                    }),
                    url: f.json_rpc_url.clone(),
                }
            });
            let mut cfg = if fork.is_some() {
                test_utils::create_test_config_with_fork::<edr_op::Hardfork>(fork)
            } else {
                test_utils::create_test_config::<edr_op::Hardfork>()
            };
            if let Some(v) = opts.allow_unlimited_contract_size {
                cfg.allow_unlimited_contract_size = v;
            }
            if let Some(v) = opts.allow_blocks_with_same_timestamp {
                cfg.allow_blocks_with_same_timestamp = v;
            }
            if let Some(v) = opts.bail_on_call_failure {
                cfg.bail_on_call_failure = v;
            }
            if let Some(v) = opts.bail_on_transaction_failure {
                cfg.bail_on_transaction_failure = v;
            }
            if let Some(v) = opts.block_gas_limit {
                if let Some(nz) = NonZeroU64::new(v) {
                    cfg.block_gas_limit = nz;
                }
            }
            if let Some(v) = opts.min_gas_price {
                cfg.min_gas_price = v;
            }
            if let Some(id) = opts.chain_id {
                cfg.chain_id = id;
                if opts.network_id.is_none() {
                    cfg.network_id = id;
                }
            }
            if let Some(v) = opts.network_id {
                cfg.network_id = v;
            }
            if let Some(ref name) = opts.hardfork {
                if let Some(spec) = parse_op_spec_id(name) {
                    cfg.hardfork = spec;
                }
            }
            if let Some(chains) = &opts.chains
                && !chains.is_empty()
            {
                cfg.chain_overrides
                    .extend(self::chain_overrides(chains, parse_op_spec_id));
            }
            insert_native_token_mirror_override(
                &mut cfg.chain_overrides,
                cfg.chain_id,
                opts.native_token_mirror.as_ref(),
            );
            if !owned_accounts.is_empty() {
                cfg.owned_accounts = owned_accounts.clone();
            }
            if !genesis_state.is_empty() {
                cfg.genesis_state.extend(genesis_state.clone());
            }
            match Provider::<OpChainSpec>::new(
                runtime.handle().clone(),
                Box::new(FfiLogger::new(id, log_cb, decode_cb, log_enabled != 0)),
                Box::new(|_event| {}),
                cfg,
                contract_decoder,
                CurrentTime,
            ) {
                Ok(p) => ProviderEntry::Op(Arc::new(p)),
                Err(_) => return 0,
            }
        }
        Chain::Generic => {
            let fork = fork_opts.as_ref().map(|f| {
                let mut chain_overrides: HashMap<u64, ChainOverride<l1::Hardfork>> =
                    HashMap::default();
                if let Some(chains) = &opts.chains {
                    chain_overrides = self::chain_overrides(chains, parse_l1_spec_id);
                }
                edr_provider::ForkConfig {
                    block_number: f.block_number,
                    cache_dir: opts
                        .cache_dir
                        .clone()
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                    chain_overrides,
                    http_headers: f.http_headers.as_ref().map(|h| {
                        h.iter()
                            .map(|h| (h.name.clone(), h.value.clone()))
                            .collect::<std::collections::HashMap<_, _>>()
                    }),
                    url: f.json_rpc_url.clone(),
                }
            });
            let mut cfg = if fork.is_some() {
                test_utils::create_test_config_with_fork::<l1::Hardfork>(fork)
            } else {
                test_utils::create_test_config::<l1::Hardfork>()
            };
            if let Some(v) = opts.allow_unlimited_contract_size {
                cfg.allow_unlimited_contract_size = v;
            }
            if let Some(v) = opts.allow_blocks_with_same_timestamp {
                cfg.allow_blocks_with_same_timestamp = v;
            }
            if let Some(v) = opts.bail_on_call_failure {
                cfg.bail_on_call_failure = v;
            }
            if let Some(v) = opts.bail_on_transaction_failure {
                cfg.bail_on_transaction_failure = v;
            }
            if let Some(v) = opts.block_gas_limit {
                if let Some(nz) = NonZeroU64::new(v) {
                    cfg.block_gas_limit = nz;
                }
            }
            if let Some(v) = opts.min_gas_price {
                cfg.min_gas_price = v;
            }
            if let Some(id) = opts.chain_id {
                cfg.chain_id = id;
                if opts.network_id.is_none() {
                    cfg.network_id = id;
                }
            }
            if let Some(v) = opts.network_id {
                cfg.network_id = v;
            }
            if let Some(ref name) = opts.hardfork {
                if let Some(spec) = parse_l1_spec_id(name) {
                    cfg.hardfork = spec;
                }
            }
            if let Some(chains) = &opts.chains {
                cfg.chain_overrides
                    .extend(self::chain_overrides(chains, parse_l1_spec_id));
            }
            if !cfg.chain_overrides.contains_key(&cfg.chain_id)
                && let Some(acts) = default_l1_activations()
            {
                insert_default_l1_activations(&mut cfg.chain_overrides, cfg.chain_id, &acts);
            }
            insert_native_token_mirror_override(
                &mut cfg.chain_overrides,
                cfg.chain_id,
                opts.native_token_mirror.as_ref(),
            );
            if !owned_accounts.is_empty() {
                cfg.owned_accounts = owned_accounts.clone();
            }
            if !genesis_state.is_empty() {
                cfg.genesis_state.extend(genesis_state.clone());
            }
            match Provider::<GenericChainSpec>::new(
                runtime.handle().clone(),
                Box::new(FfiLogger::new(id, log_cb, decode_cb, log_enabled != 0)),
                Box::new(|_event| {}),
                cfg,
                contract_decoder,
                CurrentTime,
            ) {
                Ok(p) => ProviderEntry::Generic(Arc::new(p)),
                Err(_) => return 0,
            }
        }
        Chain::Arb => {
            let fork = fork_opts.as_ref().map(|f| {
                let mut chain_overrides: HashMap<u64, ChainOverride<l1::Hardfork>> =
                    HashMap::default();
                if let Some(chains) = &opts.chains {
                    chain_overrides = self::chain_overrides(chains, parse_l1_spec_id);
                }
                edr_provider::ForkConfig {
                    block_number: f.block_number,
                    cache_dir: opts
                        .cache_dir
                        .clone()
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                    chain_overrides,
                    http_headers: f.http_headers.as_ref().map(|h| {
                        h.iter()
                            .map(|h| (h.name.clone(), h.value.clone()))
                            .collect::<std::collections::HashMap<_, _>>()
                    }),
                    url: f.json_rpc_url.clone(),
                }
            });
            let mut cfg = if fork.is_some() {
                test_utils::create_test_config_with_fork::<l1::Hardfork>(fork)
            } else {
                test_utils::create_test_config::<l1::Hardfork>()
            };
            if let Some(v) = opts.allow_unlimited_contract_size {
                cfg.allow_unlimited_contract_size = v;
            }
            if let Some(v) = opts.allow_blocks_with_same_timestamp {
                cfg.allow_blocks_with_same_timestamp = v;
            }
            if let Some(v) = opts.bail_on_call_failure {
                cfg.bail_on_call_failure = v;
            }
            if let Some(v) = opts.bail_on_transaction_failure {
                cfg.bail_on_transaction_failure = v;
            }
            if let Some(v) = opts.block_gas_limit {
                if let Some(nz) = NonZeroU64::new(v) {
                    cfg.block_gas_limit = nz;
                }
            }
            if let Some(v) = opts.min_gas_price {
                cfg.min_gas_price = v;
            }
            if let Some(id) = opts.chain_id {
                cfg.chain_id = id;
                if opts.network_id.is_none() {
                    cfg.network_id = id;
                }
            }
            if let Some(v) = opts.network_id {
                cfg.network_id = v;
            }
            if let Some(ref name) = opts.hardfork {
                if let Some(spec) = parse_l1_spec_id(name) {
                    cfg.hardfork = spec;
                }
            }
            if let Some(chains) = &opts.chains {
                cfg.chain_overrides
                    .extend(self::chain_overrides(chains, parse_l1_spec_id));
            }
            if !cfg.chain_overrides.contains_key(&cfg.chain_id)
                && let Some(acts) = default_l1_activations()
            {
                insert_default_l1_activations(&mut cfg.chain_overrides, cfg.chain_id, &acts);
            }
            insert_native_token_mirror_override(
                &mut cfg.chain_overrides,
                cfg.chain_id,
                opts.native_token_mirror.as_ref(),
            );
            if !owned_accounts.is_empty() {
                cfg.owned_accounts = owned_accounts.clone();
            }
            if !genesis_state.is_empty() {
                cfg.genesis_state.extend(genesis_state.clone());
            }
            match Provider::<ArbChainSpec>::new(
                runtime.handle().clone(),
                Box::new(FfiLogger::new(id, log_cb, decode_cb, log_enabled != 0)),
                Box::new(|_event| {}),
                cfg,
                contract_decoder,
                CurrentTime,
            ) {
                Ok(p) => ProviderEntry::Arb(Arc::new(p)),
                Err(_) => return 0,
            }
        }
        Chain::Ape => {
            let fork = fork_opts.as_ref().map(|f| {
                let mut chain_overrides: HashMap<u64, ChainOverride<l1::Hardfork>> =
                    HashMap::default();
                if let Some(chains) = &opts.chains {
                    chain_overrides = self::chain_overrides(chains, parse_l1_spec_id);
                }
                edr_provider::ForkConfig {
                    block_number: f.block_number,
                    cache_dir: opts
                        .cache_dir
                        .clone()
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                    chain_overrides,
                    http_headers: f.http_headers.as_ref().map(|h| {
                        h.iter()
                            .map(|h| (h.name.clone(), h.value.clone()))
                            .collect::<std::collections::HashMap<_, _>>()
                    }),
                    url: f.json_rpc_url.clone(),
                }
            });
            let mut cfg = if fork.is_some() {
                test_utils::create_test_config_with_fork::<l1::Hardfork>(fork)
            } else {
                test_utils::create_test_config::<l1::Hardfork>()
            };
            if let Some(v) = opts.allow_unlimited_contract_size {
                cfg.allow_unlimited_contract_size = v;
            }
            if let Some(v) = opts.allow_blocks_with_same_timestamp {
                cfg.allow_blocks_with_same_timestamp = v;
            }
            if let Some(v) = opts.bail_on_call_failure {
                cfg.bail_on_call_failure = v;
            }
            if let Some(v) = opts.bail_on_transaction_failure {
                cfg.bail_on_transaction_failure = v;
            }
            if let Some(v) = opts.block_gas_limit {
                if let Some(nz) = NonZeroU64::new(v) {
                    cfg.block_gas_limit = nz;
                }
            }
            if let Some(v) = opts.min_gas_price {
                cfg.min_gas_price = v;
            }
            if let Some(id) = opts.chain_id {
                cfg.chain_id = id;
                if opts.network_id.is_none() {
                    cfg.network_id = id;
                }
            }
            if let Some(v) = opts.network_id {
                cfg.network_id = v;
            }
            if let Some(ref name) = opts.hardfork {
                if let Some(spec) = parse_l1_spec_id(name) {
                    cfg.hardfork = spec;
                }
            }
            if let Some(chains) = &opts.chains {
                cfg.chain_overrides
                    .extend(self::chain_overrides(chains, parse_l1_spec_id));
            }
            if !cfg.chain_overrides.contains_key(&cfg.chain_id)
                && let Some(acts) = default_l1_activations()
            {
                insert_default_l1_activations(&mut cfg.chain_overrides, cfg.chain_id, &acts);
            }
            insert_native_token_mirror_override(
                &mut cfg.chain_overrides,
                cfg.chain_id,
                opts.native_token_mirror.as_ref(),
            );
            if !owned_accounts.is_empty() {
                cfg.owned_accounts = owned_accounts.clone();
            }
            if !genesis_state.is_empty() {
                cfg.genesis_state.extend(genesis_state.clone());
            }
            seed_ape_precompile_state(&mut cfg.genesis_state, fork_opts.as_ref());
            match Provider::<ApeChainSpec>::new(
                runtime.handle().clone(),
                Box::new(FfiLogger::new(id, log_cb, decode_cb, log_enabled != 0)),
                Box::new(|_event| {}),
                cfg,
                contract_decoder,
                CurrentTime,
            ) {
                Ok(p) => ProviderEntry::Ape(Arc::new(p)),
                Err(_) => return 0,
            }
        }
    };

    PROVIDERS.lock().unwrap().insert(id, entry);
    id
}

/// Handles a JSON-RPC request and returns the JSON-RPC response string.
#[deno_bindgen(non_blocking)]
pub fn provider_handle_request(id: u32, request: &str) -> String {
    let entry = {
        let map = PROVIDERS.lock().unwrap();
        match map.get(&id) {
            Some(p) => p.clone(),
            None => {
                let err = jsonrpc::ResponseData::<()>::new_error(
                    -32000,
                    "invalid provider",
                    None::<serde_json::Value>,
                );
                return serde_json::to_string(&err).unwrap();
            }
        }
    };

    match entry {
        ProviderEntry::L1(provider) => {
            let req: edr_provider::requests::ProviderRequest<L1ChainSpec> =
                match serde_json::from_str(request) {
                    Ok(r) => r,
                    Err(error) => {
                        let msg = error.to_string();
                        let value = serde_json::Value::from_str(request).ok();
                        let method = value
                            .as_ref()
                            .and_then(|v| v.get("method"))
                            .and_then(serde_json::Value::as_str);
                        let reason = InvalidRequestReason::new(method, &msg);
                        if let Some((name, provider_error)) =
                            reason.provider_error::<L1ChainSpec, CurrentTime>()
                        {
                            let _ = provider.log_failed_deserialization(name, &provider_error);
                        }
                        let err = jsonrpc::ResponseData::<()>::Error {
                            error: jsonrpc::Error {
                                code: reason.error_code(),
                                message: reason.error_message(),
                                data: value,
                            },
                        };
                        return serde_json::to_string(&err).unwrap();
                    }
                };
            let result = provider.handle_request(req);
            let response = jsonrpc::ResponseData::from(result.map(|r| r.result));
            serde_json::to_string(&response).unwrap()
        }
        ProviderEntry::Op(provider) => {
            let req: edr_provider::requests::ProviderRequest<OpChainSpec> =
                match serde_json::from_str(request) {
                    Ok(r) => r,
                    Err(error) => {
                        let msg = error.to_string();
                        let value = serde_json::Value::from_str(request).ok();
                        let method = value
                            .as_ref()
                            .and_then(|v| v.get("method"))
                            .and_then(serde_json::Value::as_str);
                        let reason = InvalidRequestReason::new(method, &msg);
                        if let Some((name, provider_error)) =
                            reason.provider_error::<OpChainSpec, CurrentTime>()
                        {
                            let _ = provider.log_failed_deserialization(name, &provider_error);
                        }
                        let err = jsonrpc::ResponseData::<()>::Error {
                            error: jsonrpc::Error {
                                code: reason.error_code(),
                                message: reason.error_message(),
                                data: value,
                            },
                        };
                        return serde_json::to_string(&err).unwrap();
                    }
                };
            let result = provider.handle_request(req);
            let response = jsonrpc::ResponseData::from(result.map(|r| r.result));
            serde_json::to_string(&response).unwrap()
        }
        ProviderEntry::Generic(provider) => {
            let req: edr_provider::requests::ProviderRequest<GenericChainSpec> =
                match serde_json::from_str(request) {
                    Ok(r) => r,
                    Err(error) => {
                        let msg = error.to_string();
                        let value = serde_json::Value::from_str(request).ok();
                        let method = value
                            .as_ref()
                            .and_then(|v| v.get("method"))
                            .and_then(serde_json::Value::as_str);
                        let reason = InvalidRequestReason::new(method, &msg);
                        if let Some((name, provider_error)) =
                            reason.provider_error::<GenericChainSpec, CurrentTime>()
                        {
                            let _ = provider.log_failed_deserialization(name, &provider_error);
                        }
                        let err = jsonrpc::ResponseData::<()>::Error {
                            error: jsonrpc::Error {
                                code: reason.error_code(),
                                message: reason.error_message(),
                                data: value,
                            },
                        };
                        return serde_json::to_string(&err).unwrap();
                    }
                };
            let result = provider.handle_request(req);
            let response = jsonrpc::ResponseData::from(result.map(|r| r.result));
            serde_json::to_string(&response).unwrap()
        }
        ProviderEntry::Arb(provider) => {
            let req: edr_provider::requests::ProviderRequest<ArbChainSpec> =
                match serde_json::from_str(request) {
                    Ok(r) => r,
                    Err(error) => {
                        let msg = error.to_string();
                        let value = serde_json::Value::from_str(request).ok();
                        let method = value
                            .as_ref()
                            .and_then(|v| v.get("method"))
                            .and_then(serde_json::Value::as_str);
                        let reason = InvalidRequestReason::new(method, &msg);
                        if let Some((name, provider_error)) =
                            reason.provider_error::<ArbChainSpec, CurrentTime>()
                        {
                            let _ = provider.log_failed_deserialization(name, &provider_error);
                        }
                        let err = jsonrpc::ResponseData::<()>::Error {
                            error: jsonrpc::Error {
                                code: reason.error_code(),
                                message: reason.error_message(),
                                data: value,
                            },
                        };
                        return serde_json::to_string(&err).unwrap();
                    }
                };
            let result = provider.handle_request(req);
            let response = jsonrpc::ResponseData::from(result.map(|r| r.result));
            serde_json::to_string(&response).unwrap()
        }
        ProviderEntry::Ape(provider) => {
            let req: edr_provider::requests::ProviderRequest<ApeChainSpec> =
                match serde_json::from_str(request) {
                    Ok(r) => r,
                    Err(error) => {
                        let msg = error.to_string();
                        let value = serde_json::Value::from_str(request).ok();
                        let method = value
                            .as_ref()
                            .and_then(|v| v.get("method"))
                            .and_then(serde_json::Value::as_str);
                        let reason = InvalidRequestReason::new(method, &msg);
                        if let Some((name, provider_error)) =
                            reason.provider_error::<ApeChainSpec, CurrentTime>()
                        {
                            let _ = provider.log_failed_deserialization(name, &provider_error);
                        }
                        let err = jsonrpc::ResponseData::<()>::Error {
                            error: jsonrpc::Error {
                                code: reason.error_code(),
                                message: reason.error_message(),
                                data: value,
                            },
                        };
                        return serde_json::to_string(&err).unwrap();
                    }
                };
            let result = provider.handle_request(req);
            let response = jsonrpc::ResponseData::from(result.map(|r| r.result));
            serde_json::to_string(&response).unwrap()
        }
    }
}

/// Enables or disables verbose tracing on the provider.
#[deno_bindgen]
pub fn provider_set_verbose_tracing(id: u32, enabled: u8) {
    if let Some(entry) = PROVIDERS.lock().unwrap().get(&id) {
        match entry {
            ProviderEntry::L1(p) => p.set_verbose_tracing(enabled != 0),
            ProviderEntry::Op(p) => p.set_verbose_tracing(enabled != 0),
            ProviderEntry::Generic(p) => p.set_verbose_tracing(enabled != 0),
            ProviderEntry::Arb(p) => p.set_verbose_tracing(enabled != 0),
            ProviderEntry::Ape(p) => p.set_verbose_tracing(enabled != 0),
        }
    }
}

/// Drops a provider created with [`provider_new`].
#[deno_bindgen]
pub fn provider_drop(id: u32) {
    PROVIDERS.lock().unwrap().remove(&id);
}
