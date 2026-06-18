use std::ffi::c_void;

pub type AnalysisKey = *const u8;

#[link(name = "amice-plugin-cpp")]
extern "C" {
    #[cfg(any(
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
        feature = "llvm21-1",
        feature = "llvm22-1"
    ))]
    pub(crate) fn passBuilderAddFullLinkTimeOptimizationLastEPCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        cb_sys: extern "C" fn(*const c_void, *mut c_void, crate::OptimizationLevel),
    );

    #[cfg(any(
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
        feature = "llvm21-1",
        feature = "llvm22-1"
    ))]
    pub(crate) fn passBuilderAddFullLinkTimeOptimizationEarlyEPCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        cb_sys: extern "C" fn(*const c_void, *mut c_void, crate::OptimizationLevel),
    );

    #[cfg(any(
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
        feature = "llvm21-1",
        feature = "llvm22-1"
    ))]
    pub(crate) fn passBuilderAddOptimizerEarlyEPCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        #[cfg(not(any(feature = "llvm20-1", feature = "llvm21-1", feature = "llvm22-1")))] cb_sys: extern "C" fn(
            *const c_void,
            *mut c_void,
            crate::OptimizationLevel,
        ),
        #[cfg(any(feature = "llvm20-1", feature = "llvm21-1", feature = "llvm22-1"))] cb_sys: extern "C" fn(
            *const c_void,
            *mut c_void,
            crate::OptimizationLevel,
            crate::ThinOrFullLTOPhase,
        ),
    );

    #[cfg(any(
        feature = "llvm11-0",
        feature = "llvm12-0",
        feature = "llvm13-0",
        feature = "llvm14-0",
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
        feature = "llvm21-1",
        feature = "llvm22-1"
    ))]
    pub(crate) fn passBuilderAddOptimizerLastEPCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        #[cfg(not(any(feature = "llvm20-1", feature = "llvm21-1", feature = "llvm22-1")))] cb_sys: extern "C" fn(
            *const c_void,
            *mut c_void,
            crate::OptimizationLevel,
        ),
        #[cfg(any(feature = "llvm20-1", feature = "llvm21-1", feature = "llvm22-1"))] cb_sys: extern "C" fn(
            *const c_void,
            *mut c_void,
            crate::OptimizationLevel,
            crate::ThinOrFullLTOPhase,
        ),
    );

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
        feature = "llvm21-1",
        feature = "llvm22-1"
    ))]
    pub(crate) fn passBuilderAddPipelineEarlySimplificationEPCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        #[cfg(not(any(feature = "llvm20-1", feature = "llvm21-1", feature = "llvm22-1")))] cb_sys: extern "C" fn(
            *const c_void,
            *mut c_void,
            crate::OptimizationLevel,
        ),
        #[cfg(any(feature = "llvm20-1", feature = "llvm21-1", feature = "llvm22-1"))] cb_sys: extern "C" fn(
            *const c_void,
            *mut c_void,
            crate::OptimizationLevel,
            crate::ThinOrFullLTOPhase,
        ),
    );

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
        feature = "llvm21-1",
        feature = "llvm22-1"
    ))]
    pub(crate) fn passBuilderAddPipelineStartEPCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        cb_sys: extern "C" fn(*const c_void, *mut c_void, crate::OptimizationLevel),
    );

    pub(crate) fn passBuilderAddVectorizerStartEPCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        cb_sys: extern "C" fn(*const c_void, *mut c_void, crate::OptimizationLevel),
    );

    pub(crate) fn passBuilderAddScalarOptimizerLateEPCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        cb_sys: extern "C" fn(*const c_void, *mut c_void, crate::OptimizationLevel),
    );

    pub(crate) fn passBuilderAddPeepholeEPCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        cb_sys: extern "C" fn(*const c_void, *mut c_void, crate::OptimizationLevel),
    );

    pub(crate) fn passBuilderAddModuleAnalysisRegistrationCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        cb_sys: extern "C" fn(*const c_void, *mut c_void),
    );

    pub(crate) fn passBuilderAddFunctionAnalysisRegistrationCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        cb_sys: extern "C" fn(*const c_void, *mut c_void),
    );

    pub(crate) fn passBuilderAddModulePipelineParsingCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        cb_sys: extern "C" fn(*const c_void, *const u8, usize, *mut c_void) -> bool,
    );

    pub(crate) fn passBuilderAddFunctionPipelineParsingCallback(
        builder: *mut c_void,
        cb: *const c_void,
        cb_deleter: extern "C" fn(*const c_void),
        cb_sys: extern "C" fn(*const c_void, *const u8, usize, *mut c_void) -> bool,
    );

    pub(crate) fn modulePassManagerAddPass(
        manager: *mut c_void,
        pass: *mut c_void,
        pass_deleter: extern "C" fn(*mut c_void),
        pass_sys: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> crate::PreservedAnalyses,
    );

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
        feature = "llvm21-1",
        feature = "llvm22-1"
    ))]
    pub(crate) fn modulePassManagerIsEmpty(manager: *mut c_void) -> bool;

    pub(crate) fn functionPassManagerAddPass(
        manager: *mut c_void,
        pass: *mut c_void,
        pass_deleter: extern "C" fn(*mut c_void),
        pass_sys: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> crate::PreservedAnalyses,
    );

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
        feature = "llvm21-1",
        feature = "llvm22-1"
    ))]
    pub(crate) fn functionPassManagerIsEmpty(manager: *mut c_void) -> bool;

    pub(crate) fn moduleAnalysisManagerRegisterPass(
        manager: *mut c_void,
        pass: *mut c_void,
        pass_deleter: extern "C" fn(*mut c_void),
        pass_sys: extern "C" fn(
            pass: *mut c_void,
            module: *mut c_void,
            manager: *mut c_void,
            res: *mut *mut c_void,
            res_deleter: *mut extern "C" fn(*mut c_void),
        ),
        id: AnalysisKey,
    ) -> bool;

    pub(crate) fn functionAnalysisManagerRegisterPass(
        manager: *mut c_void,
        pass: *mut c_void,
        pass_deleter: extern "C" fn(*mut c_void),
        pass_sys: extern "C" fn(
            pass: *mut c_void,
            module: *mut c_void,
            manager: *mut c_void,
            res: *mut *mut c_void,
            res_deleter: *mut extern "C" fn(*mut c_void),
        ),
        id: AnalysisKey,
    ) -> bool;

    fn getFunctionAnalysisManagerModuleProxy(manager: *mut c_void, function: *mut c_void) -> *mut c_void;

    fn getFunctionAnalysisManager(manager_proxy: *mut c_void) -> *mut c_void;

    fn getFunctionAnalysisResult(manager: *mut c_void, id: AnalysisKey, function: *mut c_void) -> *mut c_void;

    fn getModuleAnalysisResult(manager: *mut c_void, id: AnalysisKey, module: *mut c_void) -> *mut c_void;

    fn getFunctionAnalysisCachedResult(manager: *mut c_void, id: AnalysisKey, function: *mut c_void) -> *mut c_void;

    fn getModuleAnalysisCachedResult(manager: *mut c_void, id: AnalysisKey, module: *mut c_void) -> *mut c_void;

    fn llvmPluginApiVersion() -> u32;
}

