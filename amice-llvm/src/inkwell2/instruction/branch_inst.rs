use inkwell::basic_block::BasicBlock;
use inkwell::values::{InstructionOpcode, InstructionValue};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Copy, Clone)]
pub struct BranchInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> BranchInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert_eq!(inst.get_opcode(), InstructionOpcode::Br);
        Self { inst }
    }

    pub fn get_successor(&self, idx: u32) -> Option<BasicBlock<'ctx>> {
        if self.inst.get_num_operands() == 1 {
            return self.inst.get_operand(0)?.block()
        }

        assert!(idx < 2);

        self.inst.get_operand(2 - idx)?.block()
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
