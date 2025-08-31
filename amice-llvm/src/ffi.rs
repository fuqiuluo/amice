use crate::analysis::dominators::LLVMDominatorTreeRef;
use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMModuleRef, LLVMUseRef, LLVMValueRef};
use inkwell::values::InstructionValue;
use std::ffi::{CStr, c_char, c_void};
use crate::code_extractor::LLVMCodeExtractorRef;

#[link(name = "amice-llvm-ffi")]
unsafe extern "C" {
    #[cfg(any(
        feature = "llvm12-0",
        feature = "llvm13-0",
        feature = "llvm14-0",
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
    ))]
    pub(crate) fn amice_append_to_global_ctors(module: LLVMModuleRef, function: LLVMValueRef, priority: i32);

    pub(crate) fn amice_append_to_used(module: LLVMModuleRef, value: LLVMValueRef);

    pub(crate) fn amice_append_to_compiler_used(module: LLVMModuleRef, value: LLVMValueRef);

    pub(crate) fn amice_fix_stack(function: LLVMValueRef, at_term: i32, max_iterations: i32);

    pub(crate) fn amice_verify_function(function: LLVMValueRef, errmsg: *mut *const c_char) -> i32;

    pub(crate) fn amice_free_msg(errmsg: *const c_char) -> i32;

    pub(crate) fn amice_split_basic_block(
        block: LLVMBasicBlockRef,
        inst: LLVMValueRef,
        name: *const c_char,
        before: i32,
    ) -> *mut c_void;

    pub(crate) fn amice_get_first_insertion_pt(block: LLVMBasicBlockRef) -> LLVMValueRef;

    pub(crate) fn llvm_dominator_tree_create() -> LLVMDominatorTreeRef;

    pub(crate) fn llvm_dominator_tree_create_from_function(func: LLVMValueRef) -> LLVMDominatorTreeRef;

    pub(crate) fn llvm_dominator_tree_destroy(dt: LLVMDominatorTreeRef);

    pub(crate) fn llvm_dominator_tree_view_graph(dt: LLVMDominatorTreeRef);

    pub(crate) fn llvm_dominator_tree_dominate_BU(
        dt: LLVMDominatorTreeRef,
        b: LLVMBasicBlockRef,
        u: LLVMUseRef,
    ) -> bool;

    pub(crate) fn amice_switch_find_case_dest(inst: LLVMValueRef, b: LLVMBasicBlockRef) -> LLVMValueRef;

    pub(crate) fn amice_is_inline_marked_function(function: LLVMValueRef) -> bool;

    pub(crate) fn amice_basic_block_remove_predecessor(basic_block: LLVMBasicBlockRef, predecessor: LLVMBasicBlockRef);

    pub(crate) fn amice_phi_node_remove_incoming_value(phi_node: LLVMValueRef, incoming: LLVMBasicBlockRef);

    pub(crate) fn amice_phi_node_replace_incoming_block_with(
        phi_node: LLVMValueRef,
        incoming: LLVMBasicBlockRef,
        new_block: LLVMBasicBlockRef,
    );

    pub(crate) fn amice_specialize_function(
        original_func: LLVMValueRef,
        module: LLVMModuleRef,
        replacements: *const ArgReplacement,
        replacement_count: u32,
    ) -> LLVMValueRef;
    
    pub(crate) fn amice_gep_accumulate_constant_offset(gep: LLVMValueRef, module: LLVMModuleRef, offset: *mut u64) -> bool;
    
    pub(crate) fn amice_attribute_enum_kind_to_str(kind: u32) -> *const c_char; 
    
    pub(crate) fn amice_create_code_extractor(bbs: *const LLVMBasicBlockRef, bb_len: i32) -> LLVMCodeExtractorRef;
    
    pub(crate) fn amice_delete_code_extractor(ce: LLVMCodeExtractorRef);
    
    pub(crate) fn amice_code_extractor_is_eligible(ce: LLVMCodeExtractorRef) -> bool;
    
    pub(crate) fn amice_code_extractor_extract_code_region(ce: LLVMCodeExtractorRef, function: LLVMValueRef) -> LLVMValueRef;

    pub(crate) fn amice_get_llvm_version_major() -> i32;

    pub(crate) fn amice_get_llvm_version_minor() -> i32;
}

#[repr(C)]
pub(crate) struct ArgReplacement {
    pub index: u32,
    pub constant: LLVMValueRef,
}

