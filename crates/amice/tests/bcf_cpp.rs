//! C++ ABI and lifetime coverage across ordinary compilation, sanitizers and LTO.
mod common;

use common::{ObfuscationConfig, detect_llvm_config, ensure_plugin_built, output_dir, plugin_path, tests_root};
use std::{path::PathBuf, process::Command};

fn tool(name: &str) -> PathBuf {
    let name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    detect_llvm_config()
        .map(|config| PathBuf::from(config.prefix).join("bin").join(&name))
        .unwrap_or_else(|| name.into())
}

fn run(command: &mut Command) -> String {
    let output = command.output().expect("start C++ boundary command");
    assert!(
        output.status.success(),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn configure(command: &mut Command) {
    ObfuscationConfig::disabled().apply_to_command(command);
    command
        .env_remove("AMICE_CONFIG_PATH")
        .env("AMICE_PASS_ORDER", "BogusControlFlow")
        .env("AMICE_BOGUS_CONTROL_FLOW", "true")
        .env("AMICE_BOGUS_CONTROL_FLOW_PROB", "100")
        .env("AMICE_BOGUS_CONTROL_FLOW_CLONE", "true")
        .env("AMICE_BOGUS_CONTROL_FLOW_SEED", "42");
}

#[test]
fn cpp_lifetimes_abi_and_comdat_survive_compilation_and_lto() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-cpp");
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures = tests_root().join("c/fixtures/control_flow");
    let mut variants = vec![
        ("debug", vec!["-O0", "-g"]),
        ("optimized", vec!["-O2", "-g"]),
        ("size", vec!["-Oz", "-g"]),
    ];
    if cfg!(target_os = "linux") {
        variants.extend([
            (
                "sanitized",
                vec!["-O1", "-g", "-fsanitize=address,undefined", "-fno-sanitize-recover=all"],
            ),
            ("thin", vec!["-O2", "-g", "-flto=thin"]),
            ("full", vec!["-O3", "-g", "-flto=full"]),
        ]);
    }
    for (name, flags) in variants {
        let binary = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        let mut command = Command::new(tool("clang++"));
        configure(&mut command);
        command
            .args([
                "-std=c++20",
                "-pthread",
                "-fno-omit-frame-pointer",
                "-fstack-protector-strong",
            ])
            .args(&flags)
            .arg(format!("-fpass-plugin={}", plugin_path().display()))
            .arg(fixtures.join("bcf_cpp.cpp"))
            .arg(fixtures.join("bcf_cpp_main.cpp"))
            .arg("-o")
            .arg(&binary);
        if matches!(name, "thin" | "full") {
            command
                .arg("-fuse-ld=lld")
                .arg(format!("-Wl,--load-pass-plugin={}", plugin_path().display()));
        }
        run(&mut command);
        let output = run(&mut Command::new(binary));
        assert!(output.contains("PASS C++"), "{name}: {output}");
        println!("{name}: {output}");
    }
    // Inspect emitted IR as well: a passing executable alone could mean the pass
    // never ran, or that exception handling made it skip the entire fixture.
    let ir = dir.join("optimized.ll");
    let mut command = Command::new(tool("clang++"));
    configure(&mut command);
    run(command
        .args([
            "-std=c++20",
            "-O2",
            "-g",
            "-fno-discard-value-names",
            "-S",
            "-emit-llvm",
        ])
        .arg(format!("-fpass-plugin={}", plugin_path().display()))
        .arg(fixtures.join("bcf_cpp.cpp"))
        .arg("-o")
        .arg(&ir));
    let text = std::fs::read_to_string(&ir).unwrap();
    assert!(text.contains("bcf.condition"));
    assert!(text.contains("bcf.digits"));
    assert!(text.contains("personality"));
    assert!(!text.contains("@__amice_bcf_"));
    run(Command::new(tool("opt"))
        .args(["-passes=verify", "-disable-output"])
        .arg(ir));
}

#[test]
fn cpp_windows_funclets_inalloca_and_cross_target_objects_verify() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-cpp-abi");
    std::fs::create_dir_all(&dir).unwrap();
    let fixture = tests_root().join("c/fixtures/control_flow/bcf_cpp_abi.cpp");
    let targets = run(Command::new(tool("clang++")).arg("--print-targets"));
    for (backend, triple) in [
        ("x86", "i686-pc-windows-msvc"),
        ("x86-64", "x86_64-pc-windows-msvc"),
        ("aarch64", "aarch64-pc-windows-msvc"),
        ("aarch64", "aarch64-linux-android"),
        ("arm", "armv7-linux-androideabi"),
        ("riscv64", "riscv64-unknown-linux-gnu"),
    ] {
        if !targets
            .lines()
            .any(|line| line.split_whitespace().next() == Some(backend))
        {
            eprintln!("Skipping {triple}: this Clang has no {backend} backend");
            continue;
        }
        let original = dir.join(format!("{triple}.ll"));
        let transformed = dir.join(format!("{triple}.bcf.ll"));
        run(Command::new(tool("clang++"))
            .arg(format!("--target={triple}"))
            .args([
                "-std=c++20",
                "-fms-extensions",
                "-O1",
                "-g",
                "-fno-discard-value-names",
                "-S",
                "-emit-llvm",
            ])
            .arg(&fixture)
            .arg("-o")
            .arg(&original));
        let mut command = Command::new(tool("opt"));
        configure(&mut command);
        run(command
            .arg(format!("--load-pass-plugin={}", plugin_path().display()))
            .args(["-passes=default<O0>", "-verify-each", "-S"])
            .arg(&original)
            .arg("-o")
            .arg(&transformed));
        let text = std::fs::read_to_string(&transformed).unwrap();
        assert!(text.contains("bcf.condition"), "{triple}");
        assert!(text.contains("personality"), "{triple}");
        assert!(!text.contains("@__amice_bcf_"));
        if triple.contains("windows") {
            assert!(text.contains("catchswitch"), "{triple}");
            assert!(text.contains("dllexport"), "{triple}");
        }
        if triple == "i686-pc-windows-msvc" {
            assert!(text.contains("inalloca"));
        }
        run(Command::new(tool("clang++"))
            .arg(format!("--target={triple}"))
            .args(["-O2", "-c"])
            .arg(&transformed)
            .arg("-o")
            .arg(dir.join(format!("{triple}.o"))));
    }
}
