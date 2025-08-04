#[cfg(test)]
mod tests {
    use std::process::Command;

    fn build_amice() {
        let output = Command::new("cargo")
            .arg("build")
            .arg("--release")
            .output()
            .expect("Failed to execute cargo build command");
        assert!(output.status.success(), "Cargo build failed");
    }

    fn check_output() {
        let output = Command::new("./target/indirect_branch")
            .output()
            .expect("Failed to execute indirect_branch binary");
        assert!(output.status.success(), "Const strings test failed");

        let stdout = String::from_utf8_lossy(&output.stdout);

        let stdout = stdout.split('\n').collect::<Vec<&str>>();

        assert_eq!(stdout[0], "Running control flow test suite...");
        assert_eq!(stdout[1], "All tests completed. sink = 1");
    }

    #[test]
    fn test_const_strings_lazy_xor_stack() {
        build_amice();

        let output = Command::new("clang")
            .env("AMICE_STRING_STACK_ALLOC", "true")
            .env("AMICE_STRING_ALGORITHM", "xor")
            .env("AMICE_STRING_DECRYPT_TIMING", "lazy")
            .env("AMICE_INDIRECT_BRANCH", "true")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/indirect_branch.c")
            .arg("-o")
            .arg("target/indirect_branch")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        check_output();
    }
}
