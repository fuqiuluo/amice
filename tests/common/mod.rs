//! Common test utilities for amice integration tests.
//!
//! This module provides shared functionality for all integration tests:
//! - Cross-platform plugin path detection
//! - LLVM version detection and configuration
//! - Compilation and execution helpers

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Get the project root directory
pub fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Get the path to the compiled amice plugin library.
/// Handles platform-specific library naming conventions.
pub fn plugin_path() -> PathBuf {
    let root = project_root();
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
    project_root().join("target").join("test-outputs")
}

/// LLVM version configuration
#[derive(Debug, Clone)]
pub struct LlvmConfig {
    pub env_var: String,
    pub feature: String,
    pub prefix: String,
}

/// Detect LLVM configuration from environment variables.
/// Returns the first matching LLVM installation found.
pub fn detect_llvm_config() -> Option<LlvmConfig> {
    let llvm_versions = [
        ("LLVM_SYS_210_PREFIX", "llvm21-1"),
        ("LLVM_SYS_201_PREFIX", "llvm20-1"),
        ("LLVM_SYS_191_PREFIX", "llvm19-1"),
        ("LLVM_SYS_181_PREFIX", "llvm18-1"),
        ("LLVM_SYS_170_PREFIX", "llvm17-0"),
        ("LLVM_SYS_160_PREFIX", "llvm16-0"),
        ("LLVM_SYS_150_PREFIX", "llvm15-0"),
        ("LLVM_SYS_140_PREFIX", "llvm14-0"),
        ("LLVM_SYS_130_PREFIX", "llvm13-0"),
        ("LLVM_SYS_120_PREFIX", "llvm12-0"),
        ("LLVM_SYS_110_PREFIX", "llvm11-0"),
    ];

    for (env_var, feature) in &llvm_versions {
        if let Ok(prefix) = env::var(env_var) {
            return Some(LlvmConfig {
                env_var: env_var.to_string(),
                feature: feature.to_string(),
                prefix,
            });
        }
    }

    None
}

