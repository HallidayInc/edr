#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use core::str::FromStr;
use deno_bindgen::deno_bindgen;
use edr_eth::{
    Bytes, U256,
    l1::{self, L1ChainSpec},
    signature::{DangerousSecretKeyStr, secret_key_from_str},
};
use edr_evm::hardfork::{self, l1 as l1_hardfork};
use edr_generic::GenericChainSpec;
use edr_op::{self, OpChainSpec};
use edr_provider::{Provider, test_utils, time::CurrentTime};
use edr_rpc_client::jsonrpc;
use edr_solidity::{artifacts::BuildInfoConfig, contract_decoder::ContractDecoder};
use once_cell::sync::{Lazy, OnceCell};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU64,
    sync::atomic::{AtomicU32, Ordering},
    sync::{Arc, Mutex},
};
use tokio::runtime::Runtime;

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

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProviderOptions {
    #[serde(default)]
    fork_url: Option<String>,
    #[serde(default)]
    fork_block_number: Option<u64>,
    #[serde(default)]
    fork_headers: Option<Vec<HttpHeader>>,
    #[serde(default)]
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
    #[serde(default)]
    block_gas_limit: Option<u64>,
    #[serde(default)]
    min_gas_price: Option<u128>,
    #[serde(default)]
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
    block_number: u64,
    spec_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChainConfig {
    chain_id: u64,
    hardforks: Vec<HardforkActivation>,
}

#[derive(Deserialize)]
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

type LogCallback = extern "C" fn(*const u8, usize, u8);
type DecodeLogCallback = extern "C" fn(*const u8, usize);

#[derive(Clone)]
struct FfiLogger {
    enabled: bool,
    print_cb: Option<LogCallback>,
    decode_cb: Option<DecodeLogCallback>,
}

impl FfiLogger {
    fn new(print_ptr: usize, decode_ptr: usize, enabled: bool) -> Self {
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
            enabled,
            print_cb,
            decode_cb,
        }
    }

    fn send(&self, message: &str, replace: bool) {
        if self.enabled {
            if let Some(cb) = self.print_cb {
                let bytes = message.as_bytes();
                cb(bytes.as_ptr(), bytes.len(), replace as u8);
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
            cb(data.as_ptr(), data.len());
        }
    }
}

impl<ChainSpecT: edr_evm::spec::RuntimeSpec> edr_provider::Logger<ChainSpecT> for FfiLogger {
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
            self.send(&format!("{method}: {}", err), false);
        } else {
            self.send(method, false);
        }
        Ok(())
    }

    fn log_call(
        &mut self,
        _hardfork: ChainSpecT::Hardfork,
        _transaction: &ChainSpecT::SignedTransaction,
        result: &edr_provider::CallResult<ChainSpecT::HaltReason>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send_inputs(&result.console_log_inputs);
        Ok(())
    }

    fn log_send_transaction(
        &mut self,
        _hardfork: ChainSpecT::Hardfork,
        _tx: &ChainSpecT::SignedTransaction,
        results: &[edr_provider::DebugMineBlockResultForChainSpec<ChainSpecT>],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for r in results {
            self.send_inputs(&r.console_log_inputs);
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
            let key = match secret_key_from_str(DangerousSecretKeyStr(&acc.secret_key)) {
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

    let fork = opts.fork_url.as_ref().map(|url| edr_provider::hardhat_rpc_types::ForkConfig {
        json_rpc_url: url.clone(),
        block_number: opts.fork_block_number,
        http_headers: opts.fork_headers.as_ref().map(|h| {
            h.iter().map(|h| (h.name.clone(), h.value.clone())).collect()
        }),
    });
    let runtime = runtime();
    let contract_decoder = match ContractDecoder::new(&BuildInfoConfig::default()) {
        Ok(d) => Arc::new(d),
        Err(_) => return 0,
    };
    let entry = match opts.chain {
        Chain::L1 => {
            let mut cfg = test_utils::create_test_config_with_fork::<l1::SpecId>(fork);
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
            cfg.accounts.extend_from_slice(&owned_accounts);
            match Provider::<L1ChainSpec>::new(
                runtime.handle().clone(),
                Box::new(FfiLogger::new(log_cb, decode_cb, log_enabled != 0)),
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
            let mut cfg = test_utils::create_test_config_with_fork::<edr_op::OpSpecId>(fork);
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
            cfg.accounts.extend_from_slice(&owned_accounts);
            match Provider::<OpChainSpec>::new(
                runtime.handle().clone(),
                Box::new(FfiLogger::new(log_cb, decode_cb, log_enabled != 0)),
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
            let mut cfg = test_utils::create_test_config_with_fork::<l1::SpecId>(fork);
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
            cfg.accounts.extend_from_slice(&owned_accounts);
            match Provider::<GenericChainSpec>::new(
                runtime.handle().clone(),
                Box::new(FfiLogger::new(log_cb, decode_cb, log_enabled != 0)),
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

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
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
            None => return String::from("{\"error\":\"invalid provider\"}"),
        }
    };

    match entry {
        ProviderEntry::L1(provider) => {
            let req: edr_provider::requests::ProviderRequest<L1ChainSpec> =
                match serde_json::from_str(request) {
                    Ok(r) => r,
                    Err(e) => {
                        let err =
                            jsonrpc::ResponseData::<()>::new_error(-32600, &e.to_string(), None);
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
                    Err(e) => {
                        let err =
                            jsonrpc::ResponseData::<()>::new_error(-32600, &e.to_string(), None);
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
                    Err(e) => {
                        let err =
                            jsonrpc::ResponseData::<()>::new_error(-32600, &e.to_string(), None);
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
