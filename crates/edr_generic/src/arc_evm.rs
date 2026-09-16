use std::collections::HashMap;

use alloy_primitives::Log;
use alloy_sol_types::{sol, Revert, SolCall, SolError};
use edr_chain_spec_evm::{Evm, Inspector, InterpreterResult, JournalTrait};
use edr_primitives::{Address, Bytes, U256};
use revm_context::{
    result::{EVMError, ExecutionResult, HaltReason, InvalidTransaction, ResultAndState},
    ContextSetters, JournalTr,
};
use revm_context_interface::{
    journaled_state::JournalCheckpoint, Block as _, ContextTr, Database, Transaction,
};
use revm_handler::{
    evm::{ContextDbError, FrameInitResult},
    instructions::{EthInstructions, InstructionProvider},
    EthFrame, EvmTr, EvmTrError, ExecuteEvm, FrameInitOrResult, FrameResult, Handler, ItemOrResult,
    MainnetHandler, PrecompileProvider,
};
use revm_inspector::{InspectEvm, InspectorEvmTr, InspectorFrame, InspectorHandler, JournalExt};
use revm_interpreter::{
    gas,
    instructions::utility::IntoAddress,
    interpreter::EthInterpreter,
    interpreter_action::{CallInput, FrameInit, FrameInput},
    interpreter_types::{InputsTr, LoopControl, RuntimeFlag, StackTr},
    popn, require_non_staticcall, state_gas, CallOutcome, CallScheme, CallValue, CreateOutcome,
    CreateScheme, Gas, Host, Instruction, InstructionContext, InstructionExecResult,
    InstructionResult, InterpreterAction, InterpreterTypes,
};
use revm_state::EvmState;

use crate::precompiles::{
    arc_blocklist_slot, arc_transfer_log, ARC_CALL_FROM_ADDRESS, ARC_NATIVE_COIN_CONTROL_ADDRESS,
};
use crate::ArcHardfork;

const ERR_BLOCKED_ADDRESS: &str = "Blocked address";
const ERR_SELFDESTRUCTED_BALANCE_INCREASED: &str =
    "Cannot increase the balance of selfdestructed account";
const ERR_ZERO_ADDRESS: &str = "Zero address not allowed";
const SELFDESTRUCT: u8 = 0xff;
const ARC_MEMO_ADDRESS: Address =
    edr_primitives::address!("5294e9927c3306dcbadb03fe70b92e01ccede505");
const ARC_MULTICALL3_FROM_ADDRESS: Address =
    edr_primitives::address!("522faf9a91c41c443c66765030741e4aace147d0");
const CALL_FROM_ABI_GAS: u64 = 100;

sol! {
    interface ArcCallFrom {
        function callFrom(address sender, address target, bytes calldata data)
            external returns (bool success, bytes memory returnData);
    }
}

use ArcCallFrom::{callFromCall as arcCallFromCall, callFromReturn as arcCallFromReturn};

pub(crate) fn build_arc_instructions<W, H>(hardfork: ArcHardfork) -> EthInstructions<W, H>
where
    W: InterpreterTypes,
    H: edr_mirror::MirrorHost + ContextTr,
{
    let mut instructions = edr_mirror::build_instructions(hardfork.into());
    let selfdestruct = if hardfork.is_zero8() {
        arc_selfdestruct_zero8::<W, H>
    } else if hardfork.is_zero7() {
        arc_selfdestruct_zero7::<W, H>
    } else if hardfork.is_zero5() {
        arc_selfdestruct_zero5::<W, H>
    } else {
        arc_selfdestruct_legacy::<W, H>
    };
    instructions.insert_instruction(SELFDESTRUCT, Instruction::new(selfdestruct), 5_000);
    instructions
}

