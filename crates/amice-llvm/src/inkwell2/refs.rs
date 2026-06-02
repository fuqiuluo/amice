use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use inkwell::values::{BasicValueEnum, FunctionValue, InstructionValue, PhiValue};

pub trait LLVMBasicBlockRefExt<'ctx> {
    fn into_basic_block(self) -> Option<BasicBlock<'ctx>>;
}

impl<'ctx> LLVMBasicBlockRefExt<'ctx> for LLVMBasicBlockRef {
    fn into_basic_block(self) -> Option<BasicBlock<'ctx>> {
        unsafe { BasicBlock::new(self) }
    }
}

pub trait LLVMValueRefExt<'ctx> {
    fn into_instruction_value(self) -> InstructionValue<'ctx>;

    fn into_basic_value_enum(self) -> BasicValueEnum<'ctx>;

    fn into_function_value(self) -> Option<FunctionValue<'ctx>>;

    fn into_phi_value(self) -> PhiValue<'ctx>;
}

impl<'ctx> LLVMValueRefExt<'ctx> for LLVMValueRef {
    fn into_instruction_value(self) -> InstructionValue<'ctx> {
        unsafe { InstructionValue::new(self) }
    }

    fn into_basic_value_enum(self) -> BasicValueEnum<'ctx> {
        unsafe { BasicValueEnum::new(self) }
    }

    fn into_function_value(self) -> Option<FunctionValue<'ctx>> {
        unsafe { FunctionValue::new(self) }
    }

    fn into_phi_value(self) -> PhiValue<'ctx> {
        unsafe { PhiValue::new(self) }
    }
}
