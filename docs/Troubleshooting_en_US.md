# Troubleshooting Guide

## LLVM Not Found

**Error message:**
```
error: No suitable version of LLVM was found system-wide or pointed
       to by LLVM_SYS_<VERSION>_PREFIX.

       Refer to the llvm-sys documentation for more information.

       llvm-sys: https://crates.io/crates/llvm-sys
```

**Cause:** LLVM is not installed or the build tools cannot locate it.

**Solution:** See [LLVM Setup Guide](LLVMSetup.md)

---

## libffi Not Found

**Error message:** Linker errors about missing `-lffi`

### Linux (Fedora/RHEL/CentOS)

```bash
sudo dnf install libffi-devel
```

### Linux (Ubuntu/Debian)

```bash
sudo apt install libffi-dev
```

### macOS

```bash
brew install libffi

# If still having issues, set PKG_CONFIG_PATH
export PKG_CONFIG_PATH="$(brew --prefix libffi)/lib/pkgconfig:$PKG_CONFIG_PATH"
```

### Windows

libffi should be included with the LLVM installation. If issues persist, ensure you've installed the complete LLVM package with all components.

---

## Rust-Related Issues

### Clone Function Obfuscation May Disable Safety Checks

**Problem:** When `AMICE_CLONE_FUNCTION=true` is enabled, some Rust safety checks may be disabled or produce false results.

**Cause:** Clone Function (constant argument specialization) obfuscation creates specialized versions of functions with constant arguments and modifies call sites. This may interfere with some Rust compiler safety analyses because:

1. Function signatures are modified (constant parameters are removed)
2. Original calls are replaced with specialized function calls
3. Parameter attributes (such as `noundef`, `nonnull`, etc.) may be removed during specialization

**Affected Areas:**
- Bounds check optimizations may be affected
- Some `debug_assert!` macros may be optimized away
- LLVM's safety-related optimization passes may not correctly analyze specialized code

**Recommendations:**
- Use this obfuscation cautiously in safety-critical code
- Use function annotations `-clone_function` to exclude specific functions
- Perform thorough testing before deploying to production

### Rust Debug Builds Cannot Apply Obfuscation

**Problem:** When using debug builds, obfuscation passes report no functions or call sites found.

**Cause:** Rust uses incremental compilation and multiple codegen units by default, causing the LLVM plugin to only see a subset of functions.

**Solution:** Configure in `Cargo.toml`:

```toml
[profile.dev]
codegen-units = 1
incremental = false
```
