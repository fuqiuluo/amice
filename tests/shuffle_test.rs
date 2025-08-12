//! Integration tests for shuffle blocks functionality

mod common;

use common::CompilationTest;

#[test]
fn test_shuffle_basic() {
    let test = CompilationTest::new("tests/shuffle_test.c", "shuffle_basic");
    test.assert_output_preserved(&[("AMICE_SHUFFLE_BLOCKS", "true")]);
}

#[test]
fn test_shuffle_random_mode() {
    let test = CompilationTest::new("tests/shuffle_test.c", "shuffle_random");
    test.assert_output_preserved(&[
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "random"),
    ]);
}

#[test]
fn test_shuffle_reverse_mode() {
    let test = CompilationTest::new("tests/shuffle_test.c", "shuffle_reverse");
    test.assert_output_preserved(&[
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "reverse"),
    ]);
}

#[test]
fn test_shuffle_rotate_mode() {
    let test = CompilationTest::new("tests/shuffle_test.c", "shuffle_rotate");
    test.assert_output_preserved(&[
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "rotate"),
    ]);
}

#[test]
fn test_shuffle_combined_modes() {
    let test = CompilationTest::new("tests/shuffle_test.c", "shuffle_combined");
    test.assert_output_preserved(&[
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "reverse,rotate"),
    ]);
}

#[test]
fn test_shuffle_with_string_obfuscation() {
    let test = CompilationTest::new("tests/shuffle_test.c", "shuffle_with_strings");
    test.assert_output_preserved(&[
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "random"),
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_shuffle_edge_cases() {
    let test = CompilationTest::new("tests/shuffle_test.c", "shuffle_edge");
    // Test with multiple modes including invalid ones (should gracefully handle)
    test.assert_output_preserved(&[
        ("AMICE_SHUFFLE_BLOCKS", "true"),
        ("AMICE_SHUFFLE_BLOCKS_FLAGS", "reverse,rotate,random"),
    ]);
}