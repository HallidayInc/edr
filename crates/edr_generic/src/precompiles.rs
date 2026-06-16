use std::boxed::Box;

use alloy_primitives::Log;
use alloy_sol_types::{sol, Revert, SolCall, SolError, SolEvent, SolValue};
use edr_chain_spec_evm::{
    handler::EthPrecompiles, ContextTrait, Database, InterpreterResult, JournalTrait as _,
};
use edr_primitives::{address, keccak256, Address, Bytes, B256, U256};
use revm_context_interface::{Cfg, Transaction as _};
use revm_handler::PrecompileProvider;
use revm_interpreter::{CallInputs, Gas, InstructionResult};

use crate::{
    APE_APY_SLOT, APE_PRECOMPILE_STATE_ADDRESS, APE_SHARE_COUNT_SLOT, APE_SHARE_PRICE_SLOT,
};

const ARBSYS_ADDRESS: Address = address!("0000000000000000000000000000000000000064");
const ARBINFO_ADDRESS: Address = address!("0000000000000000000000000000000000000065");
const ARBOWNERPUBLIC_ADDRESS: Address = address!("000000000000000000000000000000000000006b");
const ARBSYS_STATE_ADDRESS: Address = address!("00000000000000000000000000000000A4B05A11");
const ADDRESS_ALIAS_OFFSET: Address = address!("1111000000000000000000000000000000001111");
const PARTIALS_SLOT_START: u64 = 2;
const APE_DEFAULT_SHARE_PRICE: u64 = 1;

sol! {
    interface ArbInfo {
        function getBalance(address account) external view returns (uint256);
        function getCode(address account) external view returns (bytes memory);
        function getBalanceValues(address account) external view returns (uint256, uint256, uint256);
        function getYieldConfiguration(address account) external view returns (uint8);
        function getDelegate(address account) external view returns (address);
        function configureAutomaticYield() external;
        function configureVoidYield() external;
        function configureDelegateYield(address account) external;
    }

    interface ArbSys {
        function arbBlockNumber() external view returns (uint256);
        function arbBlockHash(uint256 arbBlockNum) external view returns (bytes32);
        function arbChainID() external view returns (uint256);
        function arbOSVersion() external view returns (uint256);
        function getStorageGasAvailable() external view returns (uint256);
        function isTopLevelCall() external view returns (bool);
        function mapL1SenderContractAddressToL2Alias(address sender, address unused) external pure returns (address);
        function wasMyCallersAddressAliased() external view returns (bool);
        function myCallersAddressWithoutAliasing() external view returns (address);
        function withdrawEth(address destination) external payable returns (uint256);
        function sendTxToL1(address destination, bytes calldata data) external payable returns (uint256);
        function sendMerkleTreeState() external view returns (uint256 size, bytes32 root, bytes32[] memory partials);

        event L2ToL1Tx(
            address caller,
            address indexed destination,
            uint256 indexed hash,
            uint256 indexed position,
            uint256 arbBlockNum,
            uint256 ethBlockNum,
            uint256 timestamp,
            uint256 callvalue,
            bytes data
        );

        event SendMerkleUpdate(
            uint256 indexed reserved,
            bytes32 indexed hash,
            uint256 indexed position
        );

        error InvalidBlockNumber(uint256 requested, uint256 current);
    }

    interface ArbOwnerPublic {
        function isChainOwner(address addr) external view returns (bool);
        function rectifyChainOwner(address ownerToRectify) external;
        function getAllChainOwners() external view returns (address[] memory);
        function getNativeTokenManagementFrom() external view returns (uint64);
        function isNativeTokenOwner(address addr) external view returns (bool);
        function getAllNativeTokenOwners() external view returns (address[] memory);
        function getTransactionFilteringFrom() external view returns (uint64);
        function isTransactionFilterer(address filterer) external view returns (bool);
        function getAllTransactionFilterers() external view returns (address[] memory);
        function getFilteredFundsRecipient() external view returns (address);
        function getNetworkFeeAccount() external view returns (address);
        function getInfraFeeAccount() external view returns (address);
        function getBrotliCompressionLevel() external view returns (uint64);
        function getParentGasFloorPerToken() external view returns (uint64);
        function getScheduledUpgrade() external view returns (uint64 arbosVersion, uint64 scheduledForTimestamp);
        function isCalldataPriceIncreaseEnabled() external view returns (bool);
        function getCollectTips() external view returns (bool);
        function getMaxStylusContractFragments() external view returns (uint8);
        function getSharePrice() external view returns (uint64);
        function getShareCount() external view returns (uint256);
        function getApy() external view returns (uint64);

        event ChainOwnerRectified(address rectifiedOwner);
    }
}

