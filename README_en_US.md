# Amice

English | [简体中文](README.md)

Amice is an LLVM pass plugin built with Rust, `llvm-plugin-rs`, and `inkwell`. It is loaded into the compiler through clang `-fpass-plugin` and applies compile-time obfuscation transforms to LLVM IR generated from C/C++, Rust, and other LLVM-based frontends.

The repository is now a Cargo workspace. The main plugin crate lives in `crates/amice`, and the release artifact is `target/release/libamice.so`, `target/release/libamice.dylib`, or `target/release/amice.dll`.

---

## Quick Start

### 1. Build the Plugin

The default LLVM feature is `llvm21-1`, so LLVM 21 development files or a usable `llvm-config` are required. LLVM 22.1 is also supported and can be selected explicitly with the `llvm22-1` feature.

```bash
# macOS
brew install llvm@21
export LLVM_SYS_211_PREFIX=$(brew --prefix llvm@21)

# Linux example: if llvm-config/llvm-config-21 is already in PATH, PREFIX is optional
# export LLVM_SYS_211_PREFIX=/usr/lib/llvm-21

cargo build --release
```

### 2. Inject into clang

```bash
cat > /tmp/amice_hello.c <<'SRC'
extern int puts(const char *);
int main(void) { return puts("AMICE_STRING_TEST") < 0; }
SRC

AMICE_STRING_ENCRYPTION=true \
clang -fpass-plugin="$(pwd)/target/release/libamice.so" /tmp/amice_hello.c -o /tmp/amice_hello
```

On macOS, replace the plugin path with `target/release/libamice.dylib`.

---

## Supported Obfuscations

| Pass | Environment Variable | C/C++ | Rust | ObjC | Description |
|:---|:---|:---:|:---:|:---:|:---|
| String Encryption | `AMICE_STRING_ENCRYPTION` | ✅ | ✅ | ⏳ | String encryption with `xor` / `simd_xor`, lazy/global decryption, stack/heap allocation options |
| Indirect Call | `AMICE_INDIRECT_CALL` | ✅ | ✅ | ❌ | Rewrites direct calls into table/index based indirect calls |
| Indirect Branch | `AMICE_INDIRECT_BRANCH` | ✅ | ✅ | ❌ | Rewrites branches into `indirectbr`; supports dummy blocks, table shuffling, index encryption, and other flags |
| Split Basic Block | `AMICE_SPLIT_BASIC_BLOCK` | ✅ | ✅ | ❌ | Splits basic blocks according to configuration |
| Lower Switch | `AMICE_LOWER_SWITCH` | ✅ | ✅ | ❌ | Lowers LLVM `switch` instructions |
| VM Flatten | `AMICE_VM_FLATTEN` | ✅ | ✅ | ❌ | VM-style control-flow flattening |
| Flatten | `AMICE_FLATTEN` | ✅ | ✅ | ❌ | Control-flow flattening with `basic` / `dominator` modes |
| MBA | `AMICE_MBA` | ✅ | ✅ | ❌ | Mixed Boolean-arithmetic expression rewriting |
| Bogus Control Flow | `AMICE_BOGUS_CONTROL_FLOW` | ✅ | ✅ | ❌ | Inserts bogus control flow; supports basic / polaris-primes modes |
| Function Wrapper | `AMICE_FUNCTION_WRAPPER` | ✅ | ✅ | ❌ | Creates wrapper functions and replaces call sites |
| Clone Function | `AMICE_CLONE_FUNCTION` | ✅ | ✅ | ❌ | Constant-argument specialization by function cloning |
| Alias Access | `AMICE_ALIAS_ACCESS` | ✅ | ✅ | ❌ | Pointer-chain based alias access obfuscation |
| Custom Calling Conv | `AMICE_CUSTOM_CALLING_CONV` | ⏳ | ⏳ | ❌ | Custom calling convention support, usually enabled per function by annotation |
| Delay Offset Loading | `AMICE_DELAY_OFFSET_LOADING` | ✅ | ⏳ | ❌ | Delayed GEP offset loading with optional XOR protection |
| Param Aggregate | `AMICE_PARAM_AGGREGATE` | ✅ | ⏳ | ❌ | Parameter aggregation obfuscation |
| Basic Block Outlining | `AMICE_BASIC_BLOCK_OUTLINING` | ✅ | ⏳ | ❌ | Extracts basic blocks into standalone helper functions, also known as BB2Func |
| Shuffle Blocks | `AMICE_SHUFFLE_BLOCKS` | ✅ | ⏳ | ❌ | Basic block reordering |

> Legend:
> - ✅ Supported
> - ⏳ In progress / planned / untested
> - ❌ Not planned
>
> Rust string encryption usually requires `AMICE_STRING_ONLY_DOT_STRING=false` (legacy alias: `AMICE_STRING_ONLY_LLVM_STRING=false`); see [Runtime Environment Variables](docs/EnvConfig_en_US.md).

See [Runtime Environment Variables](docs/EnvConfig_en_US.md) for all options and [Function Annotations](docs/FunctionAnnotations_en_US.md) for per-function enable/disable controls.

---

## Configuration

Amice applies configuration in this order:

