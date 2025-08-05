use amice_llvm::ir;
use llvm_plugin::inkwell::llvm_sys::core::LLVMGetEntryBasicBlock;
use llvm_plugin::inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use llvm_plugin::inkwell::types::{AsTypeRef, FunctionType};
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue, PointerValue};

pub fn get_basic_block_entry(fun: &FunctionValue) -> LLVMBasicBlockRef {
    unsafe { LLVMGetEntryBasicBlock(fun.as_value_ref()) }
}

pub fn cast_ptr_to_fn_ptr<'a>(
    addr: PointerValue<'a>,
    function_type: FunctionType<'a>,
) -> Option<FunctionValue<'a>> {
    unsafe {
        let value = ir::constants::get_bitcast_constant(
            addr.as_value_ref() as *mut std::ffi::c_void,
            function_type.as_type_ref() as *mut std::ffi::c_void,
        ) as LLVMValueRef;
        FunctionValue::new(value)
    }
}