/// Build the amice plugin in release mode.
/// Automatically detects and applies LLVM configuration from environment.
pub fn build_amice() {
    let mut cmd = Command::new("cargo");
    cmd.arg("build").arg("--release");
    cmd.current_dir(project_root());

    // Apply LLVM-specific configuration if detected
    if let Some(config) = detect_llvm_config() {
        cmd.env(&config.env_var, &config.prefix);
        cmd.arg("--no-default-features").arg("--features").arg(&config.feature);

        // Add Windows-specific link features if needed
        #[cfg(target_os = "windows")]
        {
            let features = format!("{},win-link-lld", config.feature);
            cmd.args(["--features", &features]);
        }
    }

    let output = cmd.output().expect("Failed to execute cargo build");

    if !output.status.success() {
        eprintln!("=== Cargo build failed ===");
        eprintln!("STDOUT:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("STDERR:\n{}", String::from_utf8_lossy(&output.stderr));
        panic!("Cargo build failed");
    }
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
    pub string_algorithm: Option<String>,
    pub string_decrypt_timing: Option<String>,
    pub string_stack_alloc: Option<bool>,
    pub string_inline_decrypt_fn: Option<bool>,
    pub string_max_encryption_count: Option<u32>,

    // Indirect branch
    pub indirect_branch: Option<bool>,
    pub indirect_branch_flags: Option<String>,

    // Indirect call
    pub indirect_call: Option<bool>,

    // Control flow
    pub flatten: Option<bool>,
    pub bogus_control_flow: Option<bool>,
    pub vm_flatten: Option<bool>,

    // Shuffle blocks
    pub shuffle_blocks: Option<bool>,
    pub shuffle_blocks_flags: Option<String>,

    // Split basic block
    pub split_basic_block: Option<bool>,

    // MBA
    pub mba: Option<bool>,

    // Function wrapper
    pub function_wrapper: Option<bool>,

    // Clone function
    pub clone_function: Option<bool>,
}

impl ObfuscationConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create config with all obfuscation disabled
    pub fn disabled() -> Self {
        Self {
            string_encryption: Some(false),
            indirect_branch: Some(false),
            indirect_call: Some(false),
            flatten: Some(false),
            bogus_control_flow: Some(false),
            vm_flatten: Some(false),
            shuffle_blocks: Some(false),
            split_basic_block: Some(false),
            mba: Some(false),
            function_wrapper: Some(false),
            clone_function: Some(false),
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
        set_env_str!(cmd, "AMICE_STRING_ALGORITHM", self.string_algorithm);
        set_env_str!(cmd, "AMICE_STRING_DECRYPT_TIMING", self.string_decrypt_timing);
        set_env_bool!(cmd, "AMICE_STRING_STACK_ALLOC", self.string_stack_alloc);
        set_env_bool!(cmd, "AMICE_STRING_INLINE_DECRYPT_FN", self.string_inline_decrypt_fn);
        set_env_num!(
            cmd,
            "AMICE_STRING_MAX_ENCRYPTION_COUNT",
            self.string_max_encryption_count
        );

        // Indirect branch
        set_env_bool!(cmd, "AMICE_INDIRECT_BRANCH", self.indirect_branch);
        set_env_str!(cmd, "AMICE_INDIRECT_BRANCH_FLAGS", self.indirect_branch_flags);

        // Indirect call
        set_env_bool!(cmd, "AMICE_INDIRECT_CALL", self.indirect_call);

        // Control flow
        set_env_bool!(cmd, "AMICE_FLATTEN", self.flatten);
        set_env_bool!(cmd, "AMICE_BOGUS_CONTROL_FLOW", self.bogus_control_flow);
        set_env_bool!(cmd, "AMICE_VM_FLATTEN", self.vm_flatten);

        // Shuffle blocks
        set_env_bool!(cmd, "AMICE_SHUFFLE_BLOCKS", self.shuffle_blocks);
        set_env_str!(cmd, "AMICE_SHUFFLE_BLOCKS_FLAGS", self.shuffle_blocks_flags);

        // Split basic block
        set_env_bool!(cmd, "AMICE_SPLIT_BASIC_BLOCK", self.split_basic_block);

        // MBA
        set_env_bool!(cmd, "AMICE_MBA", self.mba);

        // Function wrapper
        set_env_bool!(cmd, "AMICE_FUNCTION_WRAPPER", self.function_wrapper);

        // Clone function
        set_env_bool!(cmd, "AMICE_CLONE_FUNCTION", self.clone_function);
    }
}

/// Builder for compiling C/C++ test files with the amice plugin
#[derive(Debug)]
pub struct CompileBuilder {
    source: PathBuf,
    output: PathBuf,
    compiler: String,
    config: ObfuscationConfig,
    optimization: Option<String>,
    std: Option<String>,
    extra_args: Vec<String>,
    use_plugin: bool,
}

impl CompileBuilder {
    /// Create a new compile builder for the given source file
    pub fn new(source: impl AsRef<Path>, output_name: &str) -> Self {
        let source = source.as_ref().to_path_buf();
        let is_cpp = source
            .extension()
            .map(|e| e == "cc" || e == "cpp" || e == "cxx")
            .unwrap_or(false);

        let output_dir = output_dir();
        std::fs::create_dir_all(&output_dir).ok();

        #[cfg(target_os = "windows")]
        let output = output_dir.join(format!("{}.exe", output_name));

        #[cfg(not(target_os = "windows"))]
        let output = output_dir.join(output_name);

        Self {
            source,
            output,
            compiler: if is_cpp {
                "clang++".to_string()
            } else {
                "clang".to_string()
            },
            config: ObfuscationConfig::default(),
            optimization: None,
            std: if is_cpp { Some("c++17".to_string()) } else { None },
            extra_args: Vec::new(),
            use_plugin: true,
        }
    }

    /// Set the obfuscation configuration
    pub fn config(mut self, config: ObfuscationConfig) -> Self {
        self.config = config;
        self
    }

    /// Set optimization level (e.g., "O0", "O2", "O3")
    pub fn optimization(mut self, opt: &str) -> Self {
        self.optimization = Some(opt.to_string());
        self
    }

    /// Set C/C++ standard (e.g., "c11", "c++17")
    pub fn std(mut self, std: &str) -> Self {
        self.std = Some(std.to_string());
        self
    }

    /// Add extra compiler arguments
    pub fn arg(mut self, arg: &str) -> Self {
        self.extra_args.push(arg.to_string());
        self
    }

    /// Disable using the amice plugin (for baseline comparison)
    pub fn without_plugin(mut self) -> Self {
        self.use_plugin = false;
        self
    }

    /// Compile the source file
    pub fn compile(self) -> CompileResult {
        // Clean up previous output
        if self.output.exists() {
            std::fs::remove_file(&self.output).ok();
        }

        let mut cmd = Command::new(&self.compiler);

        // Apply obfuscation config
        self.config.apply_to_command(&mut cmd);

        // Add plugin if enabled
        if self.use_plugin {
            let plugin = plugin_path();
            cmd.arg(format!("-fpass-plugin={}", plugin.display()));
        }

        // Add optimization
        if let Some(ref opt) = self.optimization {
            cmd.arg(format!("-{}", opt));
        }

        // Add std
        if let Some(ref std) = self.std {
            cmd.arg(format!("-std={}", std));
        }

        // Add extra args
        for arg in &self.extra_args {
            cmd.arg(arg);
        }

        // Add source and output
        cmd.arg(&self.source);
        cmd.arg("-o");
        cmd.arg(&self.output);

        let output = cmd.output().expect("Failed to execute compiler");

        CompileResult {
            output,
            binary_path: self.output,
        }
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

        let output = Command::new(&self.binary_path)
            .output()
            .expect("Failed to execute compiled binary");

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

/// Get the path to a fixture file
pub fn fixture_path(category: &str, filename: &str) -> PathBuf {
    project_root()
        .join("tests")
        .join("fixtures")
        .join(category)
        .join(filename)
}

/// Macro for creating simple compile-and-run tests
#[macro_export]
macro_rules! compile_run_test {
    ($name:ident, $fixture:expr, $config:expr) => {
        #[test]
        fn $name() {
            common::ensure_plugin_built();
            let result = common::CompileBuilder::new(common::fixture_path($fixture.0, $fixture.1), stringify!($name))
                .config($config)
                .compile();
            result.assert_success();
            let run = result.run();
            run.assert_success();
        }
    };
}

/// Macro for creating compile-and-compare tests
#[macro_export]
macro_rules! compile_compare_test {
    ($name:ident, $fixture:expr, $config:expr, $expected_lines:expr) => {
        #[test]
        fn $name() {
            common::ensure_plugin_built();
            let result = common::CompileBuilder::new(common::fixture_path($fixture.0, $fixture.1), stringify!($name))
                .config($config)
                .compile();
            result.assert_success();
            let run = result.run();
            run.assert_success();

            let lines = run.stdout_lines();
            let expected: Vec<&str> = $expected_lines;

            for (i, expected_line) in expected.iter().enumerate() {
                assert!(
                    i < lines.len(),
                    "Missing output line {}: expected '{}'",
                    i,
                    expected_line
                );
                assert_eq!(
                    lines[i], *expected_line,
                    "Line {} mismatch.\nExpected: '{}'\nActual: '{}'",
                    i, expected_line, lines[i]
                );
            }
        }
    };
}
