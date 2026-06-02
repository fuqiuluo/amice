use crate::code_extractor::LLVMCodeExtractorRef;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};

#[link(name = "amice-llvm-ffi")]
unsafe extern "C" {
    pub(crate) fn amice_code_extractor_create(bbs: *const LLVMBasicBlockRef, bb_len: i32) -> LLVMCodeExtractorRef;

    pub(crate) fn amice_code_extractor_delete(ce: LLVMCodeExtractorRef);

    pub(crate) fn amice_code_extractor_is_eligible(ce: LLVMCodeExtractorRef) -> bool;

    pub(crate) fn amice_code_extractor_extract_region(ce: LLVMCodeExtractorRef, function: LLVMValueRef)
    -> LLVMValueRef;
}
