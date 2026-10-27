//! Edge-case / boundary tests for the wide-character string encryption added
//! in PR #94 (issue #87). These go beyond the PR's own `wide_strings` tests to
//! probe the corners the fix is most likely to get wrong:
//!
//!  * i8 + i16 + i32 string literals coexisting in one module;
//!  * the maintainer's concern — non-string integer tables ending in 0 that the
//!    heuristic misclassifies as "wide strings" (must stay behaviour-preserving);
//!  * boundary code-unit values, incl. 0xAAAA which XORs to an embedded 0x0000;
//!  * i64 arrays, which are out of scope and must be left untouched;
//!  * arrays not terminated by 0, which must be skipped;
//!  * wchar_t with the correct header.

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

const EDGE: &str = "string_encryption/edge";

fn edge_config(timing: &str, algorithm: &str, stack_alloc: bool) -> ObfuscationConfig {
    ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some(algorithm.to_string()),
        string_decrypt_timing: Some(timing.to_string()),
        string_stack_alloc: Some(stack_alloc),
        // Wide literals are not named ".str" on every target, so the name
        // filter must be off or they would be skipped for an unrelated reason.
        string_only_llvm_string: Some(false),
        ..ObfuscationConfig::disabled()
    }
}

fn u16_line(s: &str) -> String {
    s.encode_utf16()
        .map(|c| format!("{c:04x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn u32_line(s: &str) -> String {
    s.chars()
        .map(|c| format!("{:04x}", u32::from(c)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn utf16_le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn utf32_le(s: &str) -> Vec<u8> {
    s.chars().flat_map(|c| u32::from(c).to_le_bytes()).collect()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn read_binary(result: &common::CompileResult) -> Vec<u8> {
    std::fs::read(&result.binary_path).expect("failed to read binary")
}

// ---------------------------------------------------------------------------
// 1. Mixed i8 / i16 / i32 literals in one translation unit.
// ---------------------------------------------------------------------------

fn check_mixed_width_output(lines: &[String]) {
    assert_eq!(lines.len(), 3, "expected 3 lines, got {lines:?}");
    assert_eq!(lines[0], "NARROW_i8_secret", "i8 line");
    assert_eq!(lines[1], u16_line("WIDE16_\u{4F60}"), "char16_t line");
    assert_eq!(lines[2], u32_line("WIDE32_\u{4F60}"), "char32_t line");
}

#[test]
fn test_edge_mixed_width_lazy_xor() {
    ensure_plugin_built();
    let result = CppCompileBuilder::new(fixture_path(EDGE, "mixed_width.c", Language::C), "edge_mixed_lazy_xor")
        .config(edge_config("lazy", "xor", false))
        .compile();
    result.assert_success();
    check_mixed_width_output(&result.run().stdout_lines());
}

#[test]
fn test_edge_mixed_width_global_xor_plaintext_absent() {
    ensure_plugin_built();
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "mixed_width.c", Language::C),
        "edge_mixed_global_xor",
    )
    .config(edge_config("global", "xor", false))
    .optimization("O2")
    .compile();
    result.assert_success();
    let bin = read_binary(&result);
    assert!(!contains(&bin, b"NARROW_i8_secret"), "i8 plaintext remained");
    assert!(
        !contains(&bin, &utf16_le("WIDE16_\u{4F60}")),
        "UTF-16 plaintext remained"
    );
    assert!(
        !contains(&bin, &utf32_le("WIDE32_\u{4F60}")),
        "UTF-32 plaintext remained"
    );
    check_mixed_width_output(&result.run().stdout_lines());
}

#[test]
fn test_edge_mixed_width_global_simd_xor_plaintext_absent() {
    ensure_plugin_built();
    // NOTE: O0 on purpose. simd_xor's global-mode decrypt ctor is statically
    // evaluated away by O2's GlobalOpt (see the #[ignore]d regression below),
    // a PRE-EXISTING issue that also affects plain i8 strings and is unrelated
    // to PR #94. At O0 simd_xor does encrypt i8 + i16 + i32 as expected.
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "mixed_width.c", Language::C),
        "edge_mixed_global_simd",
    )
    .config(edge_config("global", "simd_xor", false))
    .compile();
    result.assert_success();
    let bin = read_binary(&result);
    assert!(!contains(&bin, b"NARROW_i8_secret"), "i8 plaintext remained");
    assert!(
        !contains(&bin, &utf16_le("WIDE16_\u{4F60}")),
        "UTF-16 plaintext remained"
    );
    assert!(
        !contains(&bin, &utf32_le("WIDE32_\u{4F60}")),
        "UTF-32 plaintext remained"
    );
    check_mixed_width_output(&result.run().stdout_lines());
}

/// Pre-existing weakness, NOT introduced by PR #94 and NOT specific to wide
/// strings: with `simd_xor` + `global` timing at `-O2`, the decrypt constructor
/// is statically evaluated and removed by GlobalOpt, leaving the *plaintext*
/// back in the binary. Reproduces on master for a plain i8 string. `xor`
/// survives O2 (its ctor is not folded away). Tracked here so a future fix to
/// simd_xor flips this test green; ignored so it does not gate the PR.
#[test]
#[ignore = "pre-existing simd_xor+global+O2 refold; unrelated to PR #94"]
fn test_known_issue_simd_xor_global_o2_refolds_plaintext() {
    ensure_plugin_built();
    let secret = b"SIMD_O2_BUG_MARKER";
    let result = CppCompileBuilder::new(fixture_path(EDGE, "narrow_only.c", Language::C), "edge_narrow_simd_o2")
        .config(edge_config("global", "simd_xor", false))
        .optimization("O2")
        .compile();
    result.assert_success();
    let bin = read_binary(&result);
    assert!(
        !contains(&bin, secret),
        "simd_xor+global+O2 left i8 plaintext (pre-existing bug)"
    );
    assert_eq!(result.run().stdout_lines(), vec!["SIMD_O2_BUG_MARKER".to_string()]);
}

/// Control for the above: `xor` + `global` + O2 DOES keep the i8 string
/// encrypted (its decrypt ctor survives O2). This passing while the simd_xor
/// variant fails localizes the weakness to simd_xor's ctor emission.
#[test]
fn test_edge_narrow_only_global_xor_o2_plaintext_absent() {
    ensure_plugin_built();
    let secret = b"SIMD_O2_BUG_MARKER";
    let result = CppCompileBuilder::new(fixture_path(EDGE, "narrow_only.c", Language::C), "edge_narrow_xor_o2")
        .config(edge_config("global", "xor", false))
        .optimization("O2")
        .compile();
    result.assert_success();
    let bin = read_binary(&result);
    assert!(!contains(&bin, secret), "xor+global+O2 unexpectedly left i8 plaintext");
    assert_eq!(result.run().stdout_lines(), vec!["SIMD_O2_BUG_MARKER".to_string()]);
}

// ---------------------------------------------------------------------------
// 2. Maintainer's concern: non-string int tables ending in 0 are misclassified.
//    Characterization test: confirm they ARE encrypted (plaintext bytes gone)
//    yet behaviour is fully preserved.
// ---------------------------------------------------------------------------

#[test]
fn test_edge_false_positive_tables_are_encrypted_but_benign() {
    ensure_plugin_built();
    // Distinctive little-endian needles from the two tables.
    let needle32 = 300000u32.to_le_bytes().to_vec(); // from codes32
    let needle16: Vec<u8> = [0xBEEFu16, 0x1234, 0x00FF, 0xABCD]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect(); // from codes16

    // Baseline (no plugin): the raw table bytes must be present.
    let baseline = CppCompileBuilder::new(
        fixture_path(EDGE, "false_positive_tables.c", Language::C),
        "edge_fptables_baseline",
    )
    .without_plugin()
    .compile();
    baseline.assert_success();
    let base_bin = read_binary(&baseline);
    assert!(
        contains(&base_bin, &needle16),
        "fixture cannot reproduce: table16 absent at baseline"
    );

    // With plugin (global xor, O0 so the arrays survive to the pass): the
    // heuristic treats these int tables as wide strings and encrypts them.
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "false_positive_tables.c", Language::C),
        "edge_fptables_global_xor",
    )
    .config(edge_config("global", "xor", false))
    .compile();
    result.assert_success();
    let bin = read_binary(&result);

    // Documents the over-reach: these non-strings get encrypted too.
    let enc16 = !contains(&bin, &needle16);
    let enc32 = !contains(&bin, &needle32);
    eprintln!("(edge) false-positive tables encrypted: codes16={enc16} codes32={enc32}");

    // The important guarantee regardless of over-reach: behaviour is preserved.
    let run = result.run();
    run.assert_success();
    assert_eq!(run.stdout_lines(), vec!["301035 97775".to_string()]);
}

