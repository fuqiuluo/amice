mod common;

#[cfg(feature = "llvm23-1")]
mod llvm23 {
    use super::common::{
        CppCompileBuilder, Language, LlvmConfig, ObfuscationConfig, build_amice_with_llvm, fixture_path,
        llvm_config_from_env,
    };
    use serial_test::serial;
    use std::path::PathBuf;

    fn llvm23_config() -> LlvmConfig {
        llvm_config_from_env("LLVM_SYS_231_PREFIX", "llvm23-1")
            .expect("LLVM_SYS_231_PREFIX must be set for llvm23-1 tests")
    }

    fn llvm23_clang(config: &LlvmConfig) -> String {
        let clang = PathBuf::from(&config.prefix).join("bin").join("clang");
        assert!(clang.exists(), "LLVM23 clang not found at {}", clang.display());
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

    fn indirect_branch_config() -> ObfuscationConfig {
        let mut config = ObfuscationConfig::disabled();
        config.indirect_branch = Some(true);
        config
    }

    fn vm_virtualize_config() -> ObfuscationConfig {
        let mut config = ObfuscationConfig::disabled();
        config.vm_virtualize = Some(true);
        config
    }

    fn compile_and_run(compiler: &str, source: &str, output_name: &str, config: ObfuscationConfig) {
        let result = CppCompileBuilder::new(fixture_path("control_flow", source, Language::C), output_name)
            .compiler(compiler)
            .config(config)
            .compile();

        result.assert_success();
        result.run().assert_success();
    }

    #[test]
    #[serial]
    fn test_llvm23_branch_and_switch_case_api_users() {
        let config = llvm23_config();
        build_amice_with_llvm(&config);
        let clang = llvm23_clang(&config);

        // LLVM 23 splits the `br` opcode and reorders conditional-branch
        // operands, so branch users get their own coverage on top of the
        // switch-case paths shared with LLVM 22.
        compile_and_run(
            &clang,
            "llvm23_branch_cases.c",
            "llvm23_flatten_branches",
            flatten_config(),
        );
        compile_and_run(
            &clang,
            "llvm23_branch_cases.c",
            "llvm23_vm_flatten_branches",
            vm_flatten_config(),
        );
        compile_and_run(
            &clang,
            "llvm23_branch_cases.c",
            "llvm23_indirect_branches",
            indirect_branch_config(),
        );
        compile_and_run(
            &clang,
            "llvm23_branch_cases.c",
            "llvm23_vm_virtualize_branches",
            vm_virtualize_config(),
        );

        compile_and_run(
            &clang,
            "llvm22_switch_cases.c",
            "llvm23_lower_switch_cases",
            lower_switch_config(),
        );
        compile_and_run(
            &clang,
            "llvm22_switch_cases.c",
            "llvm23_flatten_switch_cases",
            flatten_config(),
        );
        compile_and_run(
            &clang,
            "llvm22_switch_cases.c",
            "llvm23_vm_flatten_switch_cases",
            vm_flatten_config(),
        );
    }
}
