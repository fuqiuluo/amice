use amice_llvm::ir;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::llvm_sys::core::{LLVMGetEntryBasicBlock, LLVMGetValueKind, LLVMIsABasicBlock};
use llvm_plugin::inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use llvm_plugin::inkwell::types::{AsTypeRef, FunctionType};
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue, PointerValue};
use log::error;

pub fn get_basic_block_entry_ref(fun: &FunctionValue) -> LLVMBasicBlockRef {
    unsafe { LLVMGetEntryBasicBlock(fun.as_value_ref()) }
}

/// Get the entry basic block of a function.
/// tips: If the function has only one basic block, the behavior of calling its get entry is UB
pub fn get_basic_block_entry(fun: FunctionValue) -> Option<BasicBlock> {
    unsafe {
        let basic_block = get_basic_block_entry_ref(&fun);
        if LLVMIsABasicBlock(basic_block as LLVMValueRef).is_null() {
            let real_ty = LLVMGetValueKind(basic_block as LLVMValueRef);
            error!(
                "(llvm-utils) Function {} unable to fetch entry basic block, ref: {:?}, real_ty: {:?}",
                fun.get_name().to_str().unwrap_or("unknown"),
                basic_block,
                real_ty
            );
            return None;
        }
        BasicBlock::new(get_basic_block_entry_ref(&fun))
    }
}

pub fn cast_ptr_to_fn_ptr<'a>(addr: PointerValue<'a>, function_type: FunctionType<'a>) -> Option<FunctionValue<'a>> {
    unsafe {
        let value = ir::constants::get_bitcast_constant(
            addr.as_value_ref() as *mut std::ffi::c_void,
            function_type.as_type_ref() as *mut std::ffi::c_void,
        ) as LLVMValueRef;
        FunctionValue::new(value)
    }
}
