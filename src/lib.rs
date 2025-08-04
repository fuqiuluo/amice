mod aotu;

use llvm_plugin::inkwell::values::FunctionValue;
use llvm_plugin::{
    FunctionAnalysisManager, LlvmFunctionPass, PipelineParsing, PreservedAnalyses,
};
use std::io::Write;
use std::process::exit;
use env_logger::builder;
use log::info;
use crate::aotu::string_encryption::StringEncryption;

#[llvm_plugin::plugin(name = "amice", version = "0.1")]
fn plugin_registrar(builder: &mut llvm_plugin::PassBuilder) {
    env_logger::builder()
        .format(|buf, record| {
            let time = buf.timestamp();
            let level = record.level();
            writeln!(buf, "[{} {} amice]: {}", time, level.as_str().to_lowercase(), record.args())
        })
        .init();

    info!("amice plugin initializing, version: {}, author: {}, llvm-sys: {}.{}",
        env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_AUTHORS"), amice_llvm::get_llvm_version_major(), amice_llvm::get_llvm_version_minor());

    builder.add_pipeline_start_ep_callback(|manager, level| {
        info!("amice plugin pipeline start callback, level: {:?}", level);

        let string_encryption = std::env::var("AMICE_STRING_ENCRYPTION")
            .unwrap_or_else(|_| "true".to_string()) == "true";

        manager.add_pass(StringEncryption::new(string_encryption))
    });

    info!("amice plugin registered");
}