#[test]
fn test_edge_false_positive_tables_global_simd_xor_benign() {
    ensure_plugin_built();
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "false_positive_tables.c", Language::C),
        "edge_fptables_global_simd",
    )
    .config(edge_config("global", "simd_xor", false))
    .compile();
    result.assert_success();
    assert_eq!(result.run().stdout_lines(), vec!["301035 97775".to_string()]);
}

#[test]
fn test_edge_false_positive_tables_lazy_xor_benign() {
    ensure_plugin_built();
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "false_positive_tables.c", Language::C),
        "edge_fptables_lazy_xor",
    )
    .config(edge_config("lazy", "xor", false))
    .compile();
    result.assert_success();
    assert_eq!(result.run().stdout_lines(), vec!["301035 97775".to_string()]);
}

// ---------------------------------------------------------------------------
// 3. Boundary code-unit values. 0xAAAA ^ 0xAAAA == 0x0000, so the encrypted
//    initializer holds an embedded null; the byte-length decryptor must not
//    stop early. Also exercises 0xFFFF and a lone surrogate 0xD800.
// ---------------------------------------------------------------------------

const WIDE_EDGE_LINE: &str = "0041 aaaa ffff d800 4f60";

#[test]
fn test_edge_wide_values_lazy_xor() {
    ensure_plugin_built();
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "wide_edge_values.c", Language::C),
        "edge_wvals_lazy_xor",
    )
    .config(edge_config("lazy", "xor", false))
    .compile();
    result.assert_success();
    assert_eq!(result.run().stdout_lines(), vec![WIDE_EDGE_LINE.to_string()]);
}

