# Amice

English | [简体中文](README.md)

[![Release](https://img.shields.io/github/v/release/fuqiuluo/amice?include_prereleases)](https://github.com/fuqiuluo/amice/releases)
[![CI](https://github.com/fuqiuluo/amice/actions/workflows/linux-x64-build.yml/badge.svg)](https://github.com/fuqiuluo/amice/actions/workflows/linux-x64-build.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

Amice is a code-obfuscation tool that ships as an LLVM pass plugin. It provides string encryption, control-flow obfuscation and instruction-level VMP virtualization for C/C++, Rust and Android native code. With a prebuilt plugin, integration is a single `-fpass-plugin` flag on your existing build — no LLVM rebuild, no compiler changes, no source modifications.

> **Status**: beta (v0.1.5-beta.4); configuration and behavior may change between releases. Prebuilt plugins cover LLVM/Clang 18–22 on Linux/macOS, plus Android NDK r27d–r30 (r29/r30 ship as complete bundles with the NDK and runtime libraries; r27d/r28c are plugin-only). No prebuilt Windows package yet; [build from source](docs/LLVMSetup_en_US.md) instead.

## Features

- **Plug-and-play** — loaded into clang as a shared library; no LLVM rebuild, and existing Clang-built projects only add one compile flag
- **String encryption** — `xor` / `simd_xor` algorithms with lazy or startup decryption and stack/heap allocation
- **Control-flow obfuscation** — control-flow flattening, VM control-flow flattening, [BCF](docs/EnvConfig_en_US.md#bogus-control-flow), indirect calls/branches, basic-block splitting and shuffling, and more
- **Instruction-level VMP** — lifts functions into VM bytecode with a generated interpreting runtime
- **MBA obfuscation** — mixed boolean-arithmetic rewriting covering integer and binary64 floating-point regions
- **Per-function control** — choose obfuscation strategies per function with `__attribute__((annotate(...)))`
- **Multi-language integration** — C/C++, Rust (nightly `-Zllvm-plugins`), Android NDK (CMake/Gradle bundles)
- **Composable** — every switch, parameter and pass ordering is configured via environment variables or `amice.toml`

See [Runtime Environment Variables](docs/EnvConfig_en_US.md) for the full pass support matrix (language coverage × environment-variable switches).

## Quick Start

Download the plugin matching your LLVM major from [Releases](https://github.com/fuqiuluo/amice/releases), then load it on your existing compile command with the feature switches you need:

```bash
AMICE_STRING_ENCRYPTION=true \
  clang-21 -fpass-plugin=/path/to/libamice-llvm21-linux-x86_64.so hello.c -o hello
# Same behavior, but plaintext strings in the binary are now encrypted away
```

New here? Follow the [Quick Start guide](docs/QuickStart_en_US.md) to run a verifiable example in five minutes, including macOS, Android, Rust and CMake integration.

> **Note**: Only use Amice on software you are authorized to protect. Obfuscation changes code generation — run your project's existing tests after enabling it. Obfuscated binaries may trigger antivirus or app-store false positives. Strong passes such as VMP increase size and reduce performance; enable them per function.

## Documentation

| What you want to do | Guide |
| --- | --- |
| Run your first example in five minutes | [Quick Start](docs/QuickStart_en_US.md) |
| Choose a download and prepare the runtime | [Download and Setup](docs/Download_en_US.md) |
| Integrate with Android CMake / Gradle | [Android NDK](docs/AndroidNDKSupport_en_US.md) |
| Integrate with Rust / Cargo | [Rust Usage](docs/RustUsage_en_US.md) |
| Look up switches, parameters, defaults and the support matrix | [Environment Variables](docs/EnvConfig_en_US.md) |
| Select or exclude individual functions | [Function Annotations](docs/FunctionAnnotations_en_US.md) |
| Control feature combinations and order | [Pass Execution Order](docs/PassOrder_en_US.md) |
| Fix loading failures or ineffective settings | [Troubleshooting](docs/Troubleshooting_en_US.md) |

## Contributing

Issues and pull requests are welcome. See the [Development Guide](docs/Development_en_US.md) for the dev environment, test workflows and project layout, and [Source Builds](docs/LLVMSetup_en_US.md) for build instructions.

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

## License

Amice is released under [Apache-2.0](LICENSE).

> © 2025-2026 Fuqiuluo & Contributors.
