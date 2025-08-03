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
        let output = Command::new("./target/const_strings")
            .output()
            .expect("Failed to execute const_strings binary");
        assert!(output.status.success(), "Const strings test failed");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let stdout = stdout.split('\n').collect::<Vec<&str>>();

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
}