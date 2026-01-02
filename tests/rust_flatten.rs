//! Integration tests for Rust control flow flattening.

mod common;

use std::env;
use common::{ObfuscationConfig, RustCompileBuilder};
use serial_test::serial;

fn flatten_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.flatten = Some(true);
    config.flatten_mode = Some("basic".to_owned());
    config
}

fn flatten_config_dominator() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.flatten = Some(true);
    config.flatten_mode = Some("dominator".to_owned());
    config
}

#[test]
#[serial]
fn test_rust_flatten_basic() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("flatten");

    let result = RustCompileBuilder::new(&project_dir, "flatten_test")
        .config(flatten_config())
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    println!("Output:\n{}", output);

    // Verify key outputs
    assert!(output.contains("=== Running Rust Control Flow Flatten Test Suite ==="));
    assert!(output.contains("Calculate tests passed!"));
    assert!(output.contains("Complex function tests passed!"));
    assert!(output.contains("Array processing tests passed!"));
    assert!(output.contains("Fibonacci tests passed!"));
    assert!(output.contains("State machine tests passed!"));
    assert!(output.contains("Nested conditions tests passed!"));
    assert!(output.contains("Loop control flow tests passed!"));
    assert!(output.contains("SUCCESS: All flatten tests passed!"));
}

#[test]
#[serial]
fn test_rust_flatten_vs_baseline() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("flatten");

    // Compile without obfuscation (baseline)
    let baseline_result = RustCompileBuilder::new(&project_dir, "flatten_test")
        .without_plugin()
        .use_stable()
        .compile();

    baseline_result.assert_success();
    let baseline_run = baseline_result.run();
    baseline_run.assert_success();
    let baseline_output = baseline_run.stdout();

    // Compile with flatten
    let obfuscated_result = RustCompileBuilder::new(&project_dir, "flatten_test")
        .config(flatten_config())
        .compile();

    obfuscated_result.assert_success();
    let obfuscated_run = obfuscated_result.run();
    obfuscated_run.assert_success();
    let obfuscated_output = obfuscated_run.stdout();

    // Both versions should produce identical output
    assert_eq!(
        baseline_output, obfuscated_output,
        "Baseline and flattened outputs differ"
    );
}

#[test]
#[serial]
fn test_rust_flatten_without_plugin() {
    let project_dir = common::project_root().join("tests").join("rust").join("flatten");

    // Verify that the test works without the plugin
    let result = RustCompileBuilder::new(&project_dir, "flatten_test")
        .without_plugin()
        .use_stable()
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("SUCCESS: All flatten tests passed!"));
}

#[test]
#[serial]
fn test_rust_flatten_optimized() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("flatten");

    // Test with optimizations enabled
    let result = RustCompileBuilder::new(&project_dir, "flatten_test")
        .config(flatten_config())
        .optimization("release")
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("SUCCESS: All flatten tests passed!"));
}

#[test]
#[serial]
fn test_rust_flatten_dominator_mode() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("flatten");

    // Test with dominator mode
    let result = RustCompileBuilder::new(&project_dir, "flatten_test")
        .config(flatten_config_dominator())
        .optimization("release")
        .compile();

    result.assert_success();

    println!("编译成功，运行开始 ====>    ");

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("SUCCESS: All flatten tests passed!"));
}
