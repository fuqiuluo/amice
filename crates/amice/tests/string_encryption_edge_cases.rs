//! Boundary tests for StringEncryption struct-field and wide-character handling.
//!
//! Probing matrix (verified against hand-written IR run through the plugin,
//! see review of PR #94):
//!
//! PASSING on PR #94 head:
//! - struct with a single wide string field      -> encrypt + round-trip OK
//! - zeroinitializer wide array                  -> skipped untouched
//! - wide data array without trailing NUL        -> skipped, plaintext intact
//! - [N x i64] array ending in 0                 -> skipped (width filter)
//! - [2 x i16] minimal wide string               -> encrypt + round-trip OK
//!
//! FAILING on PR #94 head (merge blockers, also see issue #96):
//! - `test_struct_multi_string_fields_xor`: any struct global holding TWO OR
//!   MORE encryptable array fields gets all-but-the-last field double
//!   encrypted at runtime, because each field is rebuilt against the
//!   ORIGINAL initializer (stale `get_fields()` snapshot) while decryptors
//!   are emitted for every field. PR #94 extends this pre-existing bug to
//!   wide-character fields (wide: 2 -> aaa8).
//! - `test_rust_promoted_static_slices`: rustc relocation blobs ([8 x i8]
//!   fat-pointer lengths, [N x i8] metadata) inside a static struct are
//!   swept up as "strings" because LLVMIsConstantString accepts ANY
//!   [N x i8] array; key2.len() reads 12297829382473034426 (= 16 ^ 0xAA
//!   per byte).

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};
use serial_test::serial;

fn edge_config(timing: &str, algorithm: &str) -> ObfuscationConfig {
    ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some(algorithm.to_string()),
        string_decrypt_timing: Some(timing.to_string()),
        // Fixture globals are not named ".str", so the name filter must be off.
        string_only_llvm_string: Some(false),
        ..ObfuscationConfig::disabled()
    }
}

/// UTF-16LE bytes of the wide values embedded in wide_edge_cases.c.
fn utf16_le_bytes(values: &[u16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn check_edge_plaintext_absent(binary: &[u8]) {
    let k_bytes = utf16_le_bytes(&[
        0x4E62,
        0x4F60,
        b'A' as u16,
        b'B' as u16,
        b'C' as u16,
        b'D' as u16,
        b'E' as u16,
        0,
    ]);
    let m2_bytes = utf16_le_bytes(&[0x4F60, 0]);
    for (label, needle) in [("K", k_bytes), ("M2", m2_bytes)] {
        assert!(
            !binary.windows(needle.len()).any(|w| w == needle),
            "{label} wide literal remained in plaintext"
        );
    }
}

#[test]
fn test_wide_edge_cases_lazy_xor() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "wide_edge_cases.c", Language::C),
        "wide_edge_cases_lazy_xor",
    )
    .config(edge_config("lazy", "xor"))
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
    let output = run.stdout();
    assert!(output.contains("WIDE_EDGE_OK"), "unexpected output: {output}");
}

#[test]
fn test_wide_edge_cases_global_simd_xor() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "wide_edge_cases.c", Language::C),
        "wide_edge_cases_global_simd_xor",
    )
    .config(edge_config("global", "simd_xor"))
    .optimization("O2")
    .compile();

    result.assert_success();
    let binary = std::fs::read(&result.binary_path).expect("failed to read protected binary");
    check_edge_plaintext_absent(&binary);

    let run = result.run();
    run.assert_success();
    let output = run.stdout();
    assert!(output.contains("WIDE_EDGE_OK"), "unexpected output: {output}");
}

const MULTI_FIELD_EXPECTED: [&str; 3] = ["wide:   2 14 39", "narrow: 62 78 77", "mixed:  62 2 78"];

#[test]
fn test_struct_multi_string_fields_xor() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "wide_struct_fields.c", Language::C),
        "wide_struct_fields_xor",
    )
    .config(edge_config("lazy", "xor"))
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
    let lines = run.stdout_lines();
    assert_eq!(
        lines.len(),
        MULTI_FIELD_EXPECTED.len(),
        "unexpected output: {:?}",
        lines
    );
    for (got, expected) in lines.iter().zip(MULTI_FIELD_EXPECTED.iter()) {
        assert_eq!(got.trim(), *expected, "multi-field struct corrupted");
    }
}

#[test]
#[serial]
fn test_rust_promoted_static_slices() {
    ensure_plugin_built();

    let project_dir = common::rust_fixture_project_path("string_encryption_promoted");

    let mut config = ObfuscationConfig::default();
    config.string_encryption = Some(true);
    config.string_only_llvm_string = Some(false);
    config.string_algorithm = Some("xor".to_string());
    config.string_null_terminated = Some(false); // promoted slices are not null-terminated

    let result = common::RustCompileBuilder::new(&project_dir, "string_encryption_promoted_test")
        .config(config)
        // Emit at O0: the opt-fallback route compiles post-optimization IR, and
        // at opt-level 3 rustc constant-folds the .len() loads before amice
        // ever sees the relocation blobs, hiding the issue #96 shape. Debug IR
        // keeps the blob encoding faithful to the -Zllvm-plugins flow.
        .optimization("debug")
        .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
    let output = run.stdout();
    assert!(output.contains("key1.len() = 16"), "unexpected output: {output}");
    assert!(output.contains("key2.len() = 16"), "unexpected output: {output}");
    assert!(output.contains("key len = 16"), "unexpected output: {output}");
}