use self::{
    ArbInfo::{
        configureAutomaticYieldCall, configureDelegateYieldCall, configureVoidYieldCall,
        getBalanceCall, getBalanceValuesCall, getCodeCall, getDelegateCall,
        getYieldConfigurationCall,
    },
    ArbOwnerPublic::{
        getAllChainOwnersCall, getAllNativeTokenOwnersCall, getAllTransactionFilterersCall,
        getApyCall, getBrotliCompressionLevelCall, getCollectTipsCall,
        getFilteredFundsRecipientCall, getInfraFeeAccountCall, getMaxStylusContractFragmentsCall,
        getNativeTokenManagementFromCall, getNetworkFeeAccountCall, getParentGasFloorPerTokenCall,
        getScheduledUpgradeCall, getScheduledUpgradeReturn, getShareCountCall, getSharePriceCall,
        getTransactionFilteringFromCall, isCalldataPriceIncreaseEnabledCall, isChainOwnerCall,
        isNativeTokenOwnerCall, isTransactionFiltererCall, rectifyChainOwnerCall,
        ChainOwnerRectified,
    },
    ArbSys::{
        arbBlockHashCall, arbBlockNumberCall, arbChainIDCall, arbOSVersionCall,
        getStorageGasAvailableCall, isTopLevelCallCall, mapL1SenderContractAddressToL2AliasCall,
        myCallersAddressWithoutAliasingCall, sendMerkleTreeStateCall, sendMerkleTreeStateReturn,
        sendTxToL1Call, wasMyCallersAddressAliasedCall, withdrawEthCall, InvalidBlockNumber,
        L2ToL1Tx, SendMerkleUpdate,
    },
};

/// Tempo's stablecoin DEX precompile address.
const TEMPO_DEX_ADDRESS: Address = address!("dec0000000000000000000000000000000000000");

sol! {
    interface Tip20 {
        function balanceOf(address account) external view returns (uint256);
        function transfer(address to, uint256 amount) external returns (bool);
        function transferFrom(address from, address to, uint256 amount) external returns (bool);
        function approve(address spender, uint256 amount) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
        function decimals() external view returns (uint8);
        function totalSupply() external view returns (uint256);
    }

    interface TempoDexAbi {
        function quoteSwapExactAmountIn(address tokenIn, address tokenOut, uint128 amountIn) external view returns (uint128);
        function quoteSwapExactAmountOut(address tokenIn, address tokenOut, uint128 amountOut) external view returns (uint128);
        function swapExactAmountIn(address tokenIn, address tokenOut, uint128 amountIn, uint128 minAmountOut) external returns (uint128);
        function swapExactAmountOut(address tokenIn, address tokenOut, uint128 amountOut, uint128 maxAmountIn) external returns (uint128);
    }
}

use self::{
    Tip20::{
        allowanceCall, approveCall, balanceOfCall, decimalsCall, totalSupplyCall, transferCall,
        transferFromCall,
    },
    TempoDexAbi::{
        quoteSwapExactAmountInCall, quoteSwapExactAmountOutCall, swapExactAmountInCall,
        swapExactAmountOutCall,
    },
};

/// Tempo's TIP-20 tokens are predeployed at addresses prefixed with `0x20c0`.
fn is_tip20(address: &Address) -> bool {
    let bytes = address.as_slice();
    bytes[0] == 0x20 && bytes[1] == 0xc0
}

/// Storage slot of `balances[holder]` for a standard `mapping(address => uint256)` at slot 0.
fn tip20_balance_slot(holder: Address) -> U256 {
    let mut buf = [0u8; 64];
    buf[12..32].copy_from_slice(holder.as_slice());
    U256::from_be_bytes(keccak256(buf).0)
}

/// Storage slot of `allowances[owner][spender]` for `mapping(address => mapping(address => uint256))` at slot 1.
fn tip20_allowance_slot(owner: Address, spender: Address) -> U256 {
    let mut inner = [0u8; 64];
    inner[12..32].copy_from_slice(owner.as_slice());
    inner[63] = 1;
    let inner_hash = keccak256(inner);
    let mut outer = [0u8; 64];
    outer[12..32].copy_from_slice(spender.as_slice());
    outer[32..64].copy_from_slice(inner_hash.as_slice());
    U256::from_be_bytes(keccak256(outer).0)
}

/// Arbitrum precompile provider.
///
/// This keeps Ethereum's built-in precompiles and layers in Arbitrum's Nitro
/// compatibility precompiles.
#[derive(Debug, Clone, Default)]
pub struct ArbPrecompiles {
    inner: EthPrecompiles,
}

/// ApeChain precompile provider.
///
/// This wraps the Arbitrum provider and adds ApeChain's custom selectors to
/// the standard Nitro precompile addresses.
#[derive(Debug, Clone, Default)]
pub struct ApePrecompiles {
    inner: ArbPrecompiles,
}

impl<ContextT> PrecompileProvider<ContextT> for ArbPrecompiles
where
    ContextT: ContextTrait,
{
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: <ContextT::Cfg as Cfg>::Spec) -> bool {
        <EthPrecompiles as PrecompileProvider<ContextT>>::set_spec(&mut self.inner, spec)
    }

    fn run(
        &mut self,
        context: &mut ContextT,
        inputs: &CallInputs,
    ) -> Result<Option<Self::Output>, String> {
        if inputs.bytecode_address == ARBSYS_ADDRESS {
            return run_arbsys(context, inputs).map(Some);
        }

        if inputs.bytecode_address == ARBINFO_ADDRESS {
            return run_arbinfo(context, inputs).map(Some);
        }

        if inputs.bytecode_address == ARBOWNERPUBLIC_ADDRESS {
            return run_arb_owner_public(context, inputs).map(Some);
        }

        self.inner.run(context, inputs)
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        Box::new(self.inner.warm_addresses().chain([
            ARBSYS_ADDRESS,
            ARBINFO_ADDRESS,
            ARBOWNERPUBLIC_ADDRESS,
        ]))
    }

    fn contains(&self, address: &Address) -> bool {
        *address == ARBSYS_ADDRESS
            || *address == ARBINFO_ADDRESS
            || *address == ARBOWNERPUBLIC_ADDRESS
            || self.inner.contains(address)
    }
}

