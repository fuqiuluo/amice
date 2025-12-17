//! Integration tests for control flow obfuscation.
//!
//! This module tests:
//! - Bogus control flow (BCF)
//! - Control flow flattening (Flatten)
//! - VM-based flattening (VM Flatten)

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn bcf_config() -> ObfuscationConfig {
    ObfuscationConfig {
        bogus_control_flow: Some(true),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_bogus_control_flow_basic() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("control_flow", "bogus_control_flow.c", Language::C),
        "bcf_basic",
    )
    .config(bcf_config())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    assert!(lines[0].contains("Testing bogus control flow"));
    assert!(lines.iter().any(|l| l.contains("Simple branches result: 30")));
    assert!(lines.iter().any(|l| l.contains("Nested conditions result: 11")));
    assert!(lines.iter().any(|l| l.contains("Loop result: 10")));
}

#[test]
fn test_bogus_control_flow_optimized() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("control_flow", "bogus_control_flow.c", Language::C),
        "bcf_o2",
    )
    .config(bcf_config())
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

// ============================================================================
// Control Flow Flattening Tests
// ============================================================================

fn flatten_config() -> ObfuscationConfig {
    ObfuscationConfig {
        flatten: Some(true),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_flatten_basic() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("control_flow", "bogus_control_flow.c", Language::C),
        "flatten_basic",
    )
    .config(flatten_config())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    // Same expected output as non-flattened version
    let lines = run.stdout_lines();
    assert!(lines[0].contains("Testing bogus control flow"));
}

// ============================================================================
// VM Flatten Tests
// ============================================================================

fn vm_flatten_config() -> ObfuscationConfig {
    ObfuscationConfig {
        vm_flatten: Some(true),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_vm_flatten_basic() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("control_flow", "vm_flatten.c", Language::C),
        "vm_flatten_basic",
    )
    .config(vm_flatten_config())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    // Check key output markers from vm_flatten.c
    assert!(lines.iter().any(|l| l.contains("扁平化混淆测试")));
    assert!(lines.iter().any(|l| l.contains("测试完成")));
}

#[test]
fn test_vm_flatten_complex() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("control_flow", "vm_flatten.c", Language::C),
        "vm_flatten_complex",
    )
    .config(vm_flatten_config())
    .optimization("O1")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

// ============================================================================
// Combined Control Flow Tests
// ============================================================================

#[test]
fn test_bcf_with_flatten() {
    ensure_plugin_built();

    let config = ObfuscationConfig {
        bogus_control_flow: Some(true),
        flatten: Some(true),
        ..ObfuscationConfig::disabled()
    };

    let result = CppCompileBuilder::new(
        fixture_path("control_flow", "bogus_control_flow.c", Language::C),
        "bcf_flatten_combined",
    )
    .config(config)
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_all_control_flow_combined() {
    ensure_plugin_built();

    let config = ObfuscationConfig {
        bogus_control_flow: Some(true),
        flatten: Some(true),
        indirect_branch: Some(true),
        ..ObfuscationConfig::disabled()
    };

    let result = CppCompileBuilder::new(
        fixture_path("control_flow", "bogus_control_flow.c", Language::C),
        "all_cf_combined",
    )
    .config(config)
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}
