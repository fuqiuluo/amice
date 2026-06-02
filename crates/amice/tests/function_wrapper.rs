//! Integration tests for function wrapper obfuscation.

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn function_wrapper_config() -> ObfuscationConfig {
    ObfuscationConfig {
        function_wrapper: Some(true),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_function_wrapper_basic() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("function_wrapper", "function_wrapper_test.c", Language::C),
        "function_wrapper_basic",
    )
    .config(function_wrapper_config())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();

    // Verify expected output
    assert!(lines.iter().any(|l| l.contains("Testing function wrapper")));
    assert!(lines.iter().any(|l| l.contains("In add function: 5 + 3")));
    assert!(lines.iter().any(|l| l.contains("Result of add: 8")));
    assert!(lines.iter().any(|l| l.contains("In multiply function: 4 * 7")));
    assert!(lines.iter().any(|l| l.contains("Result of multiply: 28")));
    assert!(lines.iter().any(|l| l.contains("Hello, Function Wrapper!")));
}

#[test]
fn test_function_wrapper_optimized() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("function_wrapper", "function_wrapper_test.c", Language::C),
        "function_wrapper_o2",
    )
    .config(function_wrapper_config())
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_clone_function() {
    ensure_plugin_built();

    // clone_function.c tests constant argument specialization
    let result = CppCompileBuilder::new(
        fixture_path("function_wrapper", "clone_function.c", Language::C),
        "clone_function",
    )
    .config(ObfuscationConfig::disabled())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}
