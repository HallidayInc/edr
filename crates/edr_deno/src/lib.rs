#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use deno_bindgen::deno_bindgen;

/// Returns the current version of the crate.
#[deno_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// TODO: Expose EDR provider bindings for Deno using `deno_bindgen`.
