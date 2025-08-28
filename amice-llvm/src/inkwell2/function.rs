use crate::ffi;
use crate::inkwell2::{LLVMBasicBlockRefExt, LLVMValueRefExt};
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

    #[cfg(not(feature = "android-ndk"))]
    fn clone_function(&self) -> Option<FunctionValue<'ctx>>;

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

    #[cfg(not(feature = "android-ndk"))]
    fn clone_function(&self) -> Option<FunctionValue<'ctx>> {
        let clone = unsafe { ffi::amice_clone_function(self.as_value_ref() as LLVMValueRef) };
        if clone.is_null() {
            return None;
        }
        clone.into_function_value()
    }

    unsafe fn fix_stack(&self) {
        ffi::amice_fix_stack(self.as_value_ref() as LLVMValueRef, 0, 0)
    }

    unsafe fn fix_stack_at_terminator(&self) {
        ffi::amice_fix_stack(self.as_value_ref() as LLVMValueRef, 1, 0)
    }

    unsafe fn fix_stack_with_max_iterations(&self, max_iterations: usize) {
        ffi::amice_fix_stack(self.as_value_ref() as LLVMValueRef, 0, max_iterations as i32)
    }
}
