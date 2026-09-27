//! Integration tests for Mixed Boolean Arithmetic (MBA) obfuscation.

mod common;

use crate::common::Language;
use common::{
    CppCompileBuilder, ObfuscationConfig, detect_llvm_config, ensure_plugin_built, fixture_path, output_dir,
    plugin_path,
};
use std::path::PathBuf;
use std::process::Command;

fn mba_config() -> ObfuscationConfig {
    ObfuscationConfig {
        mba: Some(true),
        ..ObfuscationConfig::disabled()
    }
}

fn mba_regions_config(float_regions: bool) -> ObfuscationConfig {
    ObfuscationConfig {
        mba: Some(true),
        mba_float_regions: Some(float_regions),
        ..ObfuscationConfig::disabled()
    }
}

/// Get expected output from non-obfuscated baseline
fn get_baseline_output(test_name: &str) -> String {
    let result = CppCompileBuilder::new(
        fixture_path("mba", "mba_constants_demo.c", Language::C),
        &format!("mba_baseline_{}", test_name),
    )
    .without_plugin()
    .compile();

    result.assert_success();
    result.run().stdout()
}

#[test]
fn test_mba_basic() {
    ensure_plugin_built();

    let baseline = get_baseline_output("basic");

    let result = CppCompileBuilder::new(fixture_path("mba", "mba_constants_demo.c", Language::C), "mba_basic")
        .config(mba_config())
        .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    // MBA should not change program output
    assert_eq!(run.stdout(), baseline, "MBA obfuscation changed program output");
}

#[test]
fn test_mba_optimized() {
    ensure_plugin_built();

    let baseline = get_baseline_output("optimized");

    let result = CppCompileBuilder::new(fixture_path("mba", "mba_constants_demo.c", Language::C), "mba_o2")
        .config(mba_config())
        .optimization("O2")
        .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    assert_eq!(run.stdout(), baseline, "MBA with O2 changed program output");
}

#[test]
fn test_mba_with_bcf() {
    ensure_plugin_built();

    let baseline = get_baseline_output("with_bcf");

    let config = ObfuscationConfig {
        mba: Some(true),
        bogus_control_flow: Some(true),
        ..ObfuscationConfig::disabled()
    };

    let result = CppCompileBuilder::new(fixture_path("mba", "mba_constants_demo.c", Language::C), "mba_with_bcf")
        .config(config)
        .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();

    assert_eq!(run.stdout(), baseline, "MBA with BCF changed program output");
}

#[test]
fn test_mba_i128() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(fixture_path("mba", "mba_i128_aux.c", Language::C), "mba_i128")
        .config(mba_regions_config(true))
        .optimization("O1")
        .compile();

    result.assert_success();
    result.run().assert_success();
}

#[test]
fn test_mba_i128_integer_only() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("mba", "mba_i128_aux.c", Language::C),
        "mba_i128_integer_only",
    )
    .config(mba_regions_config(false))
    .optimization("O1")
    .compile();

    result.assert_success();
    result.run().assert_success();
}

fn assert_mba_i1(float_regions: bool) {
    ensure_plugin_built();
    let opt_name = format!("opt{}", std::env::consts::EXE_SUFFIX);
    let opt = detect_llvm_config()
        .map(|config| PathBuf::from(config.prefix).join("bin").join(&opt_name))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from(opt_name));
    let name = format!("mba_i1_float_{float_regions}");
    std::fs::create_dir_all(output_dir()).unwrap();
    let output_ir = output_dir().join(format!("{name}.ll"));

    let mut cmd = Command::new(opt);
    let mut config = mba_regions_config(float_regions);
    // Exercise function-scoped +mba with global MBA disabled, as in issue #89.
    config.mba = Some(false);
    config.apply_to_command(&mut cmd);
    let output = cmd
        .env_remove("AMICE_CONFIG_PATH")
        .env("AMICE_PASS_ORDER", "Mba")
        .env("RUST_LOG", "warn")
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .args(["-passes=default<O0>", "-verify-each", "-S"])
        .arg(fixture_path("mba", "mba_i1.ll", Language::C))
        .arg("-o")
        .arg(&output_ir)
        .output()
        .expect("failed to execute opt for i1 MBA");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "opt failed: {stderr}");
    assert!(stderr.trim().is_empty(), "unexpected MBA diagnostics: {stderr}");

    let ir = std::fs::read_to_string(&output_ir).unwrap();
    // The fixture names every candidate %value. Merely suppressing the warning
    // or skipping i1 must not satisfy this regression test.
    assert!(!ir.contains("%value ="), "original MBA candidates survived:\n{ir}");
    for function in ["boolean_gate", "boolean_xor", "boolean_add", "boolean_sub"] {
        let body = ir
            .split_once(&format!("@{function}("))
            .unwrap()
            .1
            .split_once('}')
            .unwrap()
            .0;
        assert!(body.contains("zext i1"), "{function} did not widen its operands");
        assert!(body.contains("trunc i8"), "{function} did not restore its i1 result");
    }
    let main_body = ir.split_once("@main(").unwrap().1.split_once('}').unwrap().0;
    assert!(
        main_body.contains("%expected_or = or i1"),
        "unannotated main was rewritten"
    );

    // Execute all four truth-table rows at O0 and after normal optimization.
    // The plugin has already run; compiling here must not apply it a second time.
    for optimization in ["O0", "O2"] {
        let result = CppCompileBuilder::new(&output_ir, &format!("{name}_{optimization}"))
            .without_plugin()
            .optimization(optimization)
            .compile();
        result.assert_success();
        result.run().assert_success();
    }
}

#[test]
fn test_mba_i1_function_annotation() {
    assert_mba_i1(true);
}

#[test]
fn test_mba_i1_integer_only() {
    assert_mba_i1(false);
}
