//! Integration tests for string obfuscation with various control flows

mod common;

use common::CompilationTest;

#[test]
fn test_strings_basic_xor() {
    let test = CompilationTest::new("tests/test_strings.c", "test_strings_xor");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_strings_simd_xor() {
    let test = CompilationTest::new("tests/test_strings.c", "test_strings_simd");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_strings_global_timing() {
    let test = CompilationTest::new("tests/test_strings.c", "test_strings_global");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
    ]);
}

#[test]
fn test_strings_utf8_handling() {
    let test = CompilationTest::new("tests/test_strings.c", "test_strings_utf8");
    // Test UTF-8 and special character handling
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_strings_duplicate_elimination() {
    let test = CompilationTest::new("tests/test_strings.c", "test_strings_duplicate");
    // Test duplicate string handling
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
        ("AMICE_STRING_STACK_ALLOC", "true"),
    ]);
}

#[test]
fn test_strings_control_flow_branches() {
    let test = CompilationTest::new("tests/test_strings.c", "test_strings_branches");
    // Test string obfuscation in different control flow branches
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
        ("AMICE_STRING_STACK_ALLOC", "false"),
    ]);
}

#[test]
fn test_strings_with_shuffle() {
    let test = CompilationTest::new("tests/test_strings.c", "test_strings_shuffle");
    // Test string obfuscation combined with block shuffling
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "random"),
    ]);
}

#[test]
fn test_strings_edge_cases() {
    let test = CompilationTest::new("tests/test_strings.c", "test_strings_edge");
    // Test edge cases: escaped characters, format strings, etc.
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
        ("AMICE_STRING_STACK_ALLOC", "true"),
    ]);
}