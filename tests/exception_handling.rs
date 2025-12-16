//! Integration tests for C++ exception handling with obfuscation.
//!
//! This module tests that obfuscation passes correctly handle:
//! - C++ exceptions (throw/catch)
//! - Invoke instructions
//! - Landing pads
//! - Multiple catch blocks
//! - Nested exceptions
//!
//! Critical test: bogus_control_flow should skip functions with exception handling

mod common;

use common::{CompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};
use crate::common::Language;

#[test]
fn test_cpp_exception_with_bcf() {
    ensure_plugin_built();

    // This is a CRITICAL test - BCF currently does NOT check for exception handling
    // This test will likely FAIL or produce incorrect results
    let result = CompileBuilder::new(
        fixture_path("exception_handling", "cpp_exception_bcf.cpp", Language::Cpp),
        "cpp_exception_bcf",
    )
    .config(ObfuscationConfig {
        bogus_control_flow: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_cpp_exception_with_bcf_optimized() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("exception_handling", "cpp_exception_bcf.cpp", Language::Cpp),
        "cpp_exception_bcf_o2",
    )
    .config(ObfuscationConfig {
        bogus_control_flow: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_cpp_exception_with_flatten() {
    ensure_plugin_built();

    // Flatten should properly detect exception handling and skip the function
    let result = CompileBuilder::new(
        fixture_path("exception_handling", "cpp_exception_flatten.cpp", Language::Cpp),
        "cpp_exception_flatten",
    )
    .config(ObfuscationConfig {
        flatten: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_cpp_exception_with_flatten_optimized() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("exception_handling", "cpp_exception_flatten.cpp", Language::Cpp),
        "cpp_exception_flatten_o2",
    )
    .config(ObfuscationConfig {
        flatten: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_cpp_exception_with_indirect_branch() {
    ensure_plugin_built();

    // Indirect branch has partial EH detection - should handle this correctly
    let result = CompileBuilder::new(
        fixture_path("exception_handling", "cpp_exception_indirect_branch.cpp", Language::Cpp),
        "cpp_exception_indirect_branch",
    )
    .config(ObfuscationConfig {
        indirect_branch: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_cpp_exception_combined() {
    ensure_plugin_built();

    // Test multiple passes with exception handling
    let result = CompileBuilder::new(
        fixture_path("exception_handling", "cpp_exception_bcf.cpp", Language::Cpp),
        "cpp_exception_combined",
    )
    .config(ObfuscationConfig {
        flatten: Some(true),
        bogus_control_flow: Some(true),
        indirect_branch: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}
