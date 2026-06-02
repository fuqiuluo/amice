use std::ffi::c_void;

use inkwell::module::Module;
use inkwell::values::FunctionValue;

use super::{
    FunctionAnalysisManager, LlvmFunctionPass, LlvmModulePass, ModuleAnalysisManager,
    PreservedAnalyses,
};

/// Struct allowing to add passes on LLVM IR modules to the pass manager
/// pipeline.
pub struct ModulePassManager {
    inner: *mut c_void,
}

impl ModulePassManager {
    #[doc(hidden)]
    pub unsafe fn from_raw(pass_manager: *mut c_void) -> Self {
        Self {
            inner: pass_manager,
        }
    }

    /// Adds a pass to this pass manager.
    pub fn add_pass<T>(&mut self, pass: T)
    where
        T: LlvmModulePass,
    {
        let pass = Box::new(pass);

        extern "C" fn pass_deleter<T>(pass: *mut c_void) {
            drop(unsafe { Box::<T>::from_raw(pass.cast()) })
        }

        extern "C" fn pass_entrypoint<T>(
            pass: *mut c_void,
            module: *mut c_void,
            manager: *mut c_void,
        ) -> PreservedAnalyses
        where
            T: LlvmModulePass,
        {
            let pass = unsafe { Box::<T>::from_raw(pass.cast()) };
            let mut module = unsafe { Module::new(module.cast()) };
            let manager = unsafe { ModuleAnalysisManager::from_raw(manager, None) };

            let preserve = pass.run_pass(&mut module, &manager);

            let _ = Box::into_raw(pass);
            std::mem::forget(module);

            preserve
        }

        unsafe {
            super::modulePassManagerAddPass(
                self.inner,
                Box::into_raw(pass).cast(),
                pass_deleter::<T>,
                pass_entrypoint::<T>,
            )
        }
    }

    /// Returns if the pass manager contains any passes.
    #[cfg(any(
        doc,
        feature = "llvm12-0",
        feature = "llvm13-0",
        feature = "llvm14-0",
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
        feature = "llvm21-1"
    ))]
    pub fn is_empty(&self) -> bool {
        unsafe { super::modulePassManagerIsEmpty(self.inner) }
    }
}

/// Struct allowing to add passes on LLVM IR functions to the pass manager
/// pipeline.
pub struct FunctionPassManager {
    inner: *mut c_void,
}

impl FunctionPassManager {
    #[doc(hidden)]
    pub unsafe fn from_raw(pass_manager: *mut c_void) -> Self {
        Self {
            inner: pass_manager,
        }
    }

    /// Adds a pass to this pass manager.
    pub fn add_pass<T>(&mut self, pass: T)
    where
        T: LlvmFunctionPass,
    {
        let pass = Box::new(pass);

        extern "C" fn pass_deleter<T>(pass: *mut c_void) {
            drop(unsafe { Box::<T>::from_raw(pass.cast()) })
        }

        extern "C" fn pass_entrypoint<T>(
            pass: *mut c_void,
            function: *mut c_void,
            manager: *mut c_void,
        ) -> PreservedAnalyses
        where
            T: LlvmFunctionPass,
        {
            let pass = unsafe { Box::<T>::from_raw(pass.cast()) };
            let mut function = unsafe { FunctionValue::new(function.cast()).unwrap() };
            let manager = unsafe { FunctionAnalysisManager::from_raw(manager, None) };

            let preserve = pass.run_pass(&mut function, &manager);

            let _ = Box::into_raw(pass);
            #[allow(forgetting_copy_types)]
            std::mem::forget(function);

            preserve
        }

        unsafe {
            super::functionPassManagerAddPass(
                self.inner,
                Box::into_raw(pass).cast(),
                pass_deleter::<T>,
                pass_entrypoint::<T>,
            )
        }
    }

    /// Returns if the pass manager contains any passes.
    #[cfg(any(
        doc,
        feature = "llvm12-0",
        feature = "llvm13-0",
        feature = "llvm14-0",
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
        feature = "llvm21-1"
    ))]
    pub fn is_empty(&self) -> bool {
        unsafe { super::functionPassManagerIsEmpty(self.inner) }
    }
}
