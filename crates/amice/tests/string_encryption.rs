//! Integration tests for string encryption obfuscation.
//!
//! Tests various string encryption configurations:
//! - XOR vs SIMD XOR algorithms
//! - Lazy vs Global decryption timing
//! - Stack vs Heap allocation
//! - Multiple encryption passes

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

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
    assert!(
        lines[6] == expected[6] || lines[6] == "p1: 0x0",
        "Line 6 mismatch: expected null pointer formatting, got '{}'",
        lines[6]
    );
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

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "const_strings.c", Language::C),
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

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "const_strings.c", Language::C),
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
fn test_const_strings_global_xor() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "const_strings.c", Language::C),
        "const_strings_global_xor",
    )
    .config(string_config_global_xor())
    .optimization("O2")
    .compile();

    result.assert_success();
    let binary = std::fs::read(&result.binary_path).expect("failed to read protected binary");
    assert!(
        !binary
            .windows(b"This is a literal.".len())
            .any(|value| value == b"This is a literal."),
        "O2 restored a plaintext initializer for XOR global decryption"
    );
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    check_const_strings_output(&lines);
}

#[test]
fn test_constant_aggregate_string_uses_fall_back_without_errors() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "constant_aggregate.cpp", Language::Cpp),
        "constant_aggregate_lazy_xor",
    )
    .config(string_config_lazy_xor())
    .optimization("O2")
    .compile();

    result.assert_success();
    assert!(
        !result.stderr().contains("unexpected StructValue user"),
        "constant aggregates should use the global fallback, not emit errors: {}",
        result.stderr()
    );
    let binary = std::fs::read(&result.binary_path).expect("failed to read protected binary");
    assert!(
        !binary
            .windows(b"AMICE_AGGREGATE_SECRET_ALPHA".len())
            .any(|value| value == b"AMICE_AGGREGATE_SECRET_ALPHA"),
        "constant aggregate marker remained in plaintext"
    );

    let run = result.run();
    run.assert_success();
    assert_eq!(
        run.stdout_lines(),
        ["AMICE_AGGREGATE_SECRET_ALPHA", "AMICE_AGGREGATE_SECRET_BETA"]
    );
}

#[test]
fn test_const_strings_lazy_simd_xor() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "const_strings.c", Language::C),
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

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "const_strings.c", Language::C),
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

fn wide_strings_code_unit_lines() -> [String; 3] {
    // Mirrors wide_strings.c: every code unit is printed as "%04x " by the
    // fixture, so the expected output is independent of wchar_t's width.
    // The fixture prints one "%04x " per code unit; stdout_lines trims the
    // trailing space of each line.
    let line = |text: &str| -> String {
        text.chars()
            .map(|c| format!("{:04x}", u32::from(c)))
            .collect::<Vec<_>>()
            .join(" ")
    };
    [line("Amice16\u{4F60}"), line("Amice32\u{4F60}"), line("AmiceWide")]
}

fn check_wide_strings_output(lines: &[String]) {
    let expected = wide_strings_code_unit_lines();
    assert_eq!(lines.len(), 3, "expected 3 lines, got {}: {:?}", lines.len(), lines);
    assert_eq!(lines[0], expected[0], "UTF-16 line mismatch");
    assert_eq!(lines[1], expected[1], "UTF-32 line mismatch");
    assert_eq!(lines[2], expected[2], "wchar_t line mismatch");
}

/// The encrypted globals must not keep the UTF-16LE / UTF-32LE plaintext bytes
/// of the wide literals.
fn check_wide_strings_plaintext_absent(binary: &[u8]) {
    for (label, needle) in [
        ("UTF-16", utf16_le_bytes("Amice16\u{4F60}")),
        ("UTF-32", utf32_le_bytes("Amice32\u{4F60}")),
    ] {
        assert!(
            !binary.windows(needle.len()).any(|window| window == needle),
            "{label} literal remained in plaintext"
        );
    }
}

fn utf16_le_bytes(value: &str) -> Vec<u8> {
    value.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn utf32_le_bytes(value: &str) -> Vec<u8> {
    value.chars().flat_map(|c| u32::from(c).to_le_bytes()).collect()
}

fn wide_string_config(timing: &str, algorithm: &str) -> ObfuscationConfig {
    ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some(algorithm.to_string()),
        string_decrypt_timing: Some(timing.to_string()),
        // Clang names wide literals ".str" on Itanium targets but uses MSVC
        // decorations elsewhere, so the ".str" name filter must be off.
        string_only_llvm_string: Some(false),
        ..ObfuscationConfig::disabled()
    }
}

#[test]
fn test_wide_strings_lazy_xor() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "wide_strings.c", Language::C),
        "wide_strings_lazy_xor",
    )
    .config(wide_string_config("lazy", "xor"))
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    check_wide_strings_output(&run.stdout_lines());
}

#[test]
fn test_wide_strings_global_xor() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "wide_strings.c", Language::C),
        "wide_strings_global_xor",
    )
    .config(wide_string_config("global", "xor"))
    .optimization("O2")
    .compile();

    result.assert_success();
    let binary = std::fs::read(&result.binary_path).expect("failed to read protected binary");
    check_wide_strings_plaintext_absent(&binary);

    let run = result.run();
    run.assert_success();
    check_wide_strings_output(&run.stdout_lines());
}

#[test]
fn test_wide_strings_lazy_simd_xor() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "wide_strings.c", Language::C),
        "wide_strings_lazy_simd_xor",
    )
    .config(wide_string_config("lazy", "simd_xor"))
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    check_wide_strings_output(&run.stdout_lines());
}

#[test]
fn test_wide_strings_global_simd_xor() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "wide_strings.c", Language::C),
        "wide_strings_global_simd_xor",
    )
    .config(wide_string_config("global", "simd_xor"))
    .optimization("O2")
    .compile();

    result.assert_success();
    let binary = std::fs::read(&result.binary_path).expect("failed to read protected binary");
    check_wide_strings_plaintext_absent(&binary);

    let run = result.run();
    run.assert_success();
    check_wide_strings_output(&run.stdout_lines());
}
