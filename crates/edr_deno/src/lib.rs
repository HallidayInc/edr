#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use deno_bindgen::deno_bindgen;
use once_cell::sync::{Lazy, OnceCell};
use std::{collections::{HashMap, HashSet}, sync::{Arc, Mutex}, sync::atomic::{AtomicU32, Ordering}};
use tokio::runtime::Runtime;
use edr_provider::{Provider, test_utils, NoopLogger, time::CurrentTime};
use edr_eth::l1::{self, L1ChainSpec};
use edr_op::{self, OpChainSpec};
use edr_solidity::{contract_decoder::ContractDecoder, artifacts::BuildInfoConfig};
use edr_rpc_client::jsonrpc;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Chain {
    L1,
    Op,
}

impl Default for Chain {
    fn default() -> Self {
        Chain::L1
    }
}

#[derive(Deserialize)]
struct ProviderOptions {
    #[serde(default)]
    fork_url: Option<String>,
    #[serde(default)]
    fork_block_number: Option<u64>,
    #[serde(default)]
    chain: Chain,
}

enum ProviderEntry {
    L1(Arc<Provider<L1ChainSpec>>),
    Op(Arc<Provider<OpChainSpec>>),
}

impl Clone for ProviderEntry {
    fn clone(&self) -> Self {
        match self {
            Self::L1(p) => Self::L1(Arc::clone(p)),
            Self::Op(p) => Self::Op(Arc::clone(p)),
        }
    }
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);
static PROVIDERS: Lazy<Mutex<HashMap<u32, ProviderEntry>>> = Lazy::new(|| Mutex::new(HashMap::new()));
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
pub fn provider_new(context_id: u32, config_json: &str) -> u32 {
    if !CONTEXTS.lock().unwrap().contains(&context_id) {
        return 0;
    }

    let opts: ProviderOptions = match serde_json::from_str(config_json) {
        Ok(c) => c,
        Err(_) => ProviderOptions {
            fork_url: None,
            fork_block_number: None,
            chain: Chain::L1,
        },
    };

    let fork = opts.fork_url.map(|url| edr_provider::hardhat_rpc_types::ForkConfig {
        json_rpc_url: url,
        block_number: opts.fork_block_number,
        http_headers: None,
    });
    let runtime = runtime();
    let contract_decoder = match ContractDecoder::new(&BuildInfoConfig::default()) {
        Ok(d) => Arc::new(d),
        Err(_) => return 0,
    };
    let entry = match opts.chain {
        Chain::L1 => {
            let cfg = test_utils::create_test_config_with_fork::<l1::SpecId>(fork);
            match Provider::<L1ChainSpec>::new(
                runtime.handle().clone(),
                Box::new(NoopLogger::<L1ChainSpec>::default()),
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
            let cfg = test_utils::create_test_config_with_fork::<edr_op::OpSpecId>(fork);
            match Provider::<OpChainSpec>::new(
                runtime.handle().clone(),
                Box::new(NoopLogger::<OpChainSpec>::default()),
                Box::new(|_event| {}),
                cfg,
                contract_decoder,
                CurrentTime,
            ) {
                Ok(p) => ProviderEntry::Op(Arc::new(p)),
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
            let req: edr_provider::requests::ProviderRequest<L1ChainSpec> = match serde_json::from_str(request) {
                Ok(r) => r,
                Err(e) => {
                    let err = jsonrpc::ResponseData::<()>::new_error(-32600, &e.to_string(), None);
                    return serde_json::to_string(&err).unwrap();
                }
            };
            let result = provider.handle_request(req);
            let response = jsonrpc::ResponseData::from(result.map(|r| r.result));
            serde_json::to_string(&response).unwrap()
        }
        ProviderEntry::Op(provider) => {
            let req: edr_provider::requests::ProviderRequest<OpChainSpec> = match serde_json::from_str(request) {
                Ok(r) => r,
                Err(e) => {
                    let err = jsonrpc::ResponseData::<()>::new_error(-32600, &e.to_string(), None);
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
        }
    }
}

/// Drops a provider created with [`provider_new`].
#[deno_bindgen]
pub fn provider_drop(id: u32) {
    PROVIDERS.lock().unwrap().remove(&id);
}
