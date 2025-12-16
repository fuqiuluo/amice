//! Test case for issue #61: C++ fmt library string encryption failure
//!
//! This test tracks a known failure when obfuscating C++ code that uses the fmt library
//! with string encryption enabled.
//!
//! Issue: https://github.com/fuqiuluo/amice/issues/61
//!
//! The test is marked as #[ignore] until the issue is resolved.

mod common;

use common::{CompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn string_config_lazy_xor() -> ObfuscationConfig {
    ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some("xor".to_string()),
        string_decrypt_timing: Some("lazy".to_string()),
        ..ObfuscationConfig::disabled()
    }
}

/// Test case for issue #61: fmt library string encryption
///
/// This test attempts to compile a C++ program using the fmt library
/// with string encryption enabled. This is currently known to fail.
///
/// To run this test after fixing the issue:
/// ```bash
/// cargo test --release --no-default-features --features llvm18-1 test_issue_61_cpp_fmt_strenc -- --ignored
/// ```
// #[test]
#[ignore = "Known failure - issue #61: C++ fmt library string encryption"]
fn test_issue_61_cpp_fmt_strenc() {
    use common::project_root;

    ensure_plugin_built();

    let fmt_include = project_root().join("tests/fixtures/issues/issue_61_cpp_fmt_strenc/fmt/include");

    let result = CompileBuilder::new(
        fixture_path("issues/issue_61_cpp_fmt_strenc", "main.cpp"),
        "issue_61_cpp_fmt",
    )
    .config(string_config_lazy_xor())
    .optimization("O2")
    .std("c++17")
    .arg(&format!("-I{}", fmt_include.display()))
    .arg("-DFMT_HEADER_ONLY")
    .compile();

    // When the issue is fixed, this should succeed
    result.assert_success();

    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    assert!(!lines.is_empty(), "Expected output from fmt::format");
    // The expected output should be "hello" formatted by fmt library
    assert!(lines[0].contains("hello"), "Expected 'hello' in output");
}

/// Helper test to verify the fixture compiles without obfuscation
///
/// This test ensures the C++ code itself is valid and can compile
/// without the amice plugin.
// #[test]
fn test_issue_61_cpp_fmt_baseline() {
    use common::project_root;

    let fmt_include = project_root().join("tests/fixtures/issues/issue_61_cpp_fmt_strenc/fmt/include");

    let result = CompileBuilder::new(
        fixture_path("issues/issue_61_cpp_fmt_strenc", "main.cpp"),
        "issue_61_cpp_fmt_baseline",
    )
    .without_plugin()
    .optimization("O2")
    .std("c++17")
    .arg(&format!("-I{}", fmt_include.display()))
    .arg("-DFMT_HEADER_ONLY")
    .compile();

    result.assert_success();

    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    assert!(!lines.is_empty(), "Expected output from fmt::format");
    assert!(lines[0].contains("hello"), "Expected 'hello' in output");
}