fn arc_selfdestruct_legacy<W, H>(context: InstructionContext<'_, H, W>) -> InstructionExecResult
where
    W: InterpreterTypes,
    H: Host + ContextTr,
{
    arc_selfdestruct(context, false, false, false)
}

fn arc_selfdestruct_zero5<W, H>(context: InstructionContext<'_, H, W>) -> InstructionExecResult
where
    W: InterpreterTypes,
    H: Host + ContextTr,
{
    arc_selfdestruct(context, true, false, false)
}

fn arc_selfdestruct_zero7<W, H>(context: InstructionContext<'_, H, W>) -> InstructionExecResult
where
    W: InterpreterTypes,
    H: Host + ContextTr,
{
    arc_selfdestruct(context, true, false, true)
}

fn arc_selfdestruct_zero8<W, H>(context: InstructionContext<'_, H, W>) -> InstructionExecResult
where
    W: InterpreterTypes,
    H: Host + ContextTr,
{
    arc_selfdestruct(context, true, true, true)
}

fn arc_selfdestruct<W, H>(
    mut context: InstructionContext<'_, H, W>,
    emit_transfer_log: bool,
    transaction_warmth: bool,
    fail_closed_blocklist: bool,
) -> InstructionExecResult
where
    W: InterpreterTypes,
    H: Host + ContextTr,
{
    require_non_staticcall!(context.interpreter);
    popn!([target], context.interpreter);
    let target = target.into_address();
    let source = context.interpreter.input.target_address();
    let balance = context
        .host
        .balance(source)
        .ok_or(InstructionResult::FatalExternalError)?;
    let value = balance.data;

    let mut target_was_cold = None;
    if !value.is_zero() {
        if target == Address::ZERO {
            return arc_opcode_revert(&mut context, ERR_ZERO_ADDRESS);
        }
        if source == target {
            return Err(InstructionResult::Revert);
        }
        if is_blocklisted_host(context.host, source, fail_closed_blocklist)?
            || is_blocklisted_host(context.host, target, fail_closed_blocklist)?
        {
            return arc_opcode_revert(&mut context, ERR_BLOCKED_ADDRESS);
        }
        let target_account = context
            .host
            .journal_mut()
            .load_account(target)
            .map_err(|_error| InstructionResult::FatalExternalError)?;
        target_was_cold = Some(if transaction_warmth {
            target_account.is_cold
        } else {
            true
        });
        if target_account.is_selfdestructed() {
            return arc_opcode_revert(&mut context, ERR_SELFDESTRUCTED_BALANCE_INCREASED);
        }
    }

    let spec = context.interpreter.runtime_flag.spec_id();
    let cold_load_gas = context.host.gas_params().selfdestruct_cold_cost();
    let mut result = context.host.selfdestruct(
        source,
        target,
        context.interpreter.gas.remaining() < cold_load_gas,
    )?;
    if let Some(target_was_cold) = target_was_cold {
        result.is_cold = target_was_cold;
    }

    if emit_transfer_log && !value.is_zero() {
        context.host.log(arc_transfer_log(source, target, value));
    }

    let should_charge_topup =
        if spec.is_enabled_in(revm_primitives::hardfork::SpecId::SPURIOUS_DRAGON) {
            result.data.had_value && !result.data.target_exists
        } else {
            !result.data.target_exists
        };
    gas!(
        context.interpreter,
        context
            .host
            .gas_params()
            .selfdestruct_cost(should_charge_topup, result.is_cold)
    );
    if context.host.is_amsterdam_eip8037_enabled() && should_charge_topup {
        state_gas!(
            context.interpreter,
            context.host.gas_params().new_account_state_gas()
        );
    }
    if !result.data.previously_destroyed {
        context
            .interpreter
            .gas
            .record_refund(context.host.gas_params().selfdestruct_refund());
    }

    Err(InstructionResult::SelfDestruct)
}

fn is_blocklisted_host<H>(
    host: &mut H,
    account: Address,
    fail_closed: bool,
) -> Result<bool, InstructionResult>
where
    H: Host + ContextTr,
{
    match host
        .journal_mut()
        .sload(ARC_NATIVE_COIN_CONTROL_ADDRESS, arc_blocklist_slot(account))
    {
        Ok(load) => Ok(load.data != U256::ZERO),
        Err(_error) if !fail_closed => Ok(false),
        Err(_error) => Err(InstructionResult::FatalExternalError),
    }
}

fn arc_opcode_revert<W, H>(
    context: &mut InstructionContext<'_, H, W>,
    message: &str,
) -> InstructionExecResult
where
    W: InterpreterTypes,
    H: Host + ?Sized,
{
    context
        .interpreter
        .bytecode
        .set_action(InterpreterAction::new_return(
            InstructionResult::Revert,
            Revert::from(message).abi_encode().into(),
            context.interpreter.gas,
        ));
    Err(InstructionResult::Revert)
}

enum BeforeFrameInit {
    Log(Log),
    Reverted(FrameResult),
    None,
}

enum FrameInitOutcome {
    Pushed,
    Immediate(FrameResult),
}

struct CallFromContinuation {
    gas_limit: u64,
    initial_gas: u64,
    return_memory_offset: core::ops::Range<usize>,
    checkpoint: JournalCheckpoint,
}

struct ArcHandler<EVM, ERROR> {
    mainnet: MainnetHandler<EVM, ERROR, EthFrame<EthInterpreter>>,
    hardfork: ArcHardfork,
}

impl<EVM, ERROR> ArcHandler<EVM, ERROR> {
    fn new(hardfork: ArcHardfork) -> Self {
        Self {
            mainnet: MainnetHandler::default(),
            hardfork,
        }
    }
}

impl<EVM, ERROR> Handler for ArcHandler<EVM, ERROR>
where
    EVM: EvmTr<
        Context: ContextTr<Journal: JournalTr<State = EvmState>>,
        Frame = EthFrame<EthInterpreter>,
    >,
    ERROR: EvmTrError<EVM>,
{
    type Evm = EVM;
    type Error = ERROR;
    type HaltReason = HaltReason;

    fn pre_execution(
        &self,
        evm: &mut Self::Evm,
        init_and_floor_gas: &mut revm_interpreter::InitialAndFloorGas,
    ) -> Result<u64, Self::Error> {
        let context = evm.ctx();
        let caller = context.tx().caller();
        let transaction_kind = context.tx().kind();
        let recipient = transaction_kind.to();
        let value = context.tx().value();

        if is_blocklisted(evm.ctx_mut(), caller)? {
            return Err(InvalidTransaction::Str(ERR_BLOCKED_ADDRESS.into()).into());
        }
        if let Some(recipient) = recipient.filter(|_| !value.is_zero())
            && is_blocklisted(evm.ctx_mut(), *recipient)?
        {
            return Err(InvalidTransaction::Str(ERR_BLOCKED_ADDRESS.into()).into());
        }

        self.mainnet.pre_execution(evm, init_and_floor_gas)
    }

    fn reward_beneficiary(
        &self,
        evm: &mut Self::Evm,
        exec_result: &mut FrameResult,
    ) -> Result<(), Self::Error> {
        let context = evm.ctx();
        let beneficiary = context.block().beneficiary();
        let effective_gas_price = context
            .tx()
            .effective_gas_price(u128::from(context.block().basefee()));
        let gas_used = exec_result.gas().used();
        let total_fee = U256::from(effective_gas_price) * U256::from(gas_used);

        if self.hardfork.is_zero8()
            && evm
                .ctx_mut()
                .journal_mut()
                .load_account(beneficiary)?
                .is_selfdestructed()
        {
            return Err(
                InvalidTransaction::Str(ERR_SELFDESTRUCTED_BALANCE_INCREASED.into()).into(),
            );
        }

        evm.ctx_mut()
            .journal_mut()
            .balance_incr(beneficiary, total_fee)?;
        Ok(())
    }

    fn validate_initial_tx_gas(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<revm_interpreter::InitialAndFloorGas, Self::Error> {
        self.mainnet.validate_initial_tx_gas(evm)
    }
}

impl<EVM, ERROR> InspectorHandler for ArcHandler<EVM, ERROR>
where
    EVM: InspectorEvmTr<
        Context: ContextTr<Journal: JournalTr<State = EvmState>>,
        Frame = EthFrame<EthInterpreter>,
        Inspector: Inspector<<<Self as Handler>::Evm as EvmTr>::Context, EthInterpreter>,
    >,
    ERROR: EvmTrError<EVM>,
{
    type IT = EthInterpreter;
}

/// Arc-specific EVM wrapper.
///
/// Native value-transfer validation must happen before REVM initializes each
/// frame. Keeping it here preserves journal checkpoints and transfer-log
/// ordering for calls, creates, and precompile calls.
pub(crate) struct ArcEvm<CTX, INSP, I, P> {
    inner: Evm<CTX, INSP, I, P, EthFrame<EthInterpreter>>,
    call_from_continuations: HashMap<usize, CallFromContinuation>,
    hardfork: ArcHardfork,
}

impl<CTX, I, P> ArcEvm<CTX, (), I, P>
where
    EthFrame<EthInterpreter>: Default,
{
    pub(crate) fn new(
        context: CTX,
        hardfork: ArcHardfork,
        instructions: I,
        precompiles: P,
    ) -> Self {
        Self {
            inner: Evm::new(context, instructions, precompiles),
            call_from_continuations: HashMap::new(),
            hardfork,
        }
    }
}

impl<CTX, INSP, I, P> ArcEvm<CTX, INSP, I, P>
where
    EthFrame<EthInterpreter>: Default,
{
    pub(crate) fn new_with_inspector(
        context: CTX,
        inspector: INSP,
        hardfork: ArcHardfork,
        instructions: I,
        precompiles: P,
    ) -> Self {
        Self {
            inner: Evm::new_with_inspector(context, inspector, instructions, precompiles),
            call_from_continuations: HashMap::new(),
            hardfork,
        }
    }
}

impl<CTX, INSP, I, P> ArcEvm<CTX, INSP, I, P>
where
    CTX: ContextTr,
    I: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    fn create_transfer(
        &mut self,
        inputs: &mut revm_interpreter::CreateInputs,
        depth: usize,
    ) -> Result<Option<(Address, Address, U256)>, ContextDbError<CTX>> {
        if inputs.value().is_zero() {
            return Ok(None);
        }

        let created = match inputs.scheme() {
            CreateScheme::Create => {
                let nonce = if depth == 0 {
                    self.inner.ctx.tx().nonce()
                } else {
                    self.inner
                        .ctx
                        .journal_mut()
                        .load_account(inputs.caller())?
                        .info
                        .nonce
                };
                inputs.created_address(nonce)
            }
            CreateScheme::Create2 { .. } => {
                let address = inputs.created_address(0);
                inputs.set_scheme(CreateScheme::Custom { address });
                address
            }
            CreateScheme::Custom { address } => address,
        };

        Ok(Some((inputs.caller(), created, inputs.value())))
    }

    fn before_frame_init(
        &mut self,
        frame_init: &mut FrameInit,
    ) -> Result<BeforeFrameInit, ContextDbError<CTX>> {
        if frame_init.depth == 0 {
            let caller = match &frame_init.frame_input {
                FrameInput::Call(inputs) => inputs.caller,
                FrameInput::Create(inputs) => inputs.caller(),
                FrameInput::Empty => return Ok(BeforeFrameInit::None),
            };
            if is_blocklisted(&mut self.inner.ctx, caller)? {
                return Ok(BeforeFrameInit::Reverted(create_revert_result(
                    frame_init,
                    ERR_BLOCKED_ADDRESS,
                )));
            }
        }

        let transfer = match &mut frame_init.frame_input {
            FrameInput::Call(inputs) if inputs.scheme == CallScheme::Call => Some((
                inputs.transfer_from(),
                inputs.transfer_to(),
                inputs.transfer_value().unwrap_or(U256::ZERO),
            )),
            FrameInput::Create(inputs) => self.create_transfer(inputs, frame_init.depth)?,
            FrameInput::Call(_) | FrameInput::Empty => None,
        };

        let Some((from, to, amount)) = transfer.filter(|(_, _, amount)| !amount.is_zero()) else {
            return Ok(BeforeFrameInit::None);
        };

        if from == Address::ZERO || to == Address::ZERO {
            return Ok(BeforeFrameInit::Reverted(create_revert_result(
                frame_init,
                ERR_ZERO_ADDRESS,
            )));
        }
        if is_blocklisted(&mut self.inner.ctx, from)? || is_blocklisted(&mut self.inner.ctx, to)? {
            return Ok(BeforeFrameInit::Reverted(create_revert_result(
                frame_init,
                ERR_BLOCKED_ADDRESS,
            )));
        }
        if self
            .inner
            .ctx
            .journal_mut()
            .load_account(to)?
            .is_selfdestructed()
        {
            return Ok(BeforeFrameInit::Reverted(create_revert_result(
                frame_init,
                ERR_SELFDESTRUCTED_BALANCE_INCREASED,
            )));
        }
        if from == to {
            return Ok(BeforeFrameInit::None);
        }

        Ok(if self.hardfork.is_zero5() {
            BeforeFrameInit::Log(arc_transfer_log(from, to, amount))
        } else {
            BeforeFrameInit::None
        })
    }

    fn checked_frame_init(
        &mut self,
        mut frame_init: FrameInit,
    ) -> Result<FrameInitOutcome, ContextDbError<CTX>> {
        let transfer_log = match self.before_frame_init(&mut frame_init)? {
            BeforeFrameInit::Log(log) => Some(log),
            BeforeFrameInit::Reverted(result) => return Ok(FrameInitOutcome::Immediate(result)),
            BeforeFrameInit::None => None,
        };

        if is_precompile_call(&frame_init, &self.inner.precompiles) {
            let checkpoint = transfer_log.map(|log| {
                let checkpoint = self.inner.ctx.journal_mut().checkpoint();
                self.inner.ctx.journal_mut().log(log);
                checkpoint
            });
            let result = init_frame(
                &mut self.inner.frame_stack,
                &mut self.inner.ctx,
                &mut self.inner.precompiles,
                frame_init,
            )?;
            if let Some(checkpoint) = checkpoint {
                if should_emit_transfer_log(&result) {
                    self.inner.ctx.journal_mut().checkpoint_commit();
                } else {
                    self.inner.ctx.journal_mut().checkpoint_revert(checkpoint);
                }
            }
            Ok(frame_init_outcome(result))
        } else {
            let result = init_frame(
                &mut self.inner.frame_stack,
                &mut self.inner.ctx,
                &mut self.inner.precompiles,
                frame_init,
            )?;
            if should_emit_transfer_log(&result)
                && let Some(log) = transfer_log
            {
                self.inner.ctx.journal_mut().log(log);
            }
            Ok(frame_init_outcome(result))
        }
    }

    fn init_call_from(
        &mut self,
        frame_init: FrameInit,
    ) -> Result<FrameInitOutcome, ContextDbError<CTX>> {
        let FrameInput::Call(inputs) = &frame_init.frame_input else {
            unreachable!("CallFrom only accepts call frames");
        };
        if !matches!(
            inputs.caller,
            ARC_MEMO_ADDRESS | ARC_MULTICALL3_FROM_ADDRESS
        ) {
            return Ok(FrameInitOutcome::Immediate(create_revert_result(
                &frame_init,
                "unauthorized caller",
            )));
        }
        if inputs.scheme != CallScheme::Call {
            return Ok(FrameInitOutcome::Immediate(create_revert_result(
                &frame_init,
                "subcall precompiles only support CALL scheme",
            )));
        }
        if inputs.is_static {
            return Ok(FrameInitOutcome::Immediate(create_halt_result(
                &frame_init,
                InstructionResult::StateChangeDuringStaticCall,
            )));
        }
        if inputs.transfers_value() {
            return Ok(FrameInitOutcome::Immediate(create_revert_result(
                &frame_init,
                "subcall precompiles do not support value transfers",
            )));
        }

        let calldata = inputs.input.bytes(&self.inner.ctx);
        let Ok(call) = arcCallFromCall::abi_decode(&calldata) else {
            return Ok(FrameInitOutcome::Immediate(create_revert_result(
                &frame_init,
                "callFrom ABI decode failed",
            )));
        };
        if call.sender != inputs.caller && call.sender != self.inner.ctx.tx().caller() {
            return Ok(FrameInitOutcome::Immediate(create_revert_result(
                &frame_init,
                "sender spoofing requires tx.origin as sender",
            )));
        }

        let mut gas = Gas::new(inputs.gas_limit);
        let decode_gas = CALL_FROM_ABI_GAS.saturating_add(
            u64::try_from(call.data.len())
                .unwrap_or(u64::MAX)
                .div_ceil(32)
                .saturating_mul(revm_interpreter::gas::COPY),
        );
        if !gas.record_regular_cost(decode_gas) {
            gas.spend_all();
            return Ok(FrameInitOutcome::Immediate(create_call_result(
                inputs,
                InstructionResult::PrecompileOOG,
                Bytes::new(),
                gas,
            )));
        }

        self.inner.ctx.journal_mut().load_account(call.sender)?;
        let target = self
            .inner
            .ctx
            .journal_mut()
            .load_account_with_code(call.target)?;
        let access_gas = if target.is_cold {
            revm_interpreter::gas::COLD_ACCOUNT_ACCESS_COST
        } else {
            revm_interpreter::gas::WARM_STORAGE_READ_COST
        };
        if !gas.record_regular_cost(access_gas) {
            gas.spend_all();
            return Ok(FrameInitOutcome::Immediate(create_call_result(
                inputs,
                InstructionResult::PrecompileOOG,
                Bytes::new(),
                gas,
            )));
        }
        let mut bytecode_hash = target.info.code_hash;
        let mut bytecode = target.info.code.clone().unwrap_or_default();
        if let Some(delegate) = bytecode.eip7702_address() {
            let delegate = self
                .inner
                .ctx
                .journal_mut()
                .load_account_with_code(delegate)?;
            let delegate_access_gas = if delegate.is_cold {
                revm_interpreter::gas::COLD_ACCOUNT_ACCESS_COST
            } else {
                revm_interpreter::gas::WARM_STORAGE_READ_COST
            };
            if !gas.record_regular_cost(delegate_access_gas) {
                gas.spend_all();
                return Ok(FrameInitOutcome::Immediate(create_call_result(
                    inputs,
                    InstructionResult::PrecompileOOG,
                    Bytes::new(),
                    gas,
                )));
            }
            bytecode_hash = delegate.info.code_hash;
            bytecode = delegate.info.code.clone().unwrap_or_default();
        }

        let checkpoint = self.inner.ctx.journal_mut().checkpoint();
        self.inner.ctx.journal_mut().checkpoint_commit();
        let depth = frame_init.depth;
        let child_gas = gas.remaining() - gas.remaining() / 64;
        let child = FrameInit {
            depth: depth.saturating_add(1),
            memory: frame_init.memory,
            frame_input: FrameInput::Call(Box::new(revm_interpreter::CallInputs {
                input: CallInput::Bytes(call.data),
                return_memory_offset: 0..0,
                gas_limit: child_gas,
                reservoir: inputs.reservoir,
                bytecode_address: call.target,
                known_bytecode: (bytecode_hash, bytecode),
                target_address: call.target,
                caller: call.sender,
                value: CallValue::Transfer(U256::ZERO),
                scheme: CallScheme::Call,
                is_static: false,
                charged_new_account_state_gas: false,
            })),
        };
        let continuation = CallFromContinuation {
            gas_limit: inputs.gas_limit,
            initial_gas: gas.total_gas_spent(),
            return_memory_offset: inputs.return_memory_offset.clone(),
            checkpoint,
        };

        match self.checked_frame_init(child)? {
            FrameInitOutcome::Pushed => {
                self.call_from_continuations.insert(depth, continuation);
                Ok(FrameInitOutcome::Pushed)
            }
            FrameInitOutcome::Immediate(result) => self
                .complete_call_from(result, continuation)
                .map(FrameInitOutcome::Immediate),
        }
    }

    fn complete_call_from(
        &mut self,
        child_result: FrameResult,
        continuation: CallFromContinuation,
    ) -> Result<FrameResult, ContextDbError<CTX>> {
        let child_gas = *child_result.gas();
        let (success, output, halted) = match child_result {
            FrameResult::Call(outcome) => (
                outcome.result.result.is_ok(),
                outcome.result.output,
                !outcome.result.result.is_ok_or_revert(),
            ),
            FrameResult::Create(_) => (false, Bytes::new(), true),
        };
        let encoded = arcCallFromCall::abi_encode_returns(&arcCallFromReturn {
            success,
            returnData: output,
        });
        let completion_gas = CALL_FROM_ABI_GAS.saturating_add(
            u64::try_from(encoded.len())
                .unwrap_or(u64::MAX)
                .div_ceil(32)
                .saturating_mul(revm_interpreter::gas::COPY),
        );
        let metered = continuation
            .initial_gas
            .saturating_add(child_gas.total_gas_spent())
            .saturating_add(completion_gas);
        let mut gas = Gas::new(continuation.gas_limit);
        let gas_used = if halted {
            continuation.gas_limit.max(metered)
        } else {
            metered
        };
        if !gas.record_regular_cost(gas_used) {
            gas.spend_all();
            if success {
                self.revert_call_from_checkpoint(continuation.checkpoint);
            }
            return Ok(FrameResult::Call(CallOutcome {
                result: InterpreterResult::new(InstructionResult::PrecompileOOG, Bytes::new(), gas),
                memory_offset: continuation.return_memory_offset,
                was_precompile_called: true,
                precompile_call_logs: Vec::new(),
                charged_new_account_state_gas: false,
            }));
        }
        if success {
            gas.record_refund(child_gas.refunded());
        }
        Ok(FrameResult::Call(CallOutcome {
            result: InterpreterResult::new(InstructionResult::Return, encoded.into(), gas),
            memory_offset: continuation.return_memory_offset,
            was_precompile_called: true,
            precompile_call_logs: Vec::new(),
            charged_new_account_state_gas: false,
        }))
    }

    fn revert_call_from_checkpoint(&mut self, checkpoint: JournalCheckpoint) {
        self.inner.ctx.journal_mut().checkpoint();
        self.inner.ctx.journal_mut().checkpoint_revert(checkpoint);
    }
}

impl<CTX, INSP, I, P> EvmTr for ArcEvm<CTX, INSP, I, P>
where
    CTX: ContextTr,
    I: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    type Context = CTX;
    type Instructions = I;
    type Precompiles = P;
    type Frame = EthFrame<EthInterpreter>;

    fn all(
        &self,
    ) -> (
        &Self::Context,
        &Self::Instructions,
        &Self::Precompiles,
        &revm_context_interface::FrameStack<Self::Frame>,
    ) {
        self.inner.all()
    }

    fn all_mut(
        &mut self,
    ) -> (
        &mut Self::Context,
        &mut Self::Instructions,
        &mut Self::Precompiles,
        &mut revm_context_interface::FrameStack<Self::Frame>,
    ) {
        self.inner.all_mut()
    }

    fn frame_init(
        &mut self,
        frame_init: FrameInit,
    ) -> Result<FrameInitResult<'_, Self::Frame>, ContextDbError<Self::Context>> {
        let is_call_from = self.hardfork.is_zero7()
            && matches!(
                &frame_init.frame_input,
                FrameInput::Call(inputs) if inputs.bytecode_address == ARC_CALL_FROM_ADDRESS
            );
        let outcome = if is_call_from {
            self.init_call_from(frame_init)?
        } else {
            self.checked_frame_init(frame_init)?
        };
        match outcome {
            FrameInitOutcome::Pushed => Ok(ItemOrResult::Item(self.inner.frame_stack.get())),
            FrameInitOutcome::Immediate(result) => Ok(ItemOrResult::Result(result)),
        }
    }

    fn frame_run(
        &mut self,
    ) -> Result<FrameInitOrResult<Self::Frame>, ContextDbError<Self::Context>> {
        self.inner.frame_run()
    }

    fn frame_return_result(
        &mut self,
        result: FrameResult,
    ) -> Result<Option<FrameResult>, ContextDbError<Self::Context>> {
        let frame_finished = self.inner.frame_stack.get().is_finished();
        let finished_depth = self.inner.frame_stack.get().depth;
        if frame_finished {
            self.inner.frame_stack.pop();
        }
        let stack_empty = self.inner.frame_stack.index().is_none();

        if frame_finished
            && let Some(depth) = finished_depth.checked_sub(1)
            && let Some(continuation) = self.call_from_continuations.remove(&depth)
        {
            let result = self.complete_call_from(result, continuation)?;
            if stack_empty {
                return Ok(Some(result));
            }
            self.inner
                .frame_stack
                .get()
                .return_result::<_, ContextDbError<CTX>>(&mut self.inner.ctx, result)?;
            return Ok(None);
        }
        if stack_empty {
            return Ok(Some(result));
        }
        self.inner
            .frame_stack
            .get()
            .return_result::<_, ContextDbError<CTX>>(&mut self.inner.ctx, result)?;
        Ok(None)
    }
}

impl<CTX, INSP, I, P> InspectorEvmTr for ArcEvm<CTX, INSP, I, P>
where
    CTX: ContextTr<Journal: JournalExt>,
    INSP: Inspector<CTX>,
    I: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    type Inspector = INSP;

    fn all_inspector(
        &self,
    ) -> (
        &Self::Context,
        &Self::Instructions,
        &Self::Precompiles,
        &revm_context_interface::FrameStack<Self::Frame>,
        &Self::Inspector,
    ) {
        let (context, instructions, precompiles, frames) = self.inner.all();
        (
            context,
            instructions,
            precompiles,
            frames,
            &self.inner.inspector,
        )
    }

    fn all_mut_inspector(
        &mut self,
    ) -> (
        &mut Self::Context,
        &mut Self::Instructions,
        &mut Self::Precompiles,
        &mut revm_context_interface::FrameStack<Self::Frame>,
        &mut Self::Inspector,
    ) {
        let Evm {
            ctx,
            instruction,
            precompiles,
            frame_stack,
            inspector,
        } = &mut self.inner;
        (ctx, instruction, precompiles, frame_stack, inspector)
    }

    fn inspect_frame_init(
        &mut self,
        mut frame_init: FrameInit,
    ) -> Result<FrameInitResult<'_, Self::Frame>, ContextDbError<Self::Context>> {
        use revm_inspector::handler::{frame_end, frame_start};

        let mut trace_input = match &frame_init.frame_input {
            FrameInput::Call(inputs) if inputs.bytecode_address == ARC_CALL_FROM_ADDRESS => {
                let calldata = inputs.input.bytes(&self.inner.ctx);
                arcCallFromCall::abi_decode(&calldata).ok().map(|call| {
                    let mut child = inputs.as_ref().clone();
                    child.input = CallInput::Bytes(call.data);
                    child.return_memory_offset = 0..0;
                    child.bytecode_address = call.target;
                    child.target_address = call.target;
                    child.caller = call.sender;
                    child.value = CallValue::Transfer(U256::ZERO);
                    child.scheme = CallScheme::Call;
                    child.is_static = false;
                    FrameInput::Call(Box::new(child))
                })
            }
            FrameInput::Call(_) | FrameInput::Create(_) | FrameInput::Empty => None,
        };

        let frame_input = if let Some(trace_input) = trace_input.as_mut() {
            let (context, inspector) = self.ctx_inspector();
            if let Some(mut output) = frame_start(context, inspector, trace_input) {
                frame_end(context, inspector, trace_input, &mut output);
                return Ok(ItemOrResult::Result(output));
            }
            trace_input.clone()
        } else {
            let (context, inspector) = self.ctx_inspector();
            if let Some(mut output) = frame_start(context, inspector, &mut frame_init.frame_input) {
                frame_end(context, inspector, &frame_init.frame_input, &mut output);
                return Ok(ItemOrResult::Result(output));
            }
            frame_init.frame_input.clone()
        };

        let logs_index = self.inner.ctx.journal().logs().len();
        if let ItemOrResult::Result(mut output) = self.frame_init(frame_init)? {
            let (context, inspector) = self.ctx_inspector();
            if let FrameResult::Call(CallOutcome {
                was_precompile_called,
                precompile_call_logs,
                ..
            }) = &mut output
                && *was_precompile_called
            {
                let logs = context
                    .journal_mut()
                    .logs()
                    .get(logs_index..)
                    .unwrap_or_default()
                    .to_vec();
                for log in logs.into_iter().chain(precompile_call_logs.iter().cloned()) {
                    inspector.log(context, log);
                }
            }
            frame_end(context, inspector, &frame_input, &mut output);
            return Ok(ItemOrResult::Result(output));
        }

        let (context, inspector, frame) = self.ctx_inspector_frame();
        if let Some(frame) = frame.eth_frame() {
            inspector.initialize_interp(&mut frame.interpreter, context);
        }
        Ok(ItemOrResult::Item(frame))
    }
}

