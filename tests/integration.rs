//! Integration tests using real-world code (MD5, etc.)
//!
//! These tests verify that obfuscation passes work correctly on
//! realistic codebases without breaking functionality.

mod common;

use crate::common::Language;
use common::{CompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};
// ============================================================================
// MD5 Tests
// ============================================================================

/// Expected MD5 output (standard test vectors)
fn expected_md5_output() -> Vec<&'static str> {
    vec![
        "MD5(\"\") = 906adc8dc99e0b7e4de1afd68e879d9f",
        "MD5(\"a\") = bd3cfa105b77fc3af680893c16c78324",
        "MD5(\"abc\") = 59e8f1e370c55438207d937eb139eb8e",
        "MD5(\"message digest\") = 82330f944531d1fb7027004b1091b8fe",
        "MD5(\"abcdefghijklmnopqrstuvwxyz\") = 3c57117f1842e973ea2072eafc16c943",
        "MD5(\"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789\") = 48942efc4110c7c2af48ee9b2c979b20",
        "MD5(\"1234567890\") = ffa0f5838119587bf1323320e58298d0",
        "MD5([00 01 02 FF]) = f1a5df091e9edf48f6d359fa9ff723b7",
    ]
}

fn check_md5_output(lines: &[String]) {
    let expected = expected_md5_output();
    assert_eq!(
        lines.len(),
        expected.len(),
        "Expected {} lines, got {}",
        expected.len(),
        lines.len()
    );

    for (i, expected_line) in expected.iter().enumerate() {
        assert_eq!(
            lines[i], *expected_line,
            "Line {} mismatch.\nExpected: '{}'\nActual: '{}'",
            i, expected_line, lines[i]
        );
    }
}

fn md5_obfuscation_config() -> ObfuscationConfig {
    ObfuscationConfig {
        shuffle_blocks: Some(false),
        shuffle_blocks_flags: Some("random".to_string()),
        split_basic_block: Some(false),
        indirect_branch: Some(true),
        indirect_branch_flags: Some("chained_dummy_block".to_string()),
        string_encryption: Some(false),
        string_algorithm: Some("xor_simd".to_string()),
        string_decrypt_timing: Some("lazy".to_string()),
        string_stack_alloc: Some(true),
        string_inline_decrypt_fn: Some(true),
        indirect_call: Some(false),
        vm_flatten: Some(false),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_md5_c() {
    ensure_plugin_built();

    let result = CompileBuilder::new(fixture_path("integration", "md5.c", Language::C), "md5_c")
        .config(md5_obfuscation_config())
        .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_md5_output(&lines);
}

#[test]
fn test_md5_c_o3() {
    ensure_plugin_built();

    let result = CompileBuilder::new(fixture_path("integration", "md5.c", Language::C), "md5_c_o3")
        .config(md5_obfuscation_config())
        .optimization("O3")
        .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_md5_output(&lines);
}

#[test]
fn test_md5_cpp() {
    ensure_plugin_built();

    let result = CompileBuilder::new(fixture_path("integration", "md5.cc", Language::Cpp), "md5_cpp")
        .config(md5_obfuscation_config())
        .std("c++17")
        .arg("-Wall")
        .arg("-Wextra")
        .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_md5_output(&lines);
}

#[test]
fn test_md5_cpp_o3() {
    ensure_plugin_built();

    let result = CompileBuilder::new(fixture_path("integration", "md5.cc", Language::Cpp), "md5_cpp_o3")
        .config(md5_obfuscation_config())
        .std("c++17")
        .optimization("O3")
        .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_md5_output(&lines);
}

// ============================================================================
// Full Obfuscation Pipeline Test
// ============================================================================

#[test]
fn test_md5_full_obfuscation() {
    ensure_plugin_built();

    let config = ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some("xor".to_string()),
        string_decrypt_timing: Some("lazy".to_string()),
        indirect_branch: Some(true),
        bogus_control_flow: Some(true),
        mba: Some(true),
        shuffle_blocks: Some(true),
        shuffle_blocks_flags: Some("random".to_string()),
        ..ObfuscationConfig::disabled()
    };

    let result = CompileBuilder::new(
        fixture_path("integration", "md5.c", Language::C),
        "md5_full_obfuscation",
    )
    .config(config)
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_md5_output(&lines);
}
