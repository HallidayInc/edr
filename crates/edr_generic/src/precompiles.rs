use alloy_primitives::Log;
use alloy_sol_types::{sol, Revert, SolCall, SolError, SolEvent, SolValue};
use edr_chain_spec_evm::{
    handler::EthPrecompiles, ContextTrait, Database, InterpreterResult, JournalTrait as _,
};
use edr_primitives::{address, keccak256, Address, Bytes, B256, U256};
use edr_state_remote::RemoteStorageCall;
use revm_context_interface::{journaled_state::account::JournaledAccountTr, Cfg, Transaction as _};
use revm_handler::PrecompileProvider;
use revm_interpreter::{CallInputs, Gas, InstructionResult};
use revm_primitives::AddressSet;

use crate::{
    APE_APY_SLOT, APE_PRECOMPILE_STATE_ADDRESS, APE_SHARE_COUNT_SLOT, APE_SHARE_PRICE_SLOT,
};

const ARBSYS_ADDRESS: Address = address!("0000000000000000000000000000000000000064");
const INJECTIVE_BANK_ADDRESS: Address = address!("0000000000000000000000000000000000000064");
const INJECTIVE_BALANCE_SLOT_PREFIX: [u8; 12] = *b"EDR_INJ_BAL_";
const INJECTIVE_BANK_READ_GAS: u64 = 10_000;
const INJECTIVE_BANK_TRANSFER_GAS: u64 = 150_000;
const INJECTIVE_BANK_MINT_BURN_GAS: u64 = 200_000;
const INJECTIVE_WRITE_COST_PER_BYTE: u64 = 30;
const ARBINFO_ADDRESS: Address = address!("0000000000000000000000000000000000000065");
const ARBOWNERPUBLIC_ADDRESS: Address = address!("000000000000000000000000000000000000006b");
const ARBSYS_STATE_ADDRESS: Address = address!("00000000000000000000000000000000A4B05A11");
const ADDRESS_ALIAS_OFFSET: Address = address!("1111000000000000000000000000000000001111");
const PARTIALS_SLOT_START: u64 = 2;
const APE_DEFAULT_SHARE_PRICE: u64 = 1;

sol! {
    interface InjectiveBank {
        function balanceOf(address token, address account) external view returns (uint256);
        function totalSupply(address token) external view returns (uint256);
        function transfer(address sender, address recipient, uint256 amount) external payable returns (bool);
        function mint(address actor, uint256 amount) external payable returns (bool);
        function burn(address actor, uint256 amount) external payable returns (bool);
    }

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
    InjectiveBank::{
        balanceOfCall as bankBalanceOfCall, burnCall as bankBurnCall, mintCall as bankMintCall,
        totalSupplyCall as bankTotalSupplyCall, transferCall as bankTransferCall,
    },
};

/// Injective precompile provider.
///
/// This keeps Ethereum's built-in precompiles and adds Injective's native Bank
/// module at address `0x64`.
#[derive(Debug, Clone)]
pub struct InjectivePrecompiles {
    inner: EthPrecompiles,
    warm_addresses: AddressSet,
}

impl InjectivePrecompiles {
    pub fn new(spec: edr_chain_spec::EvmSpecId) -> Self {
        let inner = EthPrecompiles::new(spec);
        let warm_addresses = inner
            .warm_addresses()
            .iter()
            .copied()
            .chain([INJECTIVE_BANK_ADDRESS])
            .collect();
        Self {
            inner,
            warm_addresses,
        }
    }

    fn refresh_warm_addresses(&mut self) {
        self.warm_addresses = self
            .inner
            .warm_addresses()
            .iter()
            .copied()
            .chain([INJECTIVE_BANK_ADDRESS])
            .collect();
    }
}

impl<ContextT> PrecompileProvider<ContextT> for InjectivePrecompiles
where
    ContextT: ContextTrait,
{
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: <ContextT::Cfg as Cfg>::Spec) -> bool {
        let changed =
            <EthPrecompiles as PrecompileProvider<ContextT>>::set_spec(&mut self.inner, spec);
        if changed {
            self.refresh_warm_addresses();
        }
        changed
    }

    fn run(
        &mut self,
        context: &mut ContextT,
        inputs: &CallInputs,
    ) -> Result<Option<Self::Output>, String> {
        if inputs.bytecode_address == INJECTIVE_BANK_ADDRESS {
            return run_injective_bank(context, inputs).map(Some);
        }

        self.inner.run(context, inputs)
    }

    fn warm_addresses(&self) -> &AddressSet {
        &self.warm_addresses
    }

    fn contains(&self, address: &Address) -> bool {
        *address == INJECTIVE_BANK_ADDRESS || self.inner.contains(address)
    }
}