impl<CTX, INSP, I, P> ExecuteEvm for ArcEvm<CTX, INSP, I, P>
where
    CTX: ContextTr<Journal: JournalTr<State = EvmState>> + ContextSetters,
    I: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    type ExecutionResult = ExecutionResult<HaltReason>;
    type State = EvmState;
    type Error = EVMError<<CTX::Db as Database>::Error, InvalidTransaction>;
    type Tx = CTX::Tx;
    type Block = CTX::Block;

    fn transact_one(
        &mut self,
        transaction: Self::Tx,
    ) -> Result<Self::ExecutionResult, Self::Error> {
        self.call_from_continuations.clear();
        self.inner.ctx.set_tx(transaction);
        let result = ArcHandler::new(self.hardfork).run(self);
        self.call_from_continuations.clear();
        result
    }

    fn finalize(&mut self) -> Self::State {
        self.inner.ctx.journal_mut().finalize()
    }

    fn set_block(&mut self, block: Self::Block) {
        self.inner.ctx.set_block(block);
    }

    fn replay(&mut self) -> Result<ResultAndState<HaltReason>, Self::Error> {
        self.call_from_continuations.clear();
        let result = ArcHandler::new(self.hardfork).run(self);
        self.call_from_continuations.clear();
        result.map(|result| {
            let state = self.finalize();
            ResultAndState::new(result, state)
        })
    }
}

