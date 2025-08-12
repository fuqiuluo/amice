//! Integration tests for constant string obfuscation

mod common;

use common::CompilationTest;

#[test]
fn test_const_strings_lazy_xor_stack() {
    let test = CompilationTest::new("tests/const_strings.c", "const_strings_lazy_xor_stack");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_STACK_ALLOC", "true"),
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_const_strings_lazy_xor() {
    let test = CompilationTest::new("tests/const_strings.c", "const_strings_lazy_xor");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_const_strings_global_xor() {
    let test = CompilationTest::new("tests/const_strings.c", "const_strings_global_xor");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
    ]);
}

#[test]
fn test_const_strings_lazy_simd_xor() {
    let test = CompilationTest::new("tests/const_strings.c", "const_strings_lazy_simd_xor");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_const_strings_global_simd_xor() {
    let test = CompilationTest::new("tests/const_strings.c", "const_strings_global_simd_xor");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
    ]);
}

#[test]
fn test_const_strings_edge_cases() {
    let test = CompilationTest::new("tests/const_strings.c", "const_strings_edge_cases");
    // Test with empty strings and special characters
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
        ("AMICE_STRING_STACK_ALLOC", "false"),
    ]);
}

#[test]
fn test_const_strings_boundary_conditions() {
    let test = CompilationTest::new("tests/const_strings.c", "const_strings_boundary");
    // Test with different stack allocation settings
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
        ("AMICE_STRING_STACK_ALLOC", "false"),
    ]);
}
