//! Types for EVM precompiles.
#![warn(missing_docs)]

use std::marker::PhantomData;

use alloy_primitives::Log;
use edr_chain_config::NativeTokenMirror;
use edr_primitives::{keccak256, Address, Bytes, HashMap, HashSet, B256, U256};
use revm_context_interface::{Cfg, ContextTr as ContextTrait, JournalTr as _, LocalContextTr as _};
pub use revm_handler::{EthPrecompiles, PrecompileProvider};
use revm_interpreter::{CallInput, CallInputs, Gas, InstructionResult, InterpreterResult};
pub use revm_precompile::{
    secp256r1, u64_to_address, Precompile, PrecompileError, PrecompileFn, PrecompileSpecId,
    Precompiles,
};

/// A precompile provider that allows adding custom or overwriting existing
/// precompiles.
#[derive(Clone)]
pub struct OverriddenPrecompileProvider<
    BaseProviderT: PrecompileProvider<ContextT, Output = InterpreterResult>,
    ContextT: ContextTrait,
> {
    base: BaseProviderT,
    custom_precompiles: HashMap<Address, PrecompileFn>,
    native_token_mirror: Option<NativeTokenMirror>,
    // Cache of unique addresses to avoid reporting duplicates between `base` and
    // `custom_precompiles`. This speeds up the `warm_addresses` method.
    unique_addresses: HashSet<Address>,
    phantom: PhantomData<ContextT>,
}

impl<
        BaseProviderT: PrecompileProvider<ContextT, Output = InterpreterResult>,
        ContextT: ContextTrait,
    > OverriddenPrecompileProvider<BaseProviderT, ContextT>
{
    /// Creates a new custom precompile provider.
    pub fn new(base: BaseProviderT) -> Self {
        Self::with_precompiles(base, HashMap::default())
    }

    /// Creates a new custom precompile provider with custom precompiles.
    pub fn with_precompiles(
        base: BaseProviderT,
        custom_precompiles: HashMap<Address, PrecompileFn>,
    ) -> Self {
        Self::with_precompiles_and_native_token_mirror(base, custom_precompiles, None)
    }

    /// Creates a new custom precompile provider with custom precompiles and a
    /// native token mirror precompile.
    pub fn with_precompiles_and_native_token_mirror(
        base: BaseProviderT,
        custom_precompiles: HashMap<Address, PrecompileFn>,
        native_token_mirror: Option<NativeTokenMirror>,
    ) -> Self {
        let unique_addresses =
            unique_addresses(&base, &custom_precompiles, native_token_mirror.as_ref());

        Self {
            base,
            custom_precompiles,
            native_token_mirror,
            unique_addresses,
            phantom: PhantomData,
        }
    }

    /// Consumes the provider and returns the set of all unique precompile
    /// addresses.
    pub fn into_addresses(self) -> HashSet<Address> {
        self.unique_addresses
    }

    /// Adds a custom precompile.
    pub fn set_precompile(&mut self, address: Address, precompile: PrecompileFn) {
        self.custom_precompiles.insert(address, precompile);
        self.unique_addresses.insert(address);
    }
}

impl<
        BaseProviderT: PrecompileProvider<ContextT, Output = InterpreterResult>,
        ContextT: ContextTrait,
    > PrecompileProvider<ContextT> for OverriddenPrecompileProvider<BaseProviderT, ContextT>
{
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: <ContextT::Cfg as Cfg>::Spec) -> bool {
        let changed = self.base.set_spec(spec);
        if changed {
            // Update unique addresses
            self.unique_addresses = unique_addresses(
                &self.base,
                &self.custom_precompiles,
                self.native_token_mirror.as_ref(),
            );
        }

        changed
    }

    fn run(
        &mut self,
        context: &mut ContextT,
        inputs: &CallInputs,
    ) -> Result<Option<Self::Output>, String> {
        if let Some(native_token_mirror) = &self.native_token_mirror
            && inputs.bytecode_address == native_token_mirror.token
        {
            return run_native_token_mirror(context, inputs, native_token_mirror).map(Some);
        }

        let Some(precompile) = self.custom_precompiles.get(&inputs.bytecode_address) else {
            return self.base.run(context, inputs);
        };

        let mut result = InterpreterResult {
            result: InstructionResult::Return,
            gas: Gas::new(inputs.gas_limit),
            output: Bytes::new(),
        };

        let exec_result = {
            let r;
            let input_bytes = match &inputs.input {
                CallInput::SharedBuffer(range) => {
                    if let Some(slice) = context.local().shared_memory_buffer_slice(range.clone()) {
                        r = slice;
                        r.as_ref()
                    } else {
                        &[]
                    }
                }
                CallInput::Bytes(bytes) => bytes.0.iter().as_slice(),
            };
            (*precompile)(input_bytes, inputs.gas_limit)
        };

        match exec_result {
            Ok(output) => {
                let underflow = result.gas.record_cost(output.gas_used);
                assert!(underflow, "Gas underflow is not possible");
                result.result = if output.reverted {
                    InstructionResult::Revert
                } else {
                    InstructionResult::Return
                };
                result.output = output.bytes;
            }
            Err(PrecompileError::Fatal(e)) => return Err(e),
            Err(e) => {
                result.result = if e.is_oog() {
                    InstructionResult::PrecompileOOG
                } else {
                    InstructionResult::PrecompileError
                };
                // If this is a top-level precompile call (depth == 1), persist the error
                // message into the local context so it can be returned as
                // output in the final result. Only do this for non-OOG errors
                // (OOG is a distinct halt reason without output).
                if !e.is_oog() && context.journal().depth() == 1 {
                    context
                        .local_mut()
                        .set_precompile_error_context(e.to_string());
                }
            }
        }
        Ok(Some(result))
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        Box::new(self.unique_addresses.iter().cloned())
    }

    fn contains(&self, address: &Address) -> bool {
        self.unique_addresses.contains(address)
    }
}

