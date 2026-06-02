use std::ffi::c_void;

use super::{
    FunctionAnalysisManager, FunctionPassManager, ModuleAnalysisManager, ModulePassManager,
};

/// Main struct for registering callbacks.
pub struct PassBuilder {
    inner: *mut c_void,
}

impl PassBuilder {
    #[doc(hidden)]
    pub unsafe fn from_raw(pass_builder: *mut c_void) -> Self {
        Self {
            inner: pass_builder,
        }
    }

    /// Register a new pipeline parsing callback.
    ///
    /// These callbacks can be used to parse a single pass name, and populate
    /// the given [ModulePassManager] accordingly.
    pub fn add_module_pipeline_parsing_callback<T>(&mut self, cb: T)
    where
        T: Fn(&str, &mut ModulePassManager) -> PipelineParsing + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            name_ptr: *const u8,
            name_len: usize,
            manager: *mut c_void,
        ) -> bool
        where
            T: Fn(&str, &mut ModulePassManager) -> PipelineParsing + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let name = unsafe { std::slice::from_raw_parts(name_ptr, name_len) };
            let name = unsafe { std::str::from_utf8_unchecked(name) };
            let mut manager = unsafe { ModulePassManager::from_raw(manager) };

            let res = cb(name, &mut manager);

            let _ = Box::into_raw(cb);
            matches!(res, PipelineParsing::Parsed)
        }

        unsafe {
            super::passBuilderAddModulePipelineParsingCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new pipeline parsing callback.
    ///
    /// These callbacks can be used to parse a single pass name, and populate
    /// the given [FunctionPassManager] accordingly.
    pub fn add_function_pipeline_parsing_callback<T>(&mut self, cb: T)
    where
        T: Fn(&str, &mut FunctionPassManager) -> PipelineParsing + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            name_ptr: *const u8,
            name_len: usize,
            manager: *mut c_void,
        ) -> bool
        where
            T: Fn(&str, &mut FunctionPassManager) -> PipelineParsing + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let name = unsafe { std::slice::from_raw_parts(name_ptr, name_len) };
            let name = unsafe { std::str::from_utf8_unchecked(name) };
            let mut manager = unsafe { FunctionPassManager::from_raw(manager) };

            let res = cb(name, &mut manager);

            let _ = Box::into_raw(cb);
            matches!(res, PipelineParsing::Parsed)
        }

        unsafe {
            super::passBuilderAddFunctionPipelineParsingCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback for analysis registration.
    ///
    /// These callbacks can be used to register custom analyses with the given
    /// [ModuleAnalysisManager].
    pub fn add_module_analysis_registration_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut ModuleAnalysisManager) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(cb: *const c_void, manager: *mut c_void)
        where
            T: Fn(&mut ModuleAnalysisManager) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { ModuleAnalysisManager::from_raw(manager, None) };

            cb(&mut manager);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddModuleAnalysisRegistrationCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback for analysis registration.
    ///
    /// These callbacks can be used to register custom analyses with the given
    /// [FunctionAnalysisManager].
    pub fn add_function_analysis_registration_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut FunctionAnalysisManager) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(cb: *const c_void, manager: *mut c_void)
        where
            T: Fn(&mut FunctionAnalysisManager) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { FunctionAnalysisManager::from_raw(manager, None) };

            cb(&mut manager);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddFunctionAnalysisRegistrationCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the peephole
    /// extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extension point allows adding passes that perform peephole
    /// optimizations similar to the instruction combiner.
    ///
    /// These passes will be inserted after each instance of the instruction
    /// combiner pass.
    pub fn add_peephole_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut FunctionPassManager, OptimizationLevel) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
        ) where
            T: Fn(&mut FunctionPassManager, OptimizationLevel) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { FunctionPassManager::from_raw(manager) };

            cb(&mut manager, opt);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddPeepholeEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the optimizer
    /// late extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extension point allows adding optimization passes after
    /// most of the main optimizations, but before the last cleanup-ish
    /// optimizations.
    pub fn add_scalar_optimizer_late_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut FunctionPassManager, OptimizationLevel) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
        ) where
            T: Fn(&mut FunctionPassManager, OptimizationLevel) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { FunctionPassManager::from_raw(manager) };

            cb(&mut manager, opt);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddScalarOptimizerLateEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the vectorizer
    /// start extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extension point allows adding optimization passes before
    /// the vectorizer and other highly target specific optimization
    /// passes are executed.
    pub fn add_vectorizer_start_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut FunctionPassManager, OptimizationLevel) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
        ) where
            T: Fn(&mut FunctionPassManager, OptimizationLevel) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { FunctionPassManager::from_raw(manager) };

            cb(&mut manager, opt);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddVectorizerStartEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the pipeline
    /// start extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extension point allows adding optimization once at the start
    /// of the pipeline. This does not apply to 'backend' compiles (LTO and
    /// ThinLTO link-time pipelines).
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
    pub fn add_pipeline_start_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
        ) where
            T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { ModulePassManager::from_raw(manager) };

            cb(&mut manager, opt);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddPipelineStartEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the pipeline
    /// early simplification extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extension point allows adding optimization right after passes
    /// that do basic simplification of the input IR.
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
    ))]
    pub fn add_pipeline_early_simplification_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
        ) where
            T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { ModulePassManager::from_raw(manager) };

            cb(&mut manager, opt);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddPipelineEarlySimplificationEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the pipeline
    /// early simplification extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extension point allows adding optimization right after passes
    /// that do basic simplification of the input IR.
    #[cfg(any(doc, feature = "llvm20-1", feature = "llvm21-1"))]
    pub fn add_pipeline_early_simplification_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut ModulePassManager, OptimizationLevel, ThinOrFullLTOPhase) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
            phase: ThinOrFullLTOPhase,
        ) where
            T: Fn(&mut ModulePassManager, OptimizationLevel, ThinOrFullLTOPhase) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { ModulePassManager::from_raw(manager) };

            cb(&mut manager, opt, phase);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddPipelineEarlySimplificationEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the optimizer
    /// last extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extension point allows adding passes that run after everything
    /// else.
    #[cfg(any(
        doc,
        feature = "llvm11-0",
        feature = "llvm12-0",
        feature = "llvm13-0",
        feature = "llvm14-0",
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
    ))]
    pub fn add_optimizer_last_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
        ) where
            T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { ModulePassManager::from_raw(manager) };

            cb(&mut manager, opt);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddOptimizerLastEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the optimizer
    /// last extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extension point allows adding passes that run after everything
    /// else.
    #[cfg(any(doc, feature = "llvm20-1", feature = "llvm21-1"))]
    pub fn add_optimizer_last_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut ModulePassManager, OptimizationLevel, ThinOrFullLTOPhase) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
            phase: ThinOrFullLTOPhase,
        ) where
            T: Fn(&mut ModulePassManager, OptimizationLevel, ThinOrFullLTOPhase) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { ModulePassManager::from_raw(manager) };

            cb(&mut manager, opt, phase);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddOptimizerLastEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the full LTO
    /// early extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extension point allow adding passes that run at Link Time,
    /// before Full Link Time Optimization.
    #[cfg(any(
        doc,
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
        feature = "llvm21-1"
    ))]
    pub fn add_full_lto_early_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
        ) where
            T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { ModulePassManager::from_raw(manager) };

            cb(&mut manager, opt);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddFullLinkTimeOptimizationEarlyEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the full LTO
    /// last extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extensions point allow adding passes that run at Link Time,
    /// after Full Link Time Optimization.
    #[cfg(any(
        doc,
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
        feature = "llvm21-1"
    ))]
    pub fn add_full_lto_last_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
        ) where
            T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { ModulePassManager::from_raw(manager) };

            cb(&mut manager, opt);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddFullLinkTimeOptimizationLastEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the optimizer
    /// early extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extension point allows adding passes just before the main
    /// module-level optimization passes.
    #[cfg(any(
        doc,
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
    ))]
    pub fn add_optimizer_early_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
        ) where
            T: Fn(&mut ModulePassManager, OptimizationLevel) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { ModulePassManager::from_raw(manager) };

            cb(&mut manager, opt);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddOptimizerEarlyEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }

    /// Register a new callback to be triggered at the optimizer
    /// early extension point.
    ///
    /// # From the LLVM documentation
    ///
    /// This extension point allows adding passes just before the main
    /// module-level optimization passes.
    #[cfg(any(doc, feature = "llvm20-1", feature = "llvm21-1"))]
    pub fn add_optimizer_early_ep_callback<T>(&mut self, cb: T)
    where
        T: Fn(&mut ModulePassManager, OptimizationLevel, ThinOrFullLTOPhase) + 'static,
    {
        let cb = Box::new(cb);

        extern "C" fn callback_entrypoint<T>(
            cb: *const c_void,
            manager: *mut c_void,
            opt: OptimizationLevel,
            phase: ThinOrFullLTOPhase,
        ) where
            T: Fn(&mut ModulePassManager, OptimizationLevel, ThinOrFullLTOPhase) + 'static,
        {
            let cb = unsafe { Box::<T>::from_raw(cb as *mut _) };
            let mut manager = unsafe { ModulePassManager::from_raw(manager) };

            cb(&mut manager, opt, phase);

            let _ = Box::into_raw(cb);
        }

        unsafe {
            super::passBuilderAddOptimizerEarlyEPCallback(
                self.inner,
                Box::into_raw(cb).cast(),
                callback_deleter::<T>,
                callback_entrypoint::<T>,
            )
        }
    }
}

