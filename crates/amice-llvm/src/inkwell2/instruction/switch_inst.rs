use crate::ffi;
use crate::inkwell2::{LLVMBasicBlockRefExt, LLVMValueRefExt};
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
        unsafe { ffi::amice_switch_get_case_num(self.inst.as_value_ref() as LLVMValueRef) }
    }

    pub fn get_cases(&self) -> Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> {
        let case_num = self.get_case_num();
        let mut cases = Vec::with_capacity(case_num as usize);

        for index in 0..case_num {
            let switch = self.inst.as_value_ref() as LLVMValueRef;
            let case_value = unsafe { ffi::amice_switch_get_case_value(switch, index) };
            let case_block = unsafe { ffi::amice_switch_get_case_dest(switch, index) };

            assert!(!case_value.is_null());
            assert!(!case_block.is_null());

            cases.push((
                case_value.into_basic_value_enum(),
                case_block
                    .into_basic_block()
                    .expect("switch case destination should be a basic block"),
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
