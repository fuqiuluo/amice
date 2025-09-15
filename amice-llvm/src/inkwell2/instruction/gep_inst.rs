use crate::ffi::amice_gep_accumulate_constant_offset;
use inkwell::data_layout::DataLayout;
use inkwell::llvm_sys::core::{LLVMGetElementType, LLVMGetNumIndices, LLVMIsInBounds};
use inkwell::llvm_sys::prelude::LLVMValueRef;
use inkwell::module::Module;
use inkwell::types::{AsTypeRef, BasicTypeEnum};
use inkwell::values::{AsValueRef, BasicValueEnum, InstructionOpcode, InstructionValue, PointerValue};
use std::cell::Ref;
use std::ops::{Deref, DerefMut};

#[derive(Debug, Copy, Clone)]
pub struct GepInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> GepInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert_eq!(inst.get_opcode(), InstructionOpcode::GetElementPtr);
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

    pub fn get_pointer(&self) -> PointerValue<'ctx> {
        let ptr = self.get_pointer_operand().unwrap();
        assert!(ptr.is_pointer_value(), "Expected pointer value, got {:?}", ptr);
        ptr.into_pointer_value()
    }

    pub fn is_inbounds(&self) -> bool {
        unsafe { LLVMIsInBounds(self.as_value_ref()) == 1 }
    }

    pub fn get_element_type(&self) -> Option<BasicTypeEnum<'ctx>> {
        #[cfg(not(any(
            feature = "llvm11-0",
            feature = "llvm12-0",
            feature = "llvm13-0",
            feature = "llvm14-0",
        )))]
        return self.get_gep_source_element_type().ok();
        #[cfg(any(
            feature = "llvm11-0",
            feature = "llvm12-0",
            feature = "llvm13-0",
            feature = "llvm14-0",
        ))]
        {
            let pointer_type = unsafe { LLVMGetElementType(self.get_pointer().get_type().as_type_ref()) };
            return unsafe { BasicTypeEnum::new(pointer_type) };
        }
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
