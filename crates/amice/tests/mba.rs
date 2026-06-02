//! Integration tests for Mixed Boolean Arithmetic (MBA) obfuscation.

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn mba_config() -> ObfuscationConfig {
    ObfuscationConfig {
        mba: Some(true),
        ..ObfuscationConfig::disabled()
    }
}

/// Get expected output from non-obfuscated baseline
fn get_baseline_output(test_name: &str) -> String {
    let result = CppCompileBuilder::new(
        fixture_path("mba", "mba_constants_demo.c", Language::C),
        &format!("mba_baseline_{}", test_name),
    )
    .without_plugin()
    .compile();

    result.assert_success();
    result.run().stdout()
}

#[test]
fn test_mba_basic() {
    ensure_plugin_built();

    let baseline = get_baseline_output("basic");

    let result = CppCompileBuilder::new(fixture_path("mba", "mba_constants_demo.c", Language::C), "mba_basic")
        .config(mba_config())
        .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    // MBA should not change program output
    assert_eq!(run.stdout(), baseline, "MBA obfuscation changed program output");
}

#[test]
fn test_mba_optimized() {
    ensure_plugin_built();

    let baseline = get_baseline_output("optimized");

    let result = CppCompileBuilder::new(fixture_path("mba", "mba_constants_demo.c", Language::C), "mba_o2")
        .config(mba_config())
        .optimization("O2")
        .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    assert_eq!(run.stdout(), baseline, "MBA with O2 changed program output");
}

#[test]
fn test_mba_with_bcf() {
    ensure_plugin_built();

    let baseline = get_baseline_output("with_bcf");

    let config = ObfuscationConfig {
        mba: Some(true),
        bogus_control_flow: Some(true),
        ..ObfuscationConfig::disabled()
    };

    let result = CppCompileBuilder::new(fixture_path("mba", "mba_constants_demo.c", Language::C), "mba_with_bcf")
        .config(config)
        .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    assert_eq!(run.stdout(), baseline, "MBA with BCF changed program output");
}
