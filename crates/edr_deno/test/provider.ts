import { assert, assertEquals, assertRejects } from "jsr:@std/assert";
import { hmac } from "jsr:@noble/hashes/hmac.js";
import { keccak_256 } from "jsr:@noble/hashes/sha3.js";
import { sha256 } from "jsr:@noble/hashes/sha2.js";
import { getPublicKey, hashes, sign } from "jsr:@noble/secp256k1";
import { Context, Provider } from "../edr/mod.ts";

hashes.sha256 ??= sha256;
hashes.hmacSha256 ??= (key, message) => hmac(sha256, key, message);

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

async function rpcRequest(url: string, req: { method: string, params: any[] }) {
    const response = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ id: 1, jsonrpc: "2.0", ...req }),
    });
    const json = await response.json();
    if (json.result !== undefined) {
        return json.result;
    }
    throw new Error(JSON.stringify(json.error));
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

function hexToBytes(value: string) {
    let normalized = value.replace(/^0x/, "");
    if (normalized.length % 2 !== 0) {
        normalized = `0${normalized}`;
    }
    const bytes = new Uint8Array(normalized.length / 2);
    for (let i = 0; i < normalized.length; i += 2) {
        bytes[i / 2] = Number.parseInt(normalized.slice(i, i + 2), 16);
    }
    return bytes;
}

function bytesToHex(value: Uint8Array) {
    const hex = Array.from(value, (byte) => byte.toString(16).padStart(2, "0")).join("");
    return `0x${hex.length === 0 ? "0" : hex}`;
}

function toRpcQuantity(value: bigint) {
    return `0x${value.toString(16)}`;
}

function selector(signature: string) {
    return bytesToHex(keccak_256(new TextEncoder().encode(signature)).slice(0, 4));
}

function encodeAddressArg(address: string) {
    return address.toLowerCase().replace(/^0x/, "").padStart(64, "0");
}

function encodeUintArg(value: bigint) {
    return value.toString(16).padStart(64, "0");
}

function decodeFirstWord(result: string) {
    return BigInt(`0x${result.replace(/^0x/, "").slice(0, 64) || "0"}`);
}

function addressToWordBytes(address: string) {
    const addressBytes = hexToBytes(address);
    const word = new Uint8Array(32);
    word.set(addressBytes, 12);
    return word;
}

function bigintToWordBytes(value: bigint) {
    const word = new Uint8Array(32);
    const hex = value.toString(16).padStart(64, "0");
    word.set(hexToBytes(`0x${hex}`));
    return word;
}

function mappingStorageKey(address: string, slot: bigint) {
    const preimage = new Uint8Array(64);
    preimage.set(addressToWordBytes(address), 0);
    preimage.set(bigintToWordBytes(slot), 32);
    return bytesToHex(keccak_256(preimage));
}

function decodeWord(result: string, index: number) {
    const normalized = result.replace(/^0x/, "");
    const start = index * 64;
    return BigInt(`0x${normalized.slice(start, start + 64) || "0"}`);
}

function decodeBytes(result: string) {
    const normalized = result.replace(/^0x/, "");
    const offset = Number(decodeWord(result, 0)) * 2;
    const length = Number(BigInt(`0x${normalized.slice(offset, offset + 64) || "0"}`));
    const start = offset + 64;
    return `0x${normalized.slice(start, start + length * 2)}`;
}

function bigintToBytes(value: bigint) {
    if (value === 0n) return new Uint8Array([]);
    let hex = value.toString(16);
    if (hex.length % 2 !== 0) hex = `0${hex}`;
    return Uint8Array.from(hex.match(/.{1,2}/g)!.map((byte) => Number.parseInt(byte, 16)));
}

function addressFromPrivateKey(privateKey: Uint8Array) {
    const publicKey = getPublicKey(privateKey, false).slice(1);
    const hash = keccak_256(publicKey);
    return bytesToHex(hash.slice(-20));
}

function rlpEncodeBytes(bytes: Uint8Array) {
    if (bytes.length === 1 && bytes[0] < 0x80) {
        return bytes;
    }
    if (bytes.length <= 55) {
        return Uint8Array.from([0x80 + bytes.length, ...bytes]);
    }
    const lengthBytes = bigintToBytes(BigInt(bytes.length));
    return Uint8Array.from([
        0xb7 + lengthBytes.length,
        ...lengthBytes,
        ...bytes,
    ]);
}

