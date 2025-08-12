//! Integration tests for complex switch statement obfuscation

mod common;

use common::CompilationTest;

#[test]
fn test_complex_switch_basic() {
    let test = CompilationTest::new("tests/complex_switch_test.c", "complex_switch_basic");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_complex_switch_with_lowering() {
    let test = CompilationTest::new("tests/complex_switch_test.c", "complex_switch_lower");
    // Test switch lowering transformation
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_LOWER_SWITCH", "true"),
        ("AMICE_STRING_ALGORITHM", "xor"),
    ]);
}

#[test]
fn test_complex_switch_with_shuffling() {
    let test = CompilationTest::new("tests/complex_switch_test.c", "complex_switch_shuffle");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "random"),
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
    ]);
}

#[test]
fn test_complex_switch_with_vm_flatten() {
    let test = CompilationTest::new("tests/complex_switch_test.c", "complex_switch_vm");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_VM_FLATTEN", "true"),
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
    ]);
}

#[test]
fn test_complex_switch_with_indirect_branch() {
    let test = CompilationTest::new("tests/complex_switch_test.c", "complex_switch_indirect");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_INDIRECT_BRANCH", "true"),
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_complex_switch_all_obfuscations() {
    let test = CompilationTest::new("tests/complex_switch_test.c", "complex_switch_all");
    // Test complex switch with all obfuscation techniques
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_LOWER_SWITCH", "true"),
        ("AMICE_VM_FLATTEN", "true"),
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "reverse"),
        ("AMICE_INDIRECT_BRANCH", "true"),
        ("AMICE_INDIRECT_CALL", "true"),
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
        ("AMICE_STRING_STACK_ALLOC", "false"),
    ]);
}

#[test]
fn test_complex_switch_boundary_conditions() {
    let test = CompilationTest::new("tests/complex_switch_test.c", "complex_switch_boundary");
    // Test boundary conditions with large switch statements
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_LOWER_SWITCH", "true"),
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
        ("AMICE_STRING_STACK_ALLOC", "true"),
    ]);
}

#[test]
fn test_complex_switch_edge_cases() {
    let test = CompilationTest::new("tests/complex_switch_test.c", "complex_switch_edge");
    // Test edge cases: fallthrough, nested switches, etc.
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "rotate,reverse"),
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
    ]);
}