use std::ffi::c_void;

use inkwell::module::Module;
use inkwell::values::{AsValueRef, FunctionValue};

use crate::{LlvmFunctionAnalysis, LlvmModuleAnalysis};

/// Struct allowing to query the pass manager for the result of
/// analyses on function IR.
pub struct FunctionAnalysisManager {
    inner: *mut c_void,
    from_analysis_id: Option<crate::AnalysisKey>,
}

impl FunctionAnalysisManager {
    #[doc(hidden)]
    pub unsafe fn from_raw(
        inner: *mut c_void,
        from_analysis_id: Option<crate::AnalysisKey>,
    ) -> Self {
        Self {
            inner,
            from_analysis_id,
        }
    }

    /// Returns the result of the analysis on a given function IR.
    ///
    /// If the result is not in cache, the pass manager will execute the
    /// analysis pass. Otherwise, the result is directly returned from cache.
    ///
    /// # Panics
    ///
    /// Panics if the given analysis wasn't registered, or if this function was
    /// called within the given analysis itself.
    pub fn get_result<A>(&self, function: &FunctionValue<'_>) -> &A::Result
    where
        A: crate::LlvmFunctionAnalysis,
    {
        let id = A::id();
        assert!(
            !matches!(self.from_analysis_id, Some(n) if id == n),
            "Analysis cannot request its own result"
        );

        unsafe {
            let res =
                crate::get_function_analysis_result(self.inner, id, function.as_value_ref().cast());
            Box::leak(Box::from_raw(res.cast()))
        }
    }

    /// Returns the result of the analysis on a given function IR.
    ///
    /// If the result is not in cache, `None` is returned. Otherwise,
    /// the result is directly returned from cache.
    ///
    /// This function never triggers the execution of an analysis.
    ///
    /// # Panics
    ///
    /// Panics if the given analysis wasn't registered, or if this function was
    /// called within the given analysis itself.
    pub fn get_cached_result<A>(&self, function: &FunctionValue<'_>) -> Option<&A::Result>
    where
        A: crate::LlvmFunctionAnalysis,
    {
        let id = A::id();
        assert!(
            !matches!(self.from_analysis_id, Some(n) if id == n),
            "Analysis cannot request its own result"
        );

        let res = crate::get_function_analysis_cached_result(
            self.inner,
            id,
            function.as_value_ref().cast(),
        );

        if !res.is_null() {
            let res = unsafe { Box::leak(Box::from_raw(res.cast())) };
            Some(res)
        } else {
            None
        }
    }

    /// Register an analysis pass to the analysis manager.
    ///
    /// # Panics
    ///
    /// Panics if the given analysis was already registered.
    pub fn register_pass<T>(&mut self, pass: T)
    where
        T: LlvmFunctionAnalysis,
    {
        let pass = Box::new(pass);

        extern "C" fn result_deleter<T>(data: *mut c_void)
        where
            T: LlvmFunctionAnalysis,
        {
            drop(unsafe { Box::<<T as LlvmFunctionAnalysis>::Result>::from_raw(data.cast()) })
        }

        extern "C" fn pass_deleter<T>(pass: *mut c_void) {
            drop(unsafe { Box::<T>::from_raw(pass.cast()) })
        }

        extern "C" fn pass_entrypoint<T>(
            pass: *mut c_void,
            function: *mut c_void,
            manager: *mut c_void,
            res: *mut *mut c_void,
            res_deleter: *mut extern "C" fn(*mut c_void),
        ) where
            T: LlvmFunctionAnalysis,
        {
            let pass = unsafe { Box::<T>::from_raw(pass.cast()) };
            let function = unsafe { FunctionValue::new(function.cast()).unwrap() };
            let manager = unsafe { FunctionAnalysisManager::from_raw(manager, Some(T::id())) };

            let data = pass.run_analysis(&function, &manager);

            let data = Box::new(data);
            unsafe {
                *res = Box::<<T as LlvmFunctionAnalysis>::Result>::into_raw(data).cast();
                *res_deleter = result_deleter::<T>;
            }

            let _ = Box::into_raw(pass);
            #[allow(forgetting_copy_types)]
            std::mem::forget(function);
        }

        let success = unsafe {
            super::functionAnalysisManagerRegisterPass(
                self.inner,
                Box::into_raw(pass).cast(),
                pass_deleter::<T>,
                pass_entrypoint::<T>,
                T::id(),
            )
        };

        assert!(success, "analysis already registered");
    }
}

/// Struct allowing to query the pass manager for the result of
/// analyses on module IR.
pub struct ModuleAnalysisManager {
    inner: *mut c_void,
    from_analysis_id: Option<crate::AnalysisKey>,
}

impl ModuleAnalysisManager {
    #[doc(hidden)]
    pub unsafe fn from_raw(
        inner: *mut c_void,
        from_analysis_id: Option<crate::AnalysisKey>,
    ) -> Self {
        Self {
            inner,
            from_analysis_id,
        }
    }

    /// Returns the result of the analysis on a given module IR.
    ///
    /// If the result is not in cache, the pass manager will execute the
    /// analysis pass. Otherwise, the result is directly returned from cache.
    ///
    /// # Panics
    ///
    /// Panics if the given analysis wasn't registered, or if this function was
    /// called within the given analysis itself.
    pub fn get_result<A>(&self, module: &Module<'_>) -> &A::Result
    where
        A: crate::LlvmModuleAnalysis,
    {
        let id = A::id();
        assert!(
            !matches!(self.from_analysis_id, Some(n) if id == n),
            "Analysis cannot request its own result"
        );

        let res =
            crate::get_module_analysis_result(self.inner, A::id(), module.as_mut_ptr().cast());

        unsafe { Box::leak(Box::from_raw(res.cast())) }
    }

    /// Returns the result of the analysis on a given module IR.
    ///
    /// If the result is not in cache, `None` is returned. Otherwise,
    /// the result is directly returned from cache.
    ///
    /// This function never triggers the execution of an analysis.
    ///
    /// # Panics
    ///
    /// Panics if the given analysis wasn't registered, or if this function was
    /// called within the given analysis itself.
    pub fn get_cached_result<A>(&self, module: &Module<'_>) -> Option<&A::Result>
    where
        A: crate::LlvmModuleAnalysis,
    {
        let id = A::id();
        assert!(
            !matches!(self.from_analysis_id, Some(n) if id == n),
            "Analysis cannot request its own result"
        );

        let res = crate::get_module_analysis_cached_result(
            self.inner,
            A::id(),
            module.as_mut_ptr().cast(),
        );

        if !res.is_null() {
            let res = unsafe { Box::leak(Box::from_raw(res.cast())) };
            Some(res)
        } else {
            None
        }
    }

    /// Returns a [FunctionAnalysisManagerProxy], which is essentially an interface
    /// allowing management of analyses at the function level.
    pub fn get_function_analysis_manager_proxy(
        &self,
        module: &Module<'_>,
    ) -> FunctionAnalysisManagerProxy {
        let proxy = crate::get_function_analysis_manager_module_proxy(
            self.inner,
            module.as_mut_ptr().cast(),
        );
        FunctionAnalysisManagerProxy { inner: proxy }
    }

    /// Register an analysis pass to the analysis manager.
    ///
    /// # Panics
    ///
    /// Panics if the given analysis was already registered.
    pub fn register_pass<T>(&mut self, pass: T)
    where
        T: LlvmModuleAnalysis,
    {
        let pass = Box::new(pass);

        extern "C" fn result_deleter<T>(data: *mut c_void)
        where
            T: LlvmModuleAnalysis,
        {
            drop(unsafe { Box::<<T as LlvmModuleAnalysis>::Result>::from_raw(data.cast()) })
        }

        extern "C" fn pass_deleter<T>(pass: *mut c_void) {
            drop(unsafe { Box::<T>::from_raw(pass.cast()) })
        }

        extern "C" fn pass_entrypoint<T>(
            pass: *mut c_void,
            module: *mut c_void,
            manager: *mut c_void,
            res: *mut *mut c_void,
            res_deleter: *mut extern "C" fn(*mut c_void),
        ) where
            T: LlvmModuleAnalysis,
        {
            let pass = unsafe { Box::<T>::from_raw(pass.cast()) };
            let module = unsafe { Module::new(module.cast()) };
            let manager = unsafe { ModuleAnalysisManager::from_raw(manager, Some(T::id())) };

            let data = pass.run_analysis(&module, &manager);

            let data = Box::new(data);
            unsafe {
                *res = Box::<<T as LlvmModuleAnalysis>::Result>::into_raw(data).cast();
                *res_deleter = result_deleter::<T>;
            }

            let _ = Box::into_raw(pass);
            std::mem::forget(module);
        }

        let success = unsafe {
            super::moduleAnalysisManagerRegisterPass(
                self.inner,
                Box::into_raw(pass).cast(),
                pass_deleter::<T>,
                pass_entrypoint::<T>,
                T::id(),
            )
        };

        assert!(success, "analysis already registered");
    }
}

/// Struct allowing to make queries to the pass manager about function-level
/// analyses.
///
/// The main use-case of such interface is to give the ability for module-level
/// passes to trigger/query function-level analyses.
///
/// # Example
///
/// ```
/// # use llvm_plugin::inkwell::module::Module;
/// # use llvm_plugin::inkwell::values::FunctionValue;
/// # use llvm_plugin::{
/// #    AnalysisKey, FunctionAnalysisManager, LlvmFunctionAnalysis, LlvmModulePass,
/// #    ModuleAnalysisManager, PreservedAnalyses,
/// # };
/// struct Pass;
/// impl LlvmModulePass for Pass {
///     fn run_pass(
///         &self,
///         module: &mut Module,
///         manager: &ModuleAnalysisManager,
///     ) -> PreservedAnalyses {
///         let manager = manager
///             .get_function_analysis_manager_proxy(&module)
///             .get_manager();
///
///         let function = module.get_first_function().unwrap();
///         let result = manager.get_result::<Analysis>(&function);
///         assert_eq!(result, "Some result");
///
///         PreservedAnalyses::All
///     }
/// }
///
/// struct Analysis;
/// impl LlvmFunctionAnalysis for Analysis {
///     type Result = String;
///
///     fn run_analysis(
///         &self,
///         _function: &FunctionValue,
///         _manager: &FunctionAnalysisManager,
///     ) -> Self::Result {
///         "Some result".to_owned()
///     }
///
///     fn id() -> AnalysisKey {
///         1 as AnalysisKey
///     }
/// }
/// ```
pub struct FunctionAnalysisManagerProxy {
    inner: *mut c_void,
}

impl FunctionAnalysisManagerProxy {
    /// Returns the inner [FunctionAnalysisManager].
    pub fn get_manager(&self) -> FunctionAnalysisManager {
        let manager = crate::get_function_analysis_manager(self.inner);
        FunctionAnalysisManager {
            inner: manager,
            from_analysis_id: None,
        }
    }
}
