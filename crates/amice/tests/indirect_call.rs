//! Integration tests for indirect call obfuscation.

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn indirect_call_config() -> ObfuscationConfig {
    ObfuscationConfig {
        indirect_call: Some(true),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_indirect_call_basic() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("indirect_call", "indirect_call.c", Language::C),
        "indirect_call_basic",
    )
    .config(indirect_call_config())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();

    // Verify direct calls work
    assert!(lines.iter().any(|l| l.contains("=== Direct Calls")));
    assert!(lines.iter().any(|l| l.contains("Called: add")));
    assert!(lines.iter().any(|l| l.contains("Called: mul")));
    assert!(lines.iter().any(|l| l.contains("Called: sub")));

    // Verify obfuscated calls work
    assert!(lines.iter().any(|l| l.contains("=== Obfuscated Indirect Calls")));
}

#[test]
fn test_indirect_call_optimized() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("indirect_call", "indirect_call.c", Language::C),
        "indirect_call_o2",
    )
    .config(indirect_call_config())
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_indirect_call_generates_random_xor_key_by_default() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("indirect_call", "indirect_call.c", Language::C),
        "indirect_call_random_xor.ll",
    )
    .config(indirect_call_config())
    .arg("-S")
    .arg("-emit-llvm")
    .compile();

    result.assert_success();

    let ir = std::fs::read_to_string(&result.binary_path).expect("failed to read generated LLVM IR");
    assert!(
        ir.contains("@.amice_xor_key"),
        "expected default indirect-call config to emit an XOR key"
    );
    assert!(
        ir.contains(" xor i32 "),
        "expected default indirect-call config to emit XOR instructions"
    );
}
