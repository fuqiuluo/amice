# 项目结构

- 仓库：amice
- 生成时间：2025-08-11 16:47:31 UTC
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
│   │   ├── indirect_branch/
│   │   ├── indirect_call/
│   │   ├── mod.rs
│   │   ├── shuffle_blocks/
│   │   ├── split_basic_block/
│   │   ├── string_encryption/
│   │   └── vm_flatten/
│   ├── config/
│   │   ├── indirect_branch.rs
│   │   ├── indirect_call.rs
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
    ├── const_strings.c
    ├── const_strings.rs
    ├── indirect_branch.c
    ├── indirect_branch.rs
    ├── indirect_call.c
    ├── large_string.c
    ├── large_string_threshold.rs
    ├── md5.c
    ├── md5.cc
    ├── md5.rs
    ├── repeated_strings.c
    ├── repeated_strings.rs
    ├── test1.c
    ├── test_strings.c
    └── vm_flatten.c

21 directories, 52 files
```

> 本文件由 GitHub Actions 自动生成，请勿手动编辑。
