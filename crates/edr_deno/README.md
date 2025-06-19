# Deno bindings for EDR

This crate provides experimental bindings for using **EDR** directly from Deno via [deno_bindgen](https://github.com/denoland/deno_bindgen).

These bindings are still under development and only expose a minimal API. The goal is to eventually match the functionality provided by `edr_napi` without requiring an `npm:` style import.

## Usage

Run `deno_bindgen` to generate the TypeScript bindings and compile the library. When running tests or the CLI inside restricted environments, you may need to add `--unsafely-ignore-certificate-errors` to the command line so remote dependencies can be fetched.

```bash
deno_bindgen --unsafely-ignore-certificate-errors
```

If network restrictions prevent `deno_bindgen` from downloading its formatting
plugin, pre-generated bindings are provided in `bindings/`. They can be used
directly for local testing without running the generator.

The library exposes a simple context object and provider constructor. The
constructor accepts a JSON string with the following optional fields:

- `chain`: `"l1"` (default), `"op"` for OP Stack chains like Base, or
  `"generic"` for custom L1 forks such as Arbitrum
- `fork_url`: JSON-RPC endpoint to fork from
- `fork_block_number`: block height to fork at
- `chain_id`: override the provider's chain ID
- `hardfork`: starting hardfork for the chain
- `chains`: array of chain configurations with custom hardfork activations
- `allow_unlimited_contract_size`: allow deploying contracts larger than the
  usual limit
- `allow_blocks_with_same_timestamp`: permit mining blocks with duplicate
  timestamps
- `bail_on_call_failure`: return an error when `eth_call` fails
- `bail_on_transaction_failure`: return an error when a transaction fails
- `block_gas_limit`: override the block gas limit
- `min_gas_price`: minimum gas price for the next block
- `network_id`: set the network ID separately from `chain_id`

Example:

```ts
import { context_new, provider_new, provider_handle_request } from "./bindings/bindings.ts";

const ctx = context_new();
const provider = provider_new(ctx, JSON.stringify({
  chain: "op",
  fork_url: "https://base.llamarpc.com",
  block_gas_limit: 30_000_000,
}));

// fork Arbitrum and query a contract
const arb = provider_new(ctx, JSON.stringify({
  chain: "generic",
  fork_url: "https://arb1.arbitrum.io/rpc",
  chain_id: 42161,
  hardfork: "cancun",
  bail_on_call_failure: true,
  chains: [
    {
      chain_id: 42161,
      hardforks: [{ block_number: 0, spec_id: "cancun" }],
    },
  ],
}));

const call = JSON.stringify({
  id: 1,
  jsonrpc: "2.0",
  method: "eth_call",
  params: [
    {
      to: "0xFF970A61A04b1CA14834A43f5de4533ebddb5CC8",
      data:
        "0x70a08231000000000000000000000000ff970a61a04b1ca14834a43f5de4533ebddb5cc8",
    },
    "latest",
  ],
});
const res = await provider_handle_request(arb, call);
```
