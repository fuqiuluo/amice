// These fixtures exercise Clang's Itanium template parameter objects
#![cfg(not(target_env = "msvc"))]

mod common;

use common::{CppCompileBuilder, Language, ObfuscationConfig, ensure_plugin_built, fixture_path};
use std::path::Path;

fn assert_template_text_absent(path: &Path) {
    let bytes = std::fs::read(path).expect("read protected output");
    for text in [
        b"AMICE_SHARED_TEMPLATE_MESSAGE {}".as_slice(),
        b"AMICE_NTTP_SOURCE_FILE".as_slice(),
    ] {
        assert!(
            !bytes.windows(text.len()).any(|value| value == text),
            "template string remained in plaintext in {}",
            path.display()
        );
    }
}

fn check_shared_templates(timing: &str, stack_alloc: bool, optimization: &str, plain_reader: bool) {
    ensure_plugin_built();
    let name = format!("shared_templates_{timing}_{stack_alloc}_{optimization}_{plain_reader}");
    let config = ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some("simd_xor".to_string()),
        string_decrypt_timing: Some(timing.to_string()),
        string_stack_alloc: Some(stack_alloc),
        string_inline_decrypt_fn: Some(true),
        string_only_llvm_string: Some(false),
        ..ObfuscationConfig::disabled()
    };
    let source = fixture_path("string_encryption", "shared_template_string.cpp", Language::Cpp);
    let mut objects = Vec::new();
    for reader in ["read_a", "read_b"] {
        let result = CppCompileBuilder::new(&source, &format!("{name}_{reader}"))
            .config(config.clone())
            .std("c++20")
            .optimization(optimization)
            .arg("-c")
            .arg(&format!("-DREADER_NAME={reader}"))
            .arg(&format!("-DCONTROLS_NAME={reader}_controls"))
            .compile();
        result.assert_success();
        assert_template_text_absent(&result.binary_path);
        objects.push(result.binary_path);
    }
    if plain_reader {
        let result = CppCompileBuilder::new(&source, &format!("{name}_plain"))
            .without_plugin()
            .std("c++20")
            .optimization(optimization)
            .arg("-c")
            .arg("-DREADER_NAME=read_plain")
            .arg("-DCONTROLS_NAME=read_plain_controls")
            .compile();
        result.assert_success();
        objects.push(result.binary_path);
    }

    for reverse in [false, true] {
        if reverse {
            objects.reverse();
        }
        let mut link = CppCompileBuilder::new(
            fixture_path("string_encryption", "shared_template_string_main.cpp", Language::Cpp),
            &format!("{name}_{reverse}"),
        )
        .without_plugin()
        .std("c++20")
        .optimization(optimization);
        if plain_reader {
            link = link.arg("-DWITH_PLAIN_READER");
        }
        for object in &objects {
            link = link.arg(object.to_str().unwrap());
        }
        let result = link.compile();
        result.assert_success();
        if !plain_reader {
            assert_template_text_absent(&result.binary_path);
        }
        result.run().assert_success();
    }
}

#[test]
fn test_shared_templates_lazy_simd_xor() {
    check_shared_templates("lazy", false, "O2", false);
}

#[test]
fn test_shared_templates_global_simd_xor() {
    check_shared_templates("global", false, "O2", false);
}

#[test]
fn test_shared_templates_stack_simd_xor() {
    check_shared_templates("lazy", true, "O2", false);
}

#[test]
fn test_shared_templates_unoptimized_simd_xor() {
    check_shared_templates("lazy", false, "O0", false);
}

#[test]
fn test_shared_templates_mixed_lazy_simd_xor() {
    check_shared_templates("lazy", false, "O2", true);
}

#[test]
fn test_shared_templates_mixed_global_simd_xor() {
    check_shared_templates("global", false, "O2", true);
}
