pub(crate) mod aotu;
pub(crate) mod config;
pub(crate) mod llvm_utils;
pub(crate) mod pass_registry;

use crate::config::CONFIG;
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

    builder.add_pipeline_start_ep_callback(|manager, level| {
        info!("amice plugin pipeline start callback, level: {level:?}");

        let cfg = &*CONFIG;
        pass_registry::install_all(cfg, manager);
    });

    info!("amice plugin registered");
}
