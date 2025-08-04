mod aotu;
pub(crate) mod llvm_utils;

use crate::aotu::indirect_branch::IndirectBranch;
use crate::aotu::string_encryption::StringEncryption;
use log::info;
use std::io::Write;

#[llvm_plugin::plugin(name = "amice", version = "0.1")]
fn plugin_registrar(builder: &mut llvm_plugin::PassBuilder) {
    env_logger::builder()
        .format(|buf, record| {
            let time = buf.timestamp();
            let level = record.level();
            writeln!(
                buf,
                "[{} {} amice]: {}",
                time,
                level.as_str().to_lowercase(),
                record.args()
            )
        })
        .init();

    info!(
        "amice plugin initializing, version: {}, author: {}, llvm-sys: {}.{}",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_AUTHORS"),
        amice_llvm::get_llvm_version_major(),
        amice_llvm::get_llvm_version_minor()
    );

    builder.add_pipeline_start_ep_callback(|manager, level| {
        info!("amice plugin pipeline start callback, level: {level:?}");

        let string_encryption = std::env::var("AMICE_STRING_ENCRYPTION")
            .unwrap_or_else(|_| "true".to_string())
            == "true";
        let indirect_branch =
            std::env::var("AMICE_INDIRECT_BRANCH").unwrap_or_else(|_| "true".to_string()) == "true";

        manager.add_pass(StringEncryption::new(string_encryption));
        manager.add_pass(IndirectBranch::new(indirect_branch));
    });

    info!("amice plugin registered");
}
