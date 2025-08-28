# 项目结构

- 仓库：amice
- 生成时间：2025-08-28 17:34:43 UTC
- 深度：3
- 忽略：.git|target|node_modules|.idea|.vscode|dist|build

```text

├── .github/
│   ├── copilot-instructions.md
│   └── workflows/
│       ├── generate-structure.yml
│       ├── linux-x64-build-android-ndk.yml
│       ├── linux-x64-build.yml
│       ├── macos-arm64-build.yml
│       ├── rustfmt.yml
│       ├── windwos-x64-link-lld-build.yml
│       └── windwos-x64-link-opt-build.yml
├── .gitignore
├── .rustfmt.toml
├── Cargo.lock
├── Cargo.toml
├── LICENSE
├── PROJECT_STRUCTURE.md
├── README.md
├── amice-llvm/
│   ├── Cargo.lock
│   ├── Cargo.toml
│   ├── build.rs
│   ├── cpp/
│   │   ├── dominators_ffi.cc
│   │   ├── ffi.cc
│   │   ├── instructions.cc
│   │   ├── utils.cc
│   │   └── verifier.cc
│   └── src/
│       ├── analysis/
│       ├── annotate.rs
│       ├── ffi.rs
│       ├── inkwell2/
│       └── lib.rs
├── amice-macro/
│   ├── Cargo.lock
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
├── build.rs
├── src/
│   ├── aotu/
│   │   ├── alias_access/
│   │   ├── bogus_control_flow/
│   │   ├── clone_function/
│   │   ├── custom_calling_conv/
│   │   ├── flatten/
│   │   ├── function_wrapper/
│   │   ├── indirect_branch/
│   │   ├── indirect_call/
│   │   ├── lower_switch/
│   │   ├── mba/
│   │   ├── mod.rs
│   │   ├── shuffle_blocks/
│   │   ├── split_basic_block/
│   │   ├── string_encryption/
│   │   └── vm_flatten/
│   ├── config/
│   │   ├── alias_access.rs
│   │   ├── bogus_control_flow.rs
│   │   ├── clone_function.rs
│   │   ├── custom_calling_conv.rs
│   │   ├── flatten.rs
│   │   ├── function_wrapper.rs
│   │   ├── indirect_branch.rs
│   │   ├── indirect_call.rs
│   │   ├── lower_switch.rs
│   │   ├── mba.rs
│   │   ├── mod.rs
│   │   ├── pass_order.rs
│   │   ├── shuffle_blocks.rs
│   │   ├── split_basic_block.rs
│   │   ├── string_encryption.rs
│   │   └── vm_flatten.rs
│   ├── lib.rs
│   └── pass_registry/
│       └── mod.rs
└── tests/
    ├── .gitignore
    ├── bogus_control_flow.c
    ├── clone_function.c
    ├── complex_switch_test.c
    ├── const_strings.c
    ├── const_strings.rs
    ├── custom_calling_conv.c
    ├── function_wrapper_test.c
    ├── function_wrapper_test.rs
    ├── indirect_branch.c
    ├── indirect_branch.rs
    ├── indirect_call.c
    ├── large_string.c
    ├── large_string_threshold.rs
    ├── mba_constants_demo.c
    ├── md5.c
    ├── md5.cc
    ├── md5.rs
    ├── repeated_strings.c
    ├── repeated_strings.rs
    ├── shuffle_blocks_test.rs
    ├── shuffle_test.c
    ├── test1.c
    ├── test_strings.c
    └── vm_flatten.c

29 directories, 74 files
```

> 本文件由 GitHub Actions 自动生成，请勿手动编辑。
