//! Rust compilation utilities for amice integration tests.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{CompileResult, ObfuscationConfig, plugin_path};

/// Builder for compiling Rust projects with the amice plugin
#[derive(Debug)]
pub struct RustCompileBuilder {
    project_dir: PathBuf,
    output_name: String,
    config: ObfuscationConfig,
    optimization: Option<String>,
    use_plugin: bool,
    use_nightly: bool,
}

impl RustCompileBuilder {
    /// Create a new Rust compile builder for the given project directory
    pub fn new(project_dir: impl AsRef<Path>, output_name: &str) -> Self {
        Self {
            project_dir: project_dir.as_ref().to_path_buf(),
            output_name: output_name.to_string(),
            config: ObfuscationConfig::default(),
            optimization: Some("release".to_string()),
            use_plugin: true,
            use_nightly: true,
        }
    }

    /// Set the obfuscation configuration
    pub fn config(mut self, config: ObfuscationConfig) -> Self {
        self.config = config;
        self
    }

    /// Set optimization level ("debug" or "release")
    pub fn optimization(mut self, opt: &str) -> Self {
        self.optimization = Some(opt.to_string());
        self
    }

    /// Disable using the amice plugin (for baseline comparison)
    pub fn without_plugin(mut self) -> Self {
        self.use_plugin = false;
        self
    }

    /// Use stable rustc instead of nightly (plugin will not be loaded)
    pub fn use_stable(mut self) -> Self {
        self.use_nightly = false;
        self.use_plugin = false;
        self
    }

    /// Compile the Rust project
    pub fn compile(self) -> CompileResult {
        // Determine binary path first
        let profile = self.optimization.as_deref().unwrap_or("debug");
        let target_dir = self.project_dir.join("target").join(profile);

        #[cfg(target_os = "windows")]
        let binary_path = target_dir.join(format!("{}.exe", &self.output_name));

        #[cfg(not(target_os = "windows"))]
        let binary_path = target_dir.join(&self.output_name);

        // Clean the package to force recompilation.
        // This is necessary because Cargo doesn't track environment variable changes
        // (like AMICE_* config) as reasons to recompile, and even deleting the binary
        // isn't enough since Cargo will relink from cached .rlib/.rmeta files.
        let _ = Command::new("cargo")
            .arg("clean")
            .current_dir(&self.project_dir)
            .output();

        let mut cmd = Command::new("cargo");

        // Disable incremental compilation to ensure plugin changes take effect.
        // This is critical because rustc's incremental compilation doesn't track
        // changes to LLVM plugins, leading to stale cached results.
        cmd.env("CARGO_INCREMENTAL", "0");

        // Set toolchain
        if self.use_nightly {
            cmd.arg("+nightly");
        }

        // Basic command
        cmd.arg("rustc");

        // Set optimization
        if let Some(ref opt) = self.optimization {
            if opt == "release" {
                cmd.arg("--release");
            }
        }

        // Apply obfuscation config
        self.config.apply_to_command(&mut cmd);

        // Add plugin if enabled (nightly only)
        if self.use_plugin && self.use_nightly {
            let plugin = plugin_path();
            cmd.arg("--");
            cmd.arg(format!("-Zllvm-plugins={}", plugin.display()));
            cmd.arg("-Cpasses=");
            // Always emit LLVM IR for debugging/analysis
            cmd.arg("--emit=llvm-ir,link");
        }

        // Set working directory
        cmd.current_dir(&self.project_dir);

        let output = cmd.output().expect("Failed to execute cargo rustc");

        CompileResult { output, binary_path }
    }
}
