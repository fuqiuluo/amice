//! Regression tests for issue #99: string encryption under MSVC WinEH funclets.
//!
//! Covered failure modes (all crash or miscompile the funclet region):
//! - inkwell panics on token/label/metadata values (`get_user`/`get_operand`)
//! - `Comdat::new(null)` UB while clearing linkonce_odr comdats
//! - bare decrypt calls inserted inside funclet regions: MSVC unwinder state
//!   maps require every call inside a catchpad/cleanuppad funclet to carry a
//!   `"funclet"(token)` operand bundle, so an un-bundled lazy decrypt call
//!   crashes the process on the first throw
//!
//! The IR-level test runs on every host: clang can emit Windows-MSVC EH IR
//! (catchpad/catchswitch) from a header-free fixture regardless of the host
//! platform. The executable test only runs on Windows where the MSVC CRT can
//! link and the binary can execute.

mod common;

use crate::common::Language;
use common::{ObfuscationConfig, ensure_plugin_built, fixture_path, output_dir, plugin_path};
use std::path::PathBuf;
use std::process::Command;

fn tool(name: &str) -> PathBuf {
    let name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    common::detect_llvm_config()
        .map(|config| PathBuf::from(config.prefix).join("bin").join(&name))
        .unwrap_or_else(|| name.into())
}

fn run(command: &mut Command) -> String {
    let output = common::output_with_watchdog_ref(command);
    assert!(
        output.status.success(),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap_or_default()
}

/// String encryption with struct-string collection enabled (`ONLY_DOT_STRING=false`
/// is the issue #99 trigger; its default is `true`).
fn configure_strenc(command: &mut Command) {
    ObfuscationConfig::disabled().apply_to_command(command);
    command
        .env_remove("AMICE_CONFIG_PATH")
        .env("AMICE_STRING_ENCRYPTION", "true")
        .env("AMICE_STRING_ONLY_DOT_STRING", "false");
}

/// The plaintext constant form `c"SECRET_IN_HANDLER_99` must be gone: the
/// MSVC-mangled symbol name keeps the text, so only the constant data proves
/// encryption. The decrypt infrastructure proves the string was collected
/// instead of skipped.
fn assert_string_encrypted(ir: &str) {
    assert!(
        !ir.contains("c\"SECRET_IN_HANDLER_99"),
        "SECRET_IN_HANDLER_99 was not encrypted"
    );
    assert!(
        ir.contains("__amice__decrypt"),
        "no decrypt infrastructure emitted for SECRET_IN_HANDLER_99"
    );
}

/// No decrypt call may appear inside an EH pad block without a `"funclet"`
/// operand bundle: such a bare call breaks the MSVC unwinder state mapping and
/// crashes on the first throw. Text-level scan over the module: block labels
/// start at column zero inside function bodies; instructions are indented.
fn assert_no_bare_decrypt_in_pad_blocks(ir: &str) {
    let mut in_function = false;
    let mut block_has_eh_pad = false;
    for line in ir.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("define ") {
            in_function = true;
            block_has_eh_pad = false;
            continue;
        }
        if !in_function {
            continue;
        }
        if line.starts_with('}') {
            in_function = false;
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') && trimmed.ends_with(':') {
            block_has_eh_pad = false;
            continue;
        }
        if trimmed.contains("catchpad") || trimmed.contains("cleanuppad") {
            block_has_eh_pad = true;
        }
        if block_has_eh_pad && trimmed.contains("@__amice__decrypt") && !trimmed.contains("\"funclet\"") {
            panic!("bare decrypt call inside an EH pad block:\n{trimmed}");
        }
    }
}

fn emit_msvc_ir(dir: &std::path::Path, fixture: &std::path::Path, name: &str) -> PathBuf {
    let ir = dir.join(format!("{name}.ll"));
    // opt 必须来自 LLVM 前缀（插件宿主）；clang++ 前缀缺失时回退 PATH
    run(Command::new(common::clang_compiler_path(true))
        .arg("--target=x86_64-pc-windows-msvc")
        .args(["-O0", "-fno-discard-value-names", "-S", "-emit-llvm"])
        .arg(fixture)
        .arg("-o")
        .arg(&ir));
    ir
}

