use crate::ffi;
use crate::inkwell2::LLVMBasicBlockRefExt;
use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::core::{LLVMGetEntryBasicBlock, LLVMIsABasicBlock};
use inkwell::llvm_sys::prelude::LLVMValueRef;
use inkwell::values::{AsValueRef, FunctionValue};
use std::ffi::{CStr, c_char};

pub trait FunctionExt<'ctx> {
    fn verify_function(self) -> VerifyResult;

    fn verify_function_bool(self) -> bool;

    fn get_entry_block(&self) -> Option<BasicBlock<'ctx>>;

    fn is_inline_marked(&self) -> bool;

    fn is_llvm_function(&self) -> bool;

    fn is_undef_function(&self) -> bool;

    unsafe fn fix_stack(&self);

    unsafe fn fix_stack_at_terminator(&self);

    unsafe fn fix_stack_with_max_iterations(&self, max_iterations: usize);
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum VerifyResult {
    Broken(String),
    Ok,
}

impl<'ctx> FunctionExt<'ctx> for FunctionValue<'ctx> {
    fn verify_function(self) -> VerifyResult {
        let mut errmsg: *const c_char = std::ptr::null();
        let broken = unsafe {
            ffi::amice_verify_function(self.as_value_ref() as LLVMValueRef, &mut errmsg as *mut *const c_char) == 1
        };
        let result = if !errmsg.is_null() && broken {
            let c_errmsg = unsafe { CStr::from_ptr(errmsg) };
            VerifyResult::Broken(c_errmsg.to_string_lossy().into_owned())
        } else {
            VerifyResult::Ok
        };
        unsafe {
            ffi::amice_free_msg(errmsg);
        }
        result
    }

    fn verify_function_bool(self) -> bool {
        match self.verify_function() {
            VerifyResult::Broken(_) => true,
            VerifyResult::Ok => false,
        }
    }

    fn get_entry_block(&self) -> Option<BasicBlock<'ctx>> {
        unsafe {
            let basic_block = LLVMGetEntryBasicBlock(self.as_value_ref());
            if LLVMIsABasicBlock(basic_block as LLVMValueRef).is_null() {
                return None;
            }
            basic_block.into_basic_block()
        }
    }

    fn is_inline_marked(&self) -> bool {
        unsafe { ffi::amice_is_inline_marked_function(self.as_value_ref() as LLVMValueRef) }
    }

    fn is_llvm_function(&self) -> bool {
        let name = self.get_name().to_str().unwrap_or("");
        name.is_empty()
            || name.starts_with("llvm.")
            || name.starts_with("clang.")
            || name.starts_with("__")
            || name.starts_with("@")
            || self.get_intrinsic_id() != 0
    }

    fn is_undef_function(&self) -> bool {
        self.is_null() || self.is_undef() || self.count_basic_blocks() <= 0 || self.get_intrinsic_id() != 0
    }

    unsafe fn fix_stack(&self) {
        unsafe { ffi::amice_fix_stack(self.as_value_ref() as LLVMValueRef, 0, 0) }
    }

    unsafe fn fix_stack_at_terminator(&self) {
        unsafe { ffi::amice_fix_stack(self.as_value_ref() as LLVMValueRef, 1, 0) }
    }

    unsafe fn fix_stack_with_max_iterations(&self, max_iterations: usize) {
        unsafe { ffi::amice_fix_stack(self.as_value_ref() as LLVMValueRef, 0, max_iterations as i32) }
    }
}
