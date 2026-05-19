//! Native token mirror support.
//!
//! The ERC-20 balance slot stores the token-denominated quotient. The native
//! account balance stores the full native balance, whose low-order units act as
//! the remainder when the token has fewer decimals than the native unit.
//! Instruction hooks keep ERC-20 bytecode and native balance reads coherent
//! within a transaction; state projection keeps committed storage coherent.

use std::{cell::RefCell, collections::HashMap};

use alloy_primitives::{Address, B256, U256};
use edr_chain_config::NativeTokenMirror;
use edr_primitives::{Bytecode, KECCAK_EMPTY};
use edr_state_api::{
    account::{AccountInfo, BasicAccount},
    EvmStorageSlot, State, StateDebug, StateDiff,
};
use revm_context_interface::{context::ContextTr, host::LoadError, journaled_state::JournalTr};
use revm_handler::instructions::EthInstructions;
use revm_interpreter::{
    gas::{
        self, CALL_STIPEND, COLD_SLOAD_COST_ADDITIONAL, ISTANBUL_SLOAD_GAS, WARM_STORAGE_READ_COST,
    },
    instruction_context::InstructionContext,
    interpreter_types::{InputsTr, InterpreterTypes, MemoryTr, RuntimeFlag, StackTr},
    Instruction, InstructionResult,
};
use revm_primitives::{hardfork::SpecId, keccak256};

#[derive(Debug, Default)]
pub struct MirrorContext {
    pub config: Option<NativeTokenMirror>,
    pub cache: RefCell<HashMap<U256, Address>>,
}

impl MirrorContext {
    pub fn new(config: Option<NativeTokenMirror>) -> Self {
        Self {
            config,
            cache: RefCell::new(HashMap::new()),
        }
    }

    pub fn decimals(&self) -> u8 {
        self.config.as_ref().and_then(|c| c.decimals).unwrap_or(18)
    }

    pub fn observe_keccak(&self, input: &[u8], hash: B256) {
        let Some(config) = &self.config else { return };
        let Ok(chunk): Result<&[u8; 64], _> = input.try_into() else {
            return;
        };
        let (addr_word, slot_bytes) = chunk.split_first_chunk::<32>().expect("len 64");
        let slot = U256::from_be_bytes(*<&[u8; 32]>::try_from(slot_bytes).expect("len 32"));
        let (zero_prefix, addr_bytes) = addr_word.split_first_chunk::<12>().expect("len 32");
        if slot != config.balance_slot || zero_prefix.iter().any(|b| *b != 0) {
            return;
        }
        let addr = Address::from_slice(addr_bytes);
        let key = U256::from_be_bytes(hash.0);
        self.cache.borrow_mut().insert(key, addr);
    }

    pub fn balance_owner(&self, target: Address, slot: U256) -> Option<Address> {
        let config = self.config.as_ref()?;
        if target != config.token {
            return None;
        }
        self.cache.borrow().get(&slot).copied()
    }

    pub fn native_to_erc20(&self, balance: U256) -> U256 {
        self.config
            .as_ref()
            .map_or(balance, |config| config.native_to_erc20_balance(balance))
    }

    pub fn erc20_to_native(&self, value: U256) -> U256 {
        self.config
            .as_ref()
            .map_or(value, |config| config.erc20_to_native_balance(value))
    }

    pub fn erc20_to_native_preserving_remainder(&self, value: U256, current_native: U256) -> U256 {
        self.config.as_ref().map_or(value, |config| {
            config.mirrored_native_balance(value, current_native)
        })
    }
}

pub fn native_token_mirror_account_info<StateT>(
    state: &StateT,
    native_token_mirror: &NativeTokenMirror,
    address: Address,
) -> Result<Option<AccountInfo>, StateT::Error>
where
    StateT: State + ?Sized,
{
    let raw_account = state.basic(address)?;
    let raw_balance = raw_account
        .as_ref()
        .map_or(U256::ZERO, |account| account.balance);
    let erc20_balance = state.storage(
        native_token_mirror.token,
        native_token_mirror.balance_storage_key(address),
    )?;
    let balance = native_token_mirror.mirrored_native_balance(erc20_balance, raw_balance);

    if let Some(mut account) = raw_account {
        account.balance = balance;
        return Ok(Some(account));
    }

    if balance.is_zero() {
        return Ok(None);
    }

    let mut account = AccountInfo::from(BasicAccount::default());
    account.balance = balance;
    Ok(Some(account))
}

pub struct NativeTokenMirrorState<'a, StateT: ?Sized> {
    state: &'a StateT,
    native_token_mirror: &'a NativeTokenMirror,
}

impl<'a, StateT: ?Sized> NativeTokenMirrorState<'a, StateT> {
    pub fn new(state: &'a StateT, native_token_mirror: &'a NativeTokenMirror) -> Self {
        Self {
            state,
            native_token_mirror,
        }
    }
}