impl<ContextT> PrecompileProvider<ContextT> for ApePrecompiles
where
    ContextT: ContextTrait,
{
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: <ContextT::Cfg as Cfg>::Spec) -> bool {
        <ArbPrecompiles as PrecompileProvider<ContextT>>::set_spec(&mut self.inner, spec)
    }

    fn run(
        &mut self,
        context: &mut ContextT,
        inputs: &CallInputs,
    ) -> Result<Option<Self::Output>, String> {
        if inputs.bytecode_address == ARBINFO_ADDRESS {
            return run_ape_arbinfo(context, inputs).map(Some);
        }

        if inputs.bytecode_address == ARBOWNERPUBLIC_ADDRESS {
            return run_ape_arb_owner_public(context, inputs).map(Some);
        }

        self.inner.run(context, inputs)
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        <ArbPrecompiles as PrecompileProvider<ContextT>>::warm_addresses(&self.inner)
    }

    fn contains(&self, address: &Address) -> bool {
        <ArbPrecompiles as PrecompileProvider<ContextT>>::contains(&self.inner, address)
    }
}

/// Tempo precompile provider.
///
/// Tempo's TIP-20 tokens (`0x20c0…`) and stablecoin DEX (`0xdec0`) are
/// node-native precompiles whose on-chain code is a `0xef` placeholder, so a
/// standard EVM fork cannot execute them. This models them for fork simulation:
/// TIP-20s as storage-backed ERC-20s (balances at the slot-0 mapping) and the
/// DEX as a 1:1 stablecoin swap that moves those balances.
#[derive(Debug, Clone, Default)]
pub struct TempoPrecompiles {
    inner: EthPrecompiles,
}

impl<ContextT> PrecompileProvider<ContextT> for TempoPrecompiles
where
    ContextT: ContextTrait,
{
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: <ContextT::Cfg as Cfg>::Spec) -> bool {
        <EthPrecompiles as PrecompileProvider<ContextT>>::set_spec(&mut self.inner, spec)
    }

    fn run(
        &mut self,
        context: &mut ContextT,
        inputs: &CallInputs,
    ) -> Result<Option<Self::Output>, String> {
        if inputs.bytecode_address == TEMPO_DEX_ADDRESS {
            return run_tempo_dex(context, inputs).map(Some);
        }

        if is_tip20(&inputs.bytecode_address) {
            return run_tip20(context, inputs).map(Some);
        }

        self.inner.run(context, inputs)
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        Box::new(self.inner.warm_addresses().chain([TEMPO_DEX_ADDRESS]))
    }

    fn contains(&self, address: &Address) -> bool {
        *address == TEMPO_DEX_ADDRESS || is_tip20(address) || self.inner.contains(address)
    }
}

/// Move `amount` of TIP-20 `token` from `from` to `to`. Returns `Ok(false)` if
/// `from`'s balance is insufficient (the caller should revert).
fn tip20_move<ContextT>(
    context: &mut ContextT,
    token: Address,
    from: Address,
    to: Address,
    amount: U256,
) -> Result<bool, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let from_slot = tip20_balance_slot(from);
    let from_balance = read_word_at(context, token, from_slot)?;
    if from_balance < amount {
        return Ok(false);
    }
    write_word_at(context, token, from_slot, from_balance - amount)?;

    let to_slot = tip20_balance_slot(to);
    let to_balance = read_word_at(context, token, to_slot)?;
    write_word_at(context, token, to_slot, to_balance.saturating_add(amount))?;

    Ok(true)
}