pub(crate) fn injective_remote_storage_call(
    token: Address,
    slot: U256,
) -> Option<RemoteStorageCall> {
    let data = if slot == injective_total_supply_slot() {
        bankTotalSupplyCall { token }.abi_encode()
    } else {
        let account = injective_balance_account(slot)?;
        bankBalanceOfCall { token, account }.abi_encode()
    };

    Some(RemoteStorageCall {
        to: INJECTIVE_BANK_ADDRESS,
        data: data.into(),
    })
}

fn injective_balance_slot(account: Address) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[..INJECTIVE_BALANCE_SLOT_PREFIX.len()].copy_from_slice(&INJECTIVE_BALANCE_SLOT_PREFIX);
    bytes[INJECTIVE_BALANCE_SLOT_PREFIX.len()..].copy_from_slice(account.as_slice());
    U256::from_be_bytes(bytes)
}

fn injective_balance_account(slot: U256) -> Option<Address> {
    let bytes = slot.to_be_bytes::<32>();
    if bytes[..INJECTIVE_BALANCE_SLOT_PREFIX.len()] != INJECTIVE_BALANCE_SLOT_PREFIX {
        return None;
    }

    Some(Address::from_slice(
        &bytes[INJECTIVE_BALANCE_SLOT_PREFIX.len()..],
    ))
}

fn injective_total_supply_slot() -> U256 {
    U256::from_be_slice(keccak256("edr.injective.bank.totalSupply").as_slice())
}

#[derive(Clone, Copy)]
enum InjectiveSupplyChange {
    Mint,
    Burn,
}

impl InjectiveSupplyChange {
    fn apply(self, value: U256, amount: U256) -> Option<U256> {
        match self {
            Self::Mint => value.checked_add(amount),
            Self::Burn => value.checked_sub(amount),
        }
    }

    fn readonly_error(self) -> &'static str {
        match self {
            Self::Mint => "Injective Bank: mint is not readonly",
            Self::Burn => "Injective Bank: burn is not readonly",
        }
    }

    fn balance_error(self) -> &'static str {
        match self {
            Self::Mint => "Injective Bank: balance overflow",
            Self::Burn => "Injective Bank: insufficient funds",
        }
    }

    fn supply_error(self) -> &'static str {
        match self {
            Self::Mint => "Injective Bank: supply overflow",
            Self::Burn => "Injective Bank: insufficient supply",
        }
    }
}