fn unique_addresses<BaseProviderT, ContextT>(
    base: &BaseProviderT,
    custom_precompiles: &HashMap<Address, PrecompileFn>,
    native_token_mirror: Option<&NativeTokenMirror>,
) -> HashSet<Address>
where
    BaseProviderT: PrecompileProvider<ContextT, Output = InterpreterResult>,
    ContextT: ContextTrait,
{
    custom_precompiles
        .keys()
        .cloned()
        .chain(native_token_mirror.map(|mirror| mirror.token))
        .chain(base.warm_addresses())
        .collect()
}

const BALANCE_OF_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];
const TRANSFER_SELECTOR: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
const TRANSFER_FROM_SELECTOR: [u8; 4] = [0x23, 0xb8, 0x72, 0xdd];
const APPROVE_SELECTOR: [u8; 4] = [0x09, 0x5e, 0xa7, 0xb3];
const ALLOWANCE_SELECTOR: [u8; 4] = [0xdd, 0x62, 0xed, 0x3e];
const DECIMALS_SELECTOR: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];
const TRANSFER_TOPIC: &str = "Transfer(address,address,uint256)";
const APPROVAL_TOPIC: &str = "Approval(address,address,uint256)";

fn run_native_token_mirror<ContextT>(
    context: &mut ContextT,
    inputs: &CallInputs,
    mirror: &NativeTokenMirror,
) -> Result<InterpreterResult, String>
where
    ContextT: ContextTrait,
{
    let calldata = inputs.input.bytes(context);
    let Some(selector) = calldata
        .get(..4)
        .and_then(|selector| selector.try_into().ok())
    else {
        return Ok(revert(inputs, Bytes::new()));
    };

    if inputs
        .transfer_value()
        .is_some_and(|value| !value.is_zero())
    {
        return Ok(revert(inputs, Bytes::new()));
    }

    match selector {
        BALANCE_OF_SELECTOR => {
            let Some(account) = decode_address(&calldata, 4) else {
                return Ok(revert(inputs, Bytes::new()));
            };
            let balance = context
                .journal_mut()
                .load_account(account)
                .map_err(|error| error.to_string())?
                .data
                .info
                .balance;

            Ok(success(
                inputs,
                encode_u256(mirror.native_to_erc20_balance(balance)),
            ))
        }
        DECIMALS_SELECTOR => Ok(success(inputs, encode_u256(U256::from(mirror.decimals())))),
        ALLOWANCE_SELECTOR => {
            let (Some(owner), Some(spender)) =
                (decode_address(&calldata, 4), decode_address(&calldata, 36))
            else {
                return Ok(revert(inputs, Bytes::new()));
            };

            let allowance = read_storage(
                context,
                mirror.token,
                mirror.allowance_storage_key(owner, spender),
            )?;
            Ok(success(inputs, encode_u256(allowance)))
        }
        APPROVE_SELECTOR => {
            if inputs.is_static {
                return Ok(revert(inputs, Bytes::new()));
            }

            let (Some(spender), Some(amount)) =
                (decode_address(&calldata, 4), decode_u256(&calldata, 36))
            else {
                return Ok(revert(inputs, Bytes::new()));
            };

            write_storage(
                context,
                mirror.token,
                mirror.allowance_storage_key(inputs.caller, spender),
                amount,
            )?;
            log_approval(context, mirror.token, inputs.caller, spender, amount);
            Ok(success(inputs, encode_bool(true)))
        }
        TRANSFER_SELECTOR => {
            if inputs.is_static {
                return Ok(revert(inputs, Bytes::new()));
            }

            let (Some(to), Some(amount)) =
                (decode_address(&calldata, 4), decode_u256(&calldata, 36))
            else {
                return Ok(revert(inputs, Bytes::new()));
            };

            let native_amount = mirror.erc20_to_native_balance(amount);
            if !amount.is_zero() && native_amount.is_zero() {
                return Ok(success(inputs, encode_bool(false)));
            }

            if !transfer_native(context, inputs.caller, to, native_amount)? {
                return Ok(success(inputs, encode_bool(false)));
            }

            log_transfer(context, mirror.token, inputs.caller, to, amount);
            Ok(success(inputs, encode_bool(true)))
        }
        TRANSFER_FROM_SELECTOR => {
            if inputs.is_static {
                return Ok(revert(inputs, Bytes::new()));
            }

            let (Some(from), Some(to), Some(amount)) = (
                decode_address(&calldata, 4),
                decode_address(&calldata, 36),
                decode_u256(&calldata, 68),
            ) else {
                return Ok(revert(inputs, Bytes::new()));
            };

            let allowance_key = mirror.allowance_storage_key(from, inputs.caller);
            let allowance = read_storage(context, mirror.token, allowance_key)?;
            if allowance < amount {
                return Ok(success(inputs, encode_bool(false)));
            }

            let native_amount = mirror.erc20_to_native_balance(amount);
            if !amount.is_zero() && native_amount.is_zero() {
                return Ok(success(inputs, encode_bool(false)));
            }

            if !transfer_native(context, from, to, native_amount)? {
                return Ok(success(inputs, encode_bool(false)));
            }

            write_storage(context, mirror.token, allowance_key, allowance - amount)?;
            log_transfer(context, mirror.token, from, to, amount);
            Ok(success(inputs, encode_bool(true)))
        }
        _ => Ok(revert(inputs, Bytes::new())),
    }
}

