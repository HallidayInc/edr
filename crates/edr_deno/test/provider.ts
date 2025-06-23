import { assert, assertEquals } from "https://deno.land/std@0.224.0/assert/mod.ts";
import { Context, version } from "../edr/mod.ts";

Deno.test("version exports a string", () => {
  const ver = version();
  assert(typeof ver === "string" && ver.length > 0);
});

Deno.test("manual cleanup works", async () => {
  const ctx = new Context();
  const provider = ctx.createProvider({ chain: "l1" });
  try {
    const req = { id: 1, jsonrpc: "2.0", method: "eth_blockNumber", params: [] };
    const res = await provider.handleRequest(req) as any;
    assert("result" in res);
  } finally {
    provider.close();
    ctx.close();
  }
});

Deno.test("multiple providers work", async () => {
  using ctx = new Context();
  using p1 = ctx.createProvider({ chain: "l1" });
  using p2 = ctx.createProvider({ chain: "l1" });

  const req = { id: 1, jsonrpc: "2.0", method: "eth_blockNumber", params: [] };
  const r1 = await p1.handleRequest(req) as any;
  const r2 = await p2.handleRequest(req) as any;
  assert("result" in r1);
  assertEquals(r1.result, r2.result);
  // resources cleaned up by using
});

Deno.test("logging callback works", async () => {
  using ctx = new Context();
  const logs: string[] = [];
  using p = ctx.createProvider(
    { chain: "l1" },
    { printLineCallback: (msg) => logs.push(msg) },
  );
  await p.handleRequest({
    id: 1,
    jsonrpc: "2.0",
    method: "eth_blockNumber",
    params: [],
  });
  assert(logs.length > 0);
  // disposed automatically
});

Deno.test("decode logs callback", async () => {
  using ctx = new Context();
  const decoded: Uint8Array[] = [];
  using p = ctx.createProvider(
    { chain: "l1" },
    { decodeConsoleLogInputsCallback: (d) => decoded.push(d) },
  );
  await p.handleRequest({
    id: 1,
    jsonrpc: "2.0",
    method: "eth_call",
    params: [
      {
        to: "0x000000000000000000636F6e736f6c652e6c6f67",
        data:
          "0x41304fac0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000568656c6c6f000000000000000000000000000000000000000000000000000000",
      },
      "latest",
    ],
  });
  assert(decoded.length > 0);
});

Deno.test("genesis account balance", async () => {
  using ctx = new Context();
  using p = ctx.createProvider({
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
  // auto dispose
});

Deno.test("chain id override", async () => {
  using ctx = new Context();
  using p = ctx.createProvider({ chain: "l1", chain_id: 10, network_id: 100 });

  const cid = await p.handleRequest({
    id: 1,
    jsonrpc: "2.0",
    method: "eth_chainId",
    params: [],
  }) as any;

  const nid = await p.handleRequest({
    id: 2,
    jsonrpc: "2.0",
    method: "net_version",
    params: [],
  }) as any;

  assertEquals(cid.result, "0xa");
  assertEquals(nid.result, "100");
});

Deno.test("arbitrum fork eth_call", async () => {
  using ctx = new Context();
  using arb = ctx.createProvider({
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
  // automatically disposed
});

Deno.test("realish setup", async () => {
  const config = {
    allow_blocks_with_same_timestamp: true,
    allow_unlimited_contract_size: true,
    bail_on_call_failure: true,
    bail_on_transaction_failure: true,
    block_gas_limit: 36000000n,
    chain: "generic",
    chain_id: 42161n,
    chains: [],
    fork_url: "https://arb1.arbitrum.io/rpc",
    hardfork: "cancun",
    min_gas_price: 0n,
    network_id: 42161n,
  };
  using ctx = new Context();
  using arb = ctx.createProvider(config);
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
  console.log(res);
  const bal = BigInt(res.result);
  assert(bal > 0n);
});