fn run_tip20<ContextT>(
    context: &mut ContextT,
    inputs: &CallInputs,
) -> Result<InterpreterResult, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let token = inputs.bytecode_address;
    let calldata = inputs.input.bytes(context);
    let calldata = calldata.as_ref();

    if calldata.len() < 4 {
        return Ok(revert_with_message(inputs, "TIP20: missing selector"));
    }

    let selector = &calldata[..4];

    if selector == balanceOfCall::SELECTOR {
        let Ok(call) = balanceOfCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "TIP20: invalid calldata"));
        };
        let balance = read_word_at(context, token, tip20_balance_slot(call.account))?;
        return Ok(success(inputs, balance.abi_encode()));
    }

    if selector == decimalsCall::SELECTOR {
        return Ok(success(inputs, U256::from(6).abi_encode()));
    }

    if selector == totalSupplyCall::SELECTOR {
        return Ok(success(inputs, U256::ZERO.abi_encode()));
    }

    if selector == allowanceCall::SELECTOR {
        let Ok(call) = allowanceCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "TIP20: invalid calldata"));
        };
        let value = read_word_at(context, token, tip20_allowance_slot(call.owner, call.spender))?;
        return Ok(success(inputs, value.abi_encode()));
    }

    if selector == approveCall::SELECTOR {
        let Ok(call) = approveCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "TIP20: invalid calldata"));
        };
        write_word_at(
            context,
            token,
            tip20_allowance_slot(inputs.caller, call.spender),
            call.amount,
        )?;
        return Ok(success(inputs, true.abi_encode()));
    }

    if selector == transferCall::SELECTOR {
        let Ok(call) = transferCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "TIP20: invalid calldata"));
        };
        if !tip20_move(context, token, inputs.caller, call.to, call.amount)? {
            return Ok(revert_with_message(inputs, "TIP20: insufficient balance"));
        }
        return Ok(success(inputs, true.abi_encode()));
    }

    if selector == transferFromCall::SELECTOR {
        let Ok(call) = transferFromCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "TIP20: invalid calldata"));
        };
        let allowance_slot = tip20_allowance_slot(call.from, inputs.caller);
        let allowed = read_word_at(context, token, allowance_slot)?;
        if allowed < call.amount {
            return Ok(revert_with_message(inputs, "TIP20: insufficient allowance"));
        }
        if allowed != U256::MAX {
            write_word_at(context, token, allowance_slot, allowed - call.amount)?;
        }
        if !tip20_move(context, token, call.from, call.to, call.amount)? {
            return Ok(revert_with_message(inputs, "TIP20: insufficient balance"));
        }
        return Ok(success(inputs, true.abi_encode()));
    }

    Ok(revert_with_message(inputs, "TIP20: unknown selector"))
}

fn run_tempo_dex<ContextT>(
    context: &mut ContextT,
    inputs: &CallInputs,
) -> Result<InterpreterResult, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let calldata = inputs.input.bytes(context);
    let calldata = calldata.as_ref();

    if calldata.len() < 4 {
        return Ok(revert_with_message(inputs, "TempoDex: missing selector"));
    }

    let selector = &calldata[..4];

    // 1:1 quotes for same-decimal stablecoins.
    if selector == quoteSwapExactAmountInCall::SELECTOR {
        let Ok(call) = quoteSwapExactAmountInCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "TempoDex: invalid calldata"));
        };
        return Ok(success(inputs, call.amountIn.abi_encode()));
    }

    if selector == quoteSwapExactAmountOutCall::SELECTOR {
        let Ok(call) = quoteSwapExactAmountOutCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "TempoDex: invalid calldata"));
        };
        return Ok(success(inputs, call.amountOut.abi_encode()));
    }

    if selector == swapExactAmountInCall::SELECTOR {
        let Ok(call) = swapExactAmountInCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "TempoDex: invalid calldata"));
        };
        if call.amountIn < call.minAmountOut {
            return Ok(revert_with_message(inputs, "TempoDex: insufficient output"));
        }
        let amount = U256::from(call.amountIn);
        if !tempo_dex_swap(context, call.tokenIn, call.tokenOut, inputs.caller, amount)? {
            return Ok(revert_with_message(inputs, "TempoDex: insufficient input"));
        }
        return Ok(success(inputs, call.amountIn.abi_encode()));
    }

    if selector == swapExactAmountOutCall::SELECTOR {
        let Ok(call) = swapExactAmountOutCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "TempoDex: invalid calldata"));
        };
        if call.amountOut > call.maxAmountIn {
            return Ok(revert_with_message(inputs, "TempoDex: excessive input"));
        }
        let amount = U256::from(call.amountOut);
        if !tempo_dex_swap(context, call.tokenIn, call.tokenOut, inputs.caller, amount)? {
            return Ok(revert_with_message(inputs, "TempoDex: insufficient input"));
        }
        return Ok(success(inputs, call.amountOut.abi_encode()));
    }

    Ok(revert_with_message(inputs, "TempoDex: unknown selector"))
}

/// Debit `amount` of `token_in` from `account` and credit the same amount of
/// `token_out` (1:1). Returns `Ok(false)` if the input balance is insufficient.
fn tempo_dex_swap<ContextT>(
    context: &mut ContextT,
    token_in: Address,
    token_out: Address,
    account: Address,
    amount: U256,
) -> Result<bool, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let slot = tip20_balance_slot(account);
    let in_balance = read_word_at(context, token_in, slot)?;
    if in_balance < amount {
        return Ok(false);
    }
    write_word_at(context, token_in, slot, in_balance - amount)?;

    let out_balance = read_word_at(context, token_out, slot)?;
    write_word_at(context, token_out, slot, out_balance.saturating_add(amount))?;

    Ok(true)
}