fn read_storage<ContextT>(
    context: &mut ContextT,
    address: Address,
    slot: U256,
) -> Result<U256, String>
where
    ContextT: ContextTrait,
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

fn write_storage<ContextT>(
    context: &mut ContextT,
    address: Address,
    slot: U256,
    value: U256,
) -> Result<(), String>
where
    ContextT: ContextTrait,
{
    let mut account = context
        .journal_mut()
        .load_account_mut(address)
        .map_err(|error| error.to_string())?;
    account.data.touch();
    context
        .journal_mut()
        .sstore(address, slot, value)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn transfer_native<ContextT>(
    context: &mut ContextT,
    from: Address,
    to: Address,
    amount: U256,
) -> Result<bool, String>
where
    ContextT: ContextTrait,
{
    if amount.is_zero() || from == to {
        return Ok(true);
    }

    let transfer_error = context
        .journal_mut()
        .transfer(from, to, amount)
        .map_err(|error| error.to_string())?;
    Ok(transfer_error.is_none())
}

fn decode_address(input: &[u8], offset: usize) -> Option<Address> {
    input
        .get(offset..offset + 32)
        .map(|word| Address::from_slice(&word[12..]))
}

fn decode_u256(input: &[u8], offset: usize) -> Option<U256> {
    input.get(offset..offset + 32).map(U256::from_be_slice)
}

fn encode_bool(value: bool) -> Bytes {
    encode_u256(U256::from(value as u8))
}

fn encode_u256(value: U256) -> Bytes {
    Bytes::copy_from_slice(&value.to_be_bytes::<32>())
}

fn log_transfer<ContextT>(
    context: &mut ContextT,
    token: Address,
    from: Address,
    to: Address,
    amount: U256,
) where
    ContextT: ContextTrait,
{
    context.journal_mut().log(Log {
        address: token,
        data: alloy_primitives::LogData::new_unchecked(
            vec![
                keccak256(TRANSFER_TOPIC),
                address_topic(from),
                address_topic(to),
            ],
            encode_u256(amount),
        ),
    });
}

fn log_approval<ContextT>(
    context: &mut ContextT,
    token: Address,
    owner: Address,
    spender: Address,
    amount: U256,
) where
    ContextT: ContextTrait,
{
    context.journal_mut().log(Log {
        address: token,
        data: alloy_primitives::LogData::new_unchecked(
            vec![
                keccak256(APPROVAL_TOPIC),
                address_topic(owner),
                address_topic(spender),
            ],
            encode_u256(amount),
        ),
    });
}

fn address_topic(address: Address) -> B256 {
    let mut topic = [0u8; 32];
    topic[12..].copy_from_slice(address.as_slice());
    B256::from(topic)
}

fn success(inputs: &CallInputs, output: Bytes) -> InterpreterResult {
    InterpreterResult {
        result: InstructionResult::Return,
        gas: Gas::new(inputs.gas_limit),
        output,
    }
}

fn revert(inputs: &CallInputs, output: Bytes) -> InterpreterResult {
    InterpreterResult {
        result: InstructionResult::Revert,
        gas: Gas::new(inputs.gas_limit),
        output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_364_kzg_point_evaluation_present_in_cancun() {
        const KZG_POINT_EVALUATION_ADDRESS: Address = u64_to_address(0x0A);

        let precompiles = Precompiles::cancun();
        assert!(precompiles.contains(&KZG_POINT_EVALUATION_ADDRESS));
    }
}
