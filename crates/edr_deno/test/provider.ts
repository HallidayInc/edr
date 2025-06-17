import { assert, assertEquals, assertRejects } from "jsr:@std/assert";
import { Context, Provider } from "../edr/mod.ts";

async function fetchRecentBlockNumber(url: string): Promise<bigint> {
    const response = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
            id: 1,
            jsonrpc: "2.0",
            method: "eth_blockNumber",
            params: [],
        }),
    });
    const json = await response.json();
    return BigInt(json.result);
}

async function request(provider: Provider, req: { method: string, params: any[] }) {
    const res = await provider.handleRequest(JSON.stringify({ id: 1, jsonrpc: "2.0", ...req }));
    const parsed = JSON.parse(res.data);
    if (parsed.result) {
        return parsed.result;
    } else {
        throw new Error(JSON.stringify(parsed.error));
    }
}

Deno.test("manual cleanup works", async () => {
    const ctx = new Context();
    const provider = ctx.createProvider({ chain: "l1" });
    try {
        const req = { method: "eth_blockNumber", params: [] };
        const res = await request(provider, req);
        assert(res);
    } finally {
        provider.close();
        ctx.close();
    }
    const req = { method: "eth_blockNumber", params: [] };
    await assertRejects(() => request(provider, req));
});

Deno.test("cleanup on unload", () => {
    const ctx = new Context();
    ctx.createProvider({ chain: "l1" });
});

Deno.test("multiple providers work", async () => {
    using ctx = new Context();
    using p1 = ctx.createProvider({ chain: "l1" });
    using p2 = ctx.createProvider({ chain: "l1" });

    const req = { method: "eth_blockNumber", params: [] };
    const r1 = await request(p1, req);
    const r2 = await request(p2, req);
    assertEquals(r1, r2);
});

Deno.test("logging callback works", async () => {
    const logs: string[] = [];
    using ctx = new Context();
    using p = ctx.createProvider(
        { chain: "l1" },
        { printLineCallback: (msg) => logs.push(msg) },
    );
    await request(p, { method: "eth_blockNumber", params: [] });
    assert(logs.length > 0);
});

Deno.test("decode logs callback", async () => {
    const logs: Uint8Array[] = [];
    using ctx = new Context();
    using p = ctx.createProvider(
        { chain: "l1" },
        { decodeConsoleLogInputsCallback: (inputs) => logs.push(...inputs) },
    );
    await request(p, {
        method: "eth_call",
        params: [
            {
                to: "0x000000000000000000636F6e736f6c652e6c6f67",
                data: "0x41304fac0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000568656c6c6f000000000000000000000000000000000000000000000000000000",
            },
            "latest",
        ],
    });
    assert(logs.length > 0);
});

Deno.test("base fee changes by less than 12.5% between blocks", async () => {
    const rpcUrl = "https://arb1.arbitrum.io/rpc";
    using ctx = new Context();
    using arb = ctx.createProvider({
        chain: "arb",
        fork: {
            jsonRpcUrl: rpcUrl,
            blockNumber: await fetchRecentBlockNumber(rpcUrl),
        },
        chainId: 42161,
        hardfork: "cancun",
        chains: [{
            chainId: 42161,
            hardforks: [{ blockNumber: 0, specId: "cancun" }],
        }],
    });

    const blockBeforeMine = await request(arb, { method: "eth_getBlockByNumber", params: ["latest", false] });
    const baseFeeBeforeMine = BigInt(blockBeforeMine.baseFeePerGas);
    const blockNumberBeforeMine = BigInt(blockBeforeMine.number);

    await request(arb, { method: "hardhat_mine", params: ["0x1"] });

    const blockAfterMine = await request(arb, { method: "eth_getBlockByNumber", params: ["latest", false] });
    const baseFeeAfterMine = BigInt(blockAfterMine.baseFeePerGas);

    assertEquals(
        BigInt(blockAfterMine.number),
        blockNumberBeforeMine + 1n,
        "expected hardhat_mine to advance the block number by one",
    );

    if (baseFeeBeforeMine === 0n) {
        assertEquals(
            baseFeeAfterMine,
            0n,
            "base fee should remain zero if the previous block's base fee was zero",
        );
    } else {
        assert(
            baseFeeAfterMine * 8n <= baseFeeBeforeMine * 9n,
            `base fee increased by more than 12.5% between blocks: before=${baseFeeBeforeMine.toString()} after=${baseFeeAfterMine.toString()}`,
        );
        assert(
            baseFeeAfterMine * 8n >= baseFeeBeforeMine * 7n,
            `base fee decreased by more than 12.5% between blocks: before=${baseFeeBeforeMine.toString()} after=${baseFeeAfterMine.toString()}`,
        );
    }
});