function rlpEncodeList(items: Uint8Array[]) {
    const encodedItems = items.flatMap((item) => Array.from(rlpEncodeBytes(item)));
    const payload = Uint8Array.from(encodedItems);
    if (payload.length <= 55) {
        return Uint8Array.from([0xc0 + payload.length, ...payload]);
    }
    const lengthBytes = bigintToBytes(BigInt(payload.length));
    return Uint8Array.from([
        0xf7 + lengthBytes.length,
        ...lengthBytes,
        ...payload,
    ]);
}

function signAuthorization(
    privateKey: Uint8Array,
    chainId: bigint,
    address: string,
    nonce: bigint,
) {
    const addressBytes = hexToBytes(address);
    const encoded = rlpEncodeList([
        bigintToBytes(chainId),
        addressBytes,
        bigintToBytes(nonce),
    ]);
    const hash = keccak_256(new Uint8Array([0x05, ...encoded]));
    const signature = sign(hash, privateKey, { prehash: false, format: "recovered" });
    const r = signature.slice(1, 33);
    const s = signature.slice(33, 65);
    return {
        chainId: bytesToHex(bigintToBytes(chainId)),
        address,
        nonce: bytesToHex(bigintToBytes(nonce)),
        yParity: signature[0],
        r: bytesToHex(r),
        s: bytesToHex(s),
    };
}

