# Deno bindings for EDR

This crate provides experimental bindings for using **EDR** directly from Deno via [deno_bindgen](https://github.com/denoland/deno_bindgen).

These bindings are still under development and only expose a minimal API. The goal is to eventually match the functionality provided by `edr_napi` without requiring an `npm:` style import.

## Usage

Run `deno_bindgen` to generate the TypeScript bindings and compile the library. When running tests or the CLI inside restricted environments, you may need to add `--unsafely-ignore-certificate-errors` to the command line so remote dependencies can be fetched.

```bash
deno_bindgen --unsafely-ignore-certificate-errors
```

The library exposes a simple context object and provider constructor:

```ts
import { context_new, provider_new } from "./bindings/bindings.ts";

const ctx = context_new();
const provider = provider_new(ctx);
```
