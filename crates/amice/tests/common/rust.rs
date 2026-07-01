//! Rust compilation utilities for amice integration tests.

#![allow(dead_code)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use super::{
    CompileResult, LlvmConfig, ObfuscationConfig, detect_llvm_config, llvm_major_from_feature, plugin_path,
    sanitize_ir_for_llvm21,
};

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
        let profile = self.profile_name().to_string();
        let target_dir = self.project_dir.join("target").join(&profile);

        #[cfg(target_os = "windows")]
        let binary_path = target_dir.join(format!("{}.exe", &self.output_name));

        #[cfg(not(target_os = "windows"))]
        let binary_path = target_dir.join(&self.output_name);

        let llvm_config = detect_llvm_config();
        self.clean_package(llvm_config.as_ref());

        if let Some(config) = llvm_config.as_ref()
            && self.needs_opt_fallback(config)
        {
            return self.compile_with_opt_fallback(config, &profile, binary_path);
        }

        let output = self.run_cargo_rustc_direct(llvm_config.as_ref(), &profile);

        CompileResult { output, binary_path }
    }

    fn profile_name(&self) -> &str {
        self.optimization.as_deref().unwrap_or("debug")
    }

    fn clean_package(&self, config: Option<&LlvmConfig>) {
        let mut cmd = Command::new("cargo");
        self.add_cargo_toolchain(&mut cmd);
        if let Some(config) = config {
            cmd.env(&config.env_var, &config.prefix);
        }

        // Cargo does not track AMICE_* or LLVM plugin changes as rebuild inputs.
        let _ = cmd.arg("clean").current_dir(&self.project_dir).output();
    }

    fn run_cargo_rustc_direct(&self, config: Option<&LlvmConfig>, profile: &str) -> Output {
        let mut cmd = Command::new("cargo");
        cmd.env("CARGO_INCREMENTAL", "0");
        cmd.env("CCACHE_DISABLE", "1");
        if let Some(config) = config {
            cmd.env(&config.env_var, &config.prefix);
        }

        self.add_cargo_toolchain(&mut cmd);

        cmd.arg("rustc");

        if profile == "release" {
            cmd.arg("--release");
        }

        self.config.apply_to_command(&mut cmd);

        if self.use_plugin && self.use_nightly {
            cmd.env("RUSTC_BOOTSTRAP", "1");
            let plugin = plugin_path();
            cmd.arg("--");
            cmd.arg(format!("-Zllvm-plugins={}", plugin.display()));
            cmd.arg("-Cpasses=");
            // Always emit LLVM IR for debugging/analysis
            cmd.arg("--emit=llvm-ir,link");
        }

        cmd.current_dir(&self.project_dir);
        cmd.output().expect("Failed to execute cargo rustc")
    }

    fn add_cargo_toolchain(&self, cmd: &mut Command) {
        if let Some(toolchain) = rust_toolchain_override() {
            cmd.arg(format!("+{toolchain}"));
        } else if self.use_nightly && !self.use_plugin {
            cmd.arg("+nightly");
        }
    }

    fn needs_opt_fallback(&self, config: &LlvmConfig) -> bool {
        if !(self.use_plugin && self.use_nightly) {
            return false;
        }
        let toolchain = rust_toolchain_override();
        rustc_llvm_major(toolchain.as_deref())
            .map(|major| major != llvm_major_from_feature(&config.feature))
            .unwrap_or(false)
    }

    fn compile_with_opt_fallback(self, config: &LlvmConfig, profile: &str, binary_path: PathBuf) -> CompileResult {
        let target_dir = self.project_dir.join("target").join(profile);
        let fallback_dir = target_dir.join("amice-opt-fallback");
        fs::create_dir_all(&fallback_dir).expect("failed to create Rust opt fallback directory");

        let output = self.emit_input_ir(config, profile);
        if !output.status.success() {
            return CompileResult { output, binary_path };
        }

        let input_ir = self.locate_emitted_ir(&target_dir);
        sanitize_ir_for_llvm21(&input_ir);

        let optimized_ir = fallback_dir.join(format!("{}.opt.ll", self.output_name));
        let object = fallback_dir.join(format!("{}.opt.o", self.output_name));

        let output = self.run_opt(config, profile, &input_ir, &optimized_ir);
        if !output.status.success() {
            return CompileResult { output, binary_path };
        }

        let output = self.run_llc(config, &optimized_ir, &object);
        if !output.status.success() {
            return CompileResult { output, binary_path };
        }

        let output = self.link_pic_object(config, &object, &binary_path);
        CompileResult { output, binary_path }
    }

    fn emit_input_ir(&self, config: &LlvmConfig, profile: &str) -> Output {
        let mut cmd = Command::new("cargo");
        cmd.env("CARGO_INCREMENTAL", "0")
            .env("CCACHE_DISABLE", "1")
            .env(&config.env_var, &config.prefix);
        self.add_cargo_toolchain(&mut cmd);
        cmd.arg("rustc");
        if profile == "release" {
            cmd.arg("--release");
        }
        self.config.apply_to_command(&mut cmd);
        cmd.arg("--").arg("--emit=llvm-ir");
        cmd.current_dir(&self.project_dir);
        cmd.output().expect("Failed to emit Rust LLVM IR")
    }

    fn locate_emitted_ir(&self, target_dir: &Path) -> PathBuf {
        let deps_dir = target_dir.join("deps");
        let ir_files = fs::read_dir(&deps_dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", deps_dir.display()))
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "ll"))
            .collect::<Vec<_>>();

        let crate_prefix = format!("{}-", self.output_name);
        newest_path(
            ir_files
                .iter()
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(&crate_prefix))
                })
                .cloned(),
        )
        .or_else(|| newest_path(ir_files))
        .unwrap_or_else(|| panic!("cargo rustc did not emit LLVM IR under {}", deps_dir.display()))
    }

    fn run_opt(&self, config: &LlvmConfig, profile: &str, input_ir: &Path, optimized_ir: &Path) -> Output {
        let opt = Path::new(&config.prefix).join("bin").join("opt");
        let mut cmd = Command::new(opt);
        cmd.env("CCACHE_DISABLE", "1")
            .env(&config.env_var, &config.prefix)
            .arg(format!("--load-pass-plugin={}", plugin_path().display()))
            .arg(format!("-passes=default<{}>", Self::opt_pipeline_level(profile)))
            .arg("-S")
            .arg(input_ir)
            .arg("-o")
            .arg(optimized_ir);
        self.config.apply_to_command(&mut cmd);
        cmd.output().expect("Failed to execute opt")
    }

    fn run_llc(&self, config: &LlvmConfig, optimized_ir: &Path, object: &Path) -> Output {
        let llc = Path::new(&config.prefix).join("bin").join("llc");
        Command::new(llc)
            .env("CCACHE_DISABLE", "1")
            .env(&config.env_var, &config.prefix)
            .arg("-relocation-model=pic")
            .arg("-filetype=obj")
            .arg(optimized_ir)
            .arg("-o")
            .arg(object)
            .output()
            .expect("Failed to execute llc")
    }

    fn link_pic_object(&self, config: &LlvmConfig, object: &Path, binary_path: &Path) -> Output {
        let stub = object.with_extension("link.rs");
        fs::write(&stub, "#![no_main]\n").expect("failed to write Rust link stub");
        if let Some(parent) = binary_path.parent() {
            fs::create_dir_all(parent).expect("failed to create Rust binary output directory");
        }

        let mut cmd = Command::new("rustc");
        if let Some(toolchain) = rust_toolchain_override() {
            cmd.arg(format!("+{toolchain}"));
        }
        cmd.env("CCACHE_DISABLE", "1")
            .env(&config.env_var, &config.prefix)
            .arg("--edition=2021")
            .arg("--crate-name")
            .arg("amice_rust_link")
            .arg(&stub)
            .arg("-C")
            .arg(format!("link-arg={}", object.display()))
            .arg("-o")
            .arg(binary_path);
        cmd.output().expect("Failed to link Rust object")
    }

    fn opt_pipeline_level(profile: &str) -> &'static str {
        if profile == "release" { "O1" } else { "O0" }
    }
}

fn rust_toolchain_override() -> Option<String> {
    env::var("AMICE_RUST_TOOLCHAIN")
        .ok()
        .map(|toolchain| toolchain.trim().trim_start_matches('+').to_string())
        .filter(|toolchain| !toolchain.is_empty())
}

fn rustc_llvm_major(toolchain: Option<&str>) -> Option<u32> {
    let mut cmd = Command::new("rustc");
    if let Some(toolchain) = toolchain {
        cmd.arg(format!("+{}", toolchain.trim_start_matches('+')));
    }
    let output = cmd.arg("-vV").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().strip_prefix("LLVM version:"))
        .and_then(|version| version.trim().split('.').next())
        .and_then(|major| major.parse::<u32>().ok())
}

fn newest_path(paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    paths.into_iter().max_by_key(|path| modified_time(path))
}

fn modified_time(path: &Path) -> Option<SystemTime> {
    path.metadata().and_then(|metadata| metadata.modified()).ok()
}
