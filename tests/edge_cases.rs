//! Integration tests for edge cases in obfuscation passes.
//!
//! This module tests:
//! - Empty functions
//! - Single basic block functions
//! - Large functions
//! - Boundary conditions for all obfuscation passes

mod common;

use crate::common::Language;
use common::{CompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn all_passes_config() -> ObfuscationConfig {
    ObfuscationConfig {
        flatten: Some(true),
        bogus_control_flow: Some(true),
        mba: Some(true),
        indirect_branch: Some(true),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_empty_function() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("edge_cases", "empty_function.c", Language::C),
        "empty_function",
    )
    .config(all_passes_config())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_empty_function_optimized() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("edge_cases", "empty_function.c", Language::C),
        "empty_function_o2",
    )
    .config(ObfuscationConfig {
        flatten: Some(false),
        bogus_control_flow: Some(true),
        mba: Some(false),
        indirect_branch: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_large_function_flatten() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("edge_cases", "large_function.c", Language::C),
        "large_function",
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
fn test_single_block_with_bcf() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("edge_cases", "empty_function.c", Language::C),
        "single_block_bcf",
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
fn test_single_block_with_flatten() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("edge_cases", "empty_function.c", Language::C),
        "single_block_flatten",
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
fn test_single_block_with_indirect_branch() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("edge_cases", "empty_function.c", Language::C),
        "single_block_indirect_branch",
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