fn run_ape_arbinfo<ContextT>(
    context: &mut ContextT,
    inputs: &CallInputs,
) -> Result<InterpreterResult, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let calldata = inputs.input.bytes(context);
    let calldata = calldata.as_ref();

    if calldata.len() < 4 {
        return Ok(revert_with_message(inputs, "ArbInfo: missing selector"));
    }

    let selector = &calldata[..4];

    if selector == getBalanceValuesCall::SELECTOR {
        let Ok(call) = getBalanceValuesCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "ArbInfo: invalid calldata"));
        };

        let balance = load_account_balance(context, call.account)?;
        return Ok(success(
            inputs,
            (balance, U256::ZERO, U256::ZERO).abi_encode(),
        ));
    }

    if selector == getYieldConfigurationCall::SELECTOR {
        if getYieldConfigurationCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbInfo: invalid calldata"));
        }

        return Ok(success(inputs, 0u64.abi_encode()));
    }

    if selector == getDelegateCall::SELECTOR {
        if getDelegateCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbInfo: invalid calldata"));
        }

        return Ok(success(inputs, Address::ZERO.abi_encode()));
    }

    if selector == configureAutomaticYieldCall::SELECTOR {
        if configureAutomaticYieldCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbInfo: invalid calldata"));
        }

        if inputs.is_static {
            return Ok(revert_with_message(
                inputs,
                "ArbInfo: state-changing call in static context",
            ));
        }

        return Ok(success(inputs, Vec::new()));
    }

    if selector == configureVoidYieldCall::SELECTOR {
        if configureVoidYieldCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbInfo: invalid calldata"));
        }

        if inputs.is_static {
            return Ok(revert_with_message(
                inputs,
                "ArbInfo: state-changing call in static context",
            ));
        }

        return Ok(success(inputs, Vec::new()));
    }

    if selector == configureDelegateYieldCall::SELECTOR {
        if configureDelegateYieldCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbInfo: invalid calldata"));
        }

        if inputs.is_static {
            return Ok(revert_with_message(
                inputs,
                "ArbInfo: state-changing call in static context",
            ));
        }

        return Ok(success(inputs, Vec::new()));
    }

    run_arbinfo(context, inputs)
}

fn run_ape_arb_owner_public<ContextT>(
    context: &mut ContextT,
    inputs: &CallInputs,
) -> Result<InterpreterResult, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let calldata = inputs.input.bytes(context);
    let calldata = calldata.as_ref();

    if calldata.len() < 4 {
        return Ok(revert_with_message(
            inputs,
            "ArbOwnerPublic: missing selector",
        ));
    }

    let selector = &calldata[..4];

    if selector == getSharePriceCall::SELECTOR {
        if getSharePriceCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, ape_share_price(context)?.abi_encode()));
    }

    if selector == getShareCountCall::SELECTOR {
        if getShareCountCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        let share_count = read_ape_state_word(context, APE_SHARE_COUNT_SLOT)?;
        return Ok(success(inputs, share_count.abi_encode()));
    }

    if selector == getApyCall::SELECTOR {
        if getApyCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        let apy = read_ape_state_word(context, APE_APY_SLOT)?;
        let apy: u64 = apy
            .try_into()
            .map_err(|_| "ArbOwnerPublic: invalid ApeChain APY".to_string())?;

        return Ok(success(inputs, apy.abi_encode()));
    }

    run_arb_owner_public(context, inputs)
}

fn run_arbinfo<ContextT>(
    context: &mut ContextT,
    inputs: &CallInputs,
) -> Result<InterpreterResult, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let calldata = inputs.input.bytes(context);
    let calldata = calldata.as_ref();

    if calldata.len() < 4 {
        return Ok(revert_with_message(inputs, "ArbInfo: missing selector"));
    }

    let selector = &calldata[..4];

    if selector == getBalanceCall::SELECTOR {
        let Ok(call) = getBalanceCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "ArbInfo: invalid calldata"));
        };

        let balance = load_account_balance(context, call.account)?;

        return Ok(success(inputs, balance.abi_encode()));
    }

    if selector == getCodeCall::SELECTOR {
        let Ok(call) = getCodeCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "ArbInfo: invalid calldata"));
        };

        let code = load_account_code(context, call.account)?;

        return Ok(success(inputs, code.abi_encode()));
    }

    Ok(revert_with_message(inputs, "ArbInfo: unknown selector"))
}

