//! Integration tests for PHI node handling in obfuscation passes.
//!
//! This module tests that obfuscation passes correctly maintain PHI nodes:
//! - PHI nodes from if-else branches
//! - PHI nodes from loops
//! - Multiple PHI nodes in a single basic block
//! - Nested control flow with PHI nodes
//!
//! Critical test: split_basic_block should fix PHI nodes after splitting

mod common;

use common::{CompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

#[test]
fn test_phi_with_split_basic_block() {
    ensure_plugin_built();

    // This is a CRITICAL test - split_basic_block currently does NOT fix PHI nodes
    // This test will likely FAIL or produce incorrect results
    let result = CompileBuilder::new(
        fixture_path("phi_nodes", "phi_split_basic_block.c"),
        "phi_split_basic_block"
    )
    .config(ObfuscationConfig {
        split_basic_block: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_phi_with_split_basic_block_optimized() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("phi_nodes", "phi_split_basic_block.c"),
        "phi_split_basic_block_o2"
    )
    .config(ObfuscationConfig {
        split_basic_block: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_phi_with_flatten() {
    ensure_plugin_built();

    // Flatten should properly fix PHI nodes
    let result = CompileBuilder::new(
        fixture_path("phi_nodes", "phi_flatten.c"),
        "phi_flatten"
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
fn test_phi_with_flatten_optimized() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("phi_nodes", "phi_flatten.c"),
        "phi_flatten_o2"
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
fn test_phi_with_split_and_flatten() {
    ensure_plugin_built();

    // Test combination of split_basic_block and flatten
    let result = CompileBuilder::new(
        fixture_path("phi_nodes", "phi_split_basic_block.c"),
        "phi_split_and_flatten"
    )
    .config(ObfuscationConfig {
        split_basic_block: Some(true),
        flatten: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}

#[test]
fn test_phi_with_bcf() {
    ensure_plugin_built();

    // BCF should properly fix PHI nodes
    let result = CompileBuilder::new(
        fixture_path("phi_nodes", "phi_split_basic_block.c"),
        "phi_bcf"
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
fn test_phi_combined_passes() {
    ensure_plugin_built();

    // Test all control flow passes with PHI nodes
    let result = CompileBuilder::new(
        fixture_path("phi_nodes", "phi_split_basic_block.c"),
        "phi_combined"
    )
    .config(ObfuscationConfig {
        flatten: Some(true),
        bogus_control_flow: Some(true),
        split_basic_block: Some(true),
        shuffle_blocks: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}
