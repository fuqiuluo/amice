# Rust Usage

English | [简体中文](RustUsage_zh_CN.md)

## Scope and Toolchain

The direct-loading example below targets Linux/macOS and uses nightly's `-Zllvm-plugins`. This is an unstable compiler interface; adding AMICE as a normal application dependency does not load the plugin. See the [Rust Unstable Book](https://doc.rust-lang.org/unstable-book/compiler-flags/llvm-plugins.html).

Choose an installed nightly, inspect its LLVM version, then build the corresponding plugin using [LLVM Setup](LLVMSetup_en_US.md). The LLVM major reported by `rustc -vV` must match `llvm-config --version`. This is necessary but not sufficient: Rust's LLVM fork, symbols and shared libraries must also be compatible. Renaming a plugin or changing a prefix variable does not fix an ABI mismatch.

```bash
rustup toolchain list
AMICE_RUST_TOOLCHAIN=nightly
# For a pinned toolchain, use an installed nightly-YYYY-MM-DD instead
rustc +"$AMICE_RUST_TOOLCHAIN" -vV
```

If needed, install a suitable toolchain with `rustup toolchain install <toolchain-name>` and inspect its version. The latest nightly may use an unsupported LLVM and is not guaranteed to match the default LLVM 21.

The following assumes you already built the plugin with the matching LLVM. Set actual absolute paths; on macOS use `.dylib` instead of `.so`. Since the `-Zllvm-plugins` list is space-separated, use a plugin path without spaces for this example.

```bash
AMICE_LLVM=/usr/lib/llvm-21
AMICE_PLUGIN=/absolute/path/to/amice/target/release/libamice.so
test -f "$AMICE_PLUGIN" || exit 1
AMICE_RUST_LLVM=$(rustc +"$AMICE_RUST_TOOLCHAIN" -vV | awk '/^LLVM version:/ {split($3,v,"."); print v[1]}')
AMICE_PLUGIN_LLVM=$("$AMICE_LLVM/bin/llvm-config" --version | cut -d. -f1)
if [ -z "$AMICE_RUST_LLVM" ] || [ "$AMICE_RUST_LLVM" != "$AMICE_PLUGIN_LLVM" ]; then
  echo "ERROR: rustc and plugin LLVM versions do not match"
  exit 1
fi
```

## Minimal Cargo Example

Create a fresh temporary project outside the repository to avoid modifying AMICE's workspace. Environment and plugin changes are not automatically tracked by Cargo, so the two builds use separate output directories.

```bash
AMICE_DEMO=$(mktemp -d)
cargo +"$AMICE_RUST_TOOLCHAIN" new --bin --name amice-hello "$AMICE_DEMO/hello" || exit 1
cd "$AMICE_DEMO/hello" || exit 1
cat > src/main.rs <<'RS'
fn main() {
    println!("AMICE_RUST_STRING_TEST");
}
RS

cargo +"$AMICE_RUST_TOOLCHAIN" build --release --target-dir target/plain || exit 1
RUST_LOG=amice=info CARGO_INCREMENTAL=0 \
AMICE_STRING_ENCRYPTION=true AMICE_STRING_ONLY_DOT_STRING=false \
cargo +"$AMICE_RUST_TOOLCHAIN" rustc --release --bin amice-hello \
  --target-dir target/with-amice -- \
  "-Zllvm-plugins=$AMICE_PLUGIN" -Cpasses= -Ccodegen-units=1 --emit=llvm-ir,link || exit 1

./target/plain/release/amice-hello > target/plain.txt || exit 1
./target/with-amice/release/amice-hello > target/with-amice.txt || exit 1
cmp target/plain.txt target/with-amice.txt || exit 1
cat target/with-amice.txt
"$AMICE_LLVM/bin/llvm-strings" target/plain/release/amice-hello > target/plain.strings || exit 1
"$AMICE_LLVM/bin/llvm-strings" target/with-amice/release/amice-hello > target/with-amice.strings || exit 1
grep -Fq AMICE_RUST_STRING_TEST target/plain.strings || exit 1
if grep -Fq AMICE_RUST_STRING_TEST target/with-amice.strings; then
  echo "ERROR: string encryption did not hide the marker"
  exit 1
fi
echo "PASS: same output, plaintext marker hidden"
```

Success prints `AMICE_RUST_STRING_TEST` and `PASS: same output, plaintext marker hidden`. Inspect the final binary; the source and baseline outputs still contain the original string.

## Existing Projects

- `cargo rustc --bin <name> -- ...` applies these arguments to the selected target, not automatically to every dependency or the standard library. For a library use `--lib`; dependency crates still need separate planning.
- Enable one pass first and compare existing tests, output, size and runtime before adding options. Rust string encryption usually needs `AMICE_STRING_ONLY_DOT_STRING=false`; not every global byte array is a safe string candidate.
- After changing `AMICE_*`, a config file or the plugin, use a fresh `--target-dir`, or clean that dedicated output directory before rebuilding, to avoid cached results.
- Use `RUST_LOG=amice=debug` for diagnosis. Passes such as VMP can skip unsupported functions; successful compilation does not prove a function was transformed.
- C/C++ `__attribute__((annotate(...)))` syntax cannot be pasted into Rust. This guide uses global configuration and does not claim a corresponding stable Rust annotation syntax.

## Windows and Test Harness Boundaries

DLLs built with `win-link-opt` / `win-link-lld` are tied to their LLVM host. Do not pass them directly to rustc's `-Zllvm-plugins`. For Windows C/C++ validation, use the [native opt example](LLVMSetup_en_US.md#windows). Direct Windows Rust loading is not a supported introductory path in this guide.

The repository's Rust tests can use a dedicated “emit IR → opt → llc → link” fallback when LLVM majors differ. That flow contains specific IR compatibility handling and is not a promise that arbitrary Rust projects work across LLVM versions. The test harness's `AMICE_RUST_TOOLCHAIN` selects its Rust toolchain; it does not change the plugin's LLVM feature.