fn run_injective_bank<ContextT>(
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
            "Injective Bank: missing selector",
        ));
    }

    let selector = &calldata[..4];
    let base_gas =
        if selector == bankBalanceOfCall::SELECTOR || selector == bankTotalSupplyCall::SELECTOR {
            INJECTIVE_BANK_READ_GAS
        } else if selector == bankTransferCall::SELECTOR {
            INJECTIVE_BANK_TRANSFER_GAS
        } else if selector == bankMintCall::SELECTOR || selector == bankBurnCall::SELECTOR {
            INJECTIVE_BANK_MINT_BURN_GAS
        } else {
            0
        };
    let Some(gas) = injective_bank_gas(inputs, calldata.len(), base_gas) else {
        return Ok(injective_bank_out_of_gas(inputs));
    };

    if selector == bankBalanceOfCall::SELECTOR {
        let Ok(call) = bankBalanceOfCall::abi_decode(calldata) else {
            return Ok(revert_with_message_and_gas(
                gas,
                "Injective Bank: invalid balanceOf calldata",
            ));
        };

        let balance = read_word_at(context, call.token, injective_balance_slot(call.account))?;
        return Ok(success_with_gas(gas, balance.abi_encode()));
    }

    if selector == bankTotalSupplyCall::SELECTOR {
        let Ok(call) = bankTotalSupplyCall::abi_decode(calldata) else {
            return Ok(revert_with_message_and_gas(
                gas,
                "Injective Bank: invalid totalSupply calldata",
            ));
        };

        let total_supply = read_word_at(context, call.token, injective_total_supply_slot())?;
        return Ok(success_with_gas(gas, total_supply.abi_encode()));
    }

    if selector == bankTransferCall::SELECTOR {
        if inputs.is_static {
            return Ok(revert_with_message_and_gas(
                gas,
                "Injective Bank: transfer is not readonly",
            ));
        }

        let Ok(call) = bankTransferCall::abi_decode(calldata) else {
            return Ok(revert_with_message_and_gas(
                gas,
                "Injective Bank: invalid transfer calldata",
            ));
        };

        let token = inputs.caller;
        let sender_slot = injective_balance_slot(call.sender);
        let sender_balance = read_word_at(context, token, sender_slot)?;
        let Some(sender_balance) = sender_balance.checked_sub(call.amount) else {
            return Ok(revert_with_message_and_gas(
                gas,
                "Injective Bank: insufficient funds",
            ));
        };

        if call.sender != call.recipient {
            let recipient_slot = injective_balance_slot(call.recipient);
            let recipient_balance = read_word_at(context, token, recipient_slot)?;
            let Some(recipient_balance) = recipient_balance.checked_add(call.amount) else {
                return Ok(revert_with_message_and_gas(
                    gas,
                    "Injective Bank: balance overflow",
                ));
            };

            write_word_at(context, token, sender_slot, sender_balance)?;
            write_word_at(context, token, recipient_slot, recipient_balance)?;
        }

        return Ok(success_with_gas(gas, true.abi_encode()));
    }

    let supply_change = if selector == bankMintCall::SELECTOR {
        Some(InjectiveSupplyChange::Mint)
    } else if selector == bankBurnCall::SELECTOR {
        Some(InjectiveSupplyChange::Burn)
    } else {
        None
    };
    if let Some(supply_change) = supply_change {
        if inputs.is_static {
            return Ok(revert_with_message_and_gas(
                gas,
                supply_change.readonly_error(),
            ));
        }

        let (actor, amount) = match supply_change {
            InjectiveSupplyChange::Mint => {
                let Ok(call) = bankMintCall::abi_decode(calldata) else {
                    return Ok(revert_with_message_and_gas(
                        gas,
                        "Injective Bank: invalid mint calldata",
                    ));
                };
                (call.actor, call.amount)
            }
            InjectiveSupplyChange::Burn => {
                let Ok(call) = bankBurnCall::abi_decode(calldata) else {
                    return Ok(revert_with_message_and_gas(
                        gas,
                        "Injective Bank: invalid burn calldata",
                    ));
                };
                (call.actor, call.amount)
            }
        };

        let token = inputs.caller;
        let actor_slot = injective_balance_slot(actor);
        let actor_balance = read_word_at(context, token, actor_slot)?;
        let total_supply_slot = injective_total_supply_slot();
        let total_supply = read_word_at(context, token, total_supply_slot)?;
        let Some(actor_balance) = supply_change.apply(actor_balance, amount) else {
            return Ok(revert_with_message_and_gas(
                gas,
                supply_change.balance_error(),
            ));
        };
        let Some(total_supply) = supply_change.apply(total_supply, amount) else {
            return Ok(revert_with_message_and_gas(
                gas,
                supply_change.supply_error(),
            ));
        };

        write_word_at(context, token, actor_slot, actor_balance)?;
        write_word_at(context, token, total_supply_slot, total_supply)?;

        return Ok(success_with_gas(gas, true.abi_encode()));
    }

    Ok(revert_with_message_and_gas(
        gas,
        &format!(
            "Injective Bank: unsupported selector 0x{}",
            alloy_primitives::hex::encode(selector)
        ),
    ))
}

fn injective_bank_gas(inputs: &CallInputs, calldata_len: usize, base_gas: u64) -> Option<Gas> {
    let calldata_gas = u64::try_from(calldata_len)
        .unwrap_or(u64::MAX)
        .saturating_mul(INJECTIVE_WRITE_COST_PER_BYTE);
    let required_gas = base_gas.saturating_add(calldata_gas);
    let mut gas = Gas::new(inputs.gas_limit);
    gas.record_regular_cost(required_gas).then_some(gas)
}

fn injective_bank_out_of_gas(inputs: &CallInputs) -> InterpreterResult {
    let mut gas = Gas::new(inputs.gas_limit);
    gas.spend_all();
    InterpreterResult {
        result: InstructionResult::PrecompileOOG,
        gas,
        output: Bytes::new(),
    }
}

/// Arbitrum precompile provider.
///
/// This keeps Ethereum's built-in precompiles and layers in Arbitrum's Nitro
/// compatibility precompiles.
#[derive(Debug, Clone)]
pub struct ArbPrecompiles {
    inner: EthPrecompiles,
    warm_addresses: AddressSet,
}

impl ArbPrecompiles {
    pub fn new(spec: edr_chain_spec::EvmSpecId) -> Self {
        let inner = EthPrecompiles::new(spec);
        let warm_addresses = inner
            .warm_addresses()
            .iter()
            .copied()
            .chain([ARBSYS_ADDRESS, ARBINFO_ADDRESS, ARBOWNERPUBLIC_ADDRESS])
            .collect();
        Self {
            inner,
            warm_addresses,
        }
    }

    fn refresh_warm_addresses(&mut self) {
        self.warm_addresses = self
            .inner
            .warm_addresses()
            .iter()
            .copied()
            .chain([ARBSYS_ADDRESS, ARBINFO_ADDRESS, ARBOWNERPUBLIC_ADDRESS])
            .collect();
    }
}

