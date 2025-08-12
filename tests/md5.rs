//! Integration tests for MD5 implementation with obfuscation

mod common;

use common::CompilationTest;

#[test]
fn test_md5_basic() {
    let test = CompilationTest::new("tests/md5.c", "md5_basic");
    test.assert_output_preserved(&[
        ("AMICE_INDIRECT_BRANCH", "true"),
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_md5_with_optimization() {
    let test = CompilationTest::new("tests/md5.c", "md5_optimized");
    // Test with O3 optimization
    test.assert_output_preserved(&[
        ("AMICE_INDIRECT_BRANCH", "true"),
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
    ]);
}

#[test]
fn test_md5_cpp() {
    let test = CompilationTest::new("tests/md5.cc", "md5_cpp");
    // Test C++ version
    test.assert_output_preserved(&[
        ("AMICE_INDIRECT_BRANCH", "true"),
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_md5_all_obfuscations() {
    let test = CompilationTest::new("tests/md5.c", "md5_all");
    test.assert_output_preserved(&[
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "random"),
        ("AMICE_INDIRECT_BRANCH", "true"),
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
        ("AMICE_STRING_STACK_ALLOC", "true"),
        ("AMICE_INDIRECT_CALL", "true"),
        ("AMICE_VM_FLATTEN", "true"),
    ]);
}

#[test]
fn test_md5_edge_cases() {
    let test = CompilationTest::new("tests/md5.c", "md5_edge");
    // Test with specific edge case configurations
    test.assert_output_preserved(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
        ("AMICE_STRING_STACK_ALLOC", "false"),
    ]);
}

#[test]
fn test_md5_boundary_conditions() {
    let test = CompilationTest::new("tests/md5.c", "md5_boundary");
    // Test boundary conditions with large inputs
    test.assert_output_preserved(&[
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "reverse,rotate"),
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}