fn run_arbsys<ContextT>(
    context: &mut ContextT,
    inputs: &CallInputs,
) -> Result<InterpreterResult, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let calldata = inputs.input.bytes(context);
    let calldata = calldata.as_ref();

    if calldata.len() < 4 {
        return Ok(revert_with_message(inputs, "ArbSys: missing selector"));
    }

    let selector = &calldata[..4];

    if selector == arbBlockNumberCall::SELECTOR {
        if arbBlockNumberCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        }

        return Ok(success(inputs, context.block_number().abi_encode()));
    }

    if selector == arbBlockHashCall::SELECTOR {
        let Ok(call) = arbBlockHashCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        };

        let requested = call.arbBlockNum;
        let current = context.block_number();
        let max_delta = U256::from(256);

        if requested >= current || requested.saturating_add(max_delta) < current {
            return Ok(revert(
                inputs,
                InvalidBlockNumber { requested, current }.abi_encode(),
            ));
        }

        let Ok(requested_block_num) = requested.try_into() else {
            return Ok(revert(
                inputs,
                InvalidBlockNumber { requested, current }.abi_encode(),
            ));
        };

        let block_hash = context
            .block_hash(requested_block_num)
            .unwrap_or(B256::ZERO);
        return Ok(success(inputs, block_hash.abi_encode()));
    }

    if selector == arbChainIDCall::SELECTOR {
        if arbChainIDCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        }

        return Ok(success(inputs, context.chain_id().abi_encode()));
    }

    if selector == arbOSVersionCall::SELECTOR {
        if arbOSVersionCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        }

        return Ok(success(inputs, U256::from(56).abi_encode()));
    }

    if selector == getStorageGasAvailableCall::SELECTOR {
        if getStorageGasAvailableCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        }

        return Ok(success(inputs, U256::ZERO.abi_encode()));
    }

    if selector == isTopLevelCallCall::SELECTOR {
        if isTopLevelCallCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        }

        return Ok(success(
            inputs,
            (context.journal().depth() <= 2).abi_encode(),
        ));
    }

    if selector == mapL1SenderContractAddressToL2AliasCall::SELECTOR {
        let Ok(call) = mapL1SenderContractAddressToL2AliasCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        };

        return Ok(success(inputs, remap_l1_address(call.sender).abi_encode()));
    }

    if selector == wasMyCallersAddressAliasedCall::SELECTOR {
        if wasMyCallersAddressAliasedCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        }

        return Ok(success(inputs, false.abi_encode()));
    }

    if selector == myCallersAddressWithoutAliasingCall::SELECTOR {
        if myCallersAddressWithoutAliasingCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        }

        let caller = if context.journal().depth() <= 1 {
            Address::ZERO
        } else {
            context.tx().caller()
        };
        return Ok(success(inputs, caller.abi_encode()));
    }

    if selector == sendMerkleTreeStateCall::SELECTOR {
        if sendMerkleTreeStateCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        }

        let size = read_word(context, send_count_slot())?;
        let partials = read_partials(context, size)?;
        let root = merkle_root_from_partials(&partials);

        return Ok(success(
            inputs,
            sendMerkleTreeStateCall::abi_encode_returns(&sendMerkleTreeStateReturn {
                size,
                root,
                partials,
            }),
        ));
    }

    if selector == withdrawEthCall::SELECTOR {
        let Ok(call) = withdrawEthCall::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        };

        return run_send_tx_to_l1(context, inputs, call.destination, Bytes::new());
    }

    if selector == sendTxToL1Call::SELECTOR {
        let Ok(call) = sendTxToL1Call::abi_decode(calldata) else {
            return Ok(revert_with_message(inputs, "ArbSys: invalid calldata"));
        };

        return run_send_tx_to_l1(context, inputs, call.destination, call.data);
    }

    Ok(revert_with_message(inputs, "ArbSys: unknown selector"))
}

fn run_arb_owner_public<ContextT>(
    context: &mut ContextT,
    inputs: &CallInputs,
) -> Result<InterpreterResult, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let calldata = inputs.input.bytes(context);
    let calldata = calldata.as_ref();

    if calldata.len() < 4 {
        return Ok(revert_with_message(
            inputs,
            "ArbOwnerPublic: missing selector",
        ));
    }

    let selector = &calldata[..4];

    if selector == isChainOwnerCall::SELECTOR {
        if isChainOwnerCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, false.abi_encode()));
    }

    if selector == rectifyChainOwnerCall::SELECTOR {
        let Ok(call) = rectifyChainOwnerCall::abi_decode(calldata) else {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        };

        if inputs.is_static {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: state-changing call in static context",
            ));
        }

        // EDR doesn't model Nitro's owner/admin config yet; accept the call so
        // forked apps probing this precompile don't revert.
        context.journal_mut().log(Log {
            address: ARBOWNERPUBLIC_ADDRESS,
            data: ChainOwnerRectified {
                rectifiedOwner: call.ownerToRectify,
            }
            .encode_log_data(),
        });

        return Ok(success(inputs, Vec::new()));
    }

    if selector == getAllChainOwnersCall::SELECTOR {
        if getAllChainOwnersCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, Vec::<Address>::new().abi_encode()));
    }

    if selector == getNativeTokenManagementFromCall::SELECTOR {
        if getNativeTokenManagementFromCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, 0u64.abi_encode()));
    }

    if selector == isNativeTokenOwnerCall::SELECTOR {
        if isNativeTokenOwnerCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, false.abi_encode()));
    }

    if selector == getAllNativeTokenOwnersCall::SELECTOR {
        if getAllNativeTokenOwnersCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, Vec::<Address>::new().abi_encode()));
    }

    if selector == getTransactionFilteringFromCall::SELECTOR {
        if getTransactionFilteringFromCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, 0u64.abi_encode()));
    }

    if selector == isTransactionFiltererCall::SELECTOR {
        if isTransactionFiltererCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, false.abi_encode()));
    }

    if selector == getAllTransactionFilterersCall::SELECTOR {
        if getAllTransactionFilterersCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, Vec::<Address>::new().abi_encode()));
    }

    if selector == getFilteredFundsRecipientCall::SELECTOR {
        if getFilteredFundsRecipientCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, Address::ZERO.abi_encode()));
    }

    if selector == getNetworkFeeAccountCall::SELECTOR {
        if getNetworkFeeAccountCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, Address::ZERO.abi_encode()));
    }

    if selector == getInfraFeeAccountCall::SELECTOR {
        if getInfraFeeAccountCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, Address::ZERO.abi_encode()));
    }

    if selector == getBrotliCompressionLevelCall::SELECTOR {
        if getBrotliCompressionLevelCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, 0u64.abi_encode()));
    }

    if selector == getParentGasFloorPerTokenCall::SELECTOR {
        if getParentGasFloorPerTokenCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, 0u64.abi_encode()));
    }

    if selector == getScheduledUpgradeCall::SELECTOR {
        if getScheduledUpgradeCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(
            inputs,
            getScheduledUpgradeCall::abi_encode_returns(&getScheduledUpgradeReturn {
                arbosVersion: 0,
                scheduledForTimestamp: 0,
            }),
        ));
    }

    if selector == isCalldataPriceIncreaseEnabledCall::SELECTOR {
        if isCalldataPriceIncreaseEnabledCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, false.abi_encode()));
    }

    if selector == getCollectTipsCall::SELECTOR {
        if getCollectTipsCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, false.abi_encode()));
    }

    if selector == getMaxStylusContractFragmentsCall::SELECTOR {
        if getMaxStylusContractFragmentsCall::abi_decode(calldata).is_err() {
            return Ok(revert_with_message(
                inputs,
                "ArbOwnerPublic: invalid calldata",
            ));
        }

        return Ok(success(inputs, 0u64.abi_encode()));
    }

    Ok(revert_with_message(
        inputs,
        "ArbOwnerPublic: unknown selector",
    ))
}

