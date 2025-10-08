use crate::ffi::amice_gep_accumulate_constant_offset;
use inkwell::llvm_sys::core::LLVMGetNumIndices;
use inkwell::llvm_sys::prelude::LLVMValueRef;
use inkwell::module::Module;
use inkwell::values::{AsValueRef, BasicValueEnum, InstructionValue};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Copy, Clone)]
pub struct GepInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> GepInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert_eq!(inst.get_opcode(), inkwell::values::InstructionOpcode::GetElementPtr);
        assert!(!inst.as_value_ref().is_null());
        Self { inst }
    }

    pub fn get_num_indices(&self) -> u32 {
        unsafe { LLVMGetNumIndices(self.as_value_ref() as LLVMValueRef) }
    }

    pub fn get_indices(&self) -> Vec<Option<BasicValueEnum<'ctx>>> {
        (0..self.get_num_indices())
            .map(|i| self.get_operand(i + 1).unwrap().left())
            .collect()
    }

    pub fn accumulate_constant_offset(&self, module: &mut Module) -> Option<u64> {
        let mut offset = 0u64;
        if unsafe {
            amice_gep_accumulate_constant_offset(
                self.as_value_ref() as LLVMValueRef,
                module.as_mut_ptr() as _,
                &mut offset,
            )
        } {
            return Some(offset);
        }
        None
    }

    pub fn get_pointer_operand(&self) -> Option<BasicValueEnum<'ctx>> {
        assert!(self.get_num_operands() > 0);
        self.get_operand(0).unwrap().left()
    }
}

impl<'ctx> Deref for GepInst<'ctx> {
    type Target = InstructionValue<'ctx>;

    fn deref(&self) -> &Self::Target {
        &self.inst
    }
}

impl<'ctx> DerefMut for GepInst<'ctx> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inst
    }
}

impl<'ctx> From<InstructionValue<'ctx>> for GepInst<'ctx> {
    fn from(base: InstructionValue<'ctx>) -> Self {
        Self::new(base)
    }
}
