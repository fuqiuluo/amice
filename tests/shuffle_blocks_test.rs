//! Integration tests for shuffle blocks functionality
use std::path::Path;
use std::process::Command;

#[test]
fn test_shuffle_blocks_modes() {
    // Ensure we have the release build
    let plugin_path = "target/release/libamice.so";
    assert!(
        Path::new(plugin_path).exists(),
        "Plugin not found. Run: cargo build --release --no-default-features --features llvm18-1"
    );

    let test_file = "tests/shuffle_test.c";
    assert!(Path::new(test_file).exists(), "Test file not found");

    // Test normal compilation
    let output = Command::new("clang")
        .args([test_file, "-o", "target/test_normal"])
        .output()
        .expect("Failed to run clang");
    assert!(output.status.success(), "Normal compilation failed");

    // Test random mode
    let output = Command::new("clang")
        .env("AMICE_SHUFFLE_BLOCKS", "true")
        .env("AMICE_SHUFFLE_BLOCKS_FLAGS", "random")
        .args([
            "-fpass-plugin=target/release/libamice.so",
            test_file,
            "-o",
            "target/test_random",
        ])
        .output()
        .expect("Failed to run clang with random shuffle");
    assert!(output.status.success(), "Random shuffle compilation failed");

    // Test reverse mode
    let output = Command::new("clang")
        .env("AMICE_SHUFFLE_BLOCKS", "true")
        .env("AMICE_SHUFFLE_BLOCKS_FLAGS", "reverse")
        .args([
            "-fpass-plugin=target/release/libamice.so",
            test_file,
            "-o",
            "target/test_reverse",
        ])
        .output()
        .expect("Failed to run clang with reverse shuffle");
    assert!(output.status.success(), "Reverse shuffle compilation failed");

    // Test rotate mode
    let output = Command::new("clang")
        .env("AMICE_SHUFFLE_BLOCKS", "true")
        .env("AMICE_SHUFFLE_BLOCKS_FLAGS", "rotate")
        .args([
            "-fpass-plugin=target/release/libamice.so",
            test_file,
            "-o",
            "target/test_rotate",
        ])
        .output()
        .expect("Failed to run clang with rotate shuffle");
    assert!(output.status.success(), "Rotate shuffle compilation failed");

    // Test multiple modes
    let output = Command::new("clang")
        .env("AMICE_SHUFFLE_BLOCKS", "true")
        .env("AMICE_SHUFFLE_BLOCKS_FLAGS", "reverse,rotate")
        .args([
            "-fpass-plugin=target/release/libamice.so",
            test_file,
            "-o",
            "target/test_combined",
        ])
        .output()
        .expect("Failed to run clang with combined shuffle");
    assert!(output.status.success(), "Combined shuffle compilation failed");

    // Run the executables to ensure they work correctly
    let output = Command::new("./target/test_normal")
        .output()
        .expect("Failed to run normal test");
    assert!(output.status.success());
    let normal_output = String::from_utf8_lossy(&output.stdout);

    let output = Command::new("./target/test_random")
        .output()
        .expect("Failed to run random test");
    assert!(output.status.success());
    let random_output = String::from_utf8_lossy(&output.stdout);

    let output = Command::new("./target/test_reverse")
        .output()
        .expect("Failed to run reverse test");
    assert!(output.status.success());
    let reverse_output = String::from_utf8_lossy(&output.stdout);

    let output = Command::new("./target/test_rotate")
        .output()
        .expect("Failed to run rotate test");
    assert!(output.status.success());
    let rotate_output = String::from_utf8_lossy(&output.stdout);

    let output = Command::new("./target/test_combined")
        .output()
        .expect("Failed to run combined test");
    assert!(output.status.success());
    let combined_output = String::from_utf8_lossy(&output.stdout);

    // All outputs should be the same despite block shuffling
    assert_eq!(normal_output, random_output, "Random shuffle changed program behavior");
    assert_eq!(
        normal_output, reverse_output,
        "Reverse shuffle changed program behavior"
    );
    assert_eq!(normal_output, rotate_output, "Rotate shuffle changed program behavior");
    assert_eq!(
        normal_output, combined_output,
        "Combined shuffle changed program behavior"
    );
}
