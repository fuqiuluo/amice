//! Integration tests for Rust MBA (Mixed Boolean Arithmetic) obfuscation.

mod common;

use common::{ObfuscationConfig, RustCompileBuilder};
use serial_test::serial;

fn mba_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.mba = Some(true);
    config
}

#[test]
#[serial]
fn test_rust_mba_basic() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("mba");

    let result = RustCompileBuilder::new(&project_dir, "mba_test")
        .config(mba_config())
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    println!("Output:\n{}", output);

    // Verify key outputs
    assert!(output.contains("=== Running Rust MBA Test Suite ==="));
    assert!(output.contains("Basic arithmetic test passed!"));
    assert!(output.contains("Bitwise operations test passed!"));
    assert!(output.contains("Mixed operations test passed!"));
    assert!(output.contains("Shift operations test passed!"));
    assert!(output.contains("Complex expression test passed!"));
    assert!(output.contains("Loop arithmetic test passed!"));
    assert!(output.contains("Constant arithmetic test passed!"));
    assert!(output.contains("Nested arithmetic test passed!"));
    assert!(output.contains("Conditional arithmetic test passed!"));
    assert!(output.contains("Array arithmetic test passed!"));
    assert!(output.contains("SUCCESS: All MBA tests passed!"));
}

#[test]
#[serial]
fn test_rust_mba_vs_baseline() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("mba");

    // Compile without obfuscation (baseline)
    let baseline_result = RustCompileBuilder::new(&project_dir, "mba_test")
        .without_plugin()
        .use_stable()
        .compile();

    baseline_result.assert_success();
    let baseline_run = baseline_result.run();
    baseline_run.assert_success();
    let baseline_output = baseline_run.stdout();

    // Compile with MBA
    let obfuscated_result = RustCompileBuilder::new(&project_dir, "mba_test")
        .config(mba_config())
        .compile();

    obfuscated_result.assert_success();
    let obfuscated_run = obfuscated_result.run();
    obfuscated_run.assert_success();
    let obfuscated_output = obfuscated_run.stdout();

    // Both versions should produce identical output
    assert_eq!(
        baseline_output, obfuscated_output,
        "Baseline and MBA-obfuscated outputs differ"
    );
}

#[test]
#[serial]
fn test_rust_mba_without_plugin() {
    let project_dir = common::project_root().join("tests").join("rust").join("mba");

    // Verify that the test works without the plugin
    let result = RustCompileBuilder::new(&project_dir, "mba_test")
        .without_plugin()
        .use_stable()
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("SUCCESS: All MBA tests passed!"));
}

#[test]
#[serial]
fn test_rust_mba_optimized() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("mba");

    // Test with optimizations enabled
    let result = RustCompileBuilder::new(&project_dir, "mba_test")
        .config(mba_config())
        .optimization("release")
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("SUCCESS: All MBA tests passed!"));
}
