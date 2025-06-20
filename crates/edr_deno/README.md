# Deno bindings for EDR

This crate provides experimental bindings for using **EDR** directly from Deno via [deno_bindgen](https://github.com/denoland/deno_bindgen).

These bindings are still under development and only expose a minimal API. The goal is to eventually match the functionality provided by `edr_napi` without requiring an `npm:` style import.

## Usage

Download `nomicfoundation-edr-deno-<version>.tgz` from the project releases and
extract it next to your source files:

```bash
tar xf nomicfoundation-edr-deno-<version>.tgz
```

The archive contains the precompiled libraries for supported targets and a
`bindings.ts` file that automatically loads the correct one based on your
platform. Import the bindings from `crates/edr_deno` and create a context:

```ts
import { Context } from "./crates/edr_deno/bindings/bindings.ts";

using ctx = new Context();
```

The library exposes a simple context object and provider constructor. The
constructor accepts a JSON string with the following optional fields:

- `chain`: `"l1"` (default), `"op"` for OP Stack chains like Base, or
  `"generic"` for custom L1 forks such as Arbitrum
- `fork_url`: JSON-RPC endpoint to fork from
- `fork_block_number`: block height to fork at
- `fork_headers`: array of `{ name, value }` pairs sent as HTTP headers when forking
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
- `cache_dir`: directory used to cache RPC responses
- `owned_accounts`: array of accounts to pre-fund in the genesis block with the
  fields `secret_key` and `balance`

`Context.createProvider` also accepts an optional logger configuration:

```ts
const provider = ctx.createProvider(config, {
  printLineCallback: (msg, replace) => {
    console.log(msg);
  },
  decodeConsoleLogInputsCallback: (data) => {
    console.log("console.log:", new TextDecoder().decode(data));
  },
  enable: true,
});
```

Example:

```ts
import { Context } from "./bindings/bindings.ts";

using ctx = new Context();
using provider = ctx.createProvider({
  chain: "op",
  fork_url: "https://base.llamarpc.com",
  block_gas_limit: 30_000_000,
});

// create a local chain with one funded account
using local = ctx.createProvider({
  owned_accounts: [
    {
      secret_key: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
      balance: "0xde0b6b3a7640000",
    },
  ],
});

// fork Arbitrum and query a contract
using arb = ctx.createProvider({
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
});

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
const res = await arb.handleRequest(JSON.parse(call));
```

Both `Context` and `Provider` implement synchronous and asynchronous disposers,
so you can use them with JavaScript's `using` syntax and the resources will be
freed automatically when the block exits. They also expose a `close()` method
for manual cleanup if preferred.

### Local build

Run `make deno-package` from the repository root to compile `edr_deno` for your platform and create a release archive.

Compilation uses the `deno_bindgen` procedural macros which invoke the `deno` binary. Ensure Deno is installed and available in `PATH` when building. The GitHub workflow installs Deno for each build target via `denoland/setup-deno@v2` so the bindings can compile on all platforms.
