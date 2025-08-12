//! Integration tests for indirect branch obfuscation

mod common;

use common::CompilationTest;

#[test]
fn test_indirect_branch_basic() {
    let test = CompilationTest::new("tests/indirect_branch.c", "indirect_branch_basic");
    test.assert_output_preserved(&[("AMICE_INDIRECT_BRANCH", "true")]);
}

#[test]
fn test_indirect_branch_with_string_obfuscation() {
    let test = CompilationTest::new("tests/indirect_branch.c", "indirect_branch_strings");
    test.assert_output_preserved(&[
        ("AMICE_INDIRECT_BRANCH", "true"),
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_indirect_branch_with_stack_allocation() {
    let test = CompilationTest::new("tests/indirect_branch.c", "indirect_branch_stack");
    test.assert_output_preserved(&[
        ("AMICE_INDIRECT_BRANCH", "true"),
        ("AMICE_STRING_STACK_ALLOC", "true"),
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}
