#[cfg(test)]
mod tests {
    use std::process::Command;

    fn build_amice() {
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

    fn detect_llvm_config() -> Option<(String, String)> {
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

    #[test]
    fn test_large_string_warning() {
        build_amice();

        // Test with a large string that exceeds 4KB threshold
        let output = Command::new("clang")
            .env("RUST_LOG", "warn")
            .env("AMICE_STRING_STACK_ALLOC", "true")
            .env("AMICE_STRING_ALGORITHM", "xor")
            .env("AMICE_STRING_DECRYPT_TIMING", "lazy")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/large_string.c")
            .arg("-o")
            .arg("target/large_string_test")
            .output()
            .expect("Failed to execute clang command");

        // Check that clang succeeded
        if !output.status.success() {
            eprintln!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
            eprintln!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        }
        assert!(output.status.success(), "Clang command failed");

        // Check the stderr for the expected warning message
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("exceeds 4KB limit for stack allocation, using global timing instead"),
            "Expected warning about 4KB limit not found in stderr: {}",
            stderr
        );

        // Verify the compiled binary runs successfully
        let run_output = Command::new("./target/large_string_test")
            .output()
            .expect("Failed to execute compiled binary");
        assert!(run_output.status.success(), "Compiled binary failed to run");

        // Check that the large string is properly decrypted
        let stdout = String::from_utf8_lossy(&run_output.stdout);
        assert!(stdout.contains("Large string length: 4744"), "Large string not properly decrypted");
        assert!(stdout.contains("Small string: This is a small string"), "Small string not properly decrypted");
    }

    #[test]
    fn test_small_strings_normal_behavior() {
        build_amice();

        // Test with normal small strings
        let output = Command::new("clang")
            .env("RUST_LOG", "warn")
            .env("AMICE_STRING_STACK_ALLOC", "true")
            .env("AMICE_STRING_ALGORITHM", "xor")
            .env("AMICE_STRING_DECRYPT_TIMING", "lazy")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/const_strings.c")
            .arg("-o")
            .arg("target/small_strings_test")
            .output()
            .expect("Failed to execute clang command");

        // Check that clang succeeded
        if !output.status.success() {
            eprintln!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
            eprintln!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        }
        assert!(output.status.success(), "Clang command failed");

        // Check the stderr - should NOT contain the 4KB warning for small strings
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("exceeds 4KB limit for stack allocation"),
            "Unexpected 4KB warning found for small strings in stderr: {}",
            stderr
        );

        // Should contain the normal stack allocation warning
        assert!(
            stderr.contains("using stack allocation for decryption"),
            "Expected normal stack allocation warning not found in stderr: {}",
            stderr
        );
    }
}