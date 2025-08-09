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

    fn check_output() {
        let output = Command::new("./target/const_strings")
            .output()
            .expect("Failed to execute const_strings binary");
        assert!(output.status.success(), "Const strings test failed");

        let stdout = String::from_utf8_lossy(&output.stdout);

        let stdout = stdout.split('\n').collect::<Vec<&str>>();

        stdout.iter().for_each(|s| println!("{}", s));

        assert_eq!(stdout[0], "test1 (bytes): 68 65 6C 6C 6F 00 00 39 05 ");
        assert_eq!(stdout[1], "test1 string: hello");
        assert_eq!(stdout[2], "test1 int: 1337");
        assert_eq!(stdout[3], "test2 (bytes): 68 65 6C 6C 6F 00 00 39 05 00 00 ");
        assert_eq!(stdout[4], "test2 string: hello");
        assert_eq!(stdout[5], "test2 int: 1337");
        assert_eq!(stdout[6], "p1: (nil)");
        assert!(stdout[7].starts_with("p2: 0x"));
        assert_eq!(stdout[8], "1pu: corld");
        assert_eq!(stdout[9], "1pu: World");
        assert_eq!(stdout[10], "Xello world1");
        assert_eq!(stdout[11], "Xello world2");
        assert_eq!(stdout[12], "Hello world3");
        assert!(stdout[13].starts_with("This is a literal. 0x"));
        assert!(stdout[14].starts_with("This is a literal. 0x"));
    }

    #[test]
    fn test_const_strings_lazy_xor_stack() {
        build_amice();

        let output = Command::new("clang")
            .env("AMICE_STRING_STACK_ALLOC", "true")
            .env("AMICE_STRING_ALGORITHM", "xor")
            .env("AMICE_STRING_DECRYPT_TIMING", "lazy")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/const_strings.c")
            .arg("-o")
            .arg("target/const_strings")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        check_output();
    }

    #[test]
    fn test_const_strings_lazy_xor() {
        build_amice();

        let output = Command::new("clang")
            .env("AMICE_STRING_ALGORITHM", "xor")
            .env("AMICE_STRING_DECRYPT_TIMING", "lazy")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/const_strings.c")
            .arg("-o")
            .arg("target/const_strings")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        check_output();
    }

    #[test]
    fn test_const_strings_global_xor() {
        build_amice();

        let output = Command::new("clang")
            .env("AMICE_STRING_ALGORITHM", "xor")
            .env("AMICE_STRING_DECRYPT_TIMING", "global")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/const_strings.c")
            .arg("-o")
            .arg("target/const_strings")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        check_output();
    }

    #[test]
    fn test_const_strings_lazy_simd_xor() {
        build_amice();

        let output = Command::new("clang")
            .env("AMICE_STRING_ALGORITHM", "simd_xor")
            .env("AMICE_STRING_DECRYPT_TIMING", "lazy")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/const_strings.c")
            .arg("-o")
            .arg("target/const_strings")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        check_output();
    }

    #[test]
    fn test_const_strings_global_simd_xor() {
        build_amice();

        let output = Command::new("clang")
            .env("AMICE_STRING_ALGORITHM", "simd_xor")
            .env("AMICE_STRING_DECRYPT_TIMING", "global")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/const_strings.c")
            .arg("-o")
            .arg("target/const_strings")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        check_output();
    }
}
