mod aotu;
pub(crate) mod llvm_utils;
mod config;

use crate::aotu::indirect_branch::IndirectBranch;
use crate::aotu::indirect_call::IndirectCall;
use crate::aotu::split_basic_block::SplitBasicBlock;
use crate::aotu::string_encryption::StringEncryption;
use crate::aotu::vm_flatten::VmFlatten;
use log::info;
use std::io::Write;
use crate::config::CONFIG;

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

        let cfg = &*CONFIG;

        manager.add_pass(StringEncryption::new(cfg.string_encryption.enable));
        manager.add_pass(IndirectCall::new(cfg.indirect_call.enable));
        manager.add_pass(SplitBasicBlock::new(cfg.split_basic_block.enable));
        manager.add_pass(VmFlatten::new(cfg.vm_flatten.enable));
        manager.add_pass(IndirectBranch::new(cfg.indirect_branch.enable));
    });

    info!("amice plugin registered");
}
