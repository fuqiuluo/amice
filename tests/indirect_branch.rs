//! Integration tests for indirect branch obfuscation.

mod common;

use crate::common::Language;
use common::{CompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn indirect_branch_config() -> ObfuscationConfig {
    ObfuscationConfig {
        indirect_branch: Some(true),
        ..ObfuscationConfig::disabled()
    }
}

fn indirect_branch_config_chained() -> ObfuscationConfig {
    ObfuscationConfig {
        indirect_branch: Some(true),
        indirect_branch_flags: Some("chained_dummy_block".to_string()),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_indirect_branch_basic() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("indirect_branch", "indirect_branch.c", Language::C),
        "indirect_branch_basic",
    )
    .config(indirect_branch_config())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    assert_eq!(lines[0], "Running control flow test suite...");
    assert_eq!(lines[1], "All tests completed. sink = 1");
}

#[test]
fn test_indirect_branch_chained() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("indirect_branch", "indirect_branch.c", Language::C),
        "indirect_branch_chained",
    )
    .config(indirect_branch_config_chained())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    assert_eq!(lines[0], "Running control flow test suite...");
    assert_eq!(lines[1], "All tests completed. sink = 1");
}

#[test]
fn test_indirect_branch_with_string_encryption() {
    ensure_plugin_built();

    let config = ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some("xor".to_string()),
        string_decrypt_timing: Some("lazy".to_string()),
        string_stack_alloc: Some(true),
        indirect_branch: Some(true),
        ..ObfuscationConfig::disabled()
    };

    let result = CompileBuilder::new(
        fixture_path("indirect_branch", "indirect_branch.c", Language::C),
        "indirect_branch_with_strings",
    )
    .config(config)
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    assert_eq!(lines[0], "Running control flow test suite...");
    assert_eq!(lines[1], "All tests completed. sink = 1");
}
