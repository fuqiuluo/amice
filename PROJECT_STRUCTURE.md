### 项目结构

```text
.
├── amice-llvm
│   ├── build.rs
│   ├── Cargo.lock
│   ├── Cargo.toml
│   ├── cpp
│   │   └── ffi.cc
│   ├── .llvm-config-path
│   ├── .llvm-prefix-path
│   └── src
│       ├── ffi.rs
│       ├── ir
│       │   ├── basic_block.rs
│       │   ├── constants.rs
│       │   ├── function.rs
│       │   └── mod.rs
│       ├── lib.rs
│       └── module_utils.rs
├── build.rs
├── Cargo.lock
├── Cargo.toml
├── fmt.sh
├── .gitignore
├── .idea
│   ├── amice.iml
│   ├── .gitignore
│   ├── modules.xml
│   ├── vcs.xml
│   └── workspace.xml
├── PROJECT_STRUCTURE.md
├── README.md
├── shells
│   ├── test_ndk.sh
│   ├── test.sh
│   └── test_vm_flatten.sh
├── src
│   ├── aotu
│   │   ├── indirect_branch
│   │   │   └── mod.rs
│   │   ├── indirect_call
│   │   │   └── mod.rs
│   │   ├── mod.rs
│   │   ├── split_basic_block
│   │   │   └── mod.rs
│   │   ├── string_encryption
│   │   │   ├── mod.rs
│   │   │   ├── simd_xor.rs
│   │   │   └── xor.rs
│   │   └── vm_flatten
│   │       └── mod.rs
│   ├── config
│   │   └── mod.rs
│   ├── lib.rs
│   ├── llvm_utils
│   │   ├── basic_block.rs
│   │   ├── branch_inst.rs
│   │   ├── function.rs
│   │   ├── mod.rs
│   │   └── switch_inst.rs
│   └── utils
│       └── mod.rs
└── tests
    ├── const_strings.c
    ├── const_strings.rs
    ├── .gitignore
    ├── indirect_branch.c
    ├── indirect_branch.rs
    ├── indirect_call.c
    ├── repeated_strings.c
    ├── repeated_strings.rs
    ├── test1
    ├── test1.c
    ├── test1.ll
    ├── test2.c
    ├── test_ndk
    ├── test_ndk.ll
    └── vm_flatten.c

18 directories, 59 files

```