impl<CTX, INSP, I, P> InspectEvm for ArcEvm<CTX, INSP, I, P>
where
    CTX: ContextTr<Journal: JournalTr<State = EvmState> + JournalExt> + ContextSetters,
    INSP: Inspector<CTX, EthInterpreter>,
    I: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    type Inspector = INSP;

    fn set_inspector(&mut self, inspector: Self::Inspector) {
        self.inner.inspector = inspector;
    }

    fn inspect_one_tx(
        &mut self,
        transaction: Self::Tx,
    ) -> Result<Self::ExecutionResult, Self::Error> {
        self.call_from_continuations.clear();
        self.inner.ctx.set_tx(transaction);
        let result = ArcHandler::new(self.hardfork).inspect_run(self);
        self.call_from_continuations.clear();
        result
    }
}

fn init_frame<'frame, CTX, P>(
    frames: &'frame mut revm_context_interface::FrameStack<EthFrame<EthInterpreter>>,
    context: &mut CTX,
    precompiles: &mut P,
    frame_init: FrameInit,
) -> Result<FrameInitResult<'frame, EthFrame<EthInterpreter>>, ContextDbError<CTX>>
where
    CTX: ContextTr,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    let first = frames.index().is_none();
    let frame = if first {
        frames.start_init()
    } else {
        frames.get_next()
    };
    let result = EthFrame::init_with_context(frame, context, precompiles, frame_init)?;

    Ok(result.map_item(|token| {
        if first {
            unsafe { frames.end_init(token) };
        } else {
            unsafe { frames.push(token) };
        }
        frames.get()
    }))
}

