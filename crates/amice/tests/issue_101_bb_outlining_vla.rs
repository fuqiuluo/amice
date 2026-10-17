//! Regression tests for issue #101: BasicBlockOutlining miscompiled functions
//! containing a VLA. The block holding the `llvm.stackrestore` cleanup was
//! extracted into an outlined callee, where it restored the *callee's* stack
//! pointer to a value saved inside the caller's frame. The outlined function
//! then returned through a destroyed stack: usually a SIGSEGV right after the
//! last output line, occasionally a silently skipped function epilogue — both
//! depending on unrelated frame-layout luck (e.g. extra atomic-loop locals).
//!
//! The fix keeps blocks containing `llvm.stacksave`/`llvm.stackrestore` in the
//! original function. These tests pin both halves: the IR shape (the cleanup
//! call stays inside `@main` while other blocks are still outlined) and the
//! runtime behavior of the obfuscated executable.

mod common;

use crate::common::Language;
use common::{
    ObfuscationConfig, clang_compiler_path, ensure_plugin_built, fixture_path, output_dir, output_with_watchdog_ref,
    plugin_path,
};
use std::path::PathBuf;
use std::process::Command;

fn opt_path() -> PathBuf {
    common::detect_llvm_config()
        .map(|config| PathBuf::from(config.prefix).join("bin").join("opt"))
        .expect("LLVM prefix env var (e.g. LLVM_SYS_201_PREFIX) must point at the toolchain used to build the plugin")
}

fn run(command: &mut Command) {
    let output = output_with_watchdog_ref(command);
    assert!(
        output.status.success(),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Only BasicBlockOutlining runs; every other pass is explicitly disabled.
fn configure(command: &mut Command) {
    ObfuscationConfig::disabled().apply_to_command(command);
    command
        .env_remove("AMICE_CONFIG_PATH")
        .env("AMICE_BASIC_BLOCK_OUTLINING", "true");
}

/// Body of every `define internal` function in the module, used to prove the
/// VLA cleanup was not moved into an outlined callee.
fn internal_function_bodies(ir: &str) -> Vec<&str> {
    ir.split("define internal").skip(1).collect()
}

#[test]
fn vla_stackrestore_stays_in_original_function() {
    ensure_plugin_built();

    let dir = output_dir().join("issue-101-bb-outlining");
    std::fs::create_dir_all(&dir).unwrap();
    let fixture = fixture_path("basic_block_outlining", "issue_101_vla_stackrestore.c", Language::C);

    let input = dir.join("vla.input.ll");
    run(Command::new(clang_compiler_path(false))
        .env("CCACHE_DISABLE", "1")
        .args([
            "-O0",
            "-Xclang",
            "-disable-lifetime-markers",
            "-fno-discard-value-names",
        ])
        .args(["-S", "-emit-llvm"])
        .arg(&fixture)
        .arg("-o")
        .arg(&input));

    let transformed = dir.join("vla.outlining.ll");
    let mut opt = Command::new(opt_path());
    opt.env("CCACHE_DISABLE", "1")
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .args(["-passes=default<O0>", "-S"])
        .arg(&input)
        .arg("-o")
        .arg(&transformed);
    configure(&mut opt);
    run(&mut opt);

    let text = std::fs::read_to_string(&transformed).unwrap();
    let internals = internal_function_bodies(&text);
    assert!(
        internals.iter().any(|body| body.contains("@main.")),
        "expected BasicBlockOutlining to still outline some main blocks, got none"
    );
    assert!(
        internals
            .iter()
            .all(|body| !body.contains("llvm.stackrestore") && !body.contains("llvm.stacksave")),
        "llvm.stacksave/llvm.stackrestore leaked into an outlined function"
    );

    // End to end: the obfuscated program must run to completion with the exact
    // baseline output. Before the fix it died with SIGSEGV (rc=139) after the
    // last printf.
    let exe = dir.join("vla.outlining.exe");
    run(Command::new(clang_compiler_path(false))
        .env("CCACHE_DISABLE", "1")
        .arg(&transformed)
        .arg("-o")
        .arg(&exe));

    let mut run_exe = Command::new(&exe);
    let output = output_with_watchdog_ref(&mut run_exe);
    assert!(
        output.status.success(),
        "obfuscated executable failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Windows CRT emits CRLF, so compare line-wise.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, ["a1:45", "l1:49"]);
}