Deno.test("genesis account balance", async () => {
    using ctx = new Context();
    using p = ctx.createProvider({
        // no fork creates a local chain
        ownedAccounts: [
            {
                secretKey: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
                balance: 1000n * 10n ** 18n,
            },
        ],
    });
    const accounts = await request(p, { method: "eth_accounts", params: [] });
    assertEquals(accounts, ["0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"]);
    const req = {
        method: "eth_getBalance",
        params: ["0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266", "latest"],
    };
    const res = await request(p, req);
    assertEquals(BigInt(res), 1000n * 10n ** 18n);

    const txHash = await request(p, {
        method: "eth_sendTransaction" as any,
        params: [
            {
                from: "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266",
                to: "0x1234567890123456789012345678901234567890",
                value: "0xDE0B6B3A7640000",
                gas: "0x520800",
            },
        ],
    });
    assert(txHash);
});

Deno.test("chain id override", async () => {
    using ctx = new Context();
    using p = ctx.createProvider({ chain: "l1", chainId: 10n, networkId: 100n });

    const cid = await request(p, { method: "eth_chainId", params: [] });
    const nid = await request(p, { method: "net_version", params: [] });
    assertEquals(cid, "0xa");
    assertEquals(nid, "100");
});

Deno.test("arbitrum fork eth_call", async () => {
    const rpcUrl = "https://arb1.arbitrum.io/rpc";
    using ctx = new Context();
    using arb = ctx.createProvider({
        chain: "generic",
        fork: {
            jsonRpcUrl: rpcUrl,
            blockNumber: await fetchRecentBlockNumber(rpcUrl),
        },
        chainId: 42161,
        hardfork: "cancun",
        chains: [{
            chainId: 42161,
            hardforks: [{ blockNumber: 0, specId: "cancun" }],
        }],
    });
    const call = {
        method: "eth_call",
        params: [
            {
                to: "0xFF970A61A04b1CA14834A43f5de4533ebddb5CC8",
                data: "0x70a08231000000000000000000000000ff970a61a04b1ca14834a43f5de4533ebddb5cc8",
            },
            "latest",
        ],
    };
    const bal = await request(arb, call);
    assert(BigInt(bal) > 0n);
});

Deno.test("story fork eth_call", async () => {
    const rpcUrl = "https://mainnet.storyrpc.io";
    using ctx = new Context();
    using arb = ctx.createProvider({
        chain: "op",
        fork: {
            jsonRpcUrl: rpcUrl,
            blockNumber: await fetchRecentBlockNumber(rpcUrl),
        },
        chainId: 1514n,
        hardfork: "holocene",
        chains: [{
            chainId: 1514n,
            hardforks: [{ blockNumber: 0, specId: "holocene" }],
        }],
    });
    const call = {
        method: "eth_call",
        params: [
            {
                to: "0xF1815bd50389c46847f0Bda824eC8da914045D14",
                data: "0x70a08231000000000000000000000000ff970a61a04b1ca14834a43f5de4533ebddb5cc8",
            },
            "latest",
        ],
    };
    const bal = await request(arb, call);
    assert(BigInt(bal) > 0n);
});

Deno.test("sepolia fork block number", async () => {
    using ctx = new Context();
    using sepolia = ctx.createProvider({
        fork: { jsonRpcUrl: "https://sepolia.gateway.tenderly.co" },
    });
    const block = await request(sepolia, { method: "eth_blockNumber", params: [] });
    assert(BigInt(block) > 0n);
});

Deno.test("realish setup", async () => {
    const config = {
        allowBlocksWithSameTimestamp: true,
        allowUnlimitedContractSize: true,
        bailOnCallFailure: true,
        bailOnTransactionFailure: true,
        blockGasLimit: 36000000n,
        chain: "generic",
        chainId: 42161n,
        chains: [],
        fork: { jsonRpcUrl: "https://arb1.arbitrum.io/rpc" },
        hardfork: "cancun",
        minGasPrice: 0n,
        networkId: 42161n,
    };
    const forkConfig = {
        ...config.fork,
        blockNumber: await fetchRecentBlockNumber(config.fork.jsonRpcUrl),
    };
    using ctx = new Context();
    using arb = ctx.createProvider({ ...config, fork: forkConfig });
    const call = {
        method: "eth_call",
        params: [
            {
                to: "0xFF970A61A04b1CA14834A43f5de4533ebddb5CC8",
                data:
                "0x70a08231000000000000000000000000ff970a61a04b1ca14834a43f5de4533ebddb5cc8",
            },
            "latest",
        ],
    };
    const bal = await request(arb, call);
    assert(BigInt(bal) > 0n);
});

Deno.test("transaction logging details", async () => {
    const logs: string[] = [];
    using ctx = new Context();
    using p = ctx.createProvider(
        {
            ownedAccounts: [
                {
                    secretKey:
                        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
                    balance: 1000n * 10n ** 18n,
                },
            ],
        },
        { printLineCallback: (m) => logs.push(m) },
    );
    await request(p, {
        method: "eth_sendTransaction" as any,
        params: [
            {
                from: "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266",
                to: "0x1234567890123456789012345678901234567890",
                value: "0x1",
                gas: "0x5208",
            },
        ],
    });
    assert(logs.some((l) => l.includes("From")));
    assert(logs.some((l) => l.includes("To")));
});
