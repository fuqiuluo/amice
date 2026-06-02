//! Integration tests for Rust VM-based control flow flattening.

mod common;

use common::{ObfuscationConfig, RustCompileBuilder};
use serial_test::serial;

fn vm_flatten_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.vm_flatten = Some(true);
    config
}

#[test]
#[serial]
fn test_rust_vm_flatten_basic() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("vm_flatten");

    let result = RustCompileBuilder::new(&project_dir, "vm_flatten_test")
        .config(vm_flatten_config())
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    println!("Output:\n{}", output);

    // Verify key outputs
    assert!(output.contains("=== Running Rust VM Flatten Test Suite ==="));
    assert!(output.contains("Calculate tests passed!"));
    assert!(output.contains("Complex function tests passed!"));
    assert!(output.contains("Array processing tests passed!"));
    assert!(output.contains("Fibonacci tests passed!"));
    assert!(output.contains("State machine tests passed!"));
    assert!(output.contains("Nested conditions tests passed!"));
    assert!(output.contains("Loop control flow tests passed!"));
    assert!(output.contains("SUCCESS: All VM flatten tests passed!"));
}

#[test]
#[serial]
fn test_rust_vm_flatten_vs_baseline() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("vm_flatten");

    // Compile without obfuscation (baseline)
    let baseline_result = RustCompileBuilder::new(&project_dir, "vm_flatten_test")
        .without_plugin()
        .use_stable()
        .compile();

    baseline_result.assert_success();
    let baseline_run = baseline_result.run();
    baseline_run.assert_success();
    let baseline_output = baseline_run.stdout();

    // Compile with VM flatten
    let obfuscated_result = RustCompileBuilder::new(&project_dir, "vm_flatten_test")
        .config(vm_flatten_config())
        .compile();

    obfuscated_result.assert_success();
    let obfuscated_run = obfuscated_result.run();
    obfuscated_run.assert_success();
    let obfuscated_output = obfuscated_run.stdout();

    // Both versions should produce identical output
    assert_eq!(
        baseline_output, obfuscated_output,
        "Baseline and VM-flattened outputs differ"
    );
}

#[test]
#[serial]
fn test_rust_vm_flatten_without_plugin() {
    let project_dir = common::project_root().join("tests").join("rust").join("vm_flatten");

    // Verify that the test works without the plugin
    let result = RustCompileBuilder::new(&project_dir, "vm_flatten_test")
        .without_plugin()
        .use_stable()
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("SUCCESS: All VM flatten tests passed!"));
}

#[test]
#[serial]
fn test_rust_vm_flatten_optimized() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("vm_flatten");

    // Test with optimizations enabled
    let result = RustCompileBuilder::new(&project_dir, "vm_flatten_test")
        .config(vm_flatten_config())
        .optimization("release")
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("SUCCESS: All VM flatten tests passed!"));
}