/// ApeChain precompile provider.
///
/// This wraps the Arbitrum provider and adds ApeChain's custom selectors to
/// the standard Nitro precompile addresses.
#[derive(Debug, Clone)]
pub struct ApePrecompiles {
    inner: ArbPrecompiles,
}

impl ApePrecompiles {
    pub fn new(spec: edr_chain_spec::EvmSpecId) -> Self {
        Self {
            inner: ArbPrecompiles::new(spec),
        }
    }
}

impl<ContextT> PrecompileProvider<ContextT> for ArbPrecompiles
where
    ContextT: ContextTrait,
{
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: <ContextT::Cfg as Cfg>::Spec) -> bool {
        let changed =
            <EthPrecompiles as PrecompileProvider<ContextT>>::set_spec(&mut self.inner, spec);
        if changed {
            self.refresh_warm_addresses();
        }
        changed
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

    fn warm_addresses(&self) -> &AddressSet {
        &self.warm_addresses
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

    fn warm_addresses(&self) -> &AddressSet {
        <ArbPrecompiles as PrecompileProvider<ContextT>>::warm_addresses(&self.inner)
    }

    fn contains(&self, address: &Address) -> bool {
        <ArbPrecompiles as PrecompileProvider<ContextT>>::contains(&self.inner, address)
    }
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
    drop(account);

    context
        .journal_mut()
        .sstore(address, slot, value)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn success(inputs: &CallInputs, output: Vec<u8>) -> InterpreterResult {
    success_with_gas(Gas::new(inputs.gas_limit), output)
}

fn success_with_gas(gas: Gas, output: Vec<u8>) -> InterpreterResult {
    InterpreterResult {
        result: InstructionResult::Return,
        gas,
        output: output.into(),
    }
}

fn revert_with_message(inputs: &CallInputs, message: &str) -> InterpreterResult {
    revert(inputs, Revert::from(message).abi_encode())
}

fn revert_with_message_and_gas(gas: Gas, message: &str) -> InterpreterResult {
    revert_with_gas(gas, Revert::from(message).abi_encode())
}

fn revert(inputs: &CallInputs, output: Vec<u8>) -> InterpreterResult {
    revert_with_gas(Gas::new(inputs.gas_limit), output)
}

fn revert_with_gas(gas: Gas, output: Vec<u8>) -> InterpreterResult {
    InterpreterResult {
        result: InstructionResult::Revert,
        gas,
        output: output.into(),
    }
}

#[cfg(test)]
mod injective_tests {
    use alloy_sol_types::{SolCall as _, SolValue as _};
    use edr_primitives::{address, U256};

    use super::{
        bankBalanceOfCall, bankBurnCall, bankMintCall, bankTotalSupplyCall,
        injective_balance_account, injective_balance_slot, injective_remote_storage_call,
        injective_total_supply_slot, INJECTIVE_BANK_ADDRESS,
    };

    #[test]
    fn balance_slots_round_trip_accounts() {
        let account = address!("1234567890123456789012345678901234567890");
        let slot = injective_balance_slot(account);

        assert_eq!(injective_balance_account(slot), Some(account));
        assert_eq!(injective_balance_account(U256::ZERO), None);
    }

    #[test]
    fn balance_storage_resolves_to_canonical_bank_call() {
        let token = address!("1111111111111111111111111111111111111111");
        let account = address!("2222222222222222222222222222222222222222");
        let call = injective_remote_storage_call(token, injective_balance_slot(account))
            .expect("balance slots must resolve");

        assert_eq!(call.to, INJECTIVE_BANK_ADDRESS);
        let decoded = bankBalanceOfCall::abi_decode(&call.data).unwrap();
        assert_eq!(decoded.token, token);
        assert_eq!(decoded.account, account);
    }

    #[test]
    fn supply_storage_resolves_to_canonical_bank_call() {
        let token = address!("1111111111111111111111111111111111111111");
        let call = injective_remote_storage_call(token, injective_total_supply_slot())
            .expect("supply slot must resolve");

        assert_eq!(call.to, INJECTIVE_BANK_ADDRESS);
        let decoded = bankTotalSupplyCall::abi_decode(&call.data).unwrap();
        assert_eq!(decoded.token, token);
        assert!(injective_remote_storage_call(token, U256::ZERO).is_none());
    }

    #[test]
    fn bank_uint_return_is_a_single_word() {
        assert_eq!(U256::from(42).abi_encode().len(), 32);
    }

    #[test]
    fn mint_and_burn_use_canonical_selectors() {
        assert_eq!(bankMintCall::SELECTOR, [0x40, 0xc1, 0x0f, 0x19]);
        assert_eq!(bankBurnCall::SELECTOR, [0x9d, 0xc2, 0x9f, 0xac]);
    }
}
