# Troubleshooting Guide

English | [简体中文](Troubleshooting_zh_CN.md)

## Identify the Failing Step

| Symptom | First check |
| --- | --- |
| LLVM not found / C++ compilation fails | Matching feature, prefix and `llvm-config --version`; development headers and C++ compiler installed |
| Missing `libLLVM`, DLL or `undefined symbol` | Compatible plugin host OS/architecture, LLVM version and shared libraries |
| Successful compile without obfuscation | Switches, pass allowlist, annotations, cached objects and skip diagnostics |
| Configuration changes have no effect | Source was recompiled instead of reusing existing objects/binaries |

### Plugin Load Failures

After choosing a toolchain in [LLVM Setup](LLVMSetup_en_US.md), run its `clang --version`, `opt --version` and `llvm-config --version` by absolute path. A prefix helps build the plugin; it does not automatically select the right clang for your application.

For a missing `libLLVM.so` on Linux, first confirm the matching shared library exists, then set this in the compiler's terminal:

```bash
export LD_LIBRARY_PATH="$AMICE_LLVM/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
```

`AMICE_LLVM` is your selected installation directory. For a missing `libLLVM.dylib` on macOS, check the installation and plugin dependencies; `DYLD_LIBRARY_PATH` can point to that toolchain's libraries. Search paths do not fix ABI incompatibility.

On Windows follow the [opt example](LLVMSetup_en_US.md#windows) and check `LLVM-C.dll`, `opt.exe` and `opt.lib`. `win-link-opt` and `win-link-lld` target different loaders. Set `$env:LLVM_SYS_*_PREFIX` in the current terminal; building immediately after `setx` still uses the old process environment.

For Android host libraries, NDK revision mismatches and macOS signing, see [NDK Common Errors](AndroidNDKSupport_en_US.md#common-errors).

### Successful Compile Without Obfuscation

1. Enable only string encryption in the minimal example from [Quick Start](QuickStart_en_US.md), compare runtime output and check the final binary for the marker. Loading the plugin does not prove a transform occurred.
2. Inspect `AMICE_CONFIG_PATH`, `AMICE_PASS_ORDER` and other `AMICE_*` values. An explicit pass list excludes unlisted features; annotations can override global switches. Use exact [environment variable names](EnvConfig_en_US.md).
3. Missing files or invalid TOML/YAML/JSON currently fall back to defaults. Use an absolute path and a [complete config example](PassOrder_en_US.md), or temporarily use a single environment switch to isolate file configuration issues.
4. Set logging before starting the compiler and rebuild:

```bash
export RUST_LOG=amice=debug
```

```powershell
$env:RUST_LOG = 'amice=debug'
```

`amice plugin initializing` indicates the load entry point was called; `(PassName) pass done` indicates a transform occurred. VMP's `skip function` messages explain skipped functions. A pass success message does not establish coverage of every function.

5. Ensure your build system actually recompiles the source. Cargo does not automatically rebuild for `AMICE_*` or plugin changes; use a new dedicated `--target-dir`. See [Rust Usage](RustUsage_en_US.md).

## LLVM Not Found

**Error message:**
```
error: No suitable version of LLVM was found system-wide or pointed
       to by LLVM_SYS_<VERSION>_PREFIX.

       Refer to the llvm-sys documentation for more information.

       llvm-sys: https://crates.io/crates/llvm-sys
```

**Cause:** LLVM is not installed or the build tools cannot locate it.

**Solution:** See [LLVM Setup Guide](LLVMSetup_en_US.md)

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
