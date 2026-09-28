use crate::ffi;
use crate::inkwell2::LLVMBasicBlockRefExt;
use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::core::{LLVMGetEntryBasicBlock, LLVMIsABasicBlock};
use inkwell::llvm_sys::prelude::LLVMValueRef;
use inkwell::values::{AsValueRef, FunctionValue};
use std::ffi::{CStr, c_char};

pub trait FunctionExt<'ctx> {
    /// Clone this definition into its module, including local value mappings.
    fn clone_definition(self) -> Option<FunctionValue<'ctx>>;

    /// Check LLVM's structural inlining requirements without a size/cost threshold.
    fn is_inline_viable(self) -> bool;

    /// Replace this function's body using the source's parameters and metadata.
    /// The function identity and linkage are retained; LLVM copies source attributes.
    ///
    /// # Safety
    /// Functions must be distinct, in the same module, with identical types. The old
    /// body must have no blockaddress users, and all handles to it are invalidated.
    unsafe fn replace_body_from(self, source: FunctionValue<'ctx>);

    fn verify_function(self) -> VerifyResult;

    fn verify_function_bool(self) -> bool;

    fn get_entry_block(&self) -> Option<BasicBlock<'ctx>>;

    fn is_inline_marked(&self) -> bool;

    fn is_llvm_function(&self) -> bool;

    fn is_undef_function(&self) -> bool;

    fn clear_stale_analysis_attrs_after_cfg_rewrite(&self);

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
    fn is_inline_viable(self) -> bool {
        // SAFETY: FunctionValue represents a live LLVM function.
        unsafe { ffi::amice_function_is_inline_viable(self.as_value_ref()) }
    }

    fn clone_definition(self) -> Option<FunctionValue<'ctx>> {
        // SAFETY: The live function supplies its parent module and LLVM owns the clone.
        unsafe { FunctionValue::new(ffi::amice_function_clone(self.as_value_ref())) }
    }

    unsafe fn replace_body_from(self, source: FunctionValue<'ctx>) {
        // SAFETY: The caller guarantees matching types and no escaping old blocks.
        unsafe { ffi::amice_function_replace_body(self.as_value_ref(), source.as_value_ref()) };
    }

    fn verify_function(self) -> VerifyResult {
        let mut errmsg: *const c_char = std::ptr::null();
        let broken = unsafe {
            ffi::amice_function_verify(self.as_value_ref() as LLVMValueRef, &mut errmsg as *mut *const c_char) == 1
        };
        let result = if !errmsg.is_null() && broken {
            let c_errmsg = unsafe { CStr::from_ptr(errmsg) };
            VerifyResult::Broken(c_errmsg.to_string_lossy().into_owned())
        } else {
            VerifyResult::Ok
        };
        unsafe {
            ffi::amice_free_string(errmsg);
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
        unsafe { ffi::amice_function_is_inline_marked(self.as_value_ref() as LLVMValueRef) }
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

    fn clear_stale_analysis_attrs_after_cfg_rewrite(&self) {
        unsafe { ffi::amice_function_clear_stale_analysis_attrs_after_cfg_rewrite(self.as_value_ref() as LLVMValueRef) }
    }

    unsafe fn fix_stack(&self) {
        unsafe { ffi::amice_function_fix_stack(self.as_value_ref() as LLVMValueRef, 0, 0) }
    }

    unsafe fn fix_stack_at_terminator(&self) {
        unsafe { ffi::amice_function_fix_stack(self.as_value_ref() as LLVMValueRef, 1, 0) }
    }

    unsafe fn fix_stack_with_max_iterations(&self, max_iterations: usize) {
        unsafe { ffi::amice_function_fix_stack(self.as_value_ref() as LLVMValueRef, 0, max_iterations as i32) }
    }
}
