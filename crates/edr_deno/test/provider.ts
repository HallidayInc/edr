import { assert, assertEquals } from "https://deno.land/std@0.224.0/assert/mod.ts";
import { Context, version } from "../bindings/bindings.ts";

Deno.test("version exports a string", () => {
  const ver = version();
  assert(typeof ver === "string" && ver.length > 0);
});

Deno.test("multiple providers work", async () => {
  const ctx = new Context();
  const p1 = ctx.createProvider({ chain: "l1" });
  const p2 = ctx.createProvider({ chain: "l1" });

  const req = { id: 1, jsonrpc: "2.0", method: "eth_blockNumber", params: [] };
  const r1 = await p1.handleRequest(req) as any;
  const r2 = await p2.handleRequest(req) as any;
  assert("result" in r1);
  assertEquals(r1.result, r2.result);
  p1.close();
  p2.close();
  ctx.close();
});

Deno.test("logging callback works", async () => {
  const ctx = new Context();
  const logs: string[] = [];
  const p = ctx.createProvider({ chain: "l1" }, (msg) => logs.push(msg));
  await p.handleRequest({
    id: 1,
    jsonrpc: "2.0",
    method: "eth_blockNumber",
    params: [],
  });
  assert(logs.length > 0);
  p.close();
  ctx.close();
});

Deno.test("genesis account balance", async () => {
  const ctx = new Context();
  const p = ctx.createProvider({
    owned_accounts: [
      {
        secret_key: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        balance: "0xde0b6b3a7640000",
      },
    ],
  });
  const req = {
    id: 1,
    jsonrpc: "2.0",
    method: "eth_getBalance",
    params: ["0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266", "latest"],
  };
  const res = await p.handleRequest(req) as any;
  assertEquals(res.result.toLowerCase(), "0xde0b6b3a7640000");
  p.close();
  ctx.close();
});

Deno.test("arbitrum fork eth_call", async () => {
  const ctx = new Context();
  const arb = ctx.createProvider({
    chain: "generic",
    fork_url: "https://arb1.arbitrum.io/rpc",
    chain_id: 42161,
    hardfork: "cancun",
    chains: [{ chain_id: 42161, hardforks: [{ block_number: 0, spec_id: "cancun" }] }],
  });
  const call = {
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
  };
  const res = await arb.handleRequest(call) as any;
  const bal = BigInt(res.result);
  assert(bal > 0n);
  arb.close();
  ctx.close();
});