impl<StateT> State for NativeTokenMirrorState<'_, StateT>
where
    StateT: State + ?Sized,
{
    type Error = StateT::Error;

    fn basic(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        native_token_mirror_account_info(self.state, self.native_token_mirror, address)
    }

    fn code_by_hash(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.state.code_by_hash(code_hash)
    }

    fn storage(&self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.state.storage(address, index)
    }
}

pub fn apply_native_token_mirror_state_diff<StateT>(
    native_token_mirror: &NativeTokenMirror,
    diff: &mut StateDiff,
    state: &mut StateT,
) -> Result<(), <StateT as State>::Error>
where
    StateT: State + StateDebug<Error = <StateT as State>::Error> + ?Sized,
{
    let mut token_account_info = state.basic(native_token_mirror.token)?;
    if let Some(account_info) = &mut token_account_info
        && account_info.code_hash != KECCAK_EMPTY
    {
        account_info.code = Some(state.code_by_hash(account_info.code_hash)?);
    }

    let mirrored_balances = diff
        .as_inner()
        .iter()
        .map(|(address, account)| (*address, account.info.balance))
        .collect::<Vec<_>>();

    let mut mirrored_changes = StateDiff::default();
    for (address, native_balance) in mirrored_balances {
        let erc20_balance = native_token_mirror.native_to_erc20_balance(native_balance);
        let storage_key = native_token_mirror.balance_storage_key(address);
        let old_value = state.set_account_storage_slot(
            native_token_mirror.token,
            storage_key,
            erc20_balance,
        )?;
        let slot = EvmStorageSlot::new_changed(old_value, erc20_balance, 0);
        mirrored_changes.apply_storage_change(
            native_token_mirror.token,
            storage_key,
            slot,
            token_account_info.clone(),
        );
    }

    diff.apply_diff(mirrored_changes.into());

    Ok(())
}

pub trait AsMirror {
    fn as_mirror(&self) -> &MirrorContext;
}

impl AsMirror for MirrorContext {
    fn as_mirror(&self) -> &MirrorContext {
        self
    }
}

pub trait MirrorHost: ContextTr {
    fn mirror(&self) -> &MirrorContext;
    fn set_native_balance(&mut self, owner: Address, value: U256);
}

impl<T> MirrorHost for T
where
    T: ContextTr,
    T::Chain: AsMirror,
{
    fn mirror(&self) -> &MirrorContext {
        self.chain().as_mirror()
    }

    fn set_native_balance(&mut self, owner: Address, value: U256) {
        if let Ok(mut load) = self.journal_mut().load_account_mut(owner) {
            load.data.set_balance(value);
        }
    }
}

// ---------------------------------------------------------------------------
// Instruction handlers
//
// These borrow fields of `interpreter` separately (stack vs gas vs memory vs
// runtime_flag/input) so the borrow checker accepts holding the popn_top!
// reborrow alongside other field accesses. Inlining the macros makes that
// explicit.
// ---------------------------------------------------------------------------

