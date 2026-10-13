//! Common test utilities for amice integration tests.
//!
//! This module provides shared functionality for all integration tests:
//! - Cross-platform plugin path detection
//! - LLVM version detection and configuration
//! - Compilation and execution helpers

#![allow(dead_code, unused_imports)]

pub mod cpp;
pub mod rust;

use std::env;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

// Re-export for convenience
pub use cpp::CppCompileBuilder;
pub use rust::RustCompileBuilder;

/// 单条测试命令的看门狗上限。CI 的 LLVM 20/21 腿曾出现测试子进程挂死并耗尽
/// 180 分钟的步骤超时；正常命令都在秒级完成，超时在这里 kill 并把命令写进
/// panic 消息，让 CI 日志直接指认卡住的测试与命令。
pub const TEST_COMMAND_TIMEOUT: Duration = Duration::from_secs(300);

/// 带看门狗地执行命令：超时 kill 子进程并以包含命令行的错误 panic。
pub fn output_with_watchdog(command: Command) -> Output {
    run_with_watchdog(command)
}

/// 与 [`output_with_watchdog`] 相同，但借用已有的 `&mut Command`（如复用调用方
/// 已配置好参数的命令）。
pub fn output_with_watchdog_ref(command: &mut Command) -> Output {
    let _ = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_with_watchdog_spawned(
        command
            .spawn()
            .unwrap_or_else(|err| panic!("failed to spawn {command:?}: {err}")),
        command,
    )
}

fn run_with_watchdog(command: Command) -> Output {
    let mut command = command;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|err| panic!("failed to spawn {command:?}: {err}"));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() > TEST_COMMAND_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "command exceeded {:?} watchdog timeout and was killed: {command:?}",
                        TEST_COMMAND_TIMEOUT
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            },
            Err(err) => panic!("failed to wait for {command:?}: {err}"),
        }
    };
    finish_output(child, status)
}

fn run_with_watchdog_spawned(mut child: std::process::Child, command: &Command) -> Output {
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() > TEST_COMMAND_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "command exceeded {:?} watchdog timeout and was killed: {command:?}",
                        TEST_COMMAND_TIMEOUT
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            },
            Err(err) => panic!("failed to wait for {command:?}: {err}"),
        }
    };
    finish_output(child, status)
}

fn finish_output(mut child: std::process::Child, status: std::process::ExitStatus) -> Output {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr);
    }
    Output { status, stdout, stderr }
}

/// Target programming language for compilation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// C language
    C,
    /// C++ language
    Cpp,
    /// Rust language
    Rust,
}

impl Language {
    /// Get the directory name for this language in the tests directory
    pub fn dir_name(&self) -> &'static str {
        match self {
            Language::C | Language::Cpp => "c",
            Language::Rust => "rust",
        }
    }

    /// Detect language from file extension
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "c" | "h" => Some(Language::C),
            "cpp" | "cc" | "cxx" | "hpp" => Some(Language::Cpp),
            "rs" => Some(Language::Rust),
            _ => None,
        }
    }
}

/// Get the project root directory
pub fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Get the integration test root for the amice crate.
pub fn tests_root() -> PathBuf {
    project_root().join("tests")
}

/// Get the Cargo workspace root directory.
pub fn workspace_root() -> PathBuf {
    project_root()
        .ancestors()
        .nth(2)
        .expect("amice crate should live under crates/amice")
        .to_path_buf()
}

/// Get the path to the compiled amice plugin library.
/// Handles platform-specific library naming conventions.
pub fn plugin_path() -> PathBuf {
    let root = workspace_root();
    let target_dir = root.join("target").join("release");

    #[cfg(target_os = "windows")]
    let lib_name = "amice.dll";

    #[cfg(target_os = "macos")]
    let lib_name = "libamice.dylib";

    #[cfg(target_os = "linux")]
    let lib_name = "libamice.so";

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let lib_name = "libamice.so";

    target_dir.join(lib_name)
}