fn run_send_tx_to_l1<ContextT>(
    context: &mut ContextT,
    inputs: &CallInputs,
    destination: Address,
    data: Bytes,
) -> Result<InterpreterResult, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    if inputs.is_static {
        return Ok(revert_with_message(
            inputs,
            "ArbSys: state-changing call in static context",
        ));
    }

    let position = read_word(context, send_count_slot())?;
    let callvalue = inputs.transfer_value().unwrap_or(U256::ZERO);

    if callvalue > U256::ZERO {
        let mut account = context
            .journal_mut()
            .load_account_mut(inputs.transfer_to())
            .map_err(|error| error.to_string())?;

        if !account.data.decr_balance(callvalue) {
            return Ok(revert_with_message(
                inputs,
                "ArbSys: insufficient transferred value",
            ));
        }
    }

    let arb_block_num = context.block_number();
    // EDR doesn't currently carry Arbitrum's parent-L1 block number separately,
    // so expose the current block number until the broader Nitro model lands.
    let eth_block_num = arb_block_num;
    let timestamp = context.timestamp();
    let send_hash = compute_send_hash(
        inputs.caller,
        destination,
        arb_block_num,
        eth_block_num,
        timestamp,
        callvalue,
        &data,
    );

    let new_size = position + U256::from(1);
    let partials = append_partial(context, position, send_hash)?;
    let root = merkle_root_from_partials(&partials);

    write_word(context, send_count_slot(), new_size)?;
    write_hash(context, root_slot(), root)?;

    context.journal_mut().log(Log {
        address: ARBSYS_ADDRESS,
        data: L2ToL1Tx {
            caller: inputs.caller,
            destination,
            hash: U256::from_be_slice(send_hash.as_slice()),
            position,
            arbBlockNum: arb_block_num,
            ethBlockNum: eth_block_num,
            timestamp,
            callvalue,
            data: data.clone(),
        }
        .encode_log_data(),
    });

    context.journal_mut().log(Log {
        address: ARBSYS_ADDRESS,
        data: SendMerkleUpdate {
            reserved: U256::ZERO,
            hash: root,
            position,
        }
        .encode_log_data(),
    });

    Ok(success(inputs, position.abi_encode()))
}

fn remap_l1_address(sender: Address) -> Address {
    let remapped = U256::from_be_slice(sender.as_slice())
        .saturating_add(U256::from_be_slice(ADDRESS_ALIAS_OFFSET.as_slice()));

    let remapped = B256::from(remapped);
    Address::from_slice(&remapped.as_slice()[12..])
}

fn compute_send_hash(
    caller: Address,
    destination: Address,
    arb_block_num: U256,
    eth_block_num: U256,
    timestamp: U256,
    callvalue: U256,
    data: &[u8],
) -> B256 {
    let mut preimage = Vec::with_capacity(20 + 20 + (32 * 4) + data.len());
    preimage.extend_from_slice(caller.as_slice());
    preimage.extend_from_slice(destination.as_slice());
    preimage.extend_from_slice(&arb_block_num.to_be_bytes::<32>());
    preimage.extend_from_slice(&eth_block_num.to_be_bytes::<32>());
    preimage.extend_from_slice(&timestamp.to_be_bytes::<32>());
    preimage.extend_from_slice(&callvalue.to_be_bytes::<32>());
    preimage.extend_from_slice(data);

    keccak256(preimage)
}

fn append_partial<ContextT>(
    context: &mut ContextT,
    position: U256,
    leaf_hash: B256,
) -> Result<Vec<B256>, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let mut level = 0usize;
    let mut bitmap = position;
    let mut carry = leaf_hash;

    loop {
        if bitmap & U256::from(1) == U256::ZERO {
            write_hash(context, partial_slot(level), carry)?;
            break;
        }

        let left = read_hash(context, partial_slot(level))?;
        write_word(context, partial_slot(level), U256::ZERO)?;
        carry = keccak256([left.as_slice(), carry.as_slice()].concat());

        bitmap >>= 1;
        level += 1;
    }

    read_partials(context, position + U256::from(1))
}

