//! C/C++ compilation utilities for amice integration tests.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::{
    CompileResult, ObfuscationConfig, clang_compiler_path, detect_llvm_config, llvm_major_from_feature, output_dir,
    plugin_path, sanitize_ir_for_llvm21,
};

/// Builder for compiling C/C++ test files with the amice plugin
#[derive(Debug)]
pub struct CppCompileBuilder {
    source: PathBuf,
    output: PathBuf,
    compiler: PathBuf,
    config: ObfuscationConfig,
    optimization: Option<String>,
    std: Option<String>,
    extra_args: Vec<String>,
    use_plugin: bool,
}

impl CppCompileBuilder {
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
            compiler: clang_compiler_path(is_cpp),
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

    /// Override the C/C++ compiler executable.
    pub fn compiler(mut self, compiler: impl Into<String>) -> Self {
        self.compiler = PathBuf::from(compiler.into());
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

        if self.use_plugin
            && let Some(config) = detect_llvm_config()
            && compiler_major(&self.compiler) != Some(llvm_major_from_feature(&config.feature))
        {
            return self.compile_with_opt_fallback(&config);
        }

        let mut cmd = Command::new(&self.compiler);
        cmd.env("CCACHE_DISABLE", "1");

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

    fn compile_with_opt_fallback(self, config: &super::LlvmConfig) -> CompileResult {
        let out_dir = output_dir();
        let stem = self
            .output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cpp_test");
        let input_ir = out_dir.join(format!("{stem}.input.ll"));
        let optimized_ir = if self.requests_llvm_ir_output() {
            self.output.clone()
        } else {
            out_dir.join(format!("{stem}.opt.ll"))
        };

        let output = self.emit_input_ir(&input_ir);
        if !output.status.success() {
            return CompileResult {
                output,
                binary_path: self.output,
            };
        }
        sanitize_ir_for_llvm21(&input_ir);

        let output = self.run_opt(config, &input_ir, &optimized_ir);
        if !output.status.success() || self.requests_llvm_ir_output() {
            return CompileResult {
                output,
                binary_path: self.output,
            };
        }

        let output = self.link_optimized_ir(&optimized_ir);
        CompileResult {
            output,
            binary_path: self.output,
        }
    }

    fn emit_input_ir(&self, input_ir: &Path) -> Output {
        let mut cmd = Command::new(&self.compiler);
        cmd.env("CCACHE_DISABLE", "1");
        if let Some(ref opt) = self.optimization {
            cmd.arg(format!("-{}", opt));
        }
        if let Some(ref std) = self.std {
            cmd.arg(format!("-std={}", std));
        }
        for arg in self.filtered_extra_args() {
            cmd.arg(arg);
        }
        cmd.arg("-Xclang")
            .arg("-disable-lifetime-markers")
            .arg("-S")
            .arg("-emit-llvm")
            .arg(&self.source)
            .arg("-o")
            .arg(input_ir);
        cmd.output().expect("Failed to execute compiler")
    }

    fn run_opt(&self, config: &super::LlvmConfig, input_ir: &Path, optimized_ir: &Path) -> Output {
        let opt = Path::new(&config.prefix).join("bin").join("opt");
        let mut cmd = Command::new(opt);
        cmd.env("CCACHE_DISABLE", "1")
            .env(&config.env_var, &config.prefix)
            .arg(format!("--load-pass-plugin={}", plugin_path().display()))
            .arg(format!("-passes=default<{}>", self.opt_pipeline_level()))
            .arg("-S")
            .arg(input_ir)
            .arg("-o")
            .arg(optimized_ir);
        self.config.apply_to_command(&mut cmd);
        cmd.output().expect("Failed to execute opt")
    }

    fn link_optimized_ir(&self, optimized_ir: &Path) -> Output {
        let mut cmd = Command::new(&self.compiler);
        cmd.env("CCACHE_DISABLE", "1");
        for arg in self.filtered_extra_args() {
            cmd.arg(arg);
        }
        cmd.arg(optimized_ir).arg("-o").arg(&self.output);
        cmd.output().expect("Failed to execute compiler")
    }

    fn requests_llvm_ir_output(&self) -> bool {
        self.extra_args.iter().any(|arg| arg == "-emit-llvm")
    }

    fn filtered_extra_args(&self) -> impl Iterator<Item = &String> {
        self.extra_args
            .iter()
            .filter(|arg| arg.as_str() != "-S" && arg.as_str() != "-emit-llvm")
    }

    fn opt_pipeline_level(&self) -> &str {
        self.optimization.as_deref().unwrap_or("O0")
    }
}

fn compiler_major(compiler: &Path) -> Option<u32> {
    let output = Command::new(compiler)
        .env("CCACHE_DISABLE", "1")
        .arg("--version")
        .output()
        .ok()?;
    let version = String::from_utf8_lossy(&output.stdout);
    version
        .split_whitespace()
        .skip_while(|word| *word != "version")
        .nth(1)
        .and_then(|version| version.split('.').next())
        .and_then(|major| major.parse::<u32>().ok())
}
