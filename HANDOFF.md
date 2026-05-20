# Handoff: instruction-table native_token_mirror

## Status

The architectural foundation is in place and compiles clean. Remaining work
is mechanical plumbing + the build/test loop.

## What's done

Commit `d35c0b7` (branch `rahul/native-mirror-instructions`):

- New crate `crates/edr_mirror`:
  - `MirrorContext` struct (config + tx-local keccak cache)
  - `MirrorHost` extension trait with `mirror()` read API and
    `set_native_balance(owner, value)` write API via
    `journal_mut().load_account_mut()`
  - `AsMirror` trait that the chain context implements
  - Custom KECCAK256 handler: computes hash normally, observes input,
    populates `keccak(addr || balance_slot) -> addr` cache
  - Custom SLOAD handler: runs default sload for warm/cold tracking + exact
    gas, then overrides the returned value with
    `native_to_erc20(account[owner].info.balance)` when target+slot
    match the mirror
  - Custom SSTORE handler: runs default sstore for trie/gas/refund
    fidelity, then on mirror hit also updates
    `account[owner].info.balance = erc20_to_native(value)`
  - `build_instructions::<W, H>() -> EthInstructions<W, H>` builder

- Chain spec updates:
  - `L1ChainSpec`: `ContextChainSpec::Context = MirrorContext`
  - `GenericChainSpec`, `ArbChainSpec`, `ApeChainSpec`: same
  - All `Context { chain: (), ... }` initializers → `MirrorContext::new(None)`
  - All `EthInstructions::default()` calls in `dry_run` /
    `dry_run_with_inspector` → `edr_mirror::build_instructions()`

- Workspace test fixes:
  - `crates/test/blockchain/src/lib.rs:216` `&()` → `&MirrorContext::new(None)`
  - `crates/edr_op/src/receipt/block.rs:88` same

- `cargo check -p edr_deno` is clean. `cargo check --workspace --exclude edr_scenarios` is clean.
  (edr_scenarios fails on a pre-existing `chain_overrides` field issue
  unrelated to this work.)

## What's NOT done

### 1. Mirror config plumbing (~30 min of focused work)

Currently every `MirrorContext::new(None)` initializer is hardcoded to
`None`, so the mirror config never reaches the runtime. The existing
flow:

```
chain config -> EthBlockBuilder { native_token_mirror: Option<&NativeTokenMirror> }
            -> OverriddenPrecompileProvider::with_precompiles_and_native_token_mirror(...)
            -> precompile dispatch
```

still passes the mirror config to `OverriddenPrecompileProvider`. To wire
it into `Context.chain`, choose one of:

**Option A (smallest diff):** add a public accessor on
`OverriddenPrecompileProvider`:

```rust
impl<...> OverriddenPrecompileProvider<...> {
    pub fn native_token_mirror(&self) -> Option<&NativeTokenMirror> {
        self.native_token_mirror.as_ref()
    }
}
```

But the `dry_run` signature in `ChainSpec` is generic over
`PrecompileProviderT: PrecompileProvider<...>` — to call our concrete
accessor, we'd need to add a method to the `PrecompileProvider` trait
itself, OR introduce an extension trait `HasMirrorConfig` that EDR's
provider implements.

**Option B (cleaner, slightly bigger diff):** introduce a parallel
parameter alongside `precompile_provider`:

```rust
fn dry_run<...>(
    ...
    precompile_provider: PrecompileProviderT,
    mirror_config: Option<NativeTokenMirror>,  // NEW
) -> ...;
```

Then the block builder (which holds `native_token_mirror`) passes it
through. Update all `dry_run` / `dry_run_with_inspector` impls + call
sites. The L1 spec.rs and generic spec.rs initializers become:
`chain: MirrorContext::new(mirror_config.clone())`.

Recommend Option B — it's explicit, doesn't require a trait
extension dance, and makes the data flow obvious.

### 2. Neuter the precompile mirror branch (~5 LOC)

`crates/precompile/src/lib.rs:109-112`:

```rust
if let Some(native_token_mirror) = &self.native_token_mirror
    && inputs.bytecode_address == native_token_mirror.token
{
    return run_native_token_mirror(context, inputs, native_token_mirror).map(Some);
}
```

Remove this block, OR add a feature flag, OR delete
`run_native_token_mirror` entirely. The new instruction-table approach
expects the call to fall through to real bytecode at the token address.

Also remove `native_token_mirror.token` from `unique_addresses()` so the
EVM doesn't treat the token as a "warm precompile" — the real contract
should follow normal cold/warm storage access rules.

### 3. Build + vendor + test

```bash
cd /tmp/halliday-edr
cargo build --release -p edr_deno --target aarch64-apple-darwin
```

First build: 15-20 min. Then:

```bash
cp target/aarch64-apple-darwin/release/libedr_deno.dylib \
   /tmp/halliday-rebase/lib/protocol/ts/vendor/edr/edr_deno.aarch64-apple-darwin.dylib
```

Then test:

```bash
cd /tmp/halliday-rebase
AWS_PROFILE=DeveloperAccess-060515097261 DEBUG=1 \
  SERVICE=layerzero FROM_CHAIN=stable TO_CHAIN=optimism \
  make test-with-docker-db ARGS="test/graph/hop.test.ts"
```

Expected: stable LZ tests should pass because:
- `crosschainBurn` reaches the real USDT0 contract (no precompile interception)
- The real contract does `SSTORE(keccak(user, balance_slot), new_value)`
- Our SSTORE hook recognizes the slot, does the storage write, AND
  updates `account[user].info.balance` to the scaled native value
- Subsequent reads via `balanceOf` (which uses SLOAD) get the
  native-derived value, consistent with EVM-level `BALANCE` opcode reads

### 4. Linux CI builds (separate)

Local darwin-arm64 only. To produce the two linux binaries
(`linux-arm64-gnu`, `linux-x64-gnu`) for CI we'd need `cross` + docker.
Either:
- Use the existing edr-build CI workflow (which builds `edr_napi`, not
  `edr_deno` — would need a parallel workflow)
- Build manually via `cross build --target ...`

Defer to follow-up.

## Risks / things to watch

1. **Gas accounting precision**: SSTORE refund logic might
   double-count or miss edge cases. revm's
   `gas::sstore_refund` is opaque from outside; if a test surfaces
   wrong gas-used numbers, that's where to look.

2. **`hardhat_setStorageAt`**: still bypasses our SSTORE hook (it
   pokes storage via RPC, not opcode). Cheats-side workaround in
   API codebase remains necessary. To fix in EDR, modify
   `crates/edr_provider/src/requests/hardhat/state.rs::handle_set_storage_at`
   to check for mirror token + balance slot, and call
   `set_balance` alongside. Defer — cheats workaround unblocks tests.

3. **Block boundaries**: the keccak cache is in `Context.chain`
   which lives per-Evm. Each tx gets a fresh Evm, so the cache is
   tx-scoped. If EDR ever reuses Context across txs, the cache
   could grow. Not currently a concern.

4. **edr_scenarios pre-existing error**: not related to this work.
   The crate has a `chain_overrides` field that's missing in some
   initializer — orthogonal to mirror changes.

## Summary

The hard parts (revm trait shapes, instruction signature
parameterization, ContextChainSpec refactor, MirrorHost trait design)
are done and compile. What remains is plumbing — passing one piece of
config through one more layer — and the build/vendor/test cycle.
~1-2 more hours of focused work.