/// Get the target output directory for compiled test binaries
pub fn output_dir() -> PathBuf {
    workspace_root().join("target").join("test-outputs")
}

/// Get the path to a fixture file
pub fn fixture_path(category: &str, filename: &str, language: Language) -> PathBuf {
    tests_root()
        .join(language.dir_name())
        .join("fixtures")
        .join(category)
        .join(filename)
}

/// Get the path to a Rust fixture Cargo project.
pub fn rust_fixture_project_path(name: &str) -> PathBuf {
    tests_root().join("rust").join(name)
}

/// LLVM version configuration
#[derive(Debug, Clone)]
pub struct LlvmConfig {
    pub env_var: String,
    pub feature: String,
    pub prefix: String,
}

pub fn llvm_config_from_env(env_var: &str, feature: &str) -> Option<LlvmConfig> {
    env::var(env_var).ok().map(|prefix| LlvmConfig {
        env_var: env_var.to_string(),
        feature: feature.to_string(),
        prefix,
    })
}

/// Detect LLVM configuration from environment variables.
/// Returns the first matching LLVM installation found.
pub fn detect_llvm_config() -> Option<LlvmConfig> {
    let llvm_versions = [
        ("LLVM_SYS_231_PREFIX", "llvm23-1"),
        ("LLVM_SYS_211_PREFIX", "llvm21-1"),
        ("LLVM_SYS_221_PREFIX", "llvm22-1"),
        ("LLVM_SYS_201_PREFIX", "llvm20-1"),
        ("LLVM_SYS_191_PREFIX", "llvm19-1"),
        ("LLVM_SYS_181_PREFIX", "llvm18-1"),
        ("LLVM_SYS_170_PREFIX", "llvm17-0"),
        ("LLVM_SYS_160_PREFIX", "llvm16-0"),
        ("LLVM_SYS_150_PREFIX", "llvm15-0"),
        ("LLVM_SYS_140_PREFIX", "llvm14-0"),
        ("LLVM_SYS_130_PREFIX", "llvm13-0"),
        ("LLVM_SYS_120_PREFIX", "llvm12-0"),
    ];

    for (env_var, feature) in &llvm_versions {
        if let Some(config) = llvm_config_from_env(env_var, feature) {
            return Some(config);
        }
    }

    None
}

/// Extract the LLVM major version from a cargo feature name like `llvm21-1`.
pub fn llvm_major_from_feature(feature: &str) -> u32 {
    feature
        .strip_prefix("llvm")
        .and_then(|version| version.split_once('-'))
        .and_then(|(major, _)| major.parse::<u32>().ok())
        .unwrap_or(21)
}

/// Remove LLVM 22 IR spellings that LLVM 21's `opt` cannot parse.
pub fn sanitize_ir_for_llvm21(path: &Path) {
    let Ok(ir) = std::fs::read_to_string(path) else {
        return;
    };
    let sanitized = ir
        .replace(", target_mem0: none", "")
        .replace(", target_mem1: none", "")
        .replace(", target_mem2: none", "")
        .replace(", target_mem3: none", "")
        .replace(" nocreateundeforpoison", "");
    let sanitized = sanitized
        .lines()
        .filter(|line| !line.contains("@llvm.lifetime.start.") && !line.contains("@llvm.lifetime.end."))
        .collect::<Vec<_>>()
        .join("\n");
    if sanitized != ir {
        std::fs::write(path, sanitized).expect("failed to write LLVM21-compatible IR");
    }
}

