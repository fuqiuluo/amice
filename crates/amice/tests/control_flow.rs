//! Integration tests for control flow obfuscation.
//!
//! This module tests:
//! - Bogus control flow (BCF)
//! - Control flow flattening (Flatten)
//! - VM-based flattening (VM Flatten)

mod common;

use crate::common::Language;
use common::{
    CppCompileBuilder, ObfuscationConfig, detect_llvm_config, ensure_plugin_built, fixture_path, plugin_path,
};
use std::path::PathBuf;
use std::process::Command;

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

fn flatten_config_dominator() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.flatten = Some(true);
    config.flatten_mode = Some("dominator".to_owned());
    config
}

fn opt_path() -> PathBuf {
    detect_llvm_config()
        .map(|config| PathBuf::from(config.prefix).join("bin").join("opt"))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("opt"))
}

fn assert_entry_only_conditional_terminator_opt_pass(enable_env: &str) {
    ensure_plugin_built();

    let mut cmd = Command::new(opt_path());
    ObfuscationConfig::disabled().apply_to_command(&mut cmd);
    cmd.env(enable_env, "true")
        .arg(format!("-load-pass-plugin={}", plugin_path().display()))
        .arg("-passes=default<O1>")
        .arg("-disable-output")
        .arg(fixture_path(
            "control_flow",
            "entry_only_conditional_terminator.ll",
            Language::C,
        ));

    let output = cmd.output().expect("failed to execute opt");
    assert!(
        output.status.success(),
        "opt failed\nSTDOUT:\n{}\nSTDERR:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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

#[test]
fn test_flatten_dominator() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("control_flow", "bogus_control_flow.c", Language::C),
        "flatten_dominator",
    )
    .config(flatten_config_dominator())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    // Same expected output as non-flattened version
    let lines = run.stdout_lines();
    assert!(lines[0].contains("Testing bogus control flow"));

    println!("{:?}", lines);
}

#[test]
fn test_flatten_entry_only_conditional_terminator() {
    assert_entry_only_conditional_terminator_opt_pass("AMICE_FLATTEN");
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

#[test]
fn test_vm_flatten_entry_only_conditional_terminator() {
    assert_entry_only_conditional_terminator_opt_pass("AMICE_VM_FLATTEN");
}

#[test]
fn test_vm_flatten_clears_stale_analysis_attributes() {
    ensure_plugin_built();

    let mut cmd = Command::new(opt_path());
    ObfuscationConfig::disabled().apply_to_command(&mut cmd);
    cmd.env("AMICE_VM_FLATTEN", "true")
        .arg(format!("-load-pass-plugin={}", plugin_path().display()))
        .arg("-passes=default<O0>")
        .arg("-S")
        .arg(fixture_path("control_flow", "stale_analysis_attrs.ll", Language::C));

    let output = cmd.output().expect("failed to execute opt");
    assert!(
        output.status.success(),
        "opt failed\nSTDOUT:\n{}\nSTDERR:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(".amice.vm_flatten_opcodes"));
    assert!(!stdout.contains("memory(none)"), "stale memory(none) attribute kept");
    assert!(!stdout.contains("readnone"), "stale readnone attribute kept");
    assert!(!stdout.contains("willreturn"), "stale willreturn attribute kept");
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
