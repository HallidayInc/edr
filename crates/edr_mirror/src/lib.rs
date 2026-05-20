//! Native token mirror via instruction-table hooks.
//!
//! KECCAK256/SLOAD/SSTORE on the mirror token's balance map are intercepted so
//! interpreter-level ERC20 reads and writes operate on the underlying native
//! balance. This is transparent for real bytecode (transfer, balanceOf,
//! crosschainBurn, etc).
//!
//! Out-of-band RPC writes (e.g. `hardhat_setStorageAt`) are *not* mirrored:
//! the RPC delivers an already-hashed storage key with no preimage, so we
//! cannot recover the owner address to update the matching native balance.
//! Cheat layers that want to fund a mirrored balance must call
//! `hardhat_setBalance` instead.

use std::{cell::RefCell, collections::HashMap};

use alloy_primitives::{Address, B256, U256};
use edr_chain_config::NativeTokenMirror;
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
use revm_primitives::{hardfork::SpecId, keccak256, KECCAK_EMPTY};

const NATIVE_DECIMALS: u8 = 18;

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
        self.config
            .as_ref()
            .and_then(|c| c.decimals)
            .unwrap_or(NATIVE_DECIMALS)
    }

    pub fn observe_keccak(&self, input: &[u8], hash: B256) {
        let Some(config) = &self.config else { return };
        if input.len() != 64 {
            return;
        }
        let slot_bytes: [u8; 32] = input[32..64].try_into().expect("len 64");
        let slot = U256::from_be_bytes(slot_bytes);
        if slot != config.balance_slot {
            return;
        }
        if input[..12].iter().any(|b| *b != 0) {
            return;
        }
        let addr = Address::from_slice(&input[12..32]);
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
        let dec = self.decimals();
        match dec.cmp(&NATIVE_DECIMALS) {
            std::cmp::Ordering::Equal => balance,
            std::cmp::Ordering::Greater => balance.saturating_mul(pow10(dec - NATIVE_DECIMALS)),
            std::cmp::Ordering::Less => balance / pow10(NATIVE_DECIMALS - dec),
        }
    }

    pub fn erc20_to_native(&self, value: U256) -> U256 {
        let dec = self.decimals();
        match dec.cmp(&NATIVE_DECIMALS) {
            std::cmp::Ordering::Equal => value,
            std::cmp::Ordering::Greater => value / pow10(dec - NATIVE_DECIMALS),
            std::cmp::Ordering::Less => value.saturating_mul(pow10(NATIVE_DECIMALS - dec)),
        }
    }
}

fn pow10(n: u8) -> U256 {
    let mut acc = U256::from(1u64);
    for _ in 0..n {
        acc = acc.saturating_mul(U256::from(10u64));
    }
    acc
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

    let len_usize = match u256_to_usize(*top) {
        Some(v) => v,
        None => {
            interp.halt(InstructionResult::InvalidOperandOOG);
            return;
        }
    };
    let cost = match gas::keccak256_cost(len_usize) {
        Some(c) => c,
        None => {
            interp.halt_oog();
            return;
        }
    };
    if !interp.gas.record_cost(cost) {
        interp.halt_oog();
        return;
    }

    let hash = if len_usize == 0 {
        KECCAK_EMPTY
    } else {
        let from = match u256_to_usize(offset) {
            Some(v) => v,
            None => {
                interp.halt(InstructionResult::InvalidOperandOOG);
                return;
            }
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
        let native = context.host.mirror().erc20_to_native(value);
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
