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

- `chain`: either `"l1"` (default) or `"op"` for OP Stack based chains like
  Base
- `fork_url`: JSON-RPC endpoint to fork from
- `fork_block_number`: block height to fork at

Example:

```ts
import { context_new, provider_new } from "./bindings/bindings.ts";

const ctx = context_new();
const provider = provider_new(ctx, JSON.stringify({
  chain: "op",
  fork_url: "https://base.llamarpc.com",
}));
```