fn frame_init_outcome(result: FrameInitResult<'_, EthFrame<EthInterpreter>>) -> FrameInitOutcome {
    match result {
        ItemOrResult::Item(_) => FrameInitOutcome::Pushed,
        ItemOrResult::Result(result) => FrameInitOutcome::Immediate(result),
    }
}

fn is_precompile_call<CTX: ContextTr>(
    frame_init: &FrameInit,
    precompiles: &impl PrecompileProvider<CTX, Output = InterpreterResult>,
) -> bool {
    match &frame_init.frame_input {
        FrameInput::Call(inputs) => precompiles.contains(&inputs.bytecode_address),
        FrameInput::Create(_) | FrameInput::Empty => false,
    }
}

fn should_emit_transfer_log<T>(result: &ItemOrResult<T, FrameResult>) -> bool {
    match result {
        ItemOrResult::Item(_) => true,
        ItemOrResult::Result(FrameResult::Call(outcome)) => outcome.instruction_result().is_ok(),
        ItemOrResult::Result(FrameResult::Create(outcome)) => {
            outcome.instruction_result().is_ok() && outcome.address.is_some()
        }
    }
}

fn create_revert_result(frame_init: &FrameInit, message: &str) -> FrameResult {
    let output: Bytes = Revert::from(message).abi_encode().into();
    match &frame_init.frame_input {
        FrameInput::Call(inputs) => FrameResult::Call(CallOutcome {
            result: revm_interpreter::InterpreterResult::new(
                InstructionResult::Revert,
                output,
                Gas::new(inputs.gas_limit),
            ),
            memory_offset: inputs.return_memory_offset.clone(),
            was_precompile_called: false,
            precompile_call_logs: Vec::default(),
            charged_new_account_state_gas: inputs.charged_new_account_state_gas,
        }),
        FrameInput::Create(inputs) => FrameResult::Create(CreateOutcome {
            result: revm_interpreter::InterpreterResult::new(
                InstructionResult::Revert,
                output,
                Gas::new(inputs.gas_limit()),
            ),
            address: None,
        }),
        FrameInput::Empty => unreachable!("empty frames are never executed"),
    }
}

