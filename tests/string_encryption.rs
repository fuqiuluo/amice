//! Integration tests for string encryption obfuscation.
//!
//! Tests various string encryption configurations:
//! - XOR vs SIMD XOR algorithms
//! - Lazy vs Global decryption timing
//! - Stack vs Heap allocation
//! - Multiple encryption passes

mod common;

use common::{CompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

/// Expected output for const_strings.c test
fn expected_const_strings_output() -> Vec<&'static str> {
    vec![
        "test1 (bytes): 68 65 6C 6C 6F 00 00 39 05",
        "test1 string: hello",
        "test1 int: 1337",
        "test2 (bytes): 68 65 6C 6C 6F 00 00 39 05 00 00",
        "test2 string: hello",
        "test2 int: 1337",
        "p1: (nil)",
        // "p2: 0x..." - dynamic pointer, just check prefix
        "1pu: corld",
        "1pu: World",
        "Xello world1",
        "Xello world2",
        "Hello world3",
        // "This is a literal. 0x..." - dynamic pointer
    ]
}

fn check_const_strings_output(lines: &[String]) {
    let expected = expected_const_strings_output();

    assert!(lines.len() >= 14, "Expected at least 14 lines, got {}", lines.len());

    // Check exact matches
    assert_eq!(lines[0], expected[0], "Line 0 mismatch");
    assert_eq!(lines[1], expected[1], "Line 1 mismatch");
    assert_eq!(lines[2], expected[2], "Line 2 mismatch");
    assert_eq!(lines[3], expected[3], "Line 3 mismatch");
    assert_eq!(lines[4], expected[4], "Line 4 mismatch");
    assert_eq!(lines[5], expected[5], "Line 5 mismatch");
    assert_eq!(lines[6], expected[6], "Line 6 mismatch");
    // Line 7 is p2 pointer - just check prefix
    assert!(lines[7].starts_with("p2: 0x"), "Line 7 should start with 'p2: 0x'");
    assert_eq!(lines[8], expected[7], "Line 8 mismatch");
    assert_eq!(lines[9], expected[8], "Line 9 mismatch");
    assert_eq!(lines[10], expected[9], "Line 10 mismatch");
    assert_eq!(lines[11], expected[10], "Line 11 mismatch");
    assert_eq!(lines[12], expected[11], "Line 12 mismatch");
    // Lines 13-14 are literal pointers - just check prefix
    assert!(lines[13].starts_with("This is a literal. 0x"), "Line 13 mismatch");
    assert!(lines[14].starts_with("This is a literal. 0x"), "Line 14 mismatch");
}

fn string_config_lazy_xor() -> ObfuscationConfig {
    ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some("xor".to_string()),
        string_decrypt_timing: Some("lazy".to_string()),
        ..ObfuscationConfig::disabled()
    }
}

fn string_config_lazy_xor_stack() -> ObfuscationConfig {
    ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some("xor".to_string()),
        string_decrypt_timing: Some("lazy".to_string()),
        string_stack_alloc: Some(true),
        ..ObfuscationConfig::disabled()
    }
}

fn string_config_global_xor() -> ObfuscationConfig {
    ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some("xor".to_string()),
        string_decrypt_timing: Some("global".to_string()),
        ..ObfuscationConfig::disabled()
    }
}

fn string_config_lazy_simd_xor() -> ObfuscationConfig {
    ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some("simd_xor".to_string()),
        string_decrypt_timing: Some("lazy".to_string()),
        ..ObfuscationConfig::disabled()
    }
}

fn string_config_global_simd_xor() -> ObfuscationConfig {
    ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some("simd_xor".to_string()),
        string_decrypt_timing: Some("global".to_string()),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_const_strings_lazy_xor() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("string_encryption", "const_strings.c"),
        "const_strings_lazy_xor",
    )
    .config(string_config_lazy_xor())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_const_strings_output(&lines);
}

#[test]
fn test_const_strings_lazy_xor_stack() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("string_encryption", "const_strings.c"),
        "const_strings_lazy_xor_stack",
    )
    .config(string_config_lazy_xor_stack())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_const_strings_output(&lines);
}

#[test]
fn test_const_strings_lazy_xor_stack_multi() {
    ensure_plugin_built();

    let mut config = string_config_lazy_xor_stack();
    config.string_max_encryption_count = Some(2);

    let result = CompileBuilder::new(
        fixture_path("string_encryption", "const_strings.c"),
        "const_strings_lazy_xor_stack_multi",
    )
    .config(config)
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_const_strings_output(&lines);
}

#[test]
fn test_const_strings_global_xor() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("string_encryption", "const_strings.c"),
        "const_strings_global_xor",
    )
    .config(string_config_global_xor())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_const_strings_output(&lines);
}

#[test]
fn test_const_strings_lazy_simd_xor() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("string_encryption", "const_strings.c"),
        "const_strings_lazy_simd_xor",
    )
    .config(string_config_lazy_simd_xor())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_const_strings_output(&lines);
}

#[test]
fn test_const_strings_global_simd_xor() {
    ensure_plugin_built();

    let result = CompileBuilder::new(
        fixture_path("string_encryption", "const_strings.c"),
        "const_strings_global_simd_xor",
    )
    .config(string_config_global_simd_xor())
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_const_strings_output(&lines);
}
