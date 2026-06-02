use crate::ffi;
use crate::inkwell2::LLVMValueRefExt;
use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use inkwell::values::{AsValueRef, BasicValueEnum, InstructionOpcode, InstructionValue};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Copy, Clone)]
pub struct SwitchInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> SwitchInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert_eq!(inst.get_opcode(), InstructionOpcode::Switch);
        Self { inst }
    }

    pub fn get_case_num(&self) -> u32 {
        self.inst.get_num_operands() / 2 - 1
    }

    pub fn get_cases(&self) -> Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> {
        let mut cases = Vec::new();
        for i in (0..self.get_case_num()).step_by(1) {
            let case_value = self.inst.get_operand(i * 2 + 2);
            let case_block = self.inst.get_operand(i * 2 + 3);
            assert!(case_value.is_some());
            assert!(case_block.is_some());
            cases.push((
                case_value.unwrap().value().unwrap(),
                case_block.unwrap().block().unwrap(),
            ));
        }
        cases
    }

    pub fn get_condition(&self) -> BasicValueEnum<'ctx> {
        self.inst.get_operand(0).unwrap().value().unwrap()
    }

    pub fn get_default_block(&self) -> BasicBlock<'ctx> {
        self.inst.get_operand(1).unwrap().block().unwrap()
    }

    pub fn find_case_dest<'a>(&self, basic_block: BasicBlock) -> Option<BasicValueEnum<'a>> {
        let value_ref = unsafe {
            ffi::amice_switch_find_case_dest(
                self.inst.as_value_ref() as LLVMValueRef,
                basic_block.as_mut_ptr() as LLVMBasicBlockRef,
            )
        };
        if value_ref.is_null() {
            None
        } else {
            value_ref.into_basic_value_enum().into()
        }
    }
}

impl<'ctx> Deref for SwitchInst<'ctx> {
    type Target = InstructionValue<'ctx>;

    fn deref(&self) -> &Self::Target {
        &self.inst
    }
}

impl<'ctx> DerefMut for SwitchInst<'ctx> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inst
    }
}

impl<'ctx> From<InstructionValue<'ctx>> for SwitchInst<'ctx> {
    fn from(base: InstructionValue<'ctx>) -> Self {
        Self::new(base)
    }
}
