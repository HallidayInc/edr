#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use ansi_term::Color;
use core::str::FromStr;
use deno_bindgen::deno_bindgen;
use edr_eth::{
    Bytes, U256,
    l1::{self, L1ChainSpec},
    transaction::ExecutableTransaction,
};
use edr_eth::signature::{DangerousSecretKeyStr, secret_key_from_str};
use edr_evm::Block as _;
use edr_evm::hardfork::{self, l1 as l1_hardfork};
use edr_generic::GenericChainSpec;
use edr_op::{self, OpChainSpec};
use edr_provider::{InvalidRequestReason, Provider, test_utils, time::CurrentTime};
use edr_rpc_client::jsonrpc;
use edr_solidity::{artifacts::BuildInfoConfig, contract_decoder::ContractDecoder};
use once_cell::sync::{Lazy, OnceCell};
use serde::{Deserialize, Deserializer};
use serde_json::Value as JsonValue;
use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU64,
    sync::atomic::{AtomicU32, Ordering},
    sync::{Arc, Mutex},
};
use tokio::runtime::Runtime;

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

fn deserialize_opt_u128_from_str_or_int<'de, D>(deserializer: D) -> Result<Option<u128>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<JsonValue>::deserialize(deserializer)? {
        None => Ok(None),
        Some(v) => parse_u128::<D::Error>(v).map(Some),
    }
}

fn parse_l1_spec_id(name: &str) -> Option<l1::SpecId> {
    match name.to_ascii_lowercase().as_str() {
        "frontier" => Some(l1::SpecId::FRONTIER),
        "frontierthawing" | "frontier_thawing" => Some(l1::SpecId::FRONTIER_THAWING),
        "homestead" => Some(l1::SpecId::HOMESTEAD),
        "daofork" | "dao_fork" => Some(l1::SpecId::DAO_FORK),
        "tangerine" => Some(l1::SpecId::TANGERINE),
        "spuriousdragon" | "spurious_dragon" => Some(l1::SpecId::SPURIOUS_DRAGON),
        "byzantium" => Some(l1::SpecId::BYZANTIUM),
        "constantinople" => Some(l1::SpecId::CONSTANTINOPLE),
        "petersburg" => Some(l1::SpecId::PETERSBURG),
        "istanbul" => Some(l1::SpecId::ISTANBUL),
        "muirglacier" | "muir_glacier" => Some(l1::SpecId::MUIR_GLACIER),
        "berlin" => Some(l1::SpecId::BERLIN),
        "london" => Some(l1::SpecId::LONDON),
        "arrowglacier" | "arrow_glacier" => Some(l1::SpecId::ARROW_GLACIER),
        "grayglacier" | "gray_glacier" => Some(l1::SpecId::GRAY_GLACIER),
        "merge" => Some(l1::SpecId::MERGE),
        "shanghai" => Some(l1::SpecId::SHANGHAI),
        "cancun" => Some(l1::SpecId::CANCUN),
        "prague" => Some(l1::SpecId::PRAGUE),
        _ => None,
    }
}

