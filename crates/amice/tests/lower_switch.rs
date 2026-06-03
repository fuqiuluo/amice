//! Integration tests for switch lowering.

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn lower_switch_with_dummy_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.lower_switch = Some(true);
    config.lower_switch_with_dummy_code = Some(true);
    config
}

#[test]
fn test_lower_switch_with_dummy_code_verifies() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("lower_switch", "lower_switch_dummy.c", Language::C),
        "lower_switch_dummy",
    )
    .config(lower_switch_with_dummy_config())
    .optimization("O1")
    .compile();

    result.assert_success();
    assert!(
        !result.stderr().contains("is not verified"),
        "LowerSwitch dummy mode emitted verifier warning:\n{}",
        result.stderr()
    );
    result.run().assert_success();
}
