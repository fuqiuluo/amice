//! Integration tests for repeated string deduplication

mod common;

use common::{build_amice, detect_llvm_config};
use std::process::Command;

#[test]
fn test_repeated_strings_deduplication() {
    build_amice();

    let mut cmd = Command::new("clang");
    cmd.arg("-S")
        .arg("-emit-llvm")
        .arg("-fpass-plugin=target/release/libamice.so")
        .arg("tests/repeated_strings.c")
        .arg("-o")
        .arg("target/repeated_strings.ll");

    // Apply LLVM configuration if available
    if let Some((llvm_env_var, _)) = detect_llvm_config() {
        if let Ok(llvm_prefix) = std::env::var(&llvm_env_var) {
            cmd.env(&llvm_env_var, llvm_prefix);
        }
    }

    let output = cmd.output().expect("Failed to execute clang command");
    assert!(output.status.success(), "Clang command failed");

    let ll = std::fs::read_to_string("target/repeated_strings.ll")
        .expect("Failed to read LLVM IR file");

    // Verify that string decryption function is present
    let count = ll.matches("call void @decrypt_strings").count();
    assert!(
        count > 0,
        "Expected at least one call to @decrypt_strings, found: {count}"
    );
}

#[test]
fn test_repeated_strings_runtime_behavior() {
    use common::CompilationTest;
    
    let test = CompilationTest::new("tests/repeated_strings.c", "repeated_strings_runtime");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "lazy"),
    ]);
}

#[test]
fn test_repeated_strings_with_simd() {
    use common::CompilationTest;
    
    let test = CompilationTest::new("tests/repeated_strings.c", "repeated_strings_simd");
    test.assert_output_preserved_ignore_addresses(&[
        ("AMICE_STRING_ALGORITHM", "simd_xor"),
        ("AMICE_STRING_DECRYPT_TIMING", "global"),
    ]);
}
