//! Integration tests for shuffle blocks obfuscation.
//!
//! Tests various shuffle modes:
//! - Random shuffle
//! - Reverse shuffle
//! - Rotate shuffle
//! - Combined modes

mod common;

use crate::common::Language;
use common::{CompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn shuffle_config(flags: &str) -> ObfuscationConfig {
    ObfuscationConfig {
        shuffle_blocks: Some(true),
        shuffle_blocks_flags: Some(flags.to_string()),
        ..ObfuscationConfig::disabled()
    }
}

fn get_baseline_output(name: &str) -> String {
    let result = CompileBuilder::new(
        fixture_path("shuffle_blocks", "shuffle_test.c", Language::C),
        &format!("shuffle_baseline_{}", name),
    )
    .without_plugin()
    .compile();

    result.assert_success();
    result.run().stdout()
}

#[test]
fn test_shuffle_blocks_random() {
    ensure_plugin_built();

    let baseline = get_baseline_output("random");

    let result = CompileBuilder::new(
        fixture_path("shuffle_blocks", "shuffle_test.c", Language::C),
        "shuffle_random",
    )
    .config(shuffle_config("random"))
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    // Output should be identical despite block shuffling
    assert_eq!(run.stdout(), baseline, "Random shuffle changed program behavior");
}

#[test]
fn test_shuffle_blocks_reverse() {
    ensure_plugin_built();

    let baseline = get_baseline_output("reverse");

    let result = CompileBuilder::new(
        fixture_path("shuffle_blocks", "shuffle_test.c", Language::C),
        "shuffle_reverse",
    )
    .config(shuffle_config("reverse"))
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    assert_eq!(run.stdout(), baseline, "Reverse shuffle changed program behavior");
}

#[test]
fn test_shuffle_blocks_rotate() {
    ensure_plugin_built();

    let baseline = get_baseline_output("rotate");

    let result = CompileBuilder::new(
        fixture_path("shuffle_blocks", "shuffle_test.c", Language::C),
        "shuffle_rotate",
    )
    .config(shuffle_config("rotate"))
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    assert_eq!(run.stdout(), baseline, "Rotate shuffle changed program behavior");
}

#[test]
fn test_shuffle_blocks_combined() {
    ensure_plugin_built();

    let baseline = get_baseline_output("combined");

    let result = CompileBuilder::new(
        fixture_path("shuffle_blocks", "shuffle_test.c", Language::C),
        "shuffle_combined",
    )
    .config(shuffle_config("reverse,rotate"))
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    assert_eq!(run.stdout(), baseline, "Combined shuffle changed program behavior");
}

#[test]
fn test_shuffle_with_split_basic_block() {
    ensure_plugin_built();

    let baseline = get_baseline_output("split");

    let config = ObfuscationConfig {
        shuffle_blocks: Some(true),
        shuffle_blocks_flags: Some("random".to_string()),
        split_basic_block: Some(true),
        ..ObfuscationConfig::disabled()
    };

    let result = CompileBuilder::new(
        fixture_path("shuffle_blocks", "shuffle_test.c", Language::C),
        "shuffle_with_split",
    )
    .config(config)
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    assert_eq!(run.stdout(), baseline, "Shuffle with split changed program behavior");
}
