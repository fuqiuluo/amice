use inkwell::basic_block::BasicBlock;
use inkwell::values::InstructionValue;
use std::ops::{Deref, DerefMut};

use super::is_branch_opcode;

#[derive(Debug, Copy, Clone)]
pub struct BranchInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> BranchInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert!(is_branch_opcode(inst.get_opcode()));
        Self { inst }
    }

    pub fn get_successor(&self, idx: u32) -> Option<BasicBlock<'ctx>> {
        if self.inst.get_num_operands() == 1 {
            return self.inst.get_operand(0)?.block();
        }

        assert!(idx < 2);

        // Up to LLVM 22 the conditional-branch operands are laid out as
        // [condition, false destination, true destination], the reverse of
        // successor order.
        #[cfg(not(feature = "llvm23-1"))]
        return self.inst.get_operand(2 - idx)?.block();

        // LLVM 23 reorders conditional-branch operands to match successor
        // order: [condition, successor 0, successor 1].
        #[cfg(feature = "llvm23-1")]
        self.inst.get_operand(1 + idx)?.block()
    }
}

impl<'ctx> Deref for BranchInst<'ctx> {
    type Target = InstructionValue<'ctx>;

    fn deref(&self) -> &Self::Target {
        &self.inst
    }
}

impl<'ctx> DerefMut for BranchInst<'ctx> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inst
    }
}

impl<'ctx> From<InstructionValue<'ctx>> for BranchInst<'ctx> {
    fn from(base: InstructionValue<'ctx>) -> Self {
        Self::new(base)
    }
}
