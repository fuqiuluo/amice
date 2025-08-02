mod aotu;

use llvm_plugin::inkwell::values::FunctionValue;
use llvm_plugin::{
    FunctionAnalysisManager, LlvmFunctionPass, PipelineParsing, PreservedAnalyses,
};
use std::io::Write;
use log::info;
use crate::aotu::StringEncryption;

#[llvm_plugin::plugin(name = "amice", version = "0.1")]
fn plugin_registrar(builder: &mut llvm_plugin::PassBuilder) {
    env_logger::builder()
        .format(|buf, record| {
            let time = buf.timestamp();
            let level = record.level();
            writeln!(buf, "[{} {} amice]: {}", time, level.as_str().to_lowercase(), record.args())
        })
        .init();

    log::info!("amice plugin initializing, version: {}, author: {}",
        env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_AUTHORS"));

    builder.add_pipeline_start_ep_callback(|manager, level| {
        manager.add_pass(StringEncryption::new(true))
    });
    // 
    // for x in std::env::args() {
    //     info!("Argument: {}", x);
    // }

    log::info!("amice plugin registered");
}