pub(super) fn get_function_analysis_manager_module_proxy(manager: *mut c_void, function: *mut c_void) -> *mut c_void {
    unsafe { getFunctionAnalysisManagerModuleProxy(manager, function) }
}

pub(super) fn get_function_analysis_manager(manager_proxy: *mut c_void) -> *mut c_void {
    unsafe { getFunctionAnalysisManager(manager_proxy) }
}

pub(super) fn get_module_analysis_result(manager: *mut c_void, id: AnalysisKey, module: *mut c_void) -> *mut c_void {
    unsafe { getModuleAnalysisResult(manager, id, module) }
}

pub(super) fn get_function_analysis_result(
    manager: *mut c_void,
    id: AnalysisKey,
    function: *mut c_void,
) -> *mut c_void {
    unsafe { getFunctionAnalysisResult(manager, id, function) }
}

pub(super) fn get_module_analysis_cached_result(
    manager: *mut c_void,
    id: AnalysisKey,
    module: *mut c_void,
) -> *mut c_void {
    unsafe { getModuleAnalysisCachedResult(manager, id, module) }
}

pub(super) fn get_function_analysis_cached_result(
    manager: *mut c_void,
    id: AnalysisKey,
    function: *mut c_void,
) -> *mut c_void {
    unsafe { getFunctionAnalysisCachedResult(manager, id, function) }
}

#[doc(hidden)]
pub fn get_llvm_plugin_api_version__() -> u32 {
    unsafe { llvmPluginApiVersion() }
}
