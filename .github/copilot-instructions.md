# Copilot Instructions for Amice

## Repository Overview

Amice is an LLVM plugin written in Rust that provides code obfuscation transformations for C/C++ programs. It operates as a Clang plugin during compilation, offering multiple obfuscation techniques including string encryption, indirect calls/branches, block shuffling, basic block splitting, and VM flattening.

**Key Facts:**
- Primary language: Rust (edition 2024)
- Target: LLVM plugin (libamice.so) for use with clang
- Multi-crate workspace: main `amice` crate + `amice-llvm` sub-crate
- Configuration-driven with environment variable overrides
- Supports LLVM versions 11.0 through 20.1 via feature flags

## Critical Build Requirements

**ALWAYS set these environment variables before any Rust commands:**
```bash
export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
```

**ALWAYS use these cargo flags for the current environment:**
```bash
--no-default-features --features llvm18-1
```

The default feature is `llvm20-1` but the CI environment only has LLVM 18 available. Using wrong LLVM version will cause immediate compilation failures.

## Build Instructions

### Prerequisites Verification
Check LLVM installation before building:
```bash
# Verify LLVM 18 is available
llvm-config-18 --version  # Should output: 18.1.3
clang --version           # Should show clang 18.x
```

### Build Commands (Required Order)

1. **Check/Validate Code:**
```bash
export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
cargo check --no-default-features --features llvm18-1
```

2. **Debug Build:**
```bash
export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18  
cargo build --no-default-features --features llvm18-1
```

3. **Release Build (Required for Testing):**
```bash
export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
cargo build --release --no-default-features --features llvm18-1
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
export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
cargo test --no-default-features --features llvm18-1 --lib
```

**Integration Tests (Require Release Build):**
```bash
# First ensure release build exists
export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
cargo build --release --no-default-features --features llvm18-1

# Then run integration tests
cargo test --no-default-features --features llvm18-1 --test const_strings
```

**Manual Plugin Testing:**
```bash
# After release build, test plugin functionality
clang -fpass-plugin=target/release/libamice.so tests/test1.c -o target/test1
./target/test1  # Should run successfully
```

## Project Architecture

### Directory Structure
```
├── .github/workflows/          # CI/CD workflows
│   ├── rustfmt.yml            # Auto-formatting on PR/push
│   └── generate-structure.yml  # Project structure docs
├── src/                       # Main plugin source
│   ├── aotu/                  # Obfuscation techniques
│   │   ├── string_encryption/ # String obfuscation algorithms
│   │   ├── indirect_call/     # Indirect call obfuscation
│   │   ├── indirect_branch/   # Indirect branch obfuscation  
│   │   ├── shuffle_blocks/    # Basic block reordering
│   │   ├── split_basic_block/ # Block splitting
│   │   └── vm_flatten/        # Virtual machine obfuscation
│   ├── config/                # Configuration handling
│   ├── llvm_utils/           # LLVM utility functions
│   └── lib.rs                # Plugin entry point
├── amice-llvm/               # LLVM FFI bindings
│   ├── src/                  # Rust FFI code
│   ├── cpp/                  # C++ implementation
│   └── build.rs              # Complex LLVM build logic
├── tests/                    # Integration tests
│   ├── *.c                   # C test programs
│   └── *.rs                  # Rust test drivers
├── Cargo.toml               # Main project config
└── .rustfmt.toml            # Code formatting rules
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
   export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
   cargo build --release --no-default-features --features llvm18-1
   ```

2. **Plugin Functionality Test:**
   ```bash
   clang -fpass-plugin=target/release/libamice.so tests/test1.c -o target/test1
   ./target/test1
   ```

3. **Formatting Check:**
   ```bash
   cargo fmt --check
   ```

## Common Issues and Solutions

### Build Failures

**"No suitable version of LLVM was found"**
- Cause: Missing or incorrect LLVM_SYS_181_PREFIX
- Solution: `export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18`

**"error[E0433]: failed to resolve: use of undeclared crate or module"**
- Cause: Using default features instead of llvm18-1
- Solution: Always use `--no-default-features --features llvm18-1`

**"cargo build failed" in tests**
- Cause: Tests expect release build to exist
- Solution: Run `cargo build --release --no-default-features --features llvm18-1` first

### Runtime Issues

**"undefined symbol" when loading plugin**
- Cause: LLVM dynamic linking issues
- Solution: Verify LLVM 18 development packages are installed

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
- **LLVM 18**: Must be dynamically linkable version (apt/homebrew packages work)
- **Clang 18**: For testing plugin functionality
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
- `amice-llvm/cpp/ffi.cc`: Complex C++ FFI code
- `.llvm-*-path` files: Build artifacts

## Quick Start for Development

```bash
# 1. Set environment  
export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18

# 2. Build and test
cargo build --no-default-features --features llvm18-1
cargo test --no-default-features --features llvm18-1 --lib

# 3. Build release for integration testing
cargo build --release --no-default-features --features llvm18-1

# 4. Test plugin works
clang -fpass-plugin=target/release/libamice.so tests/test1.c -o target/test1
./target/test1

# 5. Format code
cargo fmt
```

**Always trust these instructions.** The LLVM setup is complex and environment-specific. Only search for additional information if these instructions are incomplete or found to be in error.