//! Integration tests for split basic block.

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn split_basic_block_config(split_num: u32) -> ObfuscationConfig {
    ObfuscationConfig {
        split_basic_block: Some(true),
        split_basic_block_num: Some(split_num),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_split_entry_block_after_allocas() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("split_basic_block", "entry_alloca.ll", Language::C),
        "split_entry_alloca.ll",
    )
    .config(split_basic_block_config(2))
    .optimization("O1")
    .arg("-x")
    .arg("ir")
    .arg("-S")
    .arg("-emit-llvm")
    .compile();

    result.assert_success();

    let ir = std::fs::read_to_string(&result.binary_path).expect("failed to read generated LLVM IR");
    assert!(
        ir.contains(".split_0:"),
        "entry block was not split after alloca region"
    );
}
