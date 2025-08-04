use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::llvm_sys::core::LLVMGetEntryBasicBlock;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMBasicBlockRef;
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue};

pub fn get_basic_block_entry(
    fun: &FunctionValue,
) -> LLVMBasicBlockRef {
    unsafe {
        LLVMGetEntryBasicBlock(fun.as_value_ref())
    }
}