use std::ffi::c_void;

use inkwell::module::Module;
use inkwell::values::FunctionValue;

use super::{FunctionAnalysisManager, LlvmFunctionPass, LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};

/// Struct allowing to add passes on LLVM IR modules to the pass manager
/// pipeline.
pub struct ModulePassManager {
    inner: *mut c_void,
    #[cfg(any(feature = "llvm22-1", feature = "llvm23-1"))]
    owned: bool,
}

impl ModulePassManager {
    #[doc(hidden)]
    pub unsafe fn from_raw(pass_manager: *mut c_void) -> Self {
        Self {
            inner: pass_manager,
            #[cfg(any(feature = "llvm22-1", feature = "llvm23-1"))]
            owned: false,
        }
    }

    /// Creates an empty module pass manager.
    #[cfg(any(feature = "llvm22-1", feature = "llvm23-1"))]
    pub fn new() -> Self {
        Self {
            inner: unsafe { super::modulePassManagerCreate() },
            owned: true,
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
            let mut pass = unsafe { Box::<T>::from_raw(pass.cast()) };
            let mut module = unsafe { Module::new(module.cast()) };
            let manager = unsafe { ModuleAnalysisManager::from_raw(manager, None) };

            let preserve = run_pass_with_policy(&mut *pass, &mut module, &manager);

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
        feature = "llvm21-1",
        feature = "llvm22-1",
        feature = "llvm23-1"
    ))]
    pub fn is_empty(&self) -> bool {
        unsafe { super::modulePassManagerIsEmpty(self.inner) }
    }

    /// Runs this pass manager on a module.
    #[cfg(any(feature = "llvm22-1", feature = "llvm23-1"))]
    pub fn run_on_module(&mut self, module: &mut Module<'_>) -> PreservedAnalyses {
        unsafe { super::modulePassManagerRun(self.inner, module.as_mut_ptr().cast()) }
    }
}

#[cfg(any(feature = "llvm22-1", feature = "llvm23-1"))]
impl Drop for ModulePassManager {
    fn drop(&mut self) {
        if self.owned && !self.inner.is_null() {
            unsafe { super::modulePassManagerDelete(self.inner) }
        }
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
        Self { inner: pass_manager }
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
        feature = "llvm21-1",
        feature = "llvm22-1",
        feature = "llvm23-1"
    ))]
    pub fn is_empty(&self) -> bool {
        unsafe { super::functionPassManagerIsEmpty(self.inner) }
    }
}

/// Pass 内部的 panic 不能穿透 extern "C" 边界（UB，直接 SIGILL/abort）。
/// 默认 fail-open：捕获 panic、告警并跳过该 Pass——对混淆器而言，构建
/// 失败比缺失部分混淆更糟。AMICE_PANIC_POLICY=abort 可切换为 fail-closed。
fn run_pass_with_policy<T: LlvmModulePass>(
    pass: &mut T,
    module: &mut Module<'_>,
    manager: &ModuleAnalysisManager,
) -> PreservedAnalyses {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pass.run_pass(module, manager))) {
        Ok(preserve) => preserve,
        Err(_) => {
            eprintln!(
        "[amice] ERROR: pass {:?} panicked; skipping this pass for the module (fail-open).                          set AMICE_PANIC_POLICY=abort to fail the build instead",
        std::any::type_name::<T>()
    );
            let abort_policy = std::env::var("AMICE_PANIC_POLICY")
                .map(|v| v.eq_ignore_ascii_case("abort"))
                .unwrap_or(false);
            if abort_policy {
                std::process::abort();
            }
            // 部分变换后模块仍可能可用，但分析状态已不可信，强制重分析
            PreservedAnalyses::None
        },
    }
}

#[cfg(test)]
mod pass_policy_tests {
    use super::*;
    use inkwell::context::Context;
    use std::ptr;

    /// run_pass 必然 panic 的测试 Pass。manager 指针不会被它触碰。
    struct PanickyPass;
    impl LlvmModulePass for PanickyPass {
        fn run_pass(&self, _module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
            panic!("boom");
        }
    }

    struct AllPreservedPass;
    impl LlvmModulePass for AllPreservedPass {
        fn run_pass(&self, _module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
            PreservedAnalyses::All
        }
    }

    /// manager 指针指向一个存活对象，避免悬垂；这些测试里的 dummy pass
    /// 不会真正解引用它。
    fn fake_manager() -> ModuleAnalysisManager {
        let live: u64 = 0;
        unsafe { ModuleAnalysisManager::from_raw(&live as *const u64 as *mut c_void, None) }
    }

    fn with_policy<F: FnOnce()>(policy: Option<&str>, f: F) {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        match policy {
            Some(v) => std::env::set_var("AMICE_PANIC_POLICY", v),
            None => std::env::remove_var("AMICE_PANIC_POLICY"),
        }
        f();
        match policy {
            Some(v) => std::env::set_var("AMICE_PANIC_POLICY", v),
            None => std::env::remove_var("AMICE_PANIC_POLICY"),
        }
        std::panic::set_hook(prev_hook);
    }

    #[test]
    fn panicking_pass_falls_back_to_none_by_default() {
        let context = Context::create();
        let mut module = context.create_module("t");
        let manager = fake_manager();
        with_policy(None, || {
            let preserve = run_pass_with_policy(&mut PanickyPass, &mut module, &manager);
            assert!(matches!(preserve, PreservedAnalyses::None));
        });
    }

    #[test]
    fn panicking_pass_falls_back_with_explicit_skip() {
        let context = Context::create();
        let mut module = context.create_module("t");
        let manager = fake_manager();
        with_policy(Some("skip"), || {
            let preserve = run_pass_with_policy(&mut PanickyPass, &mut module, &manager);
            assert!(matches!(preserve, PreservedAnalyses::None));
        });
    }

    #[test]
    fn normal_pass_result_propagates_unchanged() {
        let context = Context::create();
        let mut module = context.create_module("t");
        let manager = fake_manager();
        with_policy(None, || {
            let preserve = run_pass_with_policy(&mut AllPreservedPass, &mut module, &manager);
            assert!(matches!(preserve, PreservedAnalyses::All));
        });
    }
}
