//! Integration tests for Rust indirect call obfuscation.

mod common;

use common::{ObfuscationConfig, RustCompileBuilder};
use serial_test::serial;

fn indirect_call_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.indirect_call = Some(true);
    config
}

#[test]
#[serial]
fn test_rust_indirect_call_basic() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("indirect_call");

    let result = RustCompileBuilder::new(&project_dir, "indirect_call_test")
        .config(indirect_call_config())
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    println!("Output:\n{}", output);

    // Verify direct function calls work
    assert!(output.contains("=== Direct Function Calls ==="));
    assert!(output.contains("Called: add(10, 5)"));
    assert!(output.contains("Called: mul(10, 5)"));
    assert!(output.contains("Called: sub(10, 5)"));
    assert!(output.contains("Result: 15")); // add
    assert!(output.contains("Result: 50")); // mul
    assert!(output.contains("Result: 5")); // sub

    // Verify function with no arguments
    assert!(output.contains("Called: greet()"));

    // Verify recursive calls
    assert!(output.contains("=== Recursive Calls ==="));
    assert!(output.contains("Factorial(5) = 120"));

    // Verify method calls
    assert!(output.contains("=== Method Calls ==="));
    assert!(output.contains("Called: Calculator::new(100)"));
    assert!(output.contains("Calculator value: 175"));

    // Verify trait method calls
    assert!(output.contains("=== Trait Method Calls ==="));
    assert!(output.contains("Computed: 350"));

    // Verify closure calls
    assert!(output.contains("=== Closure Calls ==="));
    assert!(output.contains("Closure result: 10"));

    // Verify function pointer
    assert!(output.contains("=== Function Pointer ==="));
    assert!(output.contains("Function pointer result: 300"));

    assert!(output.contains("=== All tests completed! ==="));
}

#[test]
#[serial]
fn test_rust_indirect_call_vs_baseline() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("indirect_call");

    // Compile without obfuscation (baseline)
    let baseline_result = RustCompileBuilder::new(&project_dir, "indirect_call_test")
        .without_plugin()
        .use_stable()
        .compile();

    baseline_result.assert_success();
    let baseline_run = baseline_result.run();
    baseline_run.assert_success();
    let baseline_output = baseline_run.stdout();

    // Compile with indirect call obfuscation
    let obfuscated_result = RustCompileBuilder::new(&project_dir, "indirect_call_test")
        .config(indirect_call_config())
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
fn test_rust_indirect_call_optimized() {
    common::ensure_plugin_built();

    let project_dir = common::project_root().join("tests").join("rust").join("indirect_call");

    // Test with release optimization
    let result = RustCompileBuilder::new(&project_dir, "indirect_call_test")
        .config(indirect_call_config())
        .optimization("release")
        .compile();

    result.assert_success();
    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    assert!(output.contains("=== All tests completed! ==="));
}
