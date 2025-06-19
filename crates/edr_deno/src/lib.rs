#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use deno_bindgen::deno_bindgen;
use once_cell::sync::{Lazy, OnceCell};
use std::{collections::HashMap, sync::{Arc, Mutex}, sync::atomic::{AtomicU32, Ordering}};
use tokio::runtime::Runtime;
use edr_provider::{Provider, test_utils, NoopLogger, time::CurrentTime};
use edr_eth::l1::{self, L1ChainSpec};
use edr_solidity::{contract_decoder::ContractDecoder, artifacts::BuildInfoConfig};
use edr_rpc_client::jsonrpc;

static NEXT_ID: AtomicU32 = AtomicU32::new(1);
static PROVIDERS: Lazy<Mutex<HashMap<u32, Arc<Provider<L1ChainSpec>>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static RUNTIME: OnceCell<Runtime> = OnceCell::new();

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new().expect("create runtime"))
}

/// Returns the current version of the crate.
#[deno_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Creates a new provider using a default configuration.
#[deno_bindgen]
pub fn provider_new() -> u32 {
    let runtime = runtime();
    let contract_decoder = ContractDecoder::new(&BuildInfoConfig::default()).expect("decoder");
    let provider = Provider::<L1ChainSpec>::new(
        runtime.handle().clone(),
        Box::new(NoopLogger::<L1ChainSpec>::default()),
        Box::new(|_event| {}),
        test_utils::create_test_config::<l1::SpecId>(),
        Arc::new(contract_decoder),
        CurrentTime,
    ).expect("provider");

    let provider = Arc::new(provider);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    PROVIDERS.lock().unwrap().insert(id, provider);
    id
}

/// Handles a JSON-RPC request and returns the JSON-RPC response string.
#[deno_bindgen(non_blocking)]
pub fn provider_handle_request(id: u32, request: &str) -> String {
    let provider = {
        let map = PROVIDERS.lock().unwrap();
        match map.get(&id) {
            Some(p) => Arc::clone(p),
            None => return String::from("{\"error\":\"invalid provider\"}"),
        }
    };

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

/// Enables or disables verbose tracing on the provider.
#[deno_bindgen]
pub fn provider_set_verbose_tracing(id: u32, enabled: u8) {
    let provider = {
        let map = PROVIDERS.lock().unwrap();
        match map.get(&id) {
            Some(p) => Arc::clone(p),
            None => return,
        }
    };
    provider.set_verbose_tracing(enabled != 0);
}
