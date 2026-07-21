use std::{
    fmt,
    hash::{BuildHasher, Hash, Hasher},
};

use alloy_primitives::{map::DefaultHashBuilder, Address, U256};
use revm::{
    bytecode::opcode,
    context::ContextTr,
    interpreter::{
        interpreter_types::{InputsTr, Jumps},
        Interpreter,
    },
    Inspector,
};

/// Maximum number of edges tracked by the fuzzing coverage map.
const MAX_EDGE_COUNT: usize = 65_536;

/// Tracks branch edges for invariant fuzzing.
///
/// This preserves the fixed-size coverage representation used by the existing
/// fuzzing executor after the inspector was removed from `revm-inspectors`.
#[derive(Clone)]
pub struct EdgeCovInspector {
    hitcount: Vec<u8>,
    hash_builder: DefaultHashBuilder,
}

impl fmt::Debug for EdgeCovInspector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EdgeCovInspector").finish_non_exhaustive()
    }
}

impl EdgeCovInspector {
    pub fn new() -> Self {
        Self {
            hitcount: vec![0; MAX_EDGE_COUNT],
            hash_builder: DefaultHashBuilder::default(),
        }
    }

    pub fn reset(&mut self) {
        self.hitcount.fill(0);
    }

    pub fn get_hitcount(&self) -> &[u8] {
        &self.hitcount
    }

    pub fn into_hitcount(self) -> Vec<u8> {
        self.hitcount
    }

    fn store_hit(&mut self, address: Address, pc: usize, jump_dest: U256) {
        let mut hasher = self.hash_builder.build_hasher();
        address.hash(&mut hasher);
        pc.hash(&mut hasher);
        jump_dest.hash(&mut hasher);

        let edge_id = (hasher.finish() % MAX_EDGE_COUNT as u64) as usize;
        self.hitcount[edge_id] = self.hitcount[edge_id].checked_add(1).unwrap_or(1);
    }

    #[cold]
    fn do_step(&mut self, interpreter: &mut Interpreter) {
        let address = interpreter.input.target_address();
        let current_pc = interpreter.bytecode.pc();

        match interpreter.bytecode.opcode() {
            opcode::JUMP => {
                if let Ok(jump_dest) = interpreter.stack.peek(0) {
                    self.store_hit(address, current_pc, jump_dest);
                }
            }
            opcode::JUMPI => {
                if let Ok(condition) = interpreter.stack.peek(1) {
                    let jump_dest = if condition.is_zero() {
                        Ok(U256::from(current_pc + 1))
                    } else {
                        interpreter.stack.peek(0)
                    };

                    if let Ok(jump_dest) = jump_dest {
                        self.store_hit(address, current_pc, jump_dest);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Default for EdgeCovInspector {
    fn default() -> Self {
        Self::new()
    }
}

impl<ContextT> Inspector<ContextT> for EdgeCovInspector
where
    ContextT: ContextTr,
{
    #[inline]
    fn step(&mut self, interpreter: &mut Interpreter, _context: &mut ContextT) {
        if matches!(interpreter.bytecode.opcode(), opcode::JUMP | opcode::JUMPI) {
            self.do_step(interpreter);
        }
    }
}
