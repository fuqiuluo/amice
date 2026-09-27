# Developing Amice

English | [简体中文](Development_zh_CN.md)

This guide is for contributors modifying, building or testing Amice itself. To use a release, start with [Quick Start](QuickStart_en_US.md).

## Source Builds

Amice is a Cargo workspace built with Rust, `llvm-plugin` and `inkwell`. The main plugin crate is `crates/amice`; release builds produce `target/release/libamice.so` on Linux, `target/release/libamice.dylib` on macOS or `target/release/amice.dll` on Windows.

See [Building from Source](LLVMSetup_en_US.md) for dependencies, LLVM feature mapping and platform commands. The default LLVM feature is `llvm21-1`.

## Rust Test Harness Boundaries

When rustc and the plugin use different LLVM majors, the test harness can use a dedicated “emit IR → opt → llc → link” flow with specific IR compatibility handling. This does not establish support for arbitrary Rust applications. `AMICE_RUST_TOOLCHAIN` selects the test toolchain without changing the plugin's LLVM feature.

## Testing

Integration tests invoke clang with the release plugin, so use `--release`. The test script auto-detects `llvm-config` and also honors `LLVM_SYS_*_PREFIX`.

```bash
# Linux/macOS: build and run all tests
./crates/amice/tests/scripts/run_tests.sh --build

# Run tests matching a name
./crates/amice/tests/scripts/run_tests.sh -v string

# Use cargo directly
cargo test --release --no-default-features --features llvm21-1
cargo test --release --no-default-features --features llvm21-1 --test string_encryption
cargo test --release --no-default-features --features llvm21-1 test_md5

# LLVM 22 example
LLVM_SYS_221_PREFIX=/usr/lib64/llvm22 cargo test --release --no-default-features --features llvm22-1
```

On Windows, complete the [native opt verification](LLVMSetup_en_US.md#windows) first. See [crates/amice/tests/README.md](../crates/amice/tests/README.md) for the scope of the full test scripts.

---

## Project Layout

| Path | Description |
|:---|:---|
| `crates/amice` | Main clang pass plugin and obfuscation pass implementations |
| `crates/amice-llvm` | LLVM/inkwell extension layer and C++ FFI glue |
| `crates/amice-macro` | `#[amice(...)]` pass registration and config macros |
| `crates/amice-plugin` | Pass manager / pass builder adapter layer |
| `crates/amice-plugin-macros` | Macros for the plugin adapter layer |
| `crates/amice-build-support` | Build-time LLVM detection helpers |
| `docs` | Build, environment variable, function annotation, Android NDK, and troubleshooting docs |
| `scripts` | Android NDK bundle packaging and helper build scripts |

---
