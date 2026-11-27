mod common;

use common::{CppCompileBuilder, Language, ObfuscationConfig, ensure_plugin_built, fixture_path};

fn check_shared_empty_group(timing: &str) {
    ensure_plugin_built();

    let config = ObfuscationConfig {
        string_encryption: Some(true),
        string_algorithm: Some("simd_xor".to_string()),
        string_decrypt_timing: Some(timing.to_string()),
        string_stack_alloc: Some(false),
        string_only_llvm_string: Some(false),
        ..ObfuscationConfig::disabled()
    };
    let source = fixture_path("string_encryption", "shared_empty_group.cpp", Language::Cpp);
    let mut objects = Vec::new();
    for reader in ["read_a", "read_b"] {
        let result = CppCompileBuilder::new(&source, &format!("shared_empty_group_{timing}_{reader}"))
            .config(config.clone())
            .std("c++17")
            // Keep the global decrypt constructor from folding back to plaintext
            .optimization("O0")
            .arg("-c")
            .arg(&format!("-DREADER_NAME={reader}"))
            .compile();
        result.assert_success();
        objects.push(result.binary_path);
    }

    let result = CppCompileBuilder::new(
        fixture_path("string_encryption", "shared_empty_group_main.cpp", Language::Cpp),
        &format!("shared_empty_group_{timing}"),
    )
    .without_plugin()
    .std("c++17")
    .arg(objects[0].to_str().unwrap())
    .arg(objects[1].to_str().unwrap())
    .compile();
    result.assert_success();
    result.run().assert_success();
}

#[test]
fn test_shared_empty_group_lazy_simd_xor() {
    check_shared_empty_group("lazy");
}

#[test]
fn test_shared_empty_group_global_simd_xor() {
    check_shared_empty_group("global");
}
