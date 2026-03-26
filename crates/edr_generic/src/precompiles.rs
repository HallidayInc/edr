use std::{boxed::Box, iter};

use alloy_primitives::Log;
use alloy_sol_types::{Revert, SolCall, SolError, SolEvent, SolValue, sol};
use revm_context_interface::{Cfg, Transaction as _};
use edr_chain_spec_evm::{
    ContextTrait, Database, InterpreterResult, JournalTrait as _, handler::EthPrecompiles,
};
use edr_primitives::{Address, B256, Bytes, U256, address, keccak256};
use revm_handler::PrecompileProvider;
use revm_interpreter::{CallInputs, Gas, InstructionResult};

const ARBSYS_ADDRESS: Address = address!("0000000000000000000000000000000000000064");
const ARBSYS_STATE_ADDRESS: Address = address!("00000000000000000000000000000000A4B05A11");
const ADDRESS_ALIAS_OFFSET: Address = address!("1111000000000000000000000000000000001111");
const PARTIALS_SLOT_START: u64 = 2;

sol! {
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
}

use self::ArbSys::{
    InvalidBlockNumber, L2ToL1Tx, SendMerkleUpdate, arbBlockHashCall, arbBlockNumberCall,
    arbChainIDCall, arbOSVersionCall, getStorageGasAvailableCall, isTopLevelCallCall,
    mapL1SenderContractAddressToL2AliasCall, myCallersAddressWithoutAliasingCall,
    sendMerkleTreeStateCall, sendMerkleTreeStateReturn, sendTxToL1Call,
    wasMyCallersAddressAliasedCall, withdrawEthCall,
};

/// Arbitrum precompile provider.
///
/// This keeps Ethereum's built-in precompiles and layers in Arbitrum's
/// `ArbSys` precompile at `0x64`.
#[derive(Debug, Clone, Default)]
pub struct ArbPrecompiles {
    inner: EthPrecompiles,
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
        if inputs.bytecode_address != ARBSYS_ADDRESS {
            return self.inner.run(context, inputs);
        }

        run_arbsys(context, inputs).map(Some)
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        Box::new(self.inner.warm_addresses().chain(iter::once(ARBSYS_ADDRESS)))
    }

    fn contains(&self, address: &Address) -> bool {
        *address == ARBSYS_ADDRESS || self.inner.contains(address)
    }
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
            return Ok(revert(inputs, InvalidBlockNumber { requested, current }.abi_encode()));
        }

        let Ok(requested_block_num) = requested.try_into() else {
            return Ok(revert(inputs, InvalidBlockNumber { requested, current }.abi_encode()));
        };

        let block_hash = context.block_hash(requested_block_num).unwrap_or(B256::ZERO);
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

        return Ok(success(inputs, (context.journal().depth() <= 2).abi_encode()));
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
            return Ok(revert_with_message(inputs, "ArbSys: insufficient transferred value"));
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

    rest.iter()
        .copied()
        .fold(*first, |root, partial| keccak256([partial.as_slice(), root.as_slice()].concat()))
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
    context
        .journal_mut()
        .load_account(ARBSYS_STATE_ADDRESS)
        .map_err(|error| error.to_string())?;

    context
        .journal_mut()
        .sload(ARBSYS_STATE_ADDRESS, slot)
        .map(|value| value.data)
        .map_err(|error| error.to_string())
}

fn write_word<ContextT>(context: &mut ContextT, slot: U256, value: U256) -> Result<(), String>
where
    ContextT: ContextTrait,
    ContextT::Db: Database,
{
    let mut account = context
        .journal_mut()
        .load_account_mut(ARBSYS_STATE_ADDRESS)
        .map_err(|error| error.to_string())?;
    account.data.touch();
    if account.data.nonce() == 0 {
        account.data.set_nonce(1);
    }

    context
        .journal_mut()
        .sstore(ARBSYS_STATE_ADDRESS, slot, value)
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
