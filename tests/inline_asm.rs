//! Integration tests for inline assembly handling.
//!
//! This module tests that obfuscation passes correctly handle:
//! - Inline assembly blocks
//! - CallBr instructions (indirect jumps to asm labels)
//! - Functions mixing inline asm with control flow
//!
//! Critical: ALL obfuscation passes should detect inline asm and skip the function

mod common;

use common::{CompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

#[test]
fn test_inline_asm_with_flatten() {
    ensure_plugin_built();

    // Inline asm detection test - should skip functions with inline asm
    let result = CompileBuilder::new(
        fixture_path("inline_asm", "inline_asm_basic.c"),
        "inline_asm_flatten"
    )
    .config(ObfuscationConfig {
        flatten: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_inline_asm_with_bcf() {
    ensure_plugin_built();

    // BCF should detect inline asm (currently does NOT)
    let result = CompileBuilder::new(
        fixture_path("inline_asm", "inline_asm_basic.c"),
        "inline_asm_bcf"
    )
    .config(ObfuscationConfig {
        bogus_control_flow: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_inline_asm_with_indirect_branch() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("inline_asm", "inline_asm_basic.c"),
        "inline_asm_indirect_branch"
    )
    .config(ObfuscationConfig {
        indirect_branch: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_inline_asm_optimized() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("inline_asm", "inline_asm_basic.c"),
        "inline_asm_o2"
    )
    .config(ObfuscationConfig {
        flatten: Some(true),
        bogus_control_flow: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_inline_asm_combined() {
    ensure_plugin_built();

    // Test all control flow passes with inline asm
    let result = CompileBuilder::new(
        fixture_path("inline_asm", "inline_asm_basic.c"),
        "inline_asm_combined"
    )
    .config(ObfuscationConfig {
        flatten: Some(true),
        bogus_control_flow: Some(true),
        indirect_branch: Some(true),
        split_basic_block: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}
