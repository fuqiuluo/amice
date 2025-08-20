use crate::llvm_utils::to_c_str;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use llvm_plugin::inkwell::values::{AsValueRef, InstructionValue};

pub fn split_basic_block<'a>(
    block: BasicBlock<'a>,
    inst: InstructionValue<'a>,
    name: &str,
    before: bool,
) -> Option<BasicBlock<'a>> {
    let c_str_name = to_c_str(name);
    let new_block = unsafe {
        amice_llvm::ir::basic_block::split_basic_block(
            block.as_mut_ptr() as *mut std::ffi::c_void,
            inst.as_value_ref() as *mut std::ffi::c_void,
            c_str_name.as_ptr(),
            before,
        )
    };
    let value = new_block as LLVMBasicBlockRef;
    unsafe { BasicBlock::new(value) }
}

pub fn get_first_insertion_pt<'a>(
    block: BasicBlock<'a>,
) -> InstructionValue<'a> {
    let ref_ = unsafe {
        amice_llvm::ir::basic_block::get_first_insertion_pt(block.as_mut_ptr() as *mut std::ffi::c_void)
    };
    unsafe { InstructionValue::new(ref_ as LLVMValueRef) }
}