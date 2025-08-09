#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn setup_environment(cmd: &mut Command) {
        cmd.env("AMICE_SHUFFLE_BLOCKS", "true");
        cmd.env("AMICE_SHUFFLE_BLOCKS_FLAGS", "random");

        cmd.env("AMICE_SPLIT_BASIC_BLOCK", "true");

        cmd.env("AMICE_INDIRECT_BRANCH", "true");
        cmd.env("AMICE_INDIRECT_BRANCH_FLAGS", "dummy_block");

        cmd.env("AMICE_STRING_ENCRYPTION", "false");
        cmd.env("AMICE_STRING_ALGORITHM", "xor");
        cmd.env("AMICE_STRING_DECRYPT_TIMING", "lazy");
        cmd.env("AMICE_STRING_STACK_ALLOC", "false");
        cmd.env("AMICE_STRING_INLINE_DECRYPT_FN", "true");

        cmd.env("AMICE_INDIRECT_CALL", "true");

        cmd.env("AMICE_VM_FLATTEN", "true");

        let mut output_env = String::new();
        for x in cmd.get_envs() {
            output_env.push_str(&format!(
                "export {}={}\n",
                x.0.to_str().unwrap(),
                x.1.unwrap().to_str().unwrap()
            ));
        }
        let output_path = Path::new("tests/test_md5.env");
        std::fs::write(output_path, output_env).unwrap();
    }

    #[test]
    fn test_md5() {
        let output = Command::new("cargo")
            .arg("build")
            .arg("--release")
            .output()
            .expect("Failed to execute cargo build command");
        assert!(output.status.success(), "Cargo build failed");

        let path = PathBuf::from("target/md5");
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }

        let mut output = Command::new("clang");
        setup_environment(&mut output);
        let output = output
            .arg("-fpass-plugin=target/release/libamice.so")
            .arg("tests/md5.c")
            .arg("-o")
            .arg("target/md5")
            .output()
            .expect("Failed to execute clang command");
        if !output.status.success() {
            println!("Clang output: {}", String::from_utf8_lossy(&output.stderr));
        }
        assert!(output.status.success(), "Clang command failed");

        check_output()
    }

    fn check_output() {
        let output = Command::new("./target/md5")
            .output()
            .expect("Failed to execute const_strings binary");
        assert!(output.status.success(), "Const strings test failed");

        let stdout = String::from_utf8_lossy(&output.stdout);

        let stdout = stdout
            .split("\n")
            .filter(|line| !line.is_empty())
            .map(|line| line.trim())
            .collect::<Vec<&str>>();

        for x in stdout {
            println!("{}", x);
        }

        assert_eq!(stdout.len(), 8, "Expected 8 lines of output, found: {}", stdout.len());
        assert_eq!(stdout[0], "MD5(\"\") = 906adc8dc99e0b7e4de1afd68e879d9f");
        assert_eq!(stdout[1], "MD5(\"a\") = bd3cfa105b77fc3af680893c16c78324");
        assert_eq!(stdout[2], "MD5(\"abc\") = 59e8f1e370c55438207d937eb139eb8e");
        assert_eq!(stdout[3], "MD5(\"message digest\") = 82330f944531d1fb7027004b1091b8fe");
        assert_eq!(
            stdout[4],
            "MD5(\"abcdefghijklmnopqrstuvwxyz\") = 3c57117f1842e973ea2072eafc16c943"
        );
        assert_eq!(
            stdout[5],
            "MD5(\"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789\") = 48942efc4110c7c2af48ee9b2c979b20"
        );
        assert_eq!(stdout[6], "MD5(\"1234567890\") = ffa0f5838119587bf1323320e58298d0");
        assert_eq!(stdout[7], "MD5([00 01 02 FF]) = f1a5df091e9edf48f6d359fa9ff723b7");
    }
}
