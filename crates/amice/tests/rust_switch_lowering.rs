//! Integration tests for Rust switch lowering (switch to if-else conversion).

mod common;

use common::{ObfuscationConfig, RustCompileBuilder};
use serial_test::serial;

fn lower_switch_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.lower_switch = Some(true);
    config
}

fn lower_switch_with_dummy_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.lower_switch = Some(true);
    config.lower_switch_with_dummy_code = Some(true);
    config
}

#[test]
#[serial]
fn test_rust_switch_lowering_basic() {
    common::ensure_plugin_built();

    let project_dir = common::project_root()
        .join("tests")
        .join("rust")
        .join("switch_lowering");

    let result = RustCompileBuilder::new(&project_dir, "switch_lowering_test")
        .config(lower_switch_config())
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    println!("Output:\n{}", output);

    // Verify key outputs
    assert!(output.contains("=== Running Rust Switch Lowering Test Suite ==="));
    assert!(output.contains("test_simple_match:"));
    assert!(output.contains("test_sparse_match:"));
    assert!(output.contains("test_char_match:"));
    assert!(output.contains("test_enum_match:"));
    assert!(output.contains("test_nested_match:"));
    assert!(output.contains("test_multi_pattern_match:"));
    assert!(output.contains("test_large_switch:"));
    assert!(output.contains("test_match_in_loop:"));
    assert!(output.contains("test_complex_arms:"));
    assert!(output.contains("test_bool_match:"));
    assert!(output.contains("SUCCESS: All switch lowering tests passed!"));
}

#[test]
#[serial]
#[ignore = "append_dummy_code causes LLVM IR verification issues - tracked as known issue"]
fn test_rust_switch_lowering_with_dummy_code() {
    common::ensure_plugin_built();

    let project_dir = common::project_root()
        .join("tests")
        .join("rust")
        .join("switch_lowering");

    let result = RustCompileBuilder::new(&project_dir, "switch_lowering_test")
        .config(lower_switch_with_dummy_config())
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("SUCCESS: All switch lowering tests passed!"));
}

#[test]
#[serial]
fn test_rust_switch_lowering_vs_baseline() {
    common::ensure_plugin_built();

    let project_dir = common::project_root()
        .join("tests")
        .join("rust")
        .join("switch_lowering");

    // Compile without obfuscation (baseline)
    let baseline_result = RustCompileBuilder::new(&project_dir, "switch_lowering_test")
        .without_plugin()
        .use_stable()
        .compile();

    baseline_result.assert_success();
    let baseline_run = baseline_result.run();
    baseline_run.assert_success();
    let baseline_output = baseline_run.stdout();

    // Compile with switch lowering
    let obfuscated_result = RustCompileBuilder::new(&project_dir, "switch_lowering_test")
        .config(lower_switch_config())
        .compile();

    obfuscated_result.assert_success();
    let obfuscated_run = obfuscated_result.run();
    obfuscated_run.assert_success();
    let obfuscated_output = obfuscated_run.stdout();

    // Both versions should produce identical output
    assert_eq!(
        baseline_output, obfuscated_output,
        "Baseline and switch-lowered outputs differ"
    );
}

#[test]
#[serial]
fn test_rust_switch_lowering_without_plugin() {
    let project_dir = common::project_root()
        .join("tests")
        .join("rust")
        .join("switch_lowering");

    // Verify that the test works without the plugin
    let result = RustCompileBuilder::new(&project_dir, "switch_lowering_test")
        .without_plugin()
        .use_stable()
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("SUCCESS: All switch lowering tests passed!"));
}
