pub(crate) mod aotu;
pub(crate) mod config;
pub(crate) mod pass_registry;

use crate::config::CONFIG;
use crate::pass_registry::PassPosition;
use env_logger::builder;
use llvm_plugin::PipelineParsing;
use log::info;

#[llvm_plugin::plugin(name = "amice", version = "0.1.2")]
fn plugin_registrar(builder: &mut llvm_plugin::PassBuilder) {
    env_logger::builder().init();

    info!(
        "amice plugin initializing, version: {}, author: {}, llvm-sys: {}.{}",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_AUTHORS"),
        amice_llvm::get_llvm_version_major(),
        amice_llvm::get_llvm_version_minor()
    );

    builder.add_module_pipeline_parsing_callback(|name, _manager| {
        panic!("amice plugin module pipeline parsing callback, name: {}", name);
    });

    builder.add_pipeline_start_ep_callback(|manager, opt| {
        info!("amice plugin pipeline start callback, level: {opt:?}");

        let cfg = &*CONFIG;
        pass_registry::install_all(cfg, manager, PassPosition::PipelineStart);
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
        pass_registry::install_all(cfg, manager, PassPosition::OptimizerLast);
    });

    #[cfg(any(doc, feature = "llvm20-1"))]
    builder.add_optimizer_last_ep_callback(|manager, opt, phase| {
        info!("amice plugin optimizer last callback, level: {opt:?}, phase: {phase:?}");
        let cfg = &*CONFIG;
        pass_registry::install_all(cfg, manager, PassPosition::OptimizerLast);
    });

    #[cfg(any(
        doc,
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
    ))]
    builder.add_full_lto_last_ep_callback(|manager, opt| {
        info!("amice plugin full lto last callback, level: {opt:?}");
        let cfg = &*CONFIG;
        pass_registry::install_all(cfg, manager, PassPosition::FullLtoLast);
    });

    info!("amice plugin registered");
}
