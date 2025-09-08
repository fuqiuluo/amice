use inkwell::llvm_sys::core::{LLVMGetNSW, LLVMGetNUW};
use inkwell::types::{AnyTypeEnum, BasicTypeEnum};
use inkwell::values::{AsValueRef, BasicValueEnum, InstructionOpcode, InstructionValue, PointerValue};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Copy, Clone)]
pub struct AddInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> AddInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert_eq!(inst.get_opcode(), InstructionOpcode::Add);
        Self { inst }
    }

    pub fn get_lhs_value(&self) -> BasicValueEnum<'ctx> {
        self.get_operand(0).unwrap().left().unwrap()
    }

    pub fn get_rhs_value(&self) -> BasicValueEnum<'ctx> {
        self.get_operand(1).unwrap().left().unwrap()
    }

    pub fn has_nsw(&self) -> bool {
        unsafe { LLVMGetNSW(self.as_value_ref()) == 1 }
    }

    pub fn has_nuw(&self) -> bool {
        unsafe { LLVMGetNUW(self.as_value_ref()) == 1 }
    }
}

impl<'ctx> Deref for AddInst<'ctx> {
    type Target = InstructionValue<'ctx>;

    fn deref(&self) -> &Self::Target {
        &self.inst
    }
}

impl<'ctx> DerefMut for AddInst<'ctx> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inst
    }
}

impl<'ctx> From<InstructionValue<'ctx>> for AddInst<'ctx> {
    fn from(base: InstructionValue<'ctx>) -> Self {
        Self::new(base)
    }
}
