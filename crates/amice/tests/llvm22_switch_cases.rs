mod common;

#[cfg(feature = "llvm22-1")]
mod llvm22 {
    use super::common::{
        CppCompileBuilder, Language, LlvmConfig, ObfuscationConfig, build_amice_with_llvm, fixture_path,
        llvm_config_from_env,
    };
    use serial_test::serial;
    use std::path::PathBuf;

    fn llvm22_config() -> LlvmConfig {
        llvm_config_from_env("LLVM_SYS_221_PREFIX", "llvm22-1")
            .expect("LLVM_SYS_221_PREFIX must be set for llvm22-1 tests")
    }

    fn llvm22_clang(config: &LlvmConfig) -> String {
        let clang = PathBuf::from(&config.prefix).join("bin").join("clang");
        assert!(clang.exists(), "LLVM22 clang not found at {}", clang.display());
        clang.to_string_lossy().into_owned()
    }

    fn lower_switch_config() -> ObfuscationConfig {
        let mut config = ObfuscationConfig::disabled();
        config.lower_switch = Some(true);
        config
    }

    fn flatten_config() -> ObfuscationConfig {
        let mut config = ObfuscationConfig::disabled();
        config.flatten = Some(true);
        config.flatten_mode = Some("dominator".to_owned());
        config
    }

    fn vm_flatten_config() -> ObfuscationConfig {
        let mut config = ObfuscationConfig::disabled();
        config.vm_flatten = Some(true);
        config
    }

    fn compile_and_run(compiler: &str, output_name: &str, config: ObfuscationConfig) {
        let result = CppCompileBuilder::new(
            fixture_path("control_flow", "llvm22_switch_cases.c", Language::C),
            output_name,
        )
        .compiler(compiler)
        .config(config)
        .compile();

        result.assert_success();
        result.run().assert_success();
    }

    #[test]
    #[serial]
    fn test_llvm22_switch_case_api_users() {
        let config = llvm22_config();
        build_amice_with_llvm(&config);
        let clang = llvm22_clang(&config);

        compile_and_run(&clang, "llvm22_lower_switch_cases", lower_switch_config());
        compile_and_run(&clang, "llvm22_flatten_switch_cases", flatten_config());
        compile_and_run(&clang, "llvm22_vm_flatten_switch_cases", vm_flatten_config());
    }
}