async function assertDelegationRestorable(options: {
    rpcUrl: string;
    chain: "l1" | "op";
    chainId: bigint;
    hardfork: string;
    chains?: { chainId: bigint; hardforks: { blockNumber: number; specId: string }[] }[];
}) {
    const delegatedCode = "0xef01001234567890123456789012345678901234567890";
    const senderKey = hexToBytes(
        "0x59c6995e998f97a5a0044976faccf36a7b42d7b7b8a59d1c9adf3b7a7a33b5c5",
    );
    const sender = addressFromPrivateKey(senderKey);

    using ctx = new Context();
    using fork = ctx.createProvider({
        chain: options.chain,
        fork: {
            jsonRpcUrl: options.rpcUrl,
            blockNumber: await fetchRecentBlockNumber(options.rpcUrl),
        },
        chainId: options.chainId,
        hardfork: options.hardfork,
        chains: options.chains,
        ownedAccounts: [
            { secretKey: bytesToHex(senderKey), balance: 10n ** 20n },
        ],
    });

    const originalCode = await request(fork, {
        method: "eth_getCode",
        params: [sender, "latest"],
    });

    const nonce1 = BigInt(await request(fork, {
        method: "eth_getTransactionCount",
        params: [sender, "latest"],
    }));
    const auth1 = signAuthorization(
        senderKey,
        options.chainId,
        "0x1234567890123456789012345678901234567890",
        nonce1 + 1n,
    );
    await request(fork, {
        method: "eth_sendTransaction",
        params: [
            {
                from: sender,
                to: sender,
                maxFeePerGas: "0x3b9aca00",
                maxPriorityFeePerGas: "0x3b9aca00",
                gas: "0xf618",
                nonce: bytesToHex(bigintToBytes(nonce1)),
                authorizationList: [auth1],
            },
        ],
    });
    const updatedCode = await request(fork, {
        method: "eth_getCode",
        params: [sender, "latest"],
    });
    assertEquals(updatedCode.toLowerCase(), delegatedCode.toLowerCase());

    const nonce2 = BigInt(await request(fork, {
        method: "eth_getTransactionCount",
        params: [sender, "latest"],
    }));
    const auth2 = signAuthorization(
        senderKey,
        options.chainId,
        "0x0000000000000000000000000000000000000000",
        nonce2 + 1n,
    );
    await request(fork, {
        method: "eth_sendTransaction",
        params: [
            {
                from: sender,
                to: sender,
                maxFeePerGas: "0x3b9aca00",
                maxPriorityFeePerGas: "0x3b9aca00",
                gas: "0xf618",
                nonce: bytesToHex(bigintToBytes(nonce2)),
                authorizationList: [auth2],
            },
        ],
    });
    const restoredCode = await request(fork, {
        method: "eth_getCode",
        params: [sender, "latest"],
    });
    assertEquals(restoredCode.toLowerCase(), originalCode.toLowerCase());
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

Deno.test("fork delegate code can be restored", async () => {
    await assertDelegationRestorable({
        rpcUrl: "https://arb1.arbitrum.io/rpc",
        chain: "l1",
        chainId: 31337n,
        hardfork: "prague",
    });
});

Deno.test("fork delegate code can be restored on base", async () => {
    await assertDelegationRestorable({
        rpcUrl: "https://mainnet.base.org",
        chain: "op",
        chainId: 8453n,
        hardfork: "prague",
        chains: [{
            chainId: 8453n,
            hardforks: [{ blockNumber: 0, specId: "prague" }],
        }],
    });
});

Deno.test("realish setup", async () => {
    const config = {
        allowBlocksWithSameTimestamp: true,
        allowUnlimitedContractSize: true,
        bailOnCallFailure: true,
        bailOnTransactionFailure: true,
        blockGasLimit: 16000000n,
        chain: "arb",
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

Deno.test("local arbitrum ArbSys withdrawEth succeeds", async () => {
    const sender = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
    const arbSys = "0x0000000000000000000000000000000000000064";
    const destination = "0x1234567890123456789012345678901234567890";

    using ctx = new Context();
    using arb = ctx.createProvider({
        chain: "arb",
        chainId: 42161n,
        networkId: 42161n,
        hardfork: "cancun",
        chains: [{
            chainId: 42161n,
            hardforks: [{ blockNumber: 0, specId: "cancun" }],
        }],
        ownedAccounts: [{
            secretKey: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            balance: 1000n * 10n ** 18n,
        }],
    });

    const withdrawData = `${selector("withdrawEth(address)")}${encodeAddressArg(destination)}`;
    const txHash = await request(arb, {
        method: "eth_sendTransaction",
        params: [{
            from: sender,
            to: arbSys,
            gas: "0x186a0",
            value: "0x2386f26fc10000",
            data: withdrawData,
        }],
    });

    const receipt = await request(arb, {
        method: "eth_getTransactionReceipt",
        params: [txHash],
    });
    assertEquals(receipt.status, "0x1");
    assert(receipt.logs.length >= 1);
    assertEquals(receipt.logs[0].address.toLowerCase(), arbSys);

    const arbSysBalance = await request(arb, {
        method: "eth_getBalance",
        params: [arbSys, "latest"],
    });
    assertEquals(BigInt(arbSysBalance), 0n);

    const sendState = await request(arb, {
        method: "eth_call",
        params: [{
            to: arbSys,
            data: selector("sendMerkleTreeState()"),
        }, "latest"],
    });
    assertEquals(decodeFirstWord(sendState), 1n);
});

Deno.test("arbitrum fork ArbInfo precompile mirrors standard account queries", async () => {
    const rpcUrl = "https://arb1.arbitrum.io/rpc";
    const arbInfo = "0x0000000000000000000000000000000000000065";
    const account = "0xFF970A61A04b1CA14834A43f5de4533ebddb5CC8";

    using ctx = new Context();
    using arb = ctx.createProvider({
        chain: "arb",
        bailOnCallFailure: true,
        chainId: 42161n,
        fork: {
            jsonRpcUrl: rpcUrl,
            blockNumber: await fetchRecentBlockNumber(rpcUrl),
        },
        hardfork: "cancun",
        chains: [{
            chainId: 42161n,
            hardforks: [{ blockNumber: 0, specId: "cancun" }],
        }],
    });

    const expectedBalance = BigInt(await request(arb, {
        method: "eth_getBalance",
        params: [account, "latest"],
    }));
    const actualBalance = await request(arb, {
        method: "eth_call",
        params: [{
            to: arbInfo,
            data: `${selector("getBalance(address)")}${encodeAddressArg(account)}`,
        }, "latest"],
    });
    assertEquals(decodeFirstWord(actualBalance), expectedBalance);

    const expectedCode = await request(arb, {
        method: "eth_getCode",
        params: [account, "latest"],
    });
    const actualCode = await request(arb, {
        method: "eth_call",
        params: [{
            to: arbInfo,
            data: `${selector("getCode(address)")}${encodeAddressArg(account)}`,
        }, "latest"],
    });
    assertEquals(decodeBytes(actualCode).toLowerCase(), expectedCode.toLowerCase());
});

// Minimal ERC20 (balanceOf, decimals=6, transfer) with `_balances` mapping at
// storage slot 3. Compiled from solc 0.8.28 of:
//
//   contract MirrorErc20 {
//       uint256 private _r0; uint256 private _r1; uint256 private _r2;
//       mapping(address => uint256) private _balances;
//       function balanceOf(address a) external view returns (uint256) { return _balances[a]; }
//       function decimals() external pure returns (uint8) { return 6; }
//       function transfer(address to, uint256 amount) external returns (bool) {
//           uint256 fromBal = _balances[msg.sender];
//           require(fromBal >= amount, "ERC20: transfer amount exceeds balance");
//           unchecked { _balances[msg.sender] = fromBal - amount; }
//           _balances[to] += amount;
//           return true;
//       }
//   }
const MIRROR_ERC20_CODE =
    "0x608060405234801561000f575f5ffd5b506004361061003f575f3560e01c8063313ce5671461004357806370a0823114610057578063a9059cbb1461008d575b5f5ffd5b604051600681526020015b60405180910390f35b61007f610065366004610180565b6001600160a01b03165f9081526003602052604090205490565b60405190815260200161004e565b6100a061009b3660046101a0565b6100b0565b604051901515815260200161004e565b335f90815260036020526040812054828110156101225760405162461bcd60e51b815260206004820152602660248201527f45524332303a207472616e7366657220616d6f756e7420657863656564732062604482015265616c616e636560d01b606482015260840160405180910390fd5b335f9081526003602052604080822085840390556001600160a01b0386168252812080548592906101549084906101c8565b909155506001925050505b92915050565b80356001600160a01b038116811461017b575f5ffd5b919050565b5f60208284031215610190575f5ffd5b61019982610165565b9392505050565b5f5f604083850312156101b1575f5ffd5b6101ba83610165565b946020939093013593505050565b8082018082111561015f57634e487b7160e01b5f52601160045260245ffdfea264697066735822122061f9eaedb32401c8af2759fec9149551217a5d86d56a46957598455f3c01317664736f6c634300081c0033";

Deno.test("native token mirror routes real ERC20 reads/writes through native balance", async () => {
    // The mirror config redirects SLOAD/SSTORE on the configured balance slot
    // of the mirror token to the underlying native balance. This test deploys
    // a real ERC20 at the mirror address and verifies that `balanceOf`,
    // `transfer`, and direct native writes all stay in sync through the
    // ERC20's actual bytecode.
    const token = "0x1000000000000000000000000000000000000000";
    const account = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
    const recipient = "0x000000000000000000000000000000000000dEaD";
    const slot = 3n;
    const initialNative = 123n * 10n ** 18n;
    const transferAmount = 10n * 10n ** 6n;
    const expectedInitialErc20 = 123n * 10n ** 6n;
    const expectedTransferNative = 10n * 10n ** 18n;
    const directNativeWrite = 789n * 10n ** 18n;
    const expectedAfterNativeWrite = 789n * 10n ** 6n;

    using ctx = new Context();
    using arb = ctx.createProvider({
        chain: "arb",
        chainId: 42161n,
        networkId: 42161n,
        hardfork: "cancun",
        nativeTokenMirror: { token, decimals: 6, balanceSlot: slot },
        chains: [{
            chainId: 42161n,
            hardforks: [{ blockNumber: 0, specId: "cancun" }],
        }],
        ownedAccounts: [{
            secretKey: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            balance: initialNative,
        }],
    });

    // Install a real ERC20 facade at the mirror address. Without this the
    // call paths exercised below would hit empty-code, producing no SLOAD/
    // SSTORE for the mirror hook to intercept.
    await request(arb, { method: "hardhat_setCode", params: [token, MIRROR_ERC20_CODE] });

    const balanceOfAccount = `${selector("balanceOf(address)")}${encodeAddressArg(account)}`;
    const balanceOfRecipient = `${selector("balanceOf(address)")}${encodeAddressArg(recipient)}`;

    // SLOAD in `_balances[account]` is intercepted -> returns native scaled to mirror decimals.
    assertEquals(
        decodeFirstWord(await request(arb, {
            method: "eth_call",
            params: [{ to: token, data: balanceOfAccount }, "latest"],
        })),
        expectedInitialErc20,
    );

    // `decimals()` reads from the real ERC20, no mirror involvement.
    assertEquals(
        decodeFirstWord(await request(arb, {
            method: "eth_call",
            params: [{ to: token, data: selector("decimals()") }, "latest"],
        })),
        6n,
    );

    // `transfer` SSTOREs both `_balances[from]` and `_balances[to]`; both are
    // intercepted, so the native balances of both addresses move together.
    await request(arb, {
        method: "eth_sendTransaction",
        params: [{
            from: account,
            to: token,
            data: `${selector("transfer(address,uint256)")}${encodeAddressArg(recipient)}${
                encodeUintArg(transferAmount)
            }`,
            gas: "0x186a0",
        }],
    });

    assertEquals(
        BigInt(await request(arb, {
            method: "eth_getBalance",
            params: [recipient, "latest"],
        })),
        expectedTransferNative,
    );
    assertEquals(
        decodeFirstWord(await request(arb, {
            method: "eth_call",
            params: [{ to: token, data: balanceOfRecipient }, "latest"],
        })),
        transferAmount,
    );

    // A direct native write via `hardhat_setBalance` is visible through the
    // ERC20 facade on the next read.
    await request(arb, {
        method: "hardhat_setBalance",
        params: [account, toRpcQuantity(directNativeWrite)],
    });
    assertEquals(
        decodeFirstWord(await request(arb, {
            method: "eth_call",
            params: [{ to: token, data: balanceOfAccount }, "latest"],
        })),
        expectedAfterNativeWrite,
    );
});

Deno.test("local arbitrum ArbOwnerPublic compatibility calls succeed", async () => {
    const sender = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
    const arbOwnerPublic = "0x000000000000000000000000000000000000006b";

    using ctx = new Context();
    using arb = ctx.createProvider({
        chain: "arb",
        bailOnCallFailure: true,
        bailOnTransactionFailure: true,
        chainId: 42161n,
        networkId: 42161n,
        hardfork: "cancun",
        chains: [{
            chainId: 42161n,
            hardforks: [{ blockNumber: 0, specId: "cancun" }],
        }],
        ownedAccounts: [{
            secretKey: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            balance: 1000n * 10n ** 18n,
        }],
    });

    const isChainOwner = await request(arb, {
        method: "eth_call",
        params: [{
            to: arbOwnerPublic,
            data: `${selector("isChainOwner(address)")}${encodeAddressArg(sender)}`,
        }, "latest"],
    });
    assertEquals(decodeFirstWord(isChainOwner), 0n);

    const allChainOwners = await request(arb, {
        method: "eth_call",
        params: [{
            to: arbOwnerPublic,
            data: selector("getAllChainOwners()"),
        }, "latest"],
    });
    assertEquals(decodeWord(allChainOwners, 1), 0n);

    const networkFeeAccount = await request(arb, {
        method: "eth_call",
        params: [{
            to: arbOwnerPublic,
            data: selector("getNetworkFeeAccount()"),
        }, "latest"],
    });
    assertEquals(decodeFirstWord(networkFeeAccount), 0n);

    const txHash = await request(arb, {
        method: "eth_sendTransaction",
        params: [{
            from: sender,
            to: arbOwnerPublic,
            gas: "0x186a0",
            data: `${selector("rectifyChainOwner(address)")}${encodeAddressArg(sender)}`,
        }],
    });
    const receipt = await request(arb, {
        method: "eth_getTransactionReceipt",
        params: [txHash],
    });
    assertEquals(receipt.status, "0x1");
    assertEquals(receipt.logs.length, 1);
    assertEquals(receipt.logs[0].address.toLowerCase(), arbOwnerPublic);
});

Deno.test("local ape precompile compatibility calls succeed", async () => {
    const sender = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
    const arbInfo = "0x0000000000000000000000000000000000000065";
    const arbOwnerPublic = "0x000000000000000000000000000000000000006b";
    const initialBalance = 1000n * 10n ** 18n;

    using ctx = new Context();
    using ape = ctx.createProvider({
        chain: "ape",
        bailOnCallFailure: true,
        bailOnTransactionFailure: true,
        chainId: 33139n,
        networkId: 33139n,
        hardfork: "cancun",
        chains: [{
            chainId: 33139n,
            hardforks: [{ blockNumber: 0, specId: "cancun" }],
        }],
        ownedAccounts: [{
            secretKey: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            balance: initialBalance,
        }],
    });

    const sharePrice = await request(ape, {
        method: "eth_call",
        params: [{
            to: arbOwnerPublic,
            data: selector("getSharePrice()"),
        }, "latest"],
    });
    assertEquals(decodeFirstWord(sharePrice), 1n);

    const shareCount = await request(ape, {
        method: "eth_call",
        params: [{
            to: arbOwnerPublic,
            data: selector("getShareCount()"),
        }, "latest"],
    });
    assertEquals(decodeFirstWord(shareCount), 0n);

    const balanceValues = await request(ape, {
        method: "eth_call",
        params: [{
            to: arbInfo,
            data: `${selector("getBalanceValues(address)")}${encodeAddressArg(sender)}`,
        }, "latest"],
    });
    assertEquals(decodeWord(balanceValues, 0), initialBalance);
    assertEquals(decodeWord(balanceValues, 1), 0n);
    assertEquals(decodeWord(balanceValues, 2), 0n);

    const txHash = await request(ape, {
        method: "eth_sendTransaction",
        params: [{
            from: sender,
            to: arbInfo,
            gas: "0x186a0",
            data: selector("configureAutomaticYield()"),
        }],
    });
    const receipt = await request(ape, {
        method: "eth_getTransactionReceipt",
        params: [txHash],
    });
    assertEquals(receipt.status, "0x1");
});

Deno.test("ape fork WAPE calls match remote ApeChain", async () => {
    const rpcUrl = "https://rpc.apechain.com";
    const wape = "0x48b62137edfa95a428d35c09e44256a739f6b557";
    const arbOwnerPublic = "0x000000000000000000000000000000000000006b";
    const sender = "0x0000000000000000000000000000000000000001";
    const blockNumber = await fetchRecentBlockNumber(rpcUrl);
    const blockTag = toRpcQuantity(blockNumber);
    const balanceOfData = `${selector("balanceOf(address)")}${encodeAddressArg(sender)}`;
    const withdrawZeroData = `${selector("withdraw(uint256)")}${"0".padStart(64, "0")}`;

    const remoteSharePrice = await rpcRequest(rpcUrl, {
        method: "eth_call",
        params: [{
            to: arbOwnerPublic,
            data: selector("getSharePrice()"),
        }, blockTag],
    });
    const remoteBalance = await rpcRequest(rpcUrl, {
        method: "eth_call",
        params: [{
            to: wape,
            data: balanceOfData,
        }, blockTag],
    });
    const remoteWithdraw = await rpcRequest(rpcUrl, {
        method: "eth_call",
        params: [{
            from: sender,
            to: wape,
            data: withdrawZeroData,
        }, blockTag],
    });

    using ctx = new Context();
    using ape = ctx.createProvider({
        chain: "ape",
        fork: {
            jsonRpcUrl: rpcUrl,
            blockNumber,
        },
        bailOnCallFailure: true,
        chainId: 33139n,
        networkId: 33139n,
        hardfork: "cancun",
        chains: [{
            chainId: 33139n,
            hardforks: [{ blockNumber: 0, specId: "cancun" }],
        }],
    });

    const localSharePrice = await request(ape, {
        method: "eth_call",
        params: [{
            to: arbOwnerPublic,
            data: selector("getSharePrice()"),
        }, "latest"],
    });
    assertEquals(localSharePrice, remoteSharePrice);

    const localBalance = await request(ape, {
        method: "eth_call",
        params: [{
            to: wape,
            data: balanceOfData,
        }, "latest"],
    });
    assertEquals(localBalance, remoteBalance);

    const localWithdraw = await request(ape, {
        method: "eth_call",
        params: [{
            from: sender,
            to: wape,
            data: withdrawZeroData,
        }, "latest"],
    });
    assertEquals(localWithdraw, remoteWithdraw);
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
