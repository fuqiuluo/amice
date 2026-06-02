//! [![github]](https://github.com/jamesmth/llvm-plugin-rs)&ensp;[![crates-io]](https://crates.io/crates/llvm-plugin)
//!
//! [github]: https://img.shields.io/badge/github-8da0cb?style=for-the-badge&labelColor=555555&logo=github
//! [crates-io]: https://img.shields.io/badge/crates.io-fc8d62?style=for-the-badge&labelColor=555555&logo=rust
//!
//! <br>
//!
//! This crate gives the ability to safely implement passes for the [new LLVM pass manager],
//! by leveraging the strongly typed interface  provided by [Inkwell].
//!
//! If you have never developed LLVM passes before, you can take a look at the available
//! [examples]. They will (hopefully) give you a better idea of how to use this crate.
//!
//! If you want a deeper understanding of the many concepts surrounding the new LLVM
//! pass manager, you should read the [official LLVM documentation].
//!
//! [Inkwell]: https://github.com/TheDan64/inkwell
//! [new LLVM pass manager]: https://blog.llvm.org/posts/2021-03-26-the-new-pass-manager/
//! [examples]: https://github.com/jamesmth/llvm-plugin-rs/tree/master/examples
//! [official LLVM documentation]: https://llvm.org/docs/NewPassManager.html
//!
//! # Getting started
//!
//! An LLVM plugin is merely a dylib that is given a [PassBuilder] by the LLVM tool
//! (e.g. [opt], [lld]) loading it. A [PassBuilder] allows registering callbacks on
//! specific actions being performed by the LLVM tool.
//!
//! For instance, the `--passes` parameter of [opt] allows specifying a custom pass pipeline
//! to be run on a given IR module. A plugin could therefore register a callback for
//! parsing an element of the given pipeline (e.g. a pass name), in order to insert a custom
//! pass to run by [opt].
//!
//! The following code illustrates the idea:
//!
//! ```no_run
//! # use llvm_plugin::inkwell::module::Module;
//! # use llvm_plugin::{
//! #     LlvmModulePass, ModuleAnalysisManager, PassBuilder, PipelineParsing, PreservedAnalyses,
//! # };
//! // A name and version is required.
//! #[llvm_plugin::plugin(name = "plugin_name", version = "0.1")]
//! fn plugin_registrar(builder: &mut PassBuilder) {
//!     // Add a callback to parse a name from the textual representation of
//!     // the pipeline to be run.
//!     builder.add_module_pipeline_parsing_callback(|name, manager| {
//!         if name == "custom-pass" {
//!             // the input pipeline contains the name "custom-pass",
//!             // so we add our custom pass to the pass manager
//!             manager.add_pass(CustomPass);
//!
//!             // we notify the caller that we were able to parse
//!             // the given name
//!             PipelineParsing::Parsed
//!         } else {
//!             // in any other cases, we notify the caller that our
//!             // callback wasn't able to parse the given name
//!             PipelineParsing::NotParsed
//!         }
//!     });
//! }
//!
//! struct CustomPass;
//! impl LlvmModulePass for CustomPass {
//!     fn run_pass(
//!         &self,
//!         module: &mut Module,
//!         manager: &ModuleAnalysisManager
//!     ) -> PreservedAnalyses {
//!         // transform the IR
//!         # PreservedAnalyses::All
//!     }
//! }
//! ```
//!
//! Now, executing this command would run our custom pass on the input module:
//!
//! ```bash
//! opt --load-pass-plugin=libplugin.so --passes=custom-pass module.bc -disable-output
//! ```
//!
//! However, executing this command would not (`custom-pass2` cannot be parsed by our plugin):
//!
//! ```bash
//! opt --load-pass-plugin=libplugin.so --passes=custom-pass2 module.bc -disable-output
//! ```
//!
//! More callbacks are available, read the [PassBuilder] documentation for more details.
//!
//! # A note on Windows
//!
//! On this platform, LLVM plugins need the LLVM symbols directly from the executable loading
//! them (most of the time `opt.exe` or `lld.exe`). Therefore, you need to specify the
//! additional feature `win-link-opt` or `win-link-lld` while building a plugin. The former
//! will link the plugin to `opt.lib`, the latter being for `lld.lib`.
//!
//! [opt]: https://www.llvm.org/docs/CommandGuide/opt.html
//! [lld]: https://lld.llvm.org/

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
    fn run_pass(
        &self,
        module: &mut Module<'_>,
        manager: &ModuleAnalysisManager,
    ) -> PreservedAnalyses;
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
    fn run_pass(
        &self,
        function: &mut FunctionValue<'_>,
        manager: &FunctionAnalysisManager,
    ) -> PreservedAnalyses;
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
    /// # Example
    ///
    /// ```
    /// # use llvm_plugin::inkwell::module::Module;
    /// # use llvm_plugin::{AnalysisKey, LlvmModuleAnalysis, ModuleAnalysisManager};
    /// # struct Analysis;
    /// # impl LlvmModuleAnalysis for Analysis {
    /// #    type Result = ();
    /// #    fn run_analysis(
    /// #        &self,
    /// #        _module: &Module,
    /// #        _manager: &ModuleAnalysisManager,
    /// #    ) -> Self::Result {}
    /// #
    /// fn id() -> AnalysisKey {
    ///     static ID: u8 = 0;
    ///     &ID
    /// }
    /// # }
    /// ```
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
    fn run_analysis(
        &self,
        module: &FunctionValue<'_>,
        manager: &FunctionAnalysisManager,
    ) -> Self::Result;

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
    /// # Example
    ///
    /// ```
    /// # use llvm_plugin::inkwell::values::FunctionValue;
    /// # use llvm_plugin::{AnalysisKey, LlvmFunctionAnalysis, FunctionAnalysisManager};
    /// # struct Analysis;
    /// # impl LlvmFunctionAnalysis for Analysis {
    /// #    type Result = ();
    /// #    fn run_analysis(
    /// #        &self,
    /// #        _function: &FunctionValue,
    /// #        _manager: &FunctionAnalysisManager,
    /// #    ) -> Self::Result {}
    /// #
    /// fn id() -> AnalysisKey {
    ///     static ID: u8 = 0;
    ///     &ID
    /// }
    /// # }
    /// ```
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
