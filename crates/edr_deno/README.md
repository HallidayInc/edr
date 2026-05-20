# Deno bindings for EDR

This crate provides experimental bindings for using **EDR** directly from Deno via [deno_bindgen](https://github.com/denoland/deno_bindgen).

These bindings are still under development and only expose a minimal API. The goal is to eventually match the functionality provided by `edr_napi` without requiring an `npm:` style import.

## Usage

Download `nomicfoundation-edr-deno-<version>.tgz` from the project releases and extract it next to your source files:

```bash
tar xf nomicfoundation-edr-deno-<version>.tgz
```

The archive contains precompiled libraries and a `mod.ts` file that automatically loads one. Importing `edr/mod.ts` creates a single persistent connection to the Rust runtime. Contexts and providers are managed on the Rust side so Deno only tracks that connection. Create a context and providers as needed:

```ts
import { Context } from "./edr/mod.ts";

using ctx = new Context();
```

The library exposes a simple context object and provider constructor. The constructor accepts a JSON string with the following optional fields:

- `chain`: `"l1"` (default), `"op"` for OP Stack chains like Base, or
- `"generic"` for custom L1 forks,
- `"arb"` for Arbitrum-compatible chains, or
- `"ape"` for ApeChain's Arbitrum-based precompile extensions
- `fork`: `{ jsonRpcUrl, blockNumber?, httpHeaders? }` configuration for forking a remote chain
- `chainId`: override the provider's chain ID
- `hardfork`: starting hardfork for the chain
- `chains`: array of chain configurations with custom hardfork activations
- `allowUnlimitedContractSize`: allow deploying contracts larger than the usual limit
- `allowBlocksWithSameTimestamp`: permit mining blocks with duplicate timestamps
- `bailOnCallFailure`: return an error when `eth_call` fails
- `bailOnTransactionFailure`: return an error when a transaction fails
- `blockGasLimit`: override the block gas limit
- `minGasPrice`: minimum gas price for the next block
- `networkId`: set the network ID separately from `chainId`
- `cacheDir`: directory used to cache RPC responses
- `ownedAccounts`: array of accounts to pre-fund in the genesis block with the fields `secretKey` and `balance`

`Context.createProvider` also accepts an optional logger configuration:

```ts
const provider = ctx.createProvider(config, {
  printLineCallback: (msg, replace) => {
    console.log(msg);
  },
  decodeConsoleLogInputsCallback: (inputs) => {
    for (const data of inputs) {
      console.log("console.log:", new TextDecoder().decode(data));
    }
  },
  enable: true,
});
```

Example:

```ts
import { Context } from "./edr/mod.ts";

using ctx = new Context();
using provider = ctx.createProvider({
  chain: "op",
  fork: { jsonRpcUrl: "https://base.llamarpc.com" },
  blockGasLimit: 30_000_000,
});

// create a local chain with one funded account
using local = ctx.createProvider({
  ownedAccounts: [
    {
      secretKey:
        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
      balance: "0xde0b6b3a7640000",
    },
  ],
});

// fork Arbitrum and query a contract
using arb = ctx.createProvider({
  chain: "arb",
  fork: { jsonRpcUrl: "https://arb1.arbitrum.io/rpc" },
  chainId: 42161,
  hardfork: "cancun",
  bailOnCallFailure: true,
  chains: [
    {
      chainId: 42161,
      hardforks: [{ blockNumber: 0, specId: "cancun" }],
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
      data: "0x70a08231000000000000000000000000ff970a61a04b1ca14834a43f5de4533ebddb5cc8",
    },
    "latest",
  ],
});
const res = await arb.handleRequest(JSON.parse(call));
const json = typeof res.data === "string" ? JSON.parse(res.data) : res.data;
// `res.data` holds the JSON-RPC response string
```

Both `Context` and `Provider` implement synchronous and asynchronous disposers, so you can use them with JavaScript's `using` syntax or call their `close()` methods manually. Any remaining providers are automatically cleaned up when the runtime exits.

### Local build

Run `make deno-package` from the repository root to compile `edr_deno` for your platform and create a release archive.

Compilation uses the `deno_bindgen` procedural macros which invoke the `deno` binary. Ensure Deno is installed and available in `PATH` when building. The GitHub workflow installs Deno for each build target via `denoland/setup-deno@v2` so the bindings can compile on all platforms.

### Testing

Run the following commands to verify the bindings locally. The Deno tests require network access and may need certificate validation disabled when running in isolated environments:

```bash
cargo check -p edr_deno
make deno-package
deno test -A --unsafely-ignore-certificate-errors crates/edr_deno/test/provider.ts
```
