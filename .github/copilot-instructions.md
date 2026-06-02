# Copilot Instructions for Amice

## Repository Overview

Amice is an LLVM plugin written in Rust that provides code obfuscation transformations for C/C++ programs. It operates as a Clang plugin during compilation, offering multiple obfuscation techniques including string encryption, indirect calls/branches, block shuffling, basic block splitting, and VM flattening.

**Key Facts:**
- Primary language: Rust (edition 2024)
- Target: LLVM plugin (libamice.so) for use with clang
- Virtual Cargo workspace with all Rust crates under `crates/`
- Configuration-driven with environment variable overrides
- Supports LLVM versions 11.0 through 21.1 via feature flags

## Critical Build Requirements

**ALWAYS set these environment variables before any Rust commands:**
```bash
export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21
```

The default Cargo feature is `llvm21-1`, matching the local LLVM 21.1 environment.

## Build Instructions

### Prerequisites Verification
Check LLVM installation before building:
```bash
# Verify LLVM 21 is available
llvm-config --version  # Should output: 21.1.x
clang --version        # Should show clang 21.x
```

### Build Commands (Required Order)

1. **Check/Validate Code:**
```bash
export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21
cargo check
```

2. **Debug Build:**
```bash
export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21
cargo build
```

3. **Release Build (Required for Testing):**
```bash
export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21
cargo build --release
```

4. **Format Code:**
```bash
cargo fmt
```

5. **Format Check:**
```bash
cargo fmt --check
```

### Build Timing
- Initial build: ~30-60 seconds (includes dependency compilation)
- Incremental builds: ~5-15 seconds
- Release build: ~20-40 seconds
- Clean build: ~60-120 seconds

### Testing

**Unit Tests:**
```bash
export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21
cargo test --lib
```

**Integration Tests (Require Release Build):**
```bash
# First ensure release build exists
export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21
cargo build --release

# Then run integration tests
cargo test --release --test string_encryption
```

**Manual Plugin Testing:**
```bash
# After release build, test plugin functionality
clang -fpass-plugin=target/release/libamice.so \
  crates/amice/tests/c/fixtures/integration/md5.c \
  -o target/test-md5
./target/test-md5  # Should run successfully
```

## Project Architecture

### Directory Structure
```
├── .github/workflows/          # CI/CD workflows
│   ├── rustfmt.yml            # Auto-formatting on PR/push
│   └── generate-structure.yml  # Project structure docs
├── crates/
│   ├── amice/                 # Main plugin crate
│   │   ├── src/               # Main plugin source
│   │   └── tests/             # Integration tests and fixtures
│   ├── amice-llvm/            # LLVM FFI bindings
│   ├── amice-plugin/          # LLVM pass-plugin runtime
│   ├── amice-macro/           # Amice pass registration macros
│   ├── amice-plugin-macros/   # Runtime proc macros
│   └── amice-build-support/   # Shared build-script helpers
├── Cargo.toml                 # Virtual workspace manifest
├── Cargo.lock                 # Workspace lockfile
└── .rustfmt.toml              # Code formatting rules
```

### Configuration System
- **Environment Variables:** `AMICE_*` variables control runtime behavior
- **Config Files:** Support TOML/YAML/JSON via `AMICE_CONFIG_PATH` 
- **Overlay System:** Environment variables override config file settings

### Key Environment Variables
- `AMICE_STRING_ALGORITHM`: `xor` | `simd_xor`
- `AMICE_STRING_DECRYPT_TIMING`: `lazy` | `global`
- `AMICE_STRING_STACK_ALLOC`: `true` | `false`
- `RUST_LOG`: Set to `debug` for detailed plugin logs

## Validation Pipeline

### Automated Checks (GitHub Actions)
1. **rustfmt**: Auto-formats code on push/PR to main/master
2. **Project Structure**: Manual workflow to update PROJECT_STRUCTURE.md

### Manual Validation Steps
1. **Build Verification:**
   ```bash
   export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21
   cargo build --release
   ```

2. **Plugin Functionality Test:**
   ```bash
   clang -fpass-plugin=target/release/libamice.so \
     crates/amice/tests/c/fixtures/integration/md5.c \
     -o target/test-md5
   ./target/test-md5
   ```

3. **Formatting Check:**
   ```bash
   cargo fmt --check
   ```

## Common Issues and Solutions

### Build Failures

**"No suitable version of LLVM was found"**
- Cause: Missing or incorrect LLVM_SYS_211_PREFIX
- Solution: `export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21`

**"cargo build failed" in tests**
- Cause: Tests expect release build to exist
- Solution: Run `cargo build --release` first

### Runtime Issues

**"undefined symbol" when loading plugin**
- Cause: LLVM dynamic linking issues
- Solution: Verify LLVM 21 development packages are installed

**Plugin not applying transformations**
- Cause: Missing environment variables for specific techniques
- Solution: Set appropriate `AMICE_*` environment variables

### Code Quality

**Unsafe function warnings in Rust 2024**
- These are expected warnings in amice-llvm FFI code
- Do not "fix" by removing unsafe blocks without understanding implications
- These warnings do not affect functionality

## Dependencies and Workarounds

### External Dependencies
- **LLVM 21**: Must be dynamically linkable version (apt/homebrew packages work)
- **Clang 21**: For testing plugin functionality
- **Standard Rust toolchain**: Edition 2024 features required

### Forked Dependencies
- **inkwell**: Uses fork at `https://github.com/fuqiuluo/inkwell`
- **llvm-plugin**: Uses fork at `https://github.com/fuqiuluo/llvm-plugin-rs`
- These forks contain necessary patches - do not update to upstream without testing

### Platform Notes
- **Linux/macOS**: Fully supported with package manager LLVM
- **Windows**: Requires manual LLVM compilation (complex)
- **Android NDK**: Supported but requires special clang build

## Files You Should Not Modify

- `PROJECT_STRUCTURE.md`: Auto-generated by GitHub Actions
- `Cargo.lock`: Managed by Cargo
- `crates/amice-llvm/cpp/`: Complex C++ FFI code
- `.llvm-*-path` files: Build artifacts

## Quick Start for Development

```bash
# 1. Set environment  
export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21

# 2. Build and test
cargo build
cargo test --lib

# 3. Build release for integration testing
cargo build --release

# 4. Test plugin works
clang -fpass-plugin=target/release/libamice.so \
  crates/amice/tests/c/fixtures/integration/md5.c \
  -o target/test-md5
./target/test-md5

# 5. Format code
cargo fmt
```

**Always trust these instructions.** The LLVM setup is complex and environment-specific. Only search for additional information if these instructions are incomplete or found to be in error.
