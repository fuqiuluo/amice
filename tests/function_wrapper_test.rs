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

    fn check_output(binary_name: &str) {
        let output = Command::new(format!("./target/{}", binary_name))
            .output()
            .expect("Failed to execute function_wrapper_test binary");
        assert!(output.status.success(), "Function wrapper test failed");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stdout_lines = stdout.split('\n').collect::<Vec<&str>>();

        // Print output for debugging
        stdout_lines.iter().for_each(|s| println!("{}", s));

        // Check expected output
        assert_eq!(stdout_lines[0], "Testing function wrapper pass");
        assert_eq!(stdout_lines[1], "In add function: 5 + 3");
        assert_eq!(stdout_lines[2], "Result of add: 8");
        assert_eq!(stdout_lines[3], "In multiply function: 4 * 7");
        assert_eq!(stdout_lines[4], "Result of multiply: 28");
        assert_eq!(stdout_lines[5], "Hello, Function Wrapper!");
    }

    #[test]
    fn test_function_wrapper_default() {
        build_amice();

        let output = Command::new("clang")
            .env("AMICE_FUNCTION_WRAPPER", "true")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/function_wrapper_test.c")
            .arg("-o")
            .arg("target/function_wrapper_default")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        check_output("function_wrapper_default");
    }

    #[test]
    fn test_function_wrapper_high_probability() {
        build_amice();

        let output = Command::new("clang")
            .env("AMICE_FUNCTION_WRAPPER", "true")
            .env("AMICE_FUNCTION_WRAPPER_PROBABILITY", "90")
            .env("AMICE_FUNCTION_WRAPPER_TIMES", "5")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/function_wrapper_test.c")
            .arg("-o")
            .arg("target/function_wrapper_high_prob")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        check_output("function_wrapper_high_prob");
    }

    #[test]
    fn test_function_wrapper_low_probability() {
        build_amice();

        let output = Command::new("clang")
            .env("AMICE_FUNCTION_WRAPPER", "true")
            .env("AMICE_FUNCTION_WRAPPER_PROBABILITY", "30")
            .env("AMICE_FUNCTION_WRAPPER_TIMES", "1")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/function_wrapper_test.c")
            .arg("-o")
            .arg("target/function_wrapper_low_prob")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        check_output("function_wrapper_low_prob");
    }

    #[test]
    fn test_function_wrapper_disabled() {
        build_amice();

        let output = Command::new("clang")
            .env("AMICE_FUNCTION_WRAPPER", "false")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/function_wrapper_test.c")
            .arg("-o")
            .arg("target/function_wrapper_disabled")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        check_output("function_wrapper_disabled");
    }

    #[test]
    fn test_function_wrapper_multiple_times() {
        build_amice();

        let output = Command::new("clang")
            .env("AMICE_FUNCTION_WRAPPER", "true")
            .env("AMICE_FUNCTION_WRAPPER_PROBABILITY", "100")
            .env("AMICE_FUNCTION_WRAPPER_TIMES", "7")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/function_wrapper_test.c")
            .arg("-o")
            .arg("target/function_wrapper_multiple")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        check_output("function_wrapper_multiple");
    }
}
