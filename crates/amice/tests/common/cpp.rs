//! C/C++ compilation utilities for amice integration tests.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{CompileResult, ObfuscationConfig, output_dir, plugin_path};

/// Builder for compiling C/C++ test files with the amice plugin
#[derive(Debug)]
pub struct CppCompileBuilder {
    source: PathBuf,
    output: PathBuf,
    compiler: String,
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
