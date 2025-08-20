use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMUseRef, LLVMValueRef};
use inkwell::values::{AsValueRef, FunctionValue};

#[repr(C)]
pub struct LLVMDominatorTree {
    _private: [u8; 0],
}

pub type LLVMDominatorTreeRef = *mut LLVMDominatorTree;

unsafe extern "C" {
    // Lifecycle management
    fn llvm_dominator_tree_create() -> LLVMDominatorTreeRef;
    fn llvm_dominator_tree_create_from_function(func: LLVMValueRef) -> LLVMDominatorTreeRef;
    fn llvm_dominator_tree_destroy(dt: LLVMDominatorTreeRef);
    fn llvm_dominator_tree_view_graph(dt: LLVMDominatorTreeRef);
    fn llvm_dominator_tree_dominate_BU(dt: LLVMDominatorTreeRef, b: LLVMBasicBlockRef, u: LLVMUseRef) -> bool;
}

/// Safe Rust wrapper for LLVM DominatorTree
pub struct DominatorTree {
    ptr: LLVMDominatorTreeRef,
}

impl DominatorTree {
    /// Create a new empty DominatorTree
    pub fn new() -> Result<Self, &'static str> {
        let ptr = unsafe { llvm_dominator_tree_create() };
        if ptr.is_null() {
            Err("Failed to create DominatorTree")
        } else {
            Ok(DominatorTree { ptr })
        }
    }

    /// Create a DominatorTree from an LLVM Function
    pub fn from_function(func: FunctionValue) -> Result<Self, &'static str> {
        if func.is_null() {
            return Err("Function pointer is null");
        }

        let ptr = unsafe { llvm_dominator_tree_create_from_function(func.as_value_ref() as LLVMValueRef) };
        if ptr.is_null() {
            Err("Failed to create DominatorTree from function")
        } else {
            Ok(DominatorTree { ptr })
        }
    }

    pub fn from_function_ref(func: LLVMValueRef) -> Result<Self, &'static str> {
        if func.is_null() {
            return Err("Function pointer is null");
        }

        let ptr = unsafe { llvm_dominator_tree_create_from_function(func) };
        if ptr.is_null() {
            Err("Failed to create DominatorTree from function")
        } else {
            Ok(DominatorTree { ptr })
        }
    }
    
    pub fn view_graph(&self) {
        if self.ptr.is_null() {
            panic!("Cannot view graph of a null DominatorTree");
        }
        unsafe { llvm_dominator_tree_view_graph(self.ptr) }
    }
    
    pub fn dominate(&self, b: BasicBlock, u: BasicBlock) -> bool {
        if self.ptr.is_null() {
            panic!("Cannot dominate of a null DominatorTree");
        }
        
        unsafe {
            llvm_dominator_tree_dominate_BU(self.ptr, b.as_mut_ptr() as LLVMBasicBlockRef, u.as_mut_ptr() as LLVMUseRef)
        }
    }
    
    pub fn as_ptr(&self) -> LLVMDominatorTreeRef {
        self.ptr
    }
}

impl Drop for DominatorTree {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { llvm_dominator_tree_destroy(self.ptr) }
        }
    }
}

unsafe impl Send for DominatorTree {}
unsafe impl Sync for DominatorTree {}