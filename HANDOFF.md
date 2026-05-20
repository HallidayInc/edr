# native_token_mirror: instruction-table implementation — DONE

## Status: working end-to-end on `rahul/native-mirror-instructions`

Stable LZ hop tests pass for all 4 destination chains:
```
Validate layerzero: stable:usdt0 → ethereum:usdt   ok (25s)
Validate layerzero: stable:usdt0 → polygon:usdt0   ok (5s)
Validate layerzero: stable:usdt0 → arbitrum:usdt0  ok (6s)
Validate layerzero: stable:usdt0 → optimism:usdt0  ok (6s)
```

## Architecture

Replaces the precompile-based native_token_mirror with instruction-table
hooks on `KECCAK256`, `SLOAD`, `SSTORE`. Real ERC-20 bytecode runs at the
token address (no precompile interception). When the contract reads or
writes its balance slot, the mirror redirects/syncs against
`account.info.balance`.

### Hooks (in `crates/edr_mirror/src/lib.rs`)

- **KECCAK256** — computes hash normally, then if the 64-byte input matches
  `(addr_left_padded || balance_slot)` for our mirror token, caches
  `keccak_hash → addr`. This is how SLOAD/SSTORE later recover the owner
  from the storage slot without trying to invert keccak.
- **SLOAD** — runs `sload_skip_cold_load` for accurate warm/cold tracking +
  gas. If the storage access is on the mirror token's balance slot, override
  the returned value with `native_to_erc20(account[owner].info.balance)`.
- **SSTORE** — runs `sstore_skip_cold_load` for accurate trie/gas/refund.
  Then, if it's a balance-slot write, also calls `set_native_balance(owner,
  erc20_to_native(value))` to keep native in sync.

### State placement

`MirrorContext { config, cache: RefCell<HashMap<U256, Address>> }` lives in
`Context.chain` (`ChainSpec::Context`). Each Evm gets a fresh cache, so the
hash → owner mapping is tx-scoped.

### Config plumbing

`dry_run` / `dry_run_with_inspector` trait methods accept an
`Option<NativeTokenMirror>` parameter. The top-level
`crates/evm/src/lib.rs::dry_run` (and `_with_inspector`) — which already
received `native_token_mirror: Option<&NativeTokenMirror>` from the EDR
provider config — clones it and passes it through to the trait dispatch.
Each chain spec impl puts it in `MirrorContext::new(mirror_config)`.

### Crucial detail: precompile address set

`crates/precompile/src/lib.rs::unique_addresses()` no longer includes
`native_token_mirror.token`. Previously the mirror token was in
`warm_addresses()` (a precompile address hint), which caused
`revm-inspectors` to classify calls as precompile invocations and produce
shallow trace nodes. Because the call actually fell through to real
bytecode (with deeper calls/SLOAD/SSTORE underneath), the trace arena's
depth tracking would later panic with `Disconnected trace`. Removing the
token from `unique_addresses` fixed it.

The legacy `run_native_token_mirror` precompile function is left in
`precompile/src/lib.rs` but never dispatched. Cleanup is a follow-up.

## Files touched

### New
- `crates/edr_mirror/{Cargo.toml, src/lib.rs}` — the mirror crate

### Modified
- `crates/chain/spec/evm/{Cargo.toml, src/lib.rs}` — add `mirror_config`
  param to `dry_run` / `dry_run_with_inspector` trait methods
- `crates/edr_chain_l1/{Cargo.toml, src/spec.rs}` — `Context = MirrorContext`,
  thread mirror_config, swap `EthInstructions::default()` for
  `edr_mirror::build_instructions()`
- `crates/edr_generic/{Cargo.toml, src/spec.rs}` — same pattern for
  GenericChainSpec, ArbChainSpec, ApeChainSpec (3 impls)
- `crates/edr_op/{Cargo.toml, src/spec.rs, src/receipt/block.rs}` — accept
  the `mirror_config` param but ignore it (OP chain support deferred —
  OP uses `L1BlockInfo` as `Context.chain`, not `MirrorContext`)
- `crates/evm/src/lib.rs` — pass `native_token_mirror.cloned()` into trait
  dispatch
- `crates/precompile/src/lib.rs` — neuter the mirror branch + remove
  token from `unique_addresses`
- `crates/test/blockchain/{Cargo.toml, src/lib.rs}` — `&()` →
  `&MirrorContext::new(None)` in one test fixture

## Open follow-ups

### OP chain support
Currently OP impls accept the `mirror_config` parameter but discard it
(`_mirror_config`). To support OP chains with mirrored tokens, the chain
context needs to carry both `L1BlockInfo` AND `MirrorContext`. Probably
introduce `OpChainContext { l1: L1BlockInfo, mirror: MirrorContext }` and
update the `EthInstructionsContext<...>` parameterization in
`edr_op/src/{spec.rs, solidity_tests.rs}`. ~30 LOC of plumbing.

### Dead code cleanup
- `run_native_token_mirror` function in `precompile/src/lib.rs` is dead.
- `OverriddenPrecompileProvider::native_token_mirror` field is still
  populated but unused. Either remove the field or have it bypass-only.
- `pub trait AsMirror` and `pub trait HasMirrorConfig` in `edr_mirror` were
  added during exploration but only `AsMirror` is used. `HasMirrorConfig`
  is dead.

### Linux CI builds
Local darwin-arm64 only. For CI:
```bash
cross build --release -p edr_deno --target aarch64-unknown-linux-gnu
cross build --release -p edr_deno --target x86_64-unknown-linux-gnu
```
Needs `cross` + docker. Defer.

### hardhat_setStorageAt sync
This bypasses our SSTORE hook (it's a JSON-RPC method, not an opcode).
Currently the cheats layer routes mirrored tokens through
`hardhat_setBalance` instead (in
`lib/protocol/ts/src/proto/cheats/evm.ts`). To remove that workaround,
modify `crates/edr_provider/src/requests/hardhat/state.rs::handle_set_storage_at`
to detect mirror token + balance slot and sync native.

## Build / test

```bash
cd /tmp/halliday-edr
cargo build --release -p edr_deno --target aarch64-apple-darwin
cp target/aarch64-apple-darwin/release/libedr_deno.dylib \
   /path/to/Protocol/ts/vendor/edr/edr_deno.aarch64-apple-darwin.dylib

cd /path/to/HallidayAPI
AWS_PROFILE=DeveloperAccess-060515097261 SERVICE=layerzero FROM_CHAIN=stable \
  make test-with-docker-db ARGS="test/graph/hop.test.ts"
```