/// Returns the clang executable that matches the detected LLVM prefix when it exists.
pub fn clang_compiler_path(is_cpp: bool) -> PathBuf {
    let compiler = if is_cpp { "clang++" } else { "clang" };
    if let Some(config) = detect_llvm_config() {
        let candidate = PathBuf::from(config.prefix).join("bin").join(compiler);
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from(compiler)
}

fn run_amice_build(config: Option<&LlvmConfig>) {
    let mut cmd = Command::new("cargo");
    cmd.args(["build", "--release", "-p", "amice"]);
    cmd.current_dir(workspace_root());

    // Apply LLVM-specific configuration if detected.
    if let Some(config) = config {
        cmd.env(&config.env_var, &config.prefix);

        let features = config.feature.clone();
        #[cfg(target_os = "windows")]
        let features = format!("{features},win-link-lld");

        cmd.arg("--no-default-features").arg("--features").arg(features);
    }

    let output = crate::common::output_with_watchdog(cmd);

    if !output.status.success() {
        eprintln!("=== Cargo build failed ===");
        eprintln!("STDOUT:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("STDERR:\n{}", String::from_utf8_lossy(&output.stderr));
        panic!("Cargo build failed");
    }
}

/// Build the amice plugin in release mode.
/// Automatically detects and applies LLVM configuration from environment.
pub fn build_amice() {
    let config = detect_llvm_config();
    run_amice_build(config.as_ref());
}

/// Build the amice plugin in release mode for an explicit LLVM version.
pub fn build_amice_with_llvm(config: &LlvmConfig) {
    run_amice_build(Some(config));
}

/// Ensure the plugin is built and exists
pub fn ensure_plugin_built() {
    let plugin = plugin_path();
    if !plugin.exists() {
        build_amice();
    }
    assert!(
        plugin.exists(),
        "Plugin not found at {:?}. Run: cargo build --release",
        plugin
    );
}

/// Environment variables for obfuscation configuration
#[derive(Debug, Default, Clone)]
pub struct ObfuscationConfig {
    // String encryption
    pub string_encryption: Option<bool>,
    pub string_only_llvm_string: Option<bool>,
    pub string_algorithm: Option<String>,
    pub string_decrypt_timing: Option<String>,
    pub string_stack_alloc: Option<bool>,
    pub string_inline_decrypt_fn: Option<bool>,
    pub string_max_encryption_count: Option<u32>,
    pub string_null_terminated: Option<bool>,

    // Indirect branch
    pub indirect_branch: Option<bool>,
    pub indirect_branch_flags: Option<String>,

    // Indirect call
    pub indirect_call: Option<bool>,

    // Control flow
    pub flatten: Option<bool>,
    pub flatten_mode: Option<String>,
    pub bogus_control_flow: Option<bool>,
    pub vm_flatten: Option<bool>,
    pub vm_virtualize: Option<bool>,
    pub vm_profile_path: Option<String>,
    pub vm_runtime_scope: Option<String>,
    pub vm_emit_markers: Option<bool>,
    pub vm_dump_bytecode: Option<bool>,
    pub vm_dump_lowering: Option<bool>,

    // Shuffle blocks
    pub shuffle_blocks: Option<bool>,
    pub shuffle_blocks_flags: Option<String>,

    // Split basic block
    pub split_basic_block: Option<bool>,
    pub split_basic_block_num: Option<u32>,

    // MBA
    pub mba: Option<bool>,
    pub mba_float_regions: Option<bool>,
    pub mba_pre_expand: Option<bool>,
    pub mba_opaque_guard: Option<bool>,
    pub mba_max_instructions: Option<u32>,
    pub mba_max_added_instructions: Option<u32>,

    // Function wrapper
    pub function_wrapper: Option<bool>,

    // Parameter aggregation
    pub param_aggregate: Option<bool>,

    // Clone function
    pub clone_function: Option<bool>,

    // Alias access
    pub alias_access: Option<bool>,

    // Delay offset loading
    pub delay_offset_loading: Option<bool>,
    pub delay_offset_loading_xor_offset: Option<bool>,

    // Lower switch
    pub lower_switch: Option<bool>,
    pub lower_switch_with_dummy_code: Option<bool>,
}

impl ObfuscationConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create config with all obfuscation disabled
    pub fn disabled() -> Self {
        Self {
            string_encryption: Some(false),
            string_only_llvm_string: Some(true),
            indirect_branch: Some(false),
            indirect_call: Some(false),
            flatten: Some(false),
            bogus_control_flow: Some(false),
            vm_flatten: Some(false),
            vm_virtualize: Some(false),
            vm_profile_path: None,
            vm_runtime_scope: None,
            vm_emit_markers: None,
            vm_dump_bytecode: None,
            vm_dump_lowering: None,
            shuffle_blocks: Some(false),
            split_basic_block: Some(false),
            split_basic_block_num: None,
            mba: Some(false),
            function_wrapper: Some(false),
            param_aggregate: Some(false),
            clone_function: Some(false),
            alias_access: Some(false),
            delay_offset_loading: Some(false),
            delay_offset_loading_xor_offset: Some(true),
            lower_switch: Some(false),
            lower_switch_with_dummy_code: Some(false),
            ..Default::default()
        }
    }

    /// Apply configuration to a Command
    pub fn apply_to_command(&self, cmd: &mut Command) {
        macro_rules! set_env_bool {
            ($cmd:expr, $name:expr, $value:expr) => {
                if let Some(v) = $value {
                    $cmd.env($name, if v { "true" } else { "false" });
                }
            };
        }

        macro_rules! set_env_str {
            ($cmd:expr, $name:expr, $value:expr) => {
                if let Some(ref v) = $value {
                    $cmd.env($name, v);
                }
            };
        }

        macro_rules! set_env_num {
            ($cmd:expr, $name:expr, $value:expr) => {
                if let Some(v) = $value {
                    $cmd.env($name, v.to_string());
                }
            };
        }

        // String encryption
        set_env_bool!(cmd, "AMICE_STRING_ENCRYPTION", self.string_encryption);
        set_env_bool!(cmd, "AMICE_STRING_ONLY_LLVM_STRING", self.string_only_llvm_string);
        set_env_str!(cmd, "AMICE_STRING_ALGORITHM", self.string_algorithm);
        set_env_str!(cmd, "AMICE_STRING_DECRYPT_TIMING", self.string_decrypt_timing);
        set_env_bool!(cmd, "AMICE_STRING_STACK_ALLOC", self.string_stack_alloc);
        set_env_bool!(cmd, "AMICE_STRING_INLINE_DECRYPT_FN", self.string_inline_decrypt_fn);
        set_env_num!(
            cmd,
            "AMICE_STRING_MAX_ENCRYPTION_COUNT",
            self.string_max_encryption_count
        );
        set_env_bool!(cmd, "AMICE_STRING_NULL_TERMINATED", self.string_null_terminated);

        // Indirect branch
        set_env_bool!(cmd, "AMICE_INDIRECT_BRANCH", self.indirect_branch);
        set_env_str!(cmd, "AMICE_INDIRECT_BRANCH_FLAGS", self.indirect_branch_flags);

        // Indirect call
        set_env_bool!(cmd, "AMICE_INDIRECT_CALL", self.indirect_call);

        // Control flow
        set_env_bool!(cmd, "AMICE_FLATTEN", self.flatten);
        set_env_str!(cmd, "AMICE_FLATTEN_MODE", self.flatten_mode);
        set_env_bool!(cmd, "AMICE_BOGUS_CONTROL_FLOW", self.bogus_control_flow);
        set_env_bool!(cmd, "AMICE_VM_FLATTEN", self.vm_flatten);
        set_env_bool!(cmd, "AMICE_VM_VIRTUALIZE", self.vm_virtualize);
        set_env_str!(cmd, "AMICE_VM_PROFILE_PATH", self.vm_profile_path);
        set_env_str!(cmd, "AMICE_VM_RUNTIME_SCOPE", self.vm_runtime_scope);
        set_env_bool!(cmd, "AMICE_VM_EMIT_MARKERS", self.vm_emit_markers);
        set_env_bool!(cmd, "AMICE_VM_DUMP_BYTECODE", self.vm_dump_bytecode);
        set_env_bool!(cmd, "AMICE_VM_DUMP_LOWERING", self.vm_dump_lowering);

        // Shuffle blocks
        set_env_bool!(cmd, "AMICE_SHUFFLE_BLOCKS", self.shuffle_blocks);
        set_env_str!(cmd, "AMICE_SHUFFLE_BLOCKS_FLAGS", self.shuffle_blocks_flags);

        // Split basic block
        set_env_bool!(cmd, "AMICE_SPLIT_BASIC_BLOCK", self.split_basic_block);
        set_env_num!(cmd, "AMICE_SPLIT_BASIC_BLOCK_NUM", self.split_basic_block_num);

        // MBA
        set_env_bool!(cmd, "AMICE_MBA", self.mba);
        set_env_bool!(cmd, "AMICE_MBA_FLOAT_REGIONS", self.mba_float_regions);
        set_env_bool!(cmd, "AMICE_MBA_PRE_EXPAND", self.mba_pre_expand);
        set_env_bool!(cmd, "AMICE_MBA_OPAQUE_GUARD", self.mba_opaque_guard);
        set_env_num!(cmd, "AMICE_MBA_MAX_INSTRUCTIONS", self.mba_max_instructions);
        set_env_num!(cmd, "AMICE_MBA_MAX_ADDED_INSTRUCTIONS", self.mba_max_added_instructions);

        // Function wrapper
        set_env_bool!(cmd, "AMICE_FUNCTION_WRAPPER", self.function_wrapper);

        // Parameter aggregation
        set_env_bool!(cmd, "AMICE_PARAM_AGGREGATE", self.param_aggregate);

        // Clone function
        set_env_bool!(cmd, "AMICE_CLONE_FUNCTION", self.clone_function);

        // Alias access
        set_env_bool!(cmd, "AMICE_ALIAS_ACCESS", self.alias_access);

        // Delay offset loading
        set_env_bool!(cmd, "AMICE_DELAY_OFFSET_LOADING", self.delay_offset_loading);
        set_env_bool!(
            cmd,
            "AMICE_DELAY_OFFSET_LOADING_XOR_OFFSET",
            self.delay_offset_loading_xor_offset
        );

        // Lower switch
        set_env_bool!(cmd, "AMICE_LOWER_SWITCH", self.lower_switch);
        set_env_bool!(
            cmd,
            "AMICE_LOWER_SWITCH_WITH_DUMMY_CODE",
            self.lower_switch_with_dummy_code
        );
    }
}

/// Result of a compilation
#[derive(Debug)]
pub struct CompileResult {
    pub output: Output,
    pub binary_path: PathBuf,
}

impl CompileResult {
    /// Check if compilation succeeded
    pub fn success(&self) -> bool {
        self.output.status.success()
    }

    /// Assert compilation succeeded
    pub fn assert_success(&self) {
        if !self.success() {
            eprintln!("=== Compilation failed ===");
            eprintln!("STDOUT:\n{}", String::from_utf8_lossy(&self.output.stdout));
            eprintln!("STDERR:\n{}", String::from_utf8_lossy(&self.output.stderr));
            panic!("Compilation failed");
        }
    }

    /// Get stderr as string
    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).to_string()
    }

    /// Run the compiled binary and return its output
    pub fn run(&self) -> RunResult {
        self.assert_success();

        let output = output_with_watchdog(Command::new(&self.binary_path));

        RunResult { output }
    }
}

/// Result of running a compiled binary
#[derive(Debug)]
pub struct RunResult {
    pub output: Output,
}

impl RunResult {
    /// Check if execution succeeded
    pub fn success(&self) -> bool {
        self.output.status.success()
    }

    /// Assert execution succeeded
    pub fn assert_success(&self) {
        if !self.success() {
            eprintln!("=== Execution failed ===");
            eprintln!("STDOUT:\n{}", self.stdout());
            eprintln!("STDERR:\n{}", self.stderr());
            panic!("Execution failed with status: {:?}", self.output.status.code());
        }
    }

    /// Get stdout as string
    pub fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).to_string()
    }

    /// Get stderr as string
    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).to_string()
    }

    /// Get stdout lines (trimmed, non-empty)
    pub fn stdout_lines(&self) -> Vec<String> {
        self.stdout()
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}
