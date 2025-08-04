#[cfg(test)]
mod tests {
    use std::process::Command;

    #[test]
    fn test_repeated_strings() {
        let output = Command::new("cargo")
            .arg("build")
            .arg("--release")
            .output()
            .expect("Failed to execute cargo build command");
        assert!(output.status.success(), "Cargo build failed");

        let output = Command::new("clang")
            .arg("-S")
            .arg("-emit-llvm")
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/repeated_strings.c")
            .arg("-o")
            .arg("target/repeated_strings.ll")
            .output()
            .expect("Failed to execute clang command");
        assert!(output.status.success(), "Clang command failed");

        let ll = std::fs::read_to_string("target/repeated_strings.ll")
            .expect("Failed to read LLVM IR file");

        let count = ll.matches("call void @decrypt_strings").count();
        assert!(
            count > 0,
            "Expected at least one call to @decrypt_strings, found: {count}"
        );
    }
}
