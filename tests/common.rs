//! Common utilities for integration tests

use std::process::Command;

/// Build the amice plugin with proper LLVM configuration
pub fn build_amice() {
    let mut cmd = Command::new("cargo");
    cmd.arg("build").arg("--release");

    // Detect and apply LLVM-specific configuration from environment
    if let Some((llvm_env_var, llvm_feature)) = detect_llvm_config() {
        if let Ok(llvm_prefix) = std::env::var(&llvm_env_var) {
            cmd.env(&llvm_env_var, llvm_prefix);
        }
        cmd.arg("--no-default-features").arg("--features").arg(llvm_feature);
    }

    let output = cmd.output().expect("Failed to execute cargo build command");
    if !output.status.success() {
        eprintln!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
    }
    assert!(output.status.success(), "Cargo build failed");
}

/// Detect available LLVM configuration from environment variables
pub fn detect_llvm_config() -> Option<(String, String)> {
    // Check for specific LLVM environment variables in order of preference
    let llvm_versions = [
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
        if std::env::var(env_var).is_ok() {
            return Some((env_var.to_string(), feature.to_string()));
        }
    }

    None
}

/// Test structure for comparing normal vs obfuscated compilation
pub struct CompilationTest {
    pub c_file: &'static str,
    pub test_name: &'static str,
}

impl CompilationTest {
    pub fn new(c_file: &'static str, test_name: &'static str) -> Self {
        Self { c_file, test_name }
    }

    /// Compile the C file normally and return the output
    pub fn compile_normal(&self) -> String {
        let output = Command::new("clang")
            .arg(self.c_file)
            .arg("-o")
            .arg(format!("target/test_{}_normal", self.test_name))
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Normal compilation failed");

        let output = Command::new(format!("./target/test_{}_normal", self.test_name))
            .output()
            .expect("Failed to execute normal binary");
        
        // For some tests, the program returns non-zero but still produces correct output
        if !output.status.success() && output.stdout.is_empty() {
            panic!("Normal execution failed with no output");
        }

        String::from_utf8_lossy(&output.stdout).to_string()
    }

    /// Compile the C file with obfuscation enabled and return the output
    pub fn compile_with_obfuscation(&self, env_vars: &[(&str, &str)]) -> String {
        let mut cmd = Command::new("clang");
        
        // Add environment variables for obfuscation
        for (key, value) in env_vars {
            cmd.env(key, value);
        }
        
        cmd.arg("-fpass-plugin=target/release/libamice.so")
            .arg(self.c_file)
            .arg("-o")
            .arg(format!("target/test_{}_obfuscated", self.test_name));

        let output = cmd.output().expect("Failed to execute clang command");
        assert!(output.status.success(), "Obfuscated compilation failed");

        let output = Command::new(format!("./target/test_{}_obfuscated", self.test_name))
            .output()
            .expect("Failed to execute obfuscated binary");
        
        // For some tests, the program returns non-zero but still produces correct output
        if !output.status.success() && output.stdout.is_empty() {
            panic!("Obfuscated execution failed with no output");
        }

        String::from_utf8_lossy(&output.stdout).to_string()
    }

    /// Test that obfuscation preserves program behavior
    pub fn assert_output_preserved(&self, env_vars: &[(&str, &str)]) {
        build_amice();
        
        let normal_output = self.compile_normal();
        let obfuscated_output = self.compile_with_obfuscation(env_vars);
        
        assert_eq!(
            normal_output.trim(),
            obfuscated_output.trim(),
            "Obfuscation changed program behavior for test: {}",
            self.test_name
        );
    }

    /// Test that obfuscation preserves program behavior, ignoring memory addresses
    pub fn assert_output_preserved_ignore_addresses(&self, env_vars: &[(&str, &str)]) {
        build_amice();
        
        let normal_output = self.compile_normal();
        let obfuscated_output = self.compile_with_obfuscation(env_vars);
        
        let normal_normalized = normalize_output(&normal_output);
        let obfuscated_normalized = normalize_output(&obfuscated_output);
        
        assert_eq!(
            normal_normalized.trim(),
            obfuscated_normalized.trim(),
            "Obfuscation changed program behavior for test: {}",
            self.test_name
        );
    }

    /// Test compilation only (for cases where we verify pass runs via logs)
    #[allow(dead_code)]
    pub fn test_compilation_only(&self, env_vars: &[(&str, &str)]) {
        build_amice();
        
        let mut cmd = Command::new("clang");
        
        // Add environment variables for obfuscation
        for (key, value) in env_vars {
            cmd.env(key, value);
        }
        
        cmd.arg("-fpass-plugin=target/release/libamice.so")
            .arg(self.c_file)
            .arg("-o")
            .arg(format!("target/test_{}_compiled", self.test_name));

        let output = cmd.output().expect("Failed to execute clang command");
        assert!(output.status.success(), "Compilation with obfuscation failed");
    }
}

/// Normalize output by replacing memory addresses with placeholders
fn normalize_output(output: &str) -> String {
    use regex::Regex;
    
    // Replace hexadecimal addresses (0x followed by hex digits) with <ADDRESS>
    let addr_regex = Regex::new(r"0x[0-9a-fA-F]+").unwrap();
    let normalized = addr_regex.replace_all(output, "<ADDRESS>");
    
    // Replace (nil) and any other nil-like patterns
    let nil_regex = Regex::new(r"\(nil\)").unwrap();
    let normalized = nil_regex.replace_all(&normalized, "<NIL>");
    
    normalized.to_string()
}