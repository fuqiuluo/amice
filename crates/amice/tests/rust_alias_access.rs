//! Integration tests for Rust alias access (pointer chain) obfuscation.

mod common;

use common::{ObfuscationConfig, RustCompileBuilder};
use serial_test::serial;

fn alias_access_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.alias_access = Some(true);
    config
}

fn assert_alias_access_output(output: &str) {
    for expected in [
        "=== Rust Alias Access Test Suite ===",
        "--- Test 1: Simple Locals ---",
        "--- Test 2: Local Array ---",
        "--- Test 3: Multiple Locals ---",
        "--- Test 4: Conditional ---",
        "--- Test 5: Loop ---",
        "--- Test 6: Drop + ZST ---",
        "--- Test 7: Struct + Tuple + Refs ---",
        "--- Test 8: Option + Result + Slice ---",
        "--- Test 9: Trait Object + Closure ---",
        "--- Test 10: Vec + String Drop ---",
        "--- Test 11: Raw Pointer Roundtrip ---",
        "test_simple(5, 3) = 21",
        "test_array(2) = 12",
        "test_multiple(10) = 5",
        "test_conditional(5) = 30",
        "test_loop(5) = 10",
        "test_drop_and_zst(7) = 37",
        "test_struct_tuple_refs(5) = 48",
        "test_option_result_slice(4) = 21",
        "test_trait_object_and_closure(9) = 27",
        "test_vec_string_drop(11) = 77",
        "test_raw_pointer_roundtrip(6) = 22",
        "SUCCESS: All alias access tests passed!",
    ] {
        assert!(output.contains(expected), "missing output marker: {expected}");
    }
}

#[test]
#[serial]
fn test_rust_alias_access_basic() {
    common::ensure_plugin_built();

    let project_dir = common::rust_fixture_project_path("alias_access");

    let result = RustCompileBuilder::new(&project_dir, "alias_access_test")
        .config(alias_access_config())
        .optimization("debug")
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    println!("Output:\n{}", output);

    assert_alias_access_output(&output);
}

#[test]
#[serial]
fn test_rust_alias_access_vs_baseline() {
    common::ensure_plugin_built();

    let project_dir = common::rust_fixture_project_path("alias_access");

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
    assert_alias_access_output(&obfuscated_output);

    // Both versions should produce identical output
    assert_eq!(
        baseline_output, obfuscated_output,
        "Baseline and alias access outputs differ"
    );
}

#[test]
#[serial]
fn test_rust_alias_access_without_plugin() {
    let project_dir = common::rust_fixture_project_path("alias_access");

    // Verify that the test works without the plugin
    let result = RustCompileBuilder::new(&project_dir, "alias_access_test")
        .without_plugin()
        .use_stable()
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert_alias_access_output(&output);
}

#[test]
#[serial]
fn test_rust_alias_access_optimized() {
    common::ensure_plugin_built();

    let project_dir = common::rust_fixture_project_path("alias_access");

    // Test with optimizations enabled
    let result = RustCompileBuilder::new(&project_dir, "alias_access_test")
        .config(alias_access_config())
        .optimization("release")
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert_alias_access_output(&output);
}
