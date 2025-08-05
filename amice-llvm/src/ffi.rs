use std::ffi::c_void;

#[link(name = "amice-llvm-ffi")]
extern "C" {
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
    pub(crate) fn amiceAppendToGlobalCtors(module: *mut c_void, function: *mut c_void, priority: i32);

    pub(crate) fn amiceAppendToUsed(module: *mut c_void, value: *mut c_void);

    pub(crate) fn amiceAppendToCompilerUsed(module: *mut c_void, value: *mut c_void);
    
    pub(crate) fn amiceConstantGetBitCast(
        value: *mut c_void,
        ty: *mut c_void,
    ) -> *mut c_void;
    
    pub(crate) fn amiceConstantGetPtrToInt(
        value: *mut c_void,
        ty: *mut c_void,
    ) -> *mut c_void;
    
    pub(crate) fn amiceConstantGetIntToPtr(
        value: *mut c_void,
        ty: *mut c_void,
    ) -> *mut c_void;
    
    pub(crate) fn amiceConstantGetXor(
        value1: *mut c_void,
        value2: *mut c_void,
    ) -> *mut c_void;

    pub(crate) fn amiceSplitBasicBlock(
        block: *mut c_void,
        inst: *mut c_void,
        name: *const i8,
        before: i32,
    ) -> *mut c_void;
    
    pub(crate) fn amiceGetLLVMVersionMajor() -> i32;
    
    pub(crate) fn amiceGetLLVMVersionMinor() -> i32;
}