1. If `AMICE_CONFIG_PATH` is set, load TOML/YAML/JSON from that file.
2. Otherwise, start from default configuration.
3. Overlay `AMICE_*` environment variables last. Environment variables have the highest priority.

Example:

```bash
cat > /tmp/amice.toml <<'TOML'
[string_encryption]
enable = true
algorithm = "xor"

[flatten]
enable = true
mode = "basic"
TOML

AMICE_CONFIG_PATH=/tmp/amice.toml \
clang -fpass-plugin="$(pwd)/target/release/libamice.so" input.c -o output
```

Pass execution order can be controlled with `AMICE_PASS_ORDER` or `pass_order.order` in the config file. When an explicit order is provided, only passes in the list run. See [Pass Execution Order](docs/PassOrder_en_US.md) for details.

---

## Build Guide

### Linux

```bash
# Fedora / RHEL
sudo dnf install llvm llvm-devel clang
cargo build --release

# Debian / Ubuntu, preferably install LLVM 21 from https://apt.llvm.org/
sudo apt install llvm-21 llvm-21-dev clang-21
# export LLVM_SYS_211_PREFIX=/usr/lib/llvm-21
cargo build --release
```

When using a non-default LLVM version, the Cargo feature and `LLVM_SYS_*_PREFIX` must match:

```bash
# LLVM 22 example
export LLVM_SYS_221_PREFIX=/usr/lib64/llvm22
cargo build --release --no-default-features --features llvm22-1

# LLVM 18 example
export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
cargo build --release --no-default-features --features llvm18-1
```

Supported LLVM features: `llvm11-0` through `llvm22-1`.

### macOS

```bash
brew install llvm@21
export LLVM_SYS_211_PREFIX=$(brew --prefix llvm@21)
cargo build --release

# LLVM 22 example
brew install llvm@22
export LLVM_SYS_221_PREFIX=$(brew --prefix llvm@22)
cargo build --release --no-default-features --features llvm22-1
```

### Windows

Official prebuilt LLVM packages usually do not support dynamic pass plugins directly. Build LLVM yourself or use a community build that supports plugin loading.

```powershell
setx LLVM_SYS_211_PREFIX "C:\llvm21"
cargo build --release --features win-link-lld
# Or link with opt: cargo build --release --features win-link-opt
```

If default features are disabled, pass the LLVM feature explicitly:

```powershell
cargo build --release --no-default-features --features llvm21-1,win-link-lld
```

### Android NDK

The plain Android NDK usually does not include the host `libLLVM.so` / `libLLVM.dylib` required to load the plugin. Prefer the Android NDK bundle from AMICE releases:

```bash
tar xf amice-android-ndk-r29-linux-x86_64.tar.gz
cd amice-android-ndk-r29-linux-x86_64

cat > hello.c <<'SRC'
extern int puts(const char *);
int main(void) { return puts("AMICE_NDK_STRING_TEST_20260603") < 0; }
SRC

AMICE_STRING_ENCRYPTION=true ./amice/bin/aarch64-linux-android-clang hello.c -o hello
file hello
if strings -a hello | grep -q 'AMICE_NDK_STRING_TEST_20260603'; then
  echo "ERROR: string encryption did not hide the marker"
  exit 1
fi
```

See [Android NDK Usage](docs/AndroidNDKSupport_en_US.md) for details.

---

## Testing

Integration tests invoke clang with the release plugin, so use `--release`. The test script auto-detects `llvm-config` and also honors `LLVM_SYS_*_PREFIX`.

```bash
# Build and run all tests
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

See [crates/amice/tests/README.md](crates/amice/tests/README.md) for more testing details.

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

## Documentation

| Topic | Document |
|:---|:---|
| LLVM setup | [docs/LLVMSetup_en_US.md](docs/LLVMSetup_en_US.md) |
| Runtime environment variables | [docs/EnvConfig_en_US.md](docs/EnvConfig_en_US.md) |
| Function annotations | [docs/FunctionAnnotations_en_US.md](docs/FunctionAnnotations_en_US.md) |
| Pass execution order | [docs/PassOrder_en_US.md](docs/PassOrder_en_US.md) |
| Android NDK | [docs/AndroidNDKSupport_en_US.md](docs/AndroidNDKSupport_en_US.md) |
| Troubleshooting | [docs/Troubleshooting_en_US.md](docs/Troubleshooting_en_US.md) |

---

## Acknowledgements

- LLVM Project: <https://llvm.org/>
- llvm-plugin-rs
  - <https://github.com/jamesmth/llvm-plugin-rs/tree/feat/llvm-20>
  - <https://github.com/stevefan1999-personal/llvm-plugin-rs>
- Obfuscator-LLVM: <https://github.com/obfuscator-llvm/obfuscator>
- SsagePass: <https://github.com/SsageParuders/SsagePass>
- Polaris-Obfuscator: <https://github.com/za233/Polaris-Obfuscator>
- YANSOllvm: <https://github.com/emc2314/YANSOllvm>
- MBA: <https://plzin.github.io/posts/mba>
- LLVM PassManager Changes and Dynamic Registration: <https://bbs.kanxue.com/thread-272801.htm>

---

> © 2025-2026 Fuqiuluo & Contributors.<br>
> Licensed under this repository's LICENSE.
