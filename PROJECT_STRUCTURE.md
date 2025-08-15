# 项目结构

- 仓库：amice
- 生成时间：2025-08-15 18:57:36 UTC
- 深度：3
- 忽略：.git|target|node_modules|.idea|.vscode|dist|build

```text

├── .github/
│   ├── copilot-instructions.md
│   └── workflows/
│       ├── generate-structure.yml
│       └── rustfmt.yml
├── .gitignore
├── .rustfmt.toml
├── Cargo.lock
├── Cargo.toml
├── PROJECT_STRUCTURE.md
├── README.md
├── amice-llvm/
│   ├── Cargo.lock
│   ├── Cargo.toml
│   ├── build.rs
│   ├── cpp/
│   │   └── ffi.cc
│   └── src/
│       ├── ffi.rs
│       ├── ir/
│       ├── lib.rs
│       └── module_utils.rs
├── amice-macro/
│   ├── Cargo.lock
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
├── build.rs
├── src/
│   ├── aotu/
│   │   ├── bogus_control_flow/
│   │   ├── flatten/
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
│   │   ├── bogus_control_flow.rs
│   │   ├── flatten.rs
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
│   ├── llvm_utils/
│   │   ├── basic_block.rs
│   │   ├── branch_inst.rs
│   │   ├── function.rs
│   │   ├── mod.rs
│   │   └── switch_inst.rs
│   └── pass_registry/
│       └── mod.rs
└── tests/
    ├── .gitignore
    ├── bogus_control_flow.c
    ├── complex_switch_test.c
    ├── const_strings.c
    ├── const_strings.rs
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

25 directories, 61 files
```

> 本文件由 GitHub Actions 自动生成，请勿手动编辑。
