use crate::analysis::dominators::LLVMDominatorTreeRef;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};

#[link(name = "amice-llvm-ffi")]
unsafe extern "C" {
    pub(crate) fn amice_dominator_tree_create() -> LLVMDominatorTreeRef;

    pub(crate) fn amice_dominator_tree_create_from_function(func: LLVMValueRef) -> LLVMDominatorTreeRef;

    pub(crate) fn amice_dominator_tree_destroy(dt: LLVMDominatorTreeRef);

    pub(crate) fn amice_dominator_tree_view_graph(dt: LLVMDominatorTreeRef);

    pub(crate) fn amice_dominator_tree_dominates(
        dt: LLVMDominatorTreeRef,
        a: LLVMBasicBlockRef,
        b: LLVMBasicBlockRef,
    ) -> bool;
}
