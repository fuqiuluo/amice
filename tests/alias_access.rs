//! Integration tests for alias access obfuscation.

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn alias_access_config() -> ObfuscationConfig {
    ObfuscationConfig {
        alias_access: Some(true),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_alias_access_basic() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("alias_access", "alias_access.c", Language::C),
        "alias_access_basic",
    )
    .config(alias_access_config())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();

    println!("{:#?}", lines);
}

#[test]
fn test_alias_access_optimized() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("alias_access", "alias_access.c", Language::C),
        "alias_access_o2",
    )
    .config(alias_access_config())
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
}