fn run_strenc_opt(input: &std::path::Path, output: &std::path::Path) {
    let mut command = Command::new(tool("opt"));
    configure_strenc(&mut command);
    run(command
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .args(["-passes=default<O0>", "-verify-each", "-S"])
        .arg(input)
        .arg("-o")
        .arg(output));
}

/// Issue #99: a string stored inside a catch handler must stay encrypted while
/// the funclet region remains free of un-bundled decrypt calls. Runs on every
/// host: IR generation is cross-target and the backend acceptance check only
/// compiles to an object file.
#[test]
fn win_funclet_store_encrypted_and_funclets_clean() {
    ensure_plugin_built();

    let dir = output_dir().join("issue-99-strenc-funclet");
    std::fs::create_dir_all(&dir).unwrap();
    let fixture = fixture_path("string_encryption", "strenc_win_funclet_store.cpp", Language::Cpp);

    let original = emit_msvc_ir(&dir, &fixture, "funclet-store.input");
    let transformed = dir.join("funclet-store.strenc.ll");
    run_strenc_opt(&original, &transformed);

    let text = std::fs::read_to_string(&transformed).unwrap();
    assert!(text.contains("catchpad"), "fixture lost its WinEH funclet");
    assert_no_bare_decrypt_in_pad_blocks(&text);
    assert_string_encrypted(&text);

    // Backend acceptance: compiling the transformed IR to a Windows object needs
    // no platform headers or linker.
    run(Command::new(common::clang_compiler_path(true))
        .arg("--target=x86_64-pc-windows-msvc")
        .args(["-O0", "-c"])
        .arg(&transformed)
        .arg("-o")
        .arg(dir.join("funclet-store.obj")));
}

/// Issue #99 end to end on Windows: the obfuscated executable must survive the
/// throw, enter the handler and print the decrypted message.
#[cfg(target_os = "windows")]
#[test]
fn win_funclet_store_executable_runs() {
    if !Command::new("clang++")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("Skipping: clang++ not found on PATH for the executable round trip");
        return;
    }
    ensure_plugin_built();

    let dir = output_dir().join("issue-99-strenc-funclet");
    std::fs::create_dir_all(&dir).unwrap();
    // CRT variant: the bare `printf` symbol only exists in some MSVC CRTs
    // (absent from aarch64 libcmt), while <cstdio> links via the inline
    // wrapper everywhere.
    let fixture = fixture_path("string_encryption", "strenc_win_funclet_store_crt.cpp", Language::Cpp);

    // Native target: on Windows hosts clang++ defaults to the host MSVC triple,
    // so the object can link against the MSVC CRT and the exe can run.
    let original = dir.join("funclet-store.exe.input.ll");
    run(Command::new("clang++")
        .args(["-O0", "-fno-discard-value-names", "-S", "-emit-llvm"])
        .arg(&fixture)
        .arg("-o")
        .arg(&original));
    let transformed = dir.join("funclet-store.exe.strenc.ll");
    run_strenc_opt(&original, &transformed);

    let text = std::fs::read_to_string(&transformed).unwrap();
    assert_no_bare_decrypt_in_pad_blocks(&text);
    assert_string_encrypted(&text);

    let executable = dir.join("funclet-store.exe");
    let mut link = Command::new("clang++");
    link.arg("-O0").arg(&transformed).arg("-o").arg(&executable);
    let output = common::output_with_watchdog_ref(&mut link);
    assert!(
        output.status.success(),
        "link failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut run_exe = Command::new(&executable);
    let output = common::output_with_watchdog_ref(&mut run_exe);
    assert!(
        output.status.success(),
        "obfuscated executable crashed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim_end(),
        "SECRET_IN_HANDLER_99",
        "obfuscated executable printed wrong output"
    );
}
