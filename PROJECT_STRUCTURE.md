# 项目结构

- 仓库：amice
- 生成时间：2025-08-09 10:15:18 UTC
- 深度：3
- 忽略：.git|target|node_modules|.idea|.vscode|dist|build

```text

├── .github/
│   └── workflows/
│       └── generate-structure.yml
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
├── build.rs
├── fmt.sh
├── src/
│   ├── aotu/
│   │   ├── indirect_branch/
│   │   ├── indirect_call/
│   │   ├── mod.rs
│   │   ├── split_basic_block/
│   │   ├── string_encryption/
│   │   └── vm_flatten/
│   ├── config/
│   │   └── mod.rs
│   ├── lib.rs
│   └── llvm_utils/
│       ├── basic_block.rs
│       ├── branch_inst.rs
│       ├── function.rs
│       ├── mod.rs
│       └── switch_inst.rs
└── tests/
    ├── .gitignore
    ├── const_strings.c
    ├── const_strings.rs
    ├── indirect_branch.c
    ├── indirect_branch.rs
    ├── indirect_call.c
    ├── repeated_strings.c
    ├── repeated_strings.rs
    ├── test1.c
    ├── test2.c
    └── vm_flatten.c

17 directories, 35 files
```

> 本文件由 GitHub Actions 自动生成，请勿手动编辑。
