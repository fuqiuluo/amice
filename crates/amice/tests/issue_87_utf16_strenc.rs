//! Regression test for issue #87: UTF-16 literals bypass string encryption.
//!
//! Issue: https://github.com/fuqiuluo/amice/issues/87

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

const UTF16_SECRET: &str = "AMICE_ISSUE_87_宽字符";

fn string_config_lazy_xor() -> ObfuscationConfig {
    ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some("xor".to_string()),
        string_decrypt_timing: Some("lazy".to_string()),
        ..ObfuscationConfig::disabled()
    }
}

fn utf16_native_bytes(value: &str) -> Vec<u8> {
    value.encode_utf16().flat_map(u16::to_ne_bytes).collect()
}

fn utf16_hash(value: &str) -> u32 {
    value.encode_utf16().fold(2166136261, |hash, code_unit| {
        (hash ^ u32::from(code_unit)).wrapping_mul(16777619)
    })
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[test]
#[ignore = "Known failure: issue #87 leaves UTF-16 literals unencrypted"]
fn test_issue_87_utf16_literal_is_encrypted() {
    ensure_plugin_built();

    let source = fixture_path("issues/issue_87_utf16_strenc", "main.cpp", Language::Cpp);
    let plaintext = utf16_native_bytes(UTF16_SECRET);

    let baseline = CppCompileBuilder::new(&source, "issue_87_utf16_baseline")
        .without_plugin()
        .std("c++17")
        .compile();
    baseline.assert_success();

    let baseline_binary = std::fs::read(&baseline.binary_path).expect("failed to read baseline binary");
    assert!(
        contains_subslice(&baseline_binary, &plaintext),
        "baseline binary does not contain the UTF-16 marker; the fixture cannot reproduce issue #87"
    );

    let obfuscated = CppCompileBuilder::new(source, "issue_87_utf16_obfuscated")
        .config(string_config_lazy_xor())
        .std("c++17")
        .compile();
    obfuscated.assert_success();

    let run = obfuscated.run();
    run.assert_success();
    assert_eq!(run.stdout().trim(), utf16_hash(UTF16_SECRET).to_string());

    let obfuscated_binary = std::fs::read(&obfuscated.binary_path).expect("failed to read obfuscated binary");
    assert!(
        !contains_subslice(&obfuscated_binary, &plaintext),
        "UTF-16 plaintext remains in the obfuscated binary"
    );
}