extern "C" fn callback_deleter<T>(cb: *const c_void) {
    drop(unsafe { Box::<T>::from_raw(cb as *mut _) })
}

/// Enum describing whether a pipeline parsing callback
/// successfully parsed its given pipeline element.
#[derive(Clone, Copy)]
pub enum PipelineParsing {
    /// The pipeline element was successfully parsed.
    Parsed,

    /// The pipeline element wasn't parsed.
    NotParsed,
}

/// Enum for the LLVM-provided high-level optimization levels.
///
/// Each level has a specific goal and rationale.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum OptimizationLevel {
    /// This level disables as many optimizations as possible.
    O0,

    /// This level optimizes quickly without destroying debuggability.
    O1,

    /// This level optimizes for fast execution as much as possible
    /// without triggering significant incremental compile time or
    /// code size growth.
    O2,

    /// This level optimizes for fast execution as much as possible.
    O3,

    /// This level is similar to **O2** but tries to optimize
    /// for small code size instead of fast execution without
    /// triggering significant incremental execution time slowdowns.
    Os,

    /// This level will optimize for code size at any and all costs.
    Oz,
}

/// Enum for the LLVM-provided full LTO or ThinLTO optimization phases.
#[cfg(any(doc, feature = "llvm20-1", feature = "llvm21-1"))]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum ThinOrFullLTOPhase {
    /// No LTO/ThinLTO behavior needed.
    None,

    /// ThinLTO prelink (summary) phase.
    ThinLTOPreLink,

    /// ThinLTO postlink (backend compile) phase.
    ThinLTOPostLink,

    /// Full LTO prelink phase.
    FullLTOPreLink,

    /// Full LTO postlink (backend compile) phase.
    FullLTOPostLink,
}
