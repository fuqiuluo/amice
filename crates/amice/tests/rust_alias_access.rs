//! Integration tests for Rust alias access (pointer chain) obfuscation.

mod common;

use common::{ObfuscationConfig, RustCompileBuilder};
use serial_test::serial;

fn alias_access_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.alias_access = Some(true);
    config
}

#[test]
#[serial]
fn test_rust_alias_access_basic() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("alias_access");

    let result = RustCompileBuilder::new(&project_dir, "alias_access_test")
        .config(alias_access_config())
        .optimization("debug")
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    println!("Output:\n{}", output);

    // Verify key outputs
    // assert!(output.contains("=== Rust Alias Access Test Suite ==="));
    // assert!(output.contains("Test 1: Simple Locals"));
    // assert!(output.contains("Test 2: Multiple Locals"));
    // assert!(output.contains("Test 3: Local Array"));
    // assert!(output.contains("Test 4: Local Tuple"));
    // assert!(output.contains("Test 5: Conditional Locals"));
    // assert!(output.contains("Test 6: Loop Locals"));
    // assert!(output.contains("Test 7: Swap Locals"));
    // assert!(output.contains("Test 8: Bitwise Locals"));
    // assert!(output.contains("Test 9: Fibonacci Locals"));
    // assert!(output.contains("Test 10: Nested Locals"));
    // assert!(output.contains("Test 11: State Machine Locals"));
    // assert!(output.contains("Test 12: Computed Index Locals"));
    // assert!(output.contains("SUCCESS: All alias access tests passed!"));
}

#[test]
#[serial]
fn test_rust_alias_access_vs_baseline() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("alias_access");

    // Compile without obfuscation (baseline)
    let baseline_result = RustCompileBuilder::new(&project_dir, "alias_access_test")
        .without_plugin()
        .use_stable()
        .compile();

    baseline_result.assert_success();
    let baseline_run = baseline_result.run();
    baseline_run.assert_success();
    let baseline_output = baseline_run.stdout();

    // Compile with alias access
    let obfuscated_result = RustCompileBuilder::new(&project_dir, "alias_access_test")
        .config(alias_access_config())
        .compile();

    obfuscated_result.assert_success();
    let obfuscated_run = obfuscated_result.run();
    obfuscated_run.assert_success();
    let obfuscated_output = obfuscated_run.stdout();

    // Both versions should produce identical output
    assert_eq!(
        baseline_output, obfuscated_output,
        "Baseline and alias access outputs differ"
    );
}

#[test]
#[serial]
fn test_rust_alias_access_without_plugin() {
    let project_dir = common::project_root().join("tests").join("rust").join("alias_access");

    // Verify that the test works without the plugin
    let result = RustCompileBuilder::new(&project_dir, "alias_access_test")
        .without_plugin()
        .use_stable()
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("SUCCESS: All alias access tests passed!"));
}

#[test]
#[serial]
fn test_rust_alias_access_optimized() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("alias_access");

    // Test with optimizations enabled
    let result = RustCompileBuilder::new(&project_dir, "alias_access_test")
        .config(alias_access_config())
        .optimization("release")
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("SUCCESS: All alias access tests passed!"));
}
