//! Integration tests for indirect call obfuscation

mod common;

use common::CompilationTest;

#[test]
fn test_indirect_call_basic() {
    let test = CompilationTest::new("tests/indirect_call.c", "indirect_call_basic");
    test.assert_output_preserved_ignore_addresses(&[("AMICE_INDIRECT_CALL", "true")]);
}

#[test]
fn test_indirect_call_with_string_obfuscation() {
    let test = CompilationTest::new("tests/indirect_call.c", "indirect_call_strings");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_INDIRECT_CALL", "true"),
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_indirect_call_edge_case_invalid_id() {
    let test = CompilationTest::new("tests/indirect_call.c", "indirect_call_edge");
    // Test that invalid function IDs are handled correctly
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_INDIRECT_CALL", "true"),
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
    ]);
}

#[test]
fn test_indirect_call_boundary_conditions() {
    let test = CompilationTest::new("tests/indirect_call.c", "indirect_call_boundary");
    // Test with different configurations
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_INDIRECT_CALL", "true"),
        ("AMICE_STRING_STACK_ALLOC", "true"),
    ]);
}