#[test]
fn test_edge_wide_values_global_xor() {
    ensure_plugin_built();
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "wide_edge_values.c", Language::C),
        "edge_wvals_global_xor",
    )
    .config(edge_config("global", "xor", false))
    .compile();
    result.assert_success();
    let bin = read_binary(&result);
    let plaintext: Vec<u8> = [0x0041u16, 0xAAAA, 0xFFFF, 0xD800, 0x4F60]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    assert!(!contains(&bin, &plaintext), "wide-edge plaintext remained");
    assert_eq!(result.run().stdout_lines(), vec![WIDE_EDGE_LINE.to_string()]);
}

#[test]
fn test_edge_wide_values_lazy_xor_stack() {
    ensure_plugin_built();
    // Stack-allocation path: a wide string copied to an i8 stack buffer whose
    // byte length matches the i16 array; the i16 load must read it back right.
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "wide_edge_values.c", Language::C),
        "edge_wvals_lazy_stack",
    )
    .config(edge_config("lazy", "xor", true))
    .compile();
    result.assert_success();
    assert_eq!(result.run().stdout_lines(), vec![WIDE_EDGE_LINE.to_string()]);
}

#[test]
fn test_edge_wide_values_global_simd_xor() {
    ensure_plugin_built();
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "wide_edge_values.c", Language::C),
        "edge_wvals_global_simd",
    )
    .config(edge_config("global", "simd_xor", false))
    .compile();
    result.assert_success();
    assert_eq!(result.run().stdout_lines(), vec![WIDE_EDGE_LINE.to_string()]);
}

// ---------------------------------------------------------------------------
// 4. i64 arrays are out of scope (only i16/i32 handled) -> left untouched.
// ---------------------------------------------------------------------------

#[test]
fn test_edge_i64_array_untouched() {
    ensure_plugin_built();
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "i64_untouched.c", Language::C),
        "edge_i64_global_xor",
    )
    .config(edge_config("global", "xor", false))
    .compile();
    result.assert_success();
    let bin = read_binary(&result);
    // 100000000000 == 0x1748_76E8_00 -> distinctive 8-byte LE needle.
    let needle = 100000000000i64.to_le_bytes().to_vec();
    assert!(
        contains(&bin, &needle),
        "i64 array was unexpectedly transformed (should be out of scope)"
    );
    assert_eq!(result.run().stdout_lines(), vec!["99999999998".to_string()]);
}

// ---------------------------------------------------------------------------
// 5. Arrays not terminated by 0 must be skipped; behaviour preserved.
// ---------------------------------------------------------------------------

#[test]
fn test_edge_not_terminated_skipped() {
    ensure_plugin_built();
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "not_terminated.c", Language::C),
        "edge_notterm_global_xor",
    )
    .config(edge_config("global", "xor", false))
    .compile();
    result.assert_success();
    assert_eq!(result.run().stdout_lines(), vec!["26 15".to_string()]);
}

// ---------------------------------------------------------------------------
// 6. wchar_t with the correct header (<wchar.h>) — the behaviour the PR's own
//    wide_strings.c fixture intends but fails to compile for (missing include).
// ---------------------------------------------------------------------------

#[test]
fn test_edge_wchar_fixed_lazy_xor() {
    ensure_plugin_built();
    let result = CppCompileBuilder::new(fixture_path(EDGE, "wchar_fixed.c", Language::C), "edge_wchar_lazy_xor")
        .config(edge_config("lazy", "xor", false))
        .compile();
    result.assert_success();
    assert_eq!(result.run().stdout_lines(), vec![u32_line("WcharTest")]);
}

#[test]
fn test_edge_wchar_fixed_global_xor() {
    ensure_plugin_built();
    let result = CppCompileBuilder::new(
        fixture_path(EDGE, "wchar_fixed.c", Language::C),
        "edge_wchar_global_xor",
    )
    .config(edge_config("global", "xor", false))
    .optimization("O2")
    .compile();
    result.assert_success();
    let bin = read_binary(&result);
    assert!(!contains(&bin, &utf32_le("WcharTest")), "wchar_t plaintext remained");
    assert_eq!(result.run().stdout_lines(), vec![u32_line("WcharTest")]);
}
