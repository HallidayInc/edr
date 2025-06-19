import { assert, assertEquals } from "https://deno.land/std@0.224.0/assert/mod.ts";
import {
  context_new,
  context_drop,
  provider_new,
  provider_handle_request,
  provider_set_verbose_tracing,
  provider_drop,
  version,
} from "../bindings/bindings.ts";

Deno.test("version exports a string", () => {
  const ver = version();
  assert(typeof ver === "string" && ver.length > 0);
});

Deno.test("create multiple providers and handle a request", async () => {
  const ctx = context_new();
  const cfg = JSON.stringify({ chain: "l1" });
  const id1 = provider_new(ctx, cfg);
  const id2 = provider_new(ctx, cfg);

  const req = JSON.stringify({
    id: 1,
    jsonrpc: "2.0",
    method: "eth_blockNumber",
    params: [],
  });

  const res1 = JSON.parse(await provider_handle_request(id1, req));
  const res2 = JSON.parse(await provider_handle_request(id2, req));

  assert("result" in res1);
  assert("result" in res2);
  assertEquals(res1.result, res2.result);

  // ensure verbose tracing setter doesn't throw
  provider_set_verbose_tracing(id1, 1);
  provider_set_verbose_tracing(id2, 0);

  provider_drop(id1);
  provider_drop(id2);
  context_drop(ctx);
});

Deno.test("op provider works", async () => {
  const ctx = context_new();
  const cfg = JSON.stringify({ chain: "op" });
  const id = provider_new(ctx, cfg);
  const req = JSON.stringify({
    id: 1,
    jsonrpc: "2.0",
    method: "eth_blockNumber",
    params: [],
  });
  const res = JSON.parse(await provider_handle_request(id, req));
  assert("result" in res);
  provider_drop(id);
  context_drop(ctx);
});
