use crate::ffi;
use crate::inkwell2::LLVMValueRefExt;
use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use inkwell::values::{AsValueRef, FunctionValue};
use std::ptr;

#[repr(C)]
pub struct LLVMCodeExtractor {
    _private: [u8; 0],
}

pub type LLVMCodeExtractorRef = *mut LLVMCodeExtractor;

pub struct CodeExtractor {
    ptr: LLVMCodeExtractorRef,
}

impl CodeExtractor {
    pub fn new(basic_blocks: &[BasicBlock]) -> Option<Self> {
        let mut basic_block_refs = vec![ptr::null(); basic_blocks.len()];
        for (i, bb) in basic_blocks.iter().enumerate() {
            basic_block_refs[i] = bb.as_mut_ptr();
        }
        let basic_block_refs = basic_block_refs.as_ptr() as *mut LLVMBasicBlockRef;
        let ptr = unsafe { ffi::amice_create_code_extractor(basic_block_refs, basic_blocks.len() as i32) };
        if ptr.is_null() {
            None
        } else {
            Some(CodeExtractor { ptr })
        }
    }

    pub fn is_eligible(&self) -> bool {
        unsafe { ffi::amice_code_extractor_is_eligible(self.ptr) }
    }

    pub fn extract_code_region<'a>(&self, function: FunctionValue<'a>) -> Option<FunctionValue<'a>> {
        let generated_func =
            unsafe { ffi::amice_code_extractor_extract_code_region(self.ptr, function.as_value_ref() as LLVMValueRef) };
        if generated_func.is_null() {
            None
        } else {
            generated_func.into_function_value()
        }
    }
}

impl Drop for CodeExtractor {
    fn drop(&mut self) {
        unsafe {
            ffi::amice_delete_code_extractor(self.ptr);
        }
    }
}
