//! Integration tests for large string handling in obfuscation

mod common;

use common::CompilationTest;

#[test]
fn test_large_string_xor() {
    let test = CompilationTest::new("tests/large_string.c", "large_string_xor");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_large_string_simd_xor() {
    let test = CompilationTest::new("tests/large_string.c", "large_string_simd");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_large_string_global_timing() {
    let test = CompilationTest::new("tests/large_string.c", "large_string_global");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
    ]);
}

#[test]
fn test_large_string_stack_allocation() {
    let test = CompilationTest::new("tests/large_string.c", "large_string_stack");
    // Large strings should not be stack allocated - test boundary condition
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
        ("AMICE_STRING_STACK_ALLOC", "true"),
    ]);
}

#[test]
fn test_large_string_no_stack_allocation() {
    let test = CompilationTest::new("tests/large_string.c", "large_string_no_stack");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
        ("AMICE_STRING_STACK_ALLOC", "false"),
    ]);
}

#[test]
fn test_large_string_edge_cases() {
    let test = CompilationTest::new("tests/large_string.c", "large_string_edge");
    // Test with combination of all features
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
        ("AMICE_STRING_STACK_ALLOC", "true"),
    ]);
}