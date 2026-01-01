//! Integration tests for Rust indirect branch obfuscation.

mod common;

use common::{ObfuscationConfig, RustCompileBuilder};
use serial_test::serial;

fn indirect_branch_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.indirect_branch = Some(true);
    config
}

fn indirect_branch_config_chained() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.indirect_branch = Some(true);
    config.indirect_branch_flags = Some("chained_dummy_blocks".to_string());
    config
}

fn indirect_branch_config_all_flags() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.indirect_branch = Some(true);
    config.indirect_branch_flags = Some("chained_dummy_blocks,encrypt_block_index,shuffle_table".to_string());
    config
}

fn split_basic_block_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.split_basic_block = Some(true);
    config
}

fn split_and_indirect_branch_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.split_basic_block = Some(true);
    config.indirect_branch = Some(true);
    config
}

#[test]
#[serial]
fn test_rust_indirect_branch_basic() {
    common::ensure_plugin_built();

    let project_dir = common::project_root()
        .join("tests")
        .join("rust")
        .join("indirect_branch");

    let result = RustCompileBuilder::new(&project_dir, "indirect_branch_test")
        .config(indirect_branch_config())
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    println!("Output:\n{}", output);

    // Verify key outputs
    assert!(output.contains("=== Running Rust Indirect Branch Test Suite ==="));
    assert!(output.contains("test_conditional_br: x > 0"));
    assert!(output.contains("test_match_br: choice = 1"));
    assert!(output.contains("test_loop_while: sum = 15"));
    assert!(output.contains("test_loop_for: count ="));
    assert!(output.contains("test_nested_if_else:"));
    assert!(output.contains("test_labeled_loop:"));
    assert!(output.contains("test_match_guards:"));
    assert!(output.contains("test_early_return:"));
    assert!(output.contains("test_option_match:"));
    assert!(output.contains("test_complex_flow:"));
    assert!(output.contains("=== All tests completed!"));
}

#[test]
#[serial]
fn test_rust_indirect_branch_chained() {
    common::ensure_plugin_built();

    let project_dir = common::project_root()
        .join("tests")
        .join("rust")
        .join("indirect_branch");

    let result = RustCompileBuilder::new(&project_dir, "indirect_branch_test")
        .config(indirect_branch_config_chained())
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("=== All tests completed!"));
}

#[test]
#[serial]
fn test_rust_indirect_branch_all_flags() {
    common::ensure_plugin_built();

    let project_dir = common::project_root()
        .join("tests")
        .join("rust")
        .join("indirect_branch");

    let result = RustCompileBuilder::new(&project_dir, "indirect_branch_test")
        .config(indirect_branch_config_all_flags())
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("=== All tests completed!"));
}

#[test]
#[serial]
fn test_rust_indirect_branch_vs_baseline() {
    common::ensure_plugin_built();

    let project_dir = common::project_root()
        .join("tests")
        .join("rust")
        .join("indirect_branch");

    // Compile without obfuscation (baseline)
    let baseline_result = RustCompileBuilder::new(&project_dir, "indirect_branch_test")
        .without_plugin()
        .use_stable()
        .compile();

    baseline_result.assert_success();
    let baseline_run = baseline_result.run();
    baseline_run.assert_success();
    let baseline_output = baseline_run.stdout();

    // Compile with indirect branch obfuscation
    let obfuscated_result = RustCompileBuilder::new(&project_dir, "indirect_branch_test")
        .config(indirect_branch_config())
        .compile();

    obfuscated_result.assert_success();
    let obfuscated_run = obfuscated_result.run();
    obfuscated_run.assert_success();
    let obfuscated_output = obfuscated_run.stdout();

    // Both versions should produce identical output
    assert_eq!(
        baseline_output, obfuscated_output,
        "Baseline and obfuscated outputs differ"
    );
}

#[test]
#[serial]
fn test_rust_split_basic_block() {
    common::ensure_plugin_built();

    let project_dir = common::project_root()
        .join("tests")
        .join("rust")
        .join("indirect_branch");

    let result = RustCompileBuilder::new(&project_dir, "indirect_branch_test")
        .config(split_basic_block_config())
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("=== All tests completed!"));
}

#[test]
#[serial]
fn test_rust_split_and_indirect_branch() {
    common::ensure_plugin_built();

    let project_dir = common::project_root()
        .join("tests")
        .join("rust")
        .join("indirect_branch");

    // SplitBasicBlock (priority 980) runs before IndirectBranch (priority 800)
    let result = RustCompileBuilder::new(&project_dir, "indirect_branch_test")
        .config(split_and_indirect_branch_config())
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("=== All tests completed!"));
}
