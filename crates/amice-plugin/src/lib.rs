//! In-tree LLVM pass-plugin runtime used by amice.

#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

mod analysis;
pub use analysis::*;

#[doc(hidden)]
pub mod ffi;
#[doc(hidden)]
pub use ffi::*;

pub use inkwell;
use inkwell::module::Module;
use inkwell::values::FunctionValue;

mod pass_manager;
pub use pass_manager::*;

mod pass_builder;
pub use pass_builder::*;

/// Enum specifying whether analyses on an IR unit are not preserved due
/// to the modification of such unit by a transformation pass.
#[repr(C)]
#[derive(Clone, Copy)]
pub enum PreservedAnalyses {
    /// This variant hints the pass manager that all the analyses are
    /// preserved, so there is no need to re-execute analysis passes.
    ///
    /// Use this variant when a transformation pass doesn't modify some
    /// IR unit.
    All,

    /// This variant hints the pass manager that all the analyses should
    /// be re-executed.
    ///
    /// Use this variant when a transformation pass modifies some IR unit.
    None,
}

/// Trait to use for implementing a transformation pass on an LLVM module.
///
/// A transformation pass is allowed to mutate the LLVM IR.
pub trait LlvmModulePass {
    /// Entrypoint for the pass.
    ///
    /// The given analysis manager allows the pass to query the pass
    /// manager for the result of specific analysis passes.
    ///
    /// If this function makes modifications on the given module IR, it
    /// should return `PreservedAnalyses::None` to indicate to the
    /// pass manager that all analyses are now invalidated.
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses;
}

/// Trait to use for implementing a transformation pass on an LLVM function.
///
/// A transformation pass is allowed to mutate the LLVM IR.
pub trait LlvmFunctionPass {
    /// Entrypoint for the pass.
    ///
    /// The given analysis manager allows the pass to query the pass
    /// manager for the result of specific analysis passes.
    ///
    /// If this function makes modifications on the given function IR, it
    /// should return `PreservedAnalyses::None` to indicate to the
    /// pass manager that all analyses are now invalidated.
    fn run_pass(&self, function: &mut FunctionValue<'_>, manager: &FunctionAnalysisManager) -> PreservedAnalyses;
}

/// Trait to use for implementing an analysis pass on an LLVM module.
///
/// An analysis pass is not allowed to mutate the LLVM IR.
pub trait LlvmModuleAnalysis {
    /// Result of the successful execution of this pass by the pass manager.
    ///
    /// This data can be queried by passes through a [ModuleAnalysisManager].
    type Result;

    /// Entrypoint for the pass.
    ///
    /// The given analysis manager allows the pass to query the pass
    /// manager for the result of specific analysis passes.
    ///
    /// The returned result will be moved into a [Box](`std::boxed::Box`)
    /// before being given to the pass manager. This one will then add it to
    /// its internal cache, to avoid unnecessary calls to this entrypoint.
    fn run_analysis(&self, module: &Module<'_>, manager: &ModuleAnalysisManager) -> Self::Result;

    /// Identifier for the analysis type.
    ///
    /// This ID must be unique for each registered analysis type.
    ///
    /// # Warning
    ///
    /// The LLVM toolchain (e.g. [opt], [lld]) often registers builtin analysis
    /// types during execution of passes. These builtin analyses always use
    /// the address of global static variables as IDs, to prevent collisions.
    ///
    /// To make sure your custom analysis types don't collide with the builtin
    /// ones used by the LLVM tool that loads your plugin, you should use static
    /// variables' addresses as well.
    ///
    /// [opt]: https://www.llvm.org/docs/CommandGuide/opt.html
    /// [lld]: https://lld.llvm.org/
    fn id() -> AnalysisKey;
}

/// Trait to use for implementing an analysis pass on an LLVM function.
///
/// An analysis pass is not allowed to mutate the LLVM IR.
pub trait LlvmFunctionAnalysis {
    /// Result of the successful execution of this pass by the pass manager.
    ///
    /// This data can be queried by passes through a [FunctionAnalysisManager].
    type Result;

    /// Entrypoint for the pass.
    ///
    /// The given analysis manager allows the pass to query the pass
    /// manager for the result of specific analysis passes.
    ///
    /// The returned result will be moved into a [Box](`std::boxed::Box`)
    /// before being given to the pass manager. This one will then add it to
    /// its internal cache, to avoid unnecessary calls to this entrypoint.
    fn run_analysis(&self, module: &FunctionValue<'_>, manager: &FunctionAnalysisManager) -> Self::Result;

    /// Identifier for the analysis type.
    ///
    /// This ID must be unique for each registered analysis type.
    ///
    /// # Warning
    ///
    /// The LLVM toolchain (e.g. [opt], [lld]) often registers builtin analysis
    /// types during execution of passes. These builtin analyses always use
    /// the address of global static variables as IDs, to prevent collisions.
    ///
    /// To make sure your custom analysis types don't collide with the builtin
    /// ones used by the LLVM tool that loads your plugin, you should use static
    /// variables' addresses as well.
    ///
    /// [opt]: https://www.llvm.org/docs/CommandGuide/opt.html
    /// [lld]: https://lld.llvm.org/
    fn id() -> AnalysisKey;
}

#[doc(hidden)]
#[repr(C)]
pub struct PassPluginLibraryInfo {
    pub api_version: u32,
    pub plugin_name: *const u8,
    pub plugin_version: *const u8,
    pub plugin_registrar: unsafe extern "C" fn(*mut std::ffi::c_void),
    #[cfg(feature = "llvm22-1")]
    pub pre_code_gen_callback: *const std::ffi::c_void,
}

#[cfg(feature = "macros")]
pub use amice_plugin_macros::*;

#[cfg(all(
    target_os = "windows",
    any(
        all(feature = "win-link-opt", feature = "win-link-lld"),
        all(not(feature = "win-link-opt"), not(feature = "win-link-lld"))
    )
))]
compile_error!(
    "Either `win-link-opt` feature or `win-link-lld` feature
    is needed on Windows (not both)."
);

// Taken from llvm-sys source code.
//
// Since we use `llvm-no-linking`, `llvm-sys` won't trigger that error
// for us, so we need to take care of it ourselves.
#[cfg(all(not(doc), LLVM_NOT_FOUND))]
compile_error!(concat!(
    "No suitable version of LLVM was found system-wide or pointed
       to by LLVM_SYS_",
    env!("LLVM_VERSION_MAJOR"),
    "_PREFIX.

       Refer to the llvm-sys documentation for more information.

       llvm-sys: https://crates.io/crates/llvm-sys"
));
