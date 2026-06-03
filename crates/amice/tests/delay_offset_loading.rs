//! Integration tests for delay offset loading.

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn delay_offset_loading_config() -> ObfuscationConfig {
    ObfuscationConfig {
        delay_offset_loading: Some(true),
        delay_offset_loading_xor_offset: Some(false),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_delay_offset_loading_preserves_behavior() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("integration", "ama.c", Language::C),
        "delay_offset_loading_ama",
    )
    .config(delay_offset_loading_config())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    assert_eq!(lines, ["50"]);
}

#[test]
fn test_delay_offset_loading_emits_constant_offsets() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("integration", "ama.c", Language::C),
        "delay_offset_loading_ama.ll",
    )
    .config(delay_offset_loading_config())
    .arg("-S")
    .arg("-emit-llvm")
    .compile();

    result.assert_success();

    let ir = std::fs::read_to_string(&result.binary_path).expect("failed to read generated LLVM IR");
    assert!(ir.contains("@.ama.offset.4"), "POD field offset was not delayed");
    assert!(
        ir.contains("@.ama.offset.8") || ir.contains("@.ama.offset.20"),
        "nested or array constant offsets were not delayed"
    );
    assert!(
        ir.contains("load i64, ptr @.ama.offset."),
        "expected delayed offset loads in generated IR"
    );
}