fn parse_op_spec_id(name: &str) -> Option<edr_op::OpSpecId> {
    match name.to_ascii_lowercase().as_str() {
        "bedrock" => Some(edr_op::OpSpecId::BEDROCK),
        "regolith" => Some(edr_op::OpSpecId::REGOLITH),
        "canyon" => Some(edr_op::OpSpecId::CANYON),
        "ecotone" => Some(edr_op::OpSpecId::ECOTONE),
        "fjord" => Some(edr_op::OpSpecId::FJORD),
        "granite" => Some(edr_op::OpSpecId::GRANITE),
        "holocene" => Some(edr_op::OpSpecId::HOLOCENE),
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Chain {
    L1,
    Op,
    Generic,
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
struct HardforkActivation {
    #[serde(deserialize_with = "deserialize_u64_from_str_or_int")]
    block_number: u64,
    spec_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChainConfig {
    #[serde(deserialize_with = "deserialize_u64_from_str_or_int")]
    chain_id: u64,
    hardforks: Vec<HardforkActivation>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpHeader {
    name: String,
    value: String,
}

#[derive(Deserialize)]
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
        ChainSpecT: edr_evm::spec::RuntimeSpec + edr_provider::ProviderSpec<CurrentTime>,
    {
        self.send("  Contract call:       <UnrecognizedContract>", false);
        self.send(
            &format!("  From:                0x{:x}", tx.caller()),
            false,
        );
        if let Some(to) = tx.kind().to() {
            self.send(&format!("  To:                  0x{to:x}"), false);
        }
        if *tx.value() > U256::ZERO {
            self.send(
                &format!("  Value:               {}", wei_to_human_readable(tx.value())),
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

impl<ChainSpecT> edr_provider::Logger<ChainSpecT> for FfiLogger
where
    ChainSpecT: edr_evm::spec::RuntimeSpec + edr_provider::ProviderSpec<CurrentTime>,
{
    type BlockchainError = edr_evm::blockchain::BlockchainErrorForChainSpec<ChainSpecT>;

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn set_is_enabled(&mut self, is_enabled: bool) {
        self.enabled = is_enabled;
    }

    fn print_contract_decoding_error(
        &mut self,
        error: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send(error, false);
        Ok(())
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
        _hardfork: ChainSpecT::Hardfork,
        transaction: &ChainSpecT::SignedTransaction,
        result: &edr_provider::CallResult<ChainSpecT::HaltReason>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.log_tx_common::<ChainSpecT>(transaction);
        self.log_inputs(&result.console_log_inputs);

        if let Some(failure) = edr_provider::TransactionFailure::from_execution_result::<
            ChainSpecT,
            CurrentTime,
        >(&result.execution_result, None, &result.trace)
        {
            self.send(&format!("  Error: {failure}"), false);
        }
        Ok(())
    }

    fn log_estimate_gas_failure(
        &mut self,
        _hardfork: ChainSpecT::Hardfork,
        transaction: &ChainSpecT::SignedTransaction,
        result: &edr_provider::EstimateGasFailure<ChainSpecT::HaltReason>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.log_tx_common::<ChainSpecT>(transaction);
        self.log_inputs(&result.console_log_inputs);

        let failure = &result.transaction_failure.failure;
        self.send(&format!("  Error: {failure}"), false);

        Ok(())
    }

    fn log_send_transaction(
        &mut self,
        _hardfork: ChainSpecT::Hardfork,
        tx: &ChainSpecT::SignedTransaction,
        results: &[edr_provider::DebugMineBlockResultForChainSpec<ChainSpecT>],
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
                    result.block.header().number,
                    result.block.block_hash()
                ),
                false,
            );
            for input in &result.console_log_inputs {
                if let Ok(msg) = std::str::from_utf8(input) {
                    self.send(&format!("console.log: {msg}"), false);
                }
            }
            self.send_inputs(&result.console_log_inputs);
        }
        Ok(())
    }

    fn log_interval_mined(
        &mut self,
        _hardfork: ChainSpecT::Hardfork,
        result: &edr_provider::DebugMineBlockResultForChainSpec<ChainSpecT>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send_inputs(&result.console_log_inputs);
        Ok(())
    }

    fn log_mined_block(
        &mut self,
        _hardfork: ChainSpecT::Hardfork,
        results: &[edr_provider::DebugMineBlockResultForChainSpec<ChainSpecT>],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for r in results {
            self.send_inputs(&r.console_log_inputs);
        }
        Ok(())
    }
}

enum ProviderEntry {
    L1(Arc<Provider<L1ChainSpec>>),
    Op(Arc<Provider<OpChainSpec>>),
    Generic(Arc<Provider<GenericChainSpec>>),
}

impl Clone for ProviderEntry {
    fn clone(&self) -> Self {
        match self {
            Self::L1(p) => Self::L1(Arc::clone(p)),
            Self::Op(p) => Self::Op(Arc::clone(p)),
            Self::Generic(p) => Self::Generic(Arc::clone(p)),
        }
    }
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);
static PROVIDERS: Lazy<Mutex<HashMap<u32, ProviderEntry>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
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

/// Creates a new provider within the provided context using the given JSON configuration.
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

    let owned_accounts = if let Some(list) = opts.owned_accounts {
        let mut out = Vec::new();
        for acc in list {
            let key = match {
                #[allow(deprecated)]
                secret_key_from_str(DangerousSecretKeyStr(&acc.secret_key))
            } {
                Ok(k) => k,
                Err(_) => return 0,
            };
            let balance = match U256::from_str(&acc.balance) {
                Ok(b) => b,
                Err(_) => return 0,
            };
            out.push(edr_provider::config::OwnedAccount {
                secret_key: key,
                balance,
            });
        }
        out
    } else {
        Vec::new()
    };

    let fork = opts
        .fork
        .as_ref()
        .map(|f| edr_provider::hardhat_rpc_types::ForkConfig {
            json_rpc_url: f.json_rpc_url.clone(),
            block_number: f.block_number,
            http_headers: f.http_headers.as_ref().map(|h| {
                h.iter()
                    .map(|h| (h.name.clone(), h.value.clone()))
                    .collect()
            }),
        });
    let runtime = runtime();
    let contract_decoder = match ContractDecoder::new(&BuildInfoConfig::default()) {
        Ok(d) => Arc::new(d),
        Err(_) => return 0,
    };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    let entry = match opts.chain {
        Chain::L1 => {
            let mut cfg = if fork.is_some() {
                test_utils::create_test_config_with_fork::<l1::SpecId>(fork.clone())
            } else {
                test_utils::create_test_config::<l1::SpecId>()
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
            if let Some(dir) = opts.cache_dir {
                cfg.cache_dir = dir.into();
            }
            if let Some(ref name) = opts.hardfork {
                if let Some(spec) = parse_l1_spec_id(name) {
                    cfg.hardfork = spec;
                }
            }
            if let Some(chains) = &opts.chains {
                for c in chains {
                    let mut activations = Vec::new();
                    for hf in &c.hardforks {
                        if let Some(spec) = parse_l1_spec_id(&hf.spec_id) {
                            activations
                                .push((hardfork::ForkCondition::Block(hf.block_number), spec));
                        }
                    }
                    if !activations.is_empty() {
                        cfg.chains
                            .insert(c.chain_id, hardfork::Activations::new(activations));
                    }
                }
            }
            if !owned_accounts.is_empty() {
                cfg.accounts = owned_accounts.clone();
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
            let mut cfg = if fork.is_some() {
                test_utils::create_test_config_with_fork::<edr_op::OpSpecId>(fork.clone())
            } else {
                test_utils::create_test_config::<edr_op::OpSpecId>()
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
            if let Some(dir) = opts.cache_dir {
                cfg.cache_dir = dir.into();
            }
            if let Some(ref name) = opts.hardfork {
                if let Some(spec) = parse_op_spec_id(name) {
                    cfg.hardfork = spec;
                }
            }
            if let Some(chains) = &opts.chains {
                for c in chains {
                    let mut activations = Vec::new();
                    for hf in &c.hardforks {
                        if let Some(spec) = parse_op_spec_id(&hf.spec_id) {
                            activations.push((hardfork::ForkCondition::Block(hf.block_number), spec));
                        }
                    }
                    if !activations.is_empty() {
                        cfg.chains.insert(c.chain_id, hardfork::Activations::new(activations));
                    }
                }
            }
            if !owned_accounts.is_empty() {
                cfg.accounts = owned_accounts.clone();
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
            let mut cfg = if fork.is_some() {
                test_utils::create_test_config_with_fork::<l1::SpecId>(fork.clone())
            } else {
                test_utils::create_test_config::<l1::SpecId>()
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
                cfg.network_id = id;
            } else {
                cfg.chain_id = 42161;
                cfg.network_id = 42161;
            }
            if let Some(v) = opts.network_id {
                cfg.network_id = v;
            }
            if let Some(dir) = opts.cache_dir {
                cfg.cache_dir = dir.into();
            }
            if let Some(ref name) = opts.hardfork {
                if let Some(spec) = parse_l1_spec_id(name) {
                    cfg.hardfork = spec;
                }
            }
            if let Some(chains) = opts.chains {
                if chains.is_empty() {
                    if let Some(acts) = l1_hardfork::chain_hardfork_activations(1) {
                        cfg.chains.insert(cfg.chain_id, acts.clone());
                    }
                } else {
                    for c in chains {
                        let mut activations = Vec::new();
                        for hf in c.hardforks {
                            if let Some(spec) = parse_l1_spec_id(&hf.spec_id) {
                                activations
                                    .push((hardfork::ForkCondition::Block(hf.block_number), spec));
                            }
                        }
                        if !activations.is_empty() {
                            cfg.chains
                                .insert(c.chain_id, hardfork::Activations::new(activations));
                        }
                    }
                }
            } else if let Some(acts) = l1_hardfork::chain_hardfork_activations(1) {
                cfg.chains.insert(cfg.chain_id, acts.clone());
            }
            if !owned_accounts.is_empty() {
                cfg.accounts = owned_accounts.clone();
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
                        if let Some((name, provider_error)) = reason.provider_error::<L1ChainSpec>()
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
                        if let Some((name, provider_error)) = reason.provider_error::<OpChainSpec>()
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
                            reason.provider_error::<GenericChainSpec>()
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
        }
    }
}

/// Drops a provider created with [`provider_new`].
#[deno_bindgen]
pub fn provider_drop(id: u32) {
    PROVIDERS.lock().unwrap().remove(&id);
}
