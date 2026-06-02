use crate::inkwell2::refs::LLVMValueRefExt;
use inkwell::llvm_sys::prelude::LLVMValueRef;
use inkwell::values::{AsValueRef, InstructionOpcode, InstructionValue, PhiValue};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Copy, Clone)]
pub struct PhiInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> PhiInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert_eq!(inst.get_opcode(), InstructionOpcode::Phi);
        Self { inst }
    }

    pub fn into_phi_value(self) -> PhiValue<'ctx> {
        (self.inst.as_value_ref() as LLVMValueRef).into_phi_value()
    }
}

impl<'ctx> Deref for PhiInst<'ctx> {
    type Target = InstructionValue<'ctx>;

    fn deref(&self) -> &Self::Target {
        &self.inst
    }
}

impl<'ctx> DerefMut for PhiInst<'ctx> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inst
    }
}

impl<'ctx> From<InstructionValue<'ctx>> for PhiInst<'ctx> {
    fn from(base: InstructionValue<'ctx>) -> Self {
        Self::new(base)
    }
}
