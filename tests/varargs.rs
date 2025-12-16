//! Integration tests for varargs function handling.
//!
//! This module tests that obfuscation passes correctly handle:
//! - printf, sprintf, fprintf (standard varargs functions)
//! - Custom varargs functions
//! - Functions that call varargs functions
//!
//! Critical tests:
//! - indirect_call should not break varargs calls
//! - function_wrapper should not wrap varargs functions
//! - clone_function should not clone varargs functions

mod common;

use crate::common::Language;
use common::{CompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

#[test]
fn test_varargs_with_indirect_call() {
    ensure_plugin_built();

    // This is a CRITICAL test - indirect_call currently does NOT properly handle varargs
    // This test will likely FAIL or produce incorrect printf output
    let result = CompileBuilder::new(
        fixture_path("varargs", "varargs_indirect_call.c", Language::C),
        "varargs_indirect_call",
    )
    .config(ObfuscationConfig {
        indirect_call: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_varargs_with_indirect_call_optimized() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("varargs", "varargs_indirect_call.c", Language::C),
        "varargs_indirect_call_o2",
    )
    .config(ObfuscationConfig {
        indirect_call: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_varargs_with_function_wrapper() {
    ensure_plugin_built();

    // function_wrapper should detect varargs and skip wrapping
    let result = CompileBuilder::new(
        fixture_path("varargs", "varargs_function_wrapper.c", Language::C),
        "varargs_function_wrapper",
    )
    .config(ObfuscationConfig {
        function_wrapper: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_varargs_with_function_wrapper_optimized() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("varargs", "varargs_function_wrapper.c", Language::C),
        "varargs_function_wrapper_o2",
    )
    .config(ObfuscationConfig {
        function_wrapper: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_varargs_with_clone_function() {
    ensure_plugin_built();

    // clone_function should detect varargs and skip cloning
    let result = CompileBuilder::new(
        fixture_path("varargs", "varargs_clone_function.c", Language::C),
        "varargs_clone_function",
    )
    .config(ObfuscationConfig {
        clone_function: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_varargs_combined_passes() {
    ensure_plugin_built();

    // Test multiple passes with varargs
    let result = CompileBuilder::new(
        fixture_path("varargs", "varargs_indirect_call.c", Language::C),
        "varargs_combined",
    )
    .config(ObfuscationConfig {
        indirect_call: Some(true),
        function_wrapper: Some(true),
        flatten: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}
