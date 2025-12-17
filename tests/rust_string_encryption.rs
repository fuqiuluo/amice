mod common;

use std::fs;
use common::{ObfuscationConfig, RustCompileBuilder};
use serial_test::serial;

#[test]
#[serial]
fn test_rust_string_encryption_basic() {
    common::ensure_plugin_built();

    let project_dir = common::project_root()
        .join("tests")
        .join("rust")
        .join("string_encryption");

    // Test with obfuscation enabled
    let mut config = ObfuscationConfig::default();
    config.string_encryption = Some(true);
    config.string_only_llvm_string = Some(false);
    config.string_algorithm = Some("xor".to_string());

    let result = RustCompileBuilder::new(&project_dir, "string_encryption_test")
        .config(config)
        .compile();

    result.assert_success();

    let run_result = result.run();
    run_result.assert_success();

    let output = run_result.stdout();
    //fs::write("aaa.log", &run_result.output.stdout).unwrap();

    println!("Output:\n{}", output);

    assert!(output.contains("Hello, World!"));
    assert!(output.contains("Message length: 24"));
    assert!(output.contains("Welcome, User!"));
    assert!(output.contains("Obfuscated String"));
    assert!(output.contains("你好，世界！🦀"));
    assert!(output.contains("All tests completed!"));
}
//
// #[test]
// #[serial]
// fn test_rust_string_encryption_simd() {
//     common::ensure_plugin_built();
//
//     let project_dir = common::project_root()
//         .join("tests")
//         .join("rust")
//         .join("string_encryption");
//
//     // Test with obfuscation enabled
//     let mut config = ObfuscationConfig::default();
//     config.string_encryption = Some(true);
//     config.string_only_llvm_string = Some(false);
//     config.string_algorithm = Some("simd_xor".to_string());
//
//     let result = RustCompileBuilder::new(&project_dir, "string_encryption_test")
//         .config(config)
//         .compile();
//
//     result.assert_success();
//
//     let run_result = result.run();
//     run_result.assert_success();
//
//     let output = run_result.stdout();
//     assert!(output.contains("Hello, World!"));
//     assert!(output.contains("Message length: 24"));
//     assert!(output.contains("Welcome, User!"));
//     assert!(output.contains("Obfuscated String"));
//     assert!(output.contains("你好，世界！🦀"));
//     assert!(output.contains("All tests completed!"));
// }
//
// #[test]
// #[serial]
// fn test_rust_string_encryption_vs_baseline() {
//     common::ensure_plugin_built();
//
//     let project_dir = common::project_root()
//         .join("tests")
//         .join("rust")
//         .join("string_encryption");
//
//     // Compile without obfuscation (baseline)
//     let baseline_result = RustCompileBuilder::new(&project_dir, "string_encryption_test")
//         .without_plugin()
//         .use_stable()
//         .compile();
//
//     baseline_result.assert_success();
//     let baseline_run = baseline_result.run();
//     baseline_run.assert_success();
//     let baseline_output = baseline_run.stdout();
//
//     // Compile with obfuscation
//     let mut config = ObfuscationConfig::default();
//     config.string_encryption = Some(true);
//     config.string_only_llvm_string = Some(false);
//     config.string_algorithm = Some("xor".to_string());
//
//     let obfuscated_result = RustCompileBuilder::new(&project_dir, "string_encryption_test")
//         .config(config)
//         .compile();
//
//     obfuscated_result.assert_success();
//     let obfuscated_run = obfuscated_result.run();
//     obfuscated_run.assert_success();
//     let obfuscated_output = obfuscated_run.stdout();
//
//     // Both versions should produce identical output
//     assert_eq!(
//         baseline_output, obfuscated_output,
//         "Baseline and obfuscated outputs differ"
//     );
// }