fn create_halt_result(frame_init: &FrameInit, result: InstructionResult) -> FrameResult {
    match &frame_init.frame_input {
        FrameInput::Call(inputs) => {
            let mut gas = Gas::new(inputs.gas_limit);
            gas.spend_all();
            create_call_result(inputs, result, Bytes::new(), gas)
        }
        FrameInput::Create(_) | FrameInput::Empty => unreachable!("CallFrom only accepts calls"),
    }
}

fn create_call_result(
    inputs: &revm_interpreter::CallInputs,
    result: InstructionResult,
    output: Bytes,
    gas: Gas,
) -> FrameResult {
    FrameResult::Call(CallOutcome {
        result: InterpreterResult::new(result, output, gas),
        memory_offset: inputs.return_memory_offset.clone(),
        was_precompile_called: true,
        precompile_call_logs: Vec::new(),
        charged_new_account_state_gas: inputs.charged_new_account_state_gas,
    })
}

fn is_blocklisted<CTX: ContextTr>(
    context: &mut CTX,
    account: Address,
) -> Result<bool, ContextDbError<CTX>> {
    context
        .journal_mut()
        .load_account(ARC_NATIVE_COIN_CONTROL_ADDRESS)?;
    Ok(context
        .journal_mut()
        .sload(ARC_NATIVE_COIN_CONTROL_ADDRESS, arc_blocklist_slot(account))?
        .data
        != U256::ZERO)
}