fn read_partials<ContextT>(context: &mut ContextT, size: U256) -> Result<Vec<B256>, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let mut partials = Vec::new();
    let mut bitmap = size;
    let mut level = 0usize;

    while bitmap > U256::ZERO {
        if bitmap & U256::from(1) == U256::from(1) {
            partials.push(read_hash(context, partial_slot(level))?);
        }

        bitmap >>= 1;
        level += 1;
    }

    Ok(partials)
}

fn merkle_root_from_partials(partials: &[B256]) -> B256 {
    let Some((first, rest)) = partials.split_first() else {
        return B256::ZERO;
    };

    rest.iter().copied().fold(*first, |root, partial| {
        keccak256([partial.as_slice(), root.as_slice()].concat())
    })
}

fn send_count_slot() -> U256 {
    U256::ZERO
}

fn root_slot() -> U256 {
    U256::from(1)
}

fn partial_slot(level: usize) -> U256 {
    U256::from(PARTIALS_SLOT_START + level as u64)
}

fn load_account_balance<ContextT>(context: &mut ContextT, address: Address) -> Result<U256, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let account = context
        .journal_mut()
        .load_account(address)
        .map_err(|error| error.to_string())?;

    Ok(if account.info.exists() {
        account.info.balance
    } else {
        U256::ZERO
    })
}

fn ape_share_price<ContextT>(context: &mut ContextT) -> Result<u64, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let share_price = read_ape_state_word(context, APE_SHARE_PRICE_SLOT)?;
    if share_price == U256::ZERO {
        return Ok(APE_DEFAULT_SHARE_PRICE);
    }

    share_price
        .try_into()
        .map_err(|_| "ArbOwnerPublic: invalid ApeChain share price".to_string())
}

fn load_account_code<ContextT>(context: &mut ContextT, address: Address) -> Result<Bytes, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let mut account_info = context
        .journal_mut()
        .load_account(address)
        .map_err(|error| error.to_string())?
        .info
        .clone();

    if !account_info.exists() {
        return Ok(Bytes::new());
    }

    if let Some(code) = account_info.code.take() {
        return Ok(code.original_bytes());
    }

    context
        .journal_mut()
        .db_mut()
        .code_by_hash(account_info.code_hash)
        .map(|bytecode| bytecode.original_bytes())
        .map_err(|error| error.to_string())
}

fn read_ape_state_word<ContextT>(context: &mut ContextT, word_index: u64) -> Result<U256, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let code = load_account_code(context, APE_PRECOMPILE_STATE_ADDRESS)?;
    let offset = word_index as usize * 32;
    let end = offset + 32;

    if code.len() < end {
        return Ok(U256::ZERO);
    }

    Ok(U256::from_be_slice(&code[offset..end]))
}

fn read_hash<ContextT>(context: &mut ContextT, slot: U256) -> Result<B256, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    read_word(context, slot).map(B256::from)
}

fn write_hash<ContextT>(context: &mut ContextT, slot: U256, value: B256) -> Result<(), String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    write_word(context, slot, U256::from_be_slice(value.as_slice()))
}

fn read_word<ContextT>(context: &mut ContextT, slot: U256) -> Result<U256, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    read_word_at(context, ARBSYS_STATE_ADDRESS, slot)
}

fn read_word_at<ContextT>(
    context: &mut ContextT,
    address: Address,
    slot: U256,
) -> Result<U256, String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    context
        .journal_mut()
        .load_account(address)
        .map_err(|error| error.to_string())?;

    context
        .journal_mut()
        .sload(address, slot)
        .map(|value| value.data)
        .map_err(|error| error.to_string())
}

fn write_word<ContextT>(context: &mut ContextT, slot: U256, value: U256) -> Result<(), String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    write_word_at(context, ARBSYS_STATE_ADDRESS, slot, value)
}

fn write_word_at<ContextT>(
    context: &mut ContextT,
    address: Address,
    slot: U256,
    value: U256,
) -> Result<(), String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let mut account = context
        .journal_mut()
        .load_account_mut(address)
        .map_err(|error| error.to_string())?;
    account.data.touch();
    if account.data.nonce() == 0 {
        account.data.set_nonce(1);
    }

    context
        .journal_mut()
        .sstore(address, slot, value)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn success(inputs: &CallInputs, output: Vec<u8>) -> InterpreterResult {
    InterpreterResult {
        result: InstructionResult::Return,
        gas: Gas::new(inputs.gas_limit),
        output: output.into(),
    }
}

fn revert_with_message(inputs: &CallInputs, message: &str) -> InterpreterResult {
    revert(inputs, Revert::from(message).abi_encode())
}

fn revert(inputs: &CallInputs, output: Vec<u8>) -> InterpreterResult {
    InterpreterResult {
        result: InstructionResult::Revert,
        gas: Gas::new(inputs.gas_limit),
        output: output.into(),
    }
}
