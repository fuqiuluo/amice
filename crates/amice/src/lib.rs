pub(crate) mod aotu;
pub(crate) mod config;
pub(crate) mod pass_registry;

use crate::config::CONFIG;
use crate::pass_registry::AmicePassFlag;
use amice_plugin::PipelineParsing;
use log::{error, info, warn};

#[amice_plugin::plugin(name = "amice", version = "0.1.4", pre_code_gen_callback = pre_code_gen_callback)]
fn plugin_registrar(builder: &mut amice_plugin::PassBuilder) {
    if let Err(e) = env_logger::builder().try_init() {
        warn!("amice init logger failed: {e:?}");
        return;
    }

    info!(
        "amice plugin initializing, version: {}, author: {}, llvm-sys: {}.{}",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_AUTHORS"),
        amice_llvm::get_llvm_version_major(),
        amice_llvm::get_llvm_version_minor()
    );

    builder.add_module_pipeline_parsing_callback(|name, _manager| {
        error!("amice plugin module pipeline parsing callback, name: {}", name);

        PipelineParsing::NotParsed
    });

    builder.add_pipeline_start_ep_callback(|manager, opt| {
        info!("amice plugin pipeline start callback, level: {opt:?}");

        let cfg = &*CONFIG;
        pass_registry::install_all(cfg, manager, AmicePassFlag::PipelineStart);
    });

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
    builder.add_optimizer_last_ep_callback(|manager, opt| {
        info!("amice plugin optimizer last callback, level: {opt:?}");
        let cfg = &*CONFIG;
        pass_registry::install_all(cfg, manager, AmicePassFlag::OptimizerLast);
    });

    #[cfg(any(doc, feature = "llvm20-1", feature = "llvm21-1", feature = "llvm22-1"))]
    builder.add_optimizer_last_ep_callback(|manager, opt, phase| {
        info!("amice plugin optimizer last callback, level: {opt:?}, phase: {phase:?}");
        let cfg = &*CONFIG;
        pass_registry::install_all(cfg, manager, AmicePassFlag::OptimizerLast);
    });

    #[cfg(any(
        doc,
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
        feature = "llvm21-1",
        feature = "llvm22-1",
    ))]
    builder.add_full_lto_last_ep_callback(|manager, opt| {
        info!("amice plugin full lto last callback, level: {opt:?}");
        let cfg = &*CONFIG;
        pass_registry::install_all(cfg, manager, AmicePassFlag::FullLtoLast);
    });

    info!("amice plugin registered!");
}

#[cfg(feature = "llvm22-1")]
extern "C" fn pre_code_gen_callback(
    module: *mut std::ffi::c_void,
    _target_machine: *mut std::ffi::c_void,
    code_gen_file_type: amice_plugin::CodeGenFileType,
    _output_stream: *mut std::ffi::c_void,
) -> bool {
    if module.is_null() {
        return false;
    }

    info!("amice plugin pre codegen callback, file type: {code_gen_file_type:?}");

    let cfg = &*CONFIG;
    let mut module = unsafe { amice_plugin::inkwell::module::Module::new(module.cast()) };
    let mut manager = amice_plugin::ModulePassManager::new();
    pass_registry::install_all(cfg, &mut manager, AmicePassFlag::PreCodeGen);

    if !manager.is_empty() {
        manager.run_on_module(&mut module);
    }

    std::mem::forget(module);
    false
}
