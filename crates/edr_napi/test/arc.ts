import { assert } from "chai";

import {
  ARC_CHAIN_TYPE,
  arcChainProviderFactory,
  ContractDecoder,
  EdrContext,
} from "..";
import {
  fundedGenesisState,
  l1ProviderConfig,
  silentLoggerConfig,
} from "./helpers";

describe("Arc chain provider", () => {
  const context = new EdrContext();

  before(async () => {
    await context.registerProviderFactory(
      ARC_CHAIN_TYPE,
      arcChainProviderFactory()
    );
  });

  it("creates and mines with an Arc hardfork configuration", async () => {
    const provider = await context.createProvider(
      ARC_CHAIN_TYPE,
      l1ProviderConfig({
        chainId: 1337n,
        networkId: 1337n,
        hardfork: "zero8",
        genesisState: fundedGenesisState(),
      }),
      silentLoggerConfig(),
      { subscriptionCallback: () => {} },
      new ContractDecoder()
    );

    await provider.handleRequest(
      JSON.stringify({ jsonrpc: "2.0", id: 1, method: "evm_mine", params: [] })
    );
    const response = await provider.handleRequest(
      JSON.stringify({
        jsonrpc: "2.0",
        id: 2,
        method: "eth_getBlockByNumber",
        params: ["0x1", false],
      })
    );
    const block = JSON.parse(response.data).result;

    assert.strictEqual(block.number, "0x1");
    assert.strictEqual((block.extraData as string).length, 18);
  });
});