pub fn keccak256_with_mirror<W, H>(context: InstructionContext<'_, H, W>)
where
    W: InterpreterTypes,
    H: MirrorHost,
{
    let interp = context.interpreter;
    if interp.stack.len() < 2 {
        interp.halt_underflow();
        return;
    }
    let ([offset], top) = unsafe { interp.stack.popn_top::<1>().unwrap_unchecked() };

    let Some(len_usize) = u256_to_usize(*top) else {
        interp.halt(InstructionResult::InvalidOperandOOG);
        return;
    };
    let Some(cost) = gas::keccak256_cost(len_usize) else {
        interp.halt_oog();
        return;
    };
    if !interp.gas.record_cost(cost) {
        interp.halt_oog();
        return;
    }

    let hash = if len_usize == 0 {
        KECCAK_EMPTY
    } else {
        let Some(from) = u256_to_usize(offset) else {
            interp.halt(InstructionResult::InvalidOperandOOG);
            return;
        };
        if !revm_interpreter::interpreter::resize_memory(
            &mut interp.gas,
            &mut interp.memory,
            from,
            len_usize,
        ) {
            return;
        }
        let mem_slice = interp.memory.slice_len(from, len_usize);
        let bytes = mem_slice.as_ref();
        let h = keccak256(bytes);
        context.host.mirror().observe_keccak(bytes, h);
        h
    };
    *top = hash.into();
}

pub fn sload_with_mirror<W, H>(context: InstructionContext<'_, H, W>)
where
    W: InterpreterTypes,
    H: MirrorHost,
{
    let interp = context.interpreter;
    if interp.stack.len() < 1 {
        interp.halt_underflow();
        return;
    }
    let ([], index) = unsafe { interp.stack.popn_top::<0>().unwrap_unchecked() };

    let spec_id = interp.runtime_flag.spec_id();
    let target = interp.input.target_address();

    let gas_base = if spec_id.is_enabled_in(SpecId::BERLIN) {
        WARM_STORAGE_READ_COST
    } else if spec_id.is_enabled_in(SpecId::ISTANBUL) {
        ISTANBUL_SLOAD_GAS
    } else if spec_id.is_enabled_in(SpecId::TANGERINE) {
        200
    } else {
        50
    };
    if !interp.gas.record_cost(gas_base) {
        interp.halt_oog();
        return;
    }

    if spec_id.is_enabled_in(SpecId::BERLIN) {
        let skip_cold = interp.gas.remaining() < COLD_SLOAD_COST_ADDITIONAL;
        let slot = *index;
        match context.host.sload_skip_cold_load(target, slot, skip_cold) {
            Ok(storage) => {
                if storage.is_cold && !interp.gas.record_cost(COLD_SLOAD_COST_ADDITIONAL) {
                    interp.halt_oog();
                    return;
                }
                if let Some(owner) = context.host.mirror().balance_owner(target, slot) {
                    let native = context
                        .host
                        .balance(owner)
                        .map(|s| s.data)
                        .unwrap_or_default();
                    *index = context.host.mirror().native_to_erc20(native);
                } else {
                    *index = storage.data;
                }
            }
            Err(LoadError::ColdLoadSkipped) => interp.halt_oog(),
            Err(LoadError::DBError) => interp.halt_fatal(),
        }
    } else {
        let slot = *index;
        let Some(storage) = context.host.sload(target, slot) else {
            return interp.halt_fatal();
        };
        if let Some(owner) = context.host.mirror().balance_owner(target, slot) {
            let native = context
                .host
                .balance(owner)
                .map(|s| s.data)
                .unwrap_or_default();
            *index = context.host.mirror().native_to_erc20(native);
        } else {
            *index = storage.data;
        }
    }
}

pub fn sstore_with_mirror<W, H>(context: InstructionContext<'_, H, W>)
where
    W: InterpreterTypes,
    H: MirrorHost,
{
    let interp = context.interpreter;
    if interp.runtime_flag.is_static() {
        interp.halt(InstructionResult::StateChangeDuringStaticCall);
        return;
    }
    if interp.stack.len() < 2 {
        interp.halt_underflow();
        return;
    }
    let [index, value] = unsafe { interp.stack.popn::<2>().unwrap_unchecked() };

    let target = interp.input.target_address();
    let spec_id = interp.runtime_flag.spec_id();

    if spec_id.is_enabled_in(SpecId::ISTANBUL) && interp.gas.remaining() <= CALL_STIPEND {
        interp.halt(InstructionResult::ReentrancySentryOOG);
        return;
    }

    if !interp.gas.record_cost(gas::static_sstore_cost(spec_id)) {
        interp.halt_oog();
        return;
    }

    let state_load = if spec_id.is_enabled_in(SpecId::BERLIN) {
        let skip_cold = interp.gas.remaining() < COLD_SLOAD_COST_ADDITIONAL;
        match context
            .host
            .sstore_skip_cold_load(target, index, value, skip_cold)
        {
            Ok(load) => load,
            Err(LoadError::ColdLoadSkipped) => {
                interp.halt_oog();
                return;
            }
            Err(LoadError::DBError) => {
                interp.halt_fatal();
                return;
            }
        }
    } else {
        let Some(load) = context.host.sstore(target, index, value) else {
            interp.halt_fatal();
            return;
        };
        load
    };

    if !interp.gas.record_cost(gas::dyn_sstore_cost(
        spec_id,
        &state_load.data,
        state_load.is_cold,
    )) {
        interp.halt_oog();
        return;
    }

    interp
        .gas
        .record_refund(gas::sstore_refund(spec_id, &state_load.data));

    if let Some(owner) = context.host.mirror().balance_owner(target, index) {
        let current_native = context
            .host
            .balance(owner)
            .map(|s| s.data)
            .unwrap_or_default();
        let native = context
            .host
            .mirror()
            .erc20_to_native_preserving_remainder(value, current_native);
        context.host.set_native_balance(owner, native);
    }
}

#[inline]
fn u256_to_usize(v: U256) -> Option<usize> {
    if v > U256::from(usize::MAX) {
        None
    } else {
        Some(v.as_limbs()[0] as usize)
    }
}

// ---------------------------------------------------------------------------
// Instruction table builder
// ---------------------------------------------------------------------------

pub fn build_instructions<W, H>() -> EthInstructions<W, H>
where
    W: InterpreterTypes,
    H: MirrorHost,
{
    let mut table: EthInstructions<W, H> = EthInstructions::new_mainnet();
    // KECCAK256, SLOAD, SSTORE have dynamic gas (static_gas = 0) in the default
    // table.
    table.insert_instruction(0x20, Instruction::new(keccak256_with_mirror::<W, H>, 0));
    table.insert_instruction(0x54, Instruction::new(sload_with_mirror::<W, H>, 0));
    table.insert_instruction(0x55, Instruction::new(sstore_with_mirror::<W, H>, 0));
    table
}
