# Runtime Environment Variables

English | [简体中文](EnvConfig_zh_CN.md)

Variables are read by the compiler process loading the plugin. In Bash use `export AMICE_STRING_ENCRYPTION=true` or a per-command prefix; in PowerShell use `$env:AMICE_STRING_ENCRYPTION = 'true'`.

Global precedence is **environment > config file > defaults**. Passes supporting [Function Annotations](FunctionAnnotations_en_US.md) then apply those overrides. Environment booleans accept `true/false`, `1/0` and `on/off` case-insensitively; the annotation value `yes` is not a true environment value.

## Support Matrix

| Pass | Environment Variable | C/C++ | Rust | ObjC | Description |
|:---|:---|:---:|:---:|:---:|:---|
| String Encryption | `AMICE_STRING_ENCRYPTION` | ✅ | ✅ | ⏳ | String encryption with `xor` / `simd_xor`, lazy/global decryption, stack/heap allocation options |
| Indirect Call | `AMICE_INDIRECT_CALL` | ✅ | ✅ | ❌ | Rewrites direct calls into table/index based indirect calls |
| Indirect Branch | `AMICE_INDIRECT_BRANCH` | ✅ | ✅ | ❌ | Rewrites branches into `indirectbr`; supports dummy blocks, table shuffling, index encryption, and other flags |
| Split Basic Block | `AMICE_SPLIT_BASIC_BLOCK` | ✅ | ✅ | ❌ | Splits basic blocks according to configuration |
| Lower Switch | `AMICE_LOWER_SWITCH` | ✅ | ✅ | ❌ | Lowers LLVM `switch` instructions |
| VM Flatten | `AMICE_VM_FLATTEN` | ✅ | ✅ | ❌ | VM control-flow flattening |
| VM Virtualize | `AMICE_VM_VIRTUALIZE` | ✅ | ✅ | ❌ | Instruction-level VMP virtualization, enabled globally or per function with annotations |
| Flatten | `AMICE_FLATTEN` | ✅ | ✅ | ❌ | Control-flow flattening with `basic` / `dominator` modes |
| MBA | `AMICE_MBA` | ✅ | ✅ | ❌ | Mixed Boolean-arithmetic expression rewriting |
| Bogus Control Flow | `AMICE_BOGUS_CONTROL_FLOW` | ✅ | ✅ | ❌ | Obfuscates control flow to make reverse engineering more difficult |
| Function Wrapper | `AMICE_FUNCTION_WRAPPER` | ✅ | ✅ | ❌ | Creates wrapper functions and replaces call sites |
| Clone Function | `AMICE_CLONE_FUNCTION` | ✅ | ✅ | ❌ | Constant-argument specialization by function cloning |
| Alias Access | `AMICE_ALIAS_ACCESS` | ✅ | ✅ | ❌ | Pointer-chain based alias access obfuscation |
| Custom Calling Conv | `AMICE_CUSTOM_CALLING_CONV` | ⏳ | ⏳ | ❌ | Placeholder; currently performs no calling-convention transform |
| Delay Offset Loading | `AMICE_DELAY_OFFSET_LOADING` | ✅ | ⏳ | ❌ | Delayed GEP offset loading with optional XOR protection |
| Param Aggregate | `AMICE_PARAM_AGGREGATE` | ✅ | ⏳ | ❌ | Parameter aggregation obfuscation |
| Basic Block Outlining | `AMICE_BASIC_BLOCK_OUTLINING` | ✅ | ⏳ | ❌ | Extracts basic blocks into standalone helper functions, also known as BB2Func |
| Shuffle Blocks | `AMICE_SHUFFLE_BLOCKS` | ✅ | ⏳ | ❌ | Basic block reordering |

> Legend: ✅ supported; ⏳ in progress / planned / untested; ❌ not planned.

Full parameters for each pass are in the sections below; for per-function enable/disable see [Function Annotations](FunctionAnnotations_en_US.md).

## Files, Ordering and Logging

| Variable | Usage | Default |
| --- | --- | --- |
| `AMICE_CONFIG_PATH` | TOML/YAML/JSON path, preferably absolute; see [complete examples](PassOrder_en_US.md) | No file |
| `AMICE_PASS_ORDER` | Comma- or semicolon-separated pass names; only listed passes are installed, without enabling them automatically | Priority order |
| `AMICE_PASS_PRIORITY_OVERRIDE` | For example `IndirectCall=1100,StringEncryption=1000`; ignored when explicit order is present | No override |
| `RUST_LOG` | `amice=info` for load/execution messages; `amice=debug` for detailed diagnosis | Not explicitly configured |

Configuration read errors currently fall back to defaults. Setting a path does not prove its contents were applied. Verify the output artifact; see [Troubleshooting](Troubleshooting_en_US.md).

## String Encryption

Source code: `crates/amice/src/aotu/string_encryption`

| Variable                                 | Description                                                                                                                                                                                                                                           | Default |
|------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------|
| AMICE_STRING_ENCRYPTION                  | Enable string encryption:<br/>- `true` — enabled<br/>- `false` — disabled                                                                                                                                                                             | false   |
| AMICE_STRING_ALGORITHM                   | String encryption algorithm:<br/>- `xor` — XOR encryption<br/>- `simd_xor` — (beta) SIMD-based XOR encryption                                                                                                                                         | xor     |
| AMICE_STRING_DECRYPT_TIMING              | String decryption timing:<br/>- `global` — decrypt all protected strings at global initialization during program startup<br/>- `lazy` — decrypt on-demand before first use (then cache)<br/>Note: Stack-allocated strings do not support this config! | lazy    |
| AMICE_STRING_STACK_ALLOC                 | (beta) Memory allocation for decrypted strings:<br/>- `true` — allocate on stack<br/>- `false` — allocate on heap<br/>Note: Stack allocation only supports `lazy` timing!                                                                             | false   |
| AMICE_STRING_INLINE_DECRYPT_FN           | Inline decrypt function:<br/>- `true` — inline<br/>- `false` — don't inline                                                                                                                                                                           | false   |
| AMICE_STRING_ONLY_DOT_STRING             | Only process strings in `.str` section:<br/>- `true` — only encrypt `.str` strings<br/>- `false` — may encrypt char[] global variables in llvm::Module, possibly causing crashes                                                                      | true    |
| AMICE_STRING_ALLOW_NON_ENTRY_STACK_ALLOC | Allow stack allocation in non-entry blocks (stack decryption mode):<br/>- `true` — allow (many LLVM optimization passes assume all alloca are in entry block)<br/>- `false` — recommended                                                             | false   |
| AMICE_STRING_MAX_ENCRYPTION_COUNT        | Encryption count for non-global decryption; clamped to 1–100000                                                                 | 1       |

> **Note for Rust**: 
> ```bash
> export AMICE_STRING_ONLY_DOT_STRING=false  # Rust string globals are named alloc_xxx, not .str
> ```

> `AMICE_STRING_ONLY_LLVM_STRING` is a legacy alias of `AMICE_STRING_ONLY_DOT_STRING`; the latter wins if both are set.

## Indirect Call Obfuscation

Source code: `crates/amice/src/aotu/indirect_call`

| Variable                    | Description                                                                     | Default |
|-----------------------------|---------------------------------------------------------------------------------|---------|
| AMICE_INDIRECT_CALL         | Enable indirect calls:<br/>- `true` — enabled<br/>- `false` — disabled          | false   |
| AMICE_INDIRECT_CALL_XOR_KEY | XOR key for indirect call index<br/>Note: Input `0` to disable index encryption | Random  |

## Indirect Branch Obfuscation

Source code: `crates/amice/src/aotu/indirect_branch`

| Variable                    | Description                                                                                                                                      | Default                     |
|-----------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------|-----------------------------|
| AMICE_INDIRECT_BRANCH       | Enable indirect branch:<br/>- `true` — enabled<br/>- `false` — disabled                                                                          | false                       |
| AMICE_INDIRECT_BRANCH_FLAGS | Additional obfuscation extensions for indirect branch, specified as comma-separated string. [Available extensions](#amice_indirect_branch_flags) | `""` (empty, no extensions) |

### AMICE_INDIRECT_BRANCH_FLAGS

- `dummy_block` — When converting unconditional jumps (`br label`) to `indirectbr`, insert one or more dummy blocks that execute 1-3 meaningless instructions (empty additions, bit operations, etc.) before jumping to the real target block
- `chained_dummy_blocks` — Enhanced `dummy_block`, supports inserting multiple consecutive dummy blocks forming a jump chain, significantly increasing control flow complexity
- `encrypt_block_index` — Encrypt the basic block index in the jump table
- ~~`dummy_junk` — Insert junk instructions in dummy blocks~~
- `shuffle_table` — Shuffle jump table order, randomize basic block positions (disabled by default)

## Split Basic Block

Source code: `crates/amice/src/aotu/split_basic_block`

| Variable                     | Description                                                                   | Default |
|------------------------------|-------------------------------------------------------------------------------|---------|
| AMICE_SPLIT_BASIC_BLOCK      | Enable basic block splitting:<br/>- `true` — enabled<br/>- `false` — disabled | false   |
| AMICE_SPLIT_BASIC_BLOCK_NUM | Number of split iterations                                                    | 3       |

## Switch Lowering

Source code: `crates/amice/src/aotu/lower_switch`

| Variable                               | Description                                                                                | Default |
|----------------------------------------|--------------------------------------------------------------------------------------------|---------|
| AMICE_LOWER_SWITCH                     | Enable switch lowering:<br/>- `true` — enabled<br/>- `false` — disabled                    | false   |
| ~~AMICE_LOWER_SWITCH_WITH_DUMMY_CODE~~ | Insert dummy code after lowering (may fail module verification causing `-O1` etc. to fail) | false   |

## VM control-flow flattening

Source code: `crates/amice/src/aotu/vm_flatten`

| Variable         | Description                                                        | Default |
|------------------|--------------------------------------------------------------------|---------|
| AMICE_VM_FLATTEN | Enable VM flatten:<br/>- `true` — enabled<br/>- `false` — disabled | false   |
| AMICE_VM_FLATTEN_DISTRIBUTED | Enable the distributed transfer prototype; also requires `AMICE_VM_FLATTEN` | false |
| AMICE_VM_FLATTEN_MAX_OPS | Generate 1..n reversible index operations per transfer, including one S-box substitution; n is clamped to 1..32. Each operation may expand into multiple IR instructions; address decoding is separate | 8 |
| AMICE_VM_FLATTEN_PROGRAM_VARIANTS | Number of interleaved reversible index programs per transfer; clamped to 1..3. Larger values increase size and compile/optimization cost | 3 |
| AMICE_VM_FLATTEN_SEED | Decimal u64 seed for distributed mode; explicit values, including 0, are reproducible | Random |

VM control-flow flattening runs at the optimizer's end, after `BogusControlFlow`. The same ordering applies at the end of full LTO. Conflicting explicit orders or priority overrides are adjusted with a warning; this does not enable disabled passes.

Distributed mode emits index calculations, address-table loads and `indirectbr` at individual transfer sites, using gateway blocks and PHI repair to preserve program semantics without a central bytecode interpreter. The default mode uses the interpreter; `random_none_node_opcode` only applies to that mode. Configuration-file fields are `vm_flatten.distributed`, `vm_flatten.max_ops`, `vm_flatten.program_variants` and `vm_flatten.seed`.

Each transfer generates 1..3 equal-length reversible index programs and interleaves their operation candidates; `program_variants` controls the count. A schedule advanced from invocation state and operation position produces the selector for each operation; the source inverse and decoder forward path use the same schedule. Each program includes one table-free substitution whose network combines addition/subtraction, XOR, odd multiplication and shifts/rotations, keyed by invocation state.

Private initializers materialize target addresses, while transfer sites use invocation-local sparse integer tables and state-dependent decoding. Addresses remain analyzable through initialization and execution. Index programs choose candidates using integer calculations and LLVM `select`; the complete gateway path still contains conditional branches, and machine-level branching depends on the backend. Runtime keys and stack-derived masks are not secret keys.

```powershell
$env:AMICE_VM_FLATTEN = "true"
$env:AMICE_VM_FLATTEN_DISTRIBUTED = "true"
$env:AMICE_VM_FLATTEN_MAX_OPS = "8"
$env:AMICE_VM_FLATTEN_PROGRAM_VARIANTS = "3"
$env:AMICE_VM_FLATTEN_SEED = "42"
```

IR successor sets remain visible, and single-successor indirect branches may become direct branches during optimization. Volatile accesses resist constant folding; they do not guarantee resistance to extraction, switch recognition, or recovery. Initialization and runtime decoding remain observable. Functions with exception handling, existing blockaddress uses, other indirect transfers, `naked`, or `ExactMatch` / `SameSize` COMDAT are preserved. Address encoding requires integral 32/64-bit pointers in the default code, global and stack address spaces; unsupported layouts are skipped with a warning. Start with a small set of functions and inspect final artifacts and performance.

## VM Virtualize

Source code: `crates/amice/src/aotu/vm_virtualize`

| Variable                | Description                                                                                       | Default                         |
|-------------------------|---------------------------------------------------------------------------------------------------|---------------------------------|
| AMICE_VM_VIRTUALIZE     | Enable instruction-level VMP virtualization:<br/>- `true` — enabled<br/>- `false` — disabled      | false                           |
| AMICE_VM_PROFILE_PATH   | Path to a VM profile package. If empty, AMICE uses the built-in `amice-simple-vmp` profile package | built-in `amice-simple-vmp`     |
| AMICE_VM_RUNTIME_SCOPE  | Override profile runtime scope:<br/>- `func` — per-function runtime<br/>- `module` — shared module runtime | profile-declared runtime scope |
| AMICE_VM_EMIT_MARKERS   | Emit stable VMP test/debug markers such as `AMICEVMP` bytecode magic, `AMICE_VMP_RUNTIME_BYTECODE`, `.amice.vm.meta.*`, and readable bytecode symbols. Keep disabled for production. | false                           |
| AMICE_VM_DUMP_BYTECODE  | Dump encoded VM bytecode through debug logs                                                       | false                           |
| AMICE_VM_DUMP_LOWERING  | Dump LLVM IR to VM IR lowering through debug logs                                                 | false                           |

## Control Flow Flattening

Source code: `crates/amice/src/aotu/flatten`

| Variable                    | Description                                                                              | Default |
|-----------------------------|------------------------------------------------------------------------------------------|---------|
| AMICE_FLATTEN               | Enable flattening:<br/>- `true` — enabled<br/>- `false` — disabled                       | false   |
| AMICE_FLATTEN_MODE          | Obfuscation mode:<br/>- `basic` — basic mode<br/>- `dominator` — dominator-enhanced mode | `basic` |
| AMICE_FLATTEN_FIX_STACK     | Execute `fixStack` to fix phi after obfuscation                                          | true    |
| AMICE_FLATTEN_LOWER_SWITCH  | Automatically lower switch                                                               | true    |
| AMICE_FLATTEN_LOOP_COUNT    | Loop count (recommended <= 7)                                                            | 1       |
| AMICE_FLATTEN_ALWAYS_INLINE | Inline the `key_array` update function in `dominator` mode                               | false   |
| `AMICE_FLATTEN_SKIP_BIG_FUNCTION` | Skip oversized functions | false |

## MBA Arithmetic Obfuscation

Source code: `crates/amice/src/aotu/mba`

| Variable                             | Description                                                 | Default |
|--------------------------------------|-------------------------------------------------------------|---------|
| AMICE_MBA                            | Enable MBA:<br/>- `true` — enabled<br/>- `false` — disabled | false   |
| AMICE_MBA_FLOAT_REGIONS              | Enable exact binary64 SSA regions on supported FP targets    | true    |
| AMICE_MBA_PRE_EXPAND                 | Lower integer identity DAGs directly into FP regions        | true    |
| AMICE_MBA_OPAQUE_GUARD               | Read a private global seed once per call using volatile     | true    |
| AMICE_MBA_MAX_INSTRUCTIONS           | Source instructions per function; capped at 512; zero skips | `128`   |
| AMICE_MBA_MAX_ADDED_INSTRUCTIONS     | Conservative emitted-instruction budget per function       | `2048`  |

MBA now changes SSA representations across arithmetic, boolean operations, select and phi nodes.
The old auxiliary-variable, rewrite-depth, stack-fixing and optnone settings have been removed.
See [MBA region design](MbaRegions.md) for supported targets, semantic constraints and testing.

## Bogus Control Flow

Source: `crates/amice/src/aotu/bogus_control_flow`.

BCF obfuscates control flow to make reverse engineering more difficult and runs at OptimizerLast / FullLtoLast. It enables whole-original-function cloning and forced inlining by default; this addition can be disabled independently while retaining integer region rewriting. Integer regions support i8/i16/i32/i64 add/sub/and/or/xor, skipping regions in existing cycles and exception-handling functions.

| Variable | Description | Default |
|---|---|---|
| AMICE_BOGUS_CONTROL_FLOW | Enable BCF | `false` |
| AMICE_BOGUS_CONTROL_FLOW_PROB | Selection probability for whole-function cloning and candidate regions, 0–100; clamped to 100 | `80` |
| AMICE_BOGUS_CONTROL_FLOW_CLONE | Enable whole-original-function cloning and forced inlining; disabling retains integer region rewriting | `true` |
| AMICE_BOGUS_CONTROL_FLOW_MAX_REGIONS | Maximum regions per function; 0 disables, hard cap 16 | `2` |
| AMICE_BOGUS_CONTROL_FLOW_MAX_REGION_INSTRUCTIONS | Original instructions per region; below 2 disables, clamped to 16 | `8` |
| AMICE_BOGUS_CONTROL_FLOW_SEED | Decimal u64 seed; randomly generated when omitted, reproducible when explicitly set | Random |

Region budgets do not limit the size of the whole-function copy; `MAX_REGIONS=0` or `MAX_REGION_INSTRUCTIONS<2` disables BCF. Variadic, directly recursive, exception-handling functions, escaping block addresses and other unsupported inlining cases skip the cloning addition. If inlining fails, integer region rewriting still applies. Pointer arguments are forwarded unchanged. Integer arguments are perturbed only when operation preconditions remain valid. Later optimization may merge equivalent branches; inspect the final artifact to check what remains.

The configuration file switch is `bogus_control_flow.clone`. BCF increases runtime and code size, and copying a complete function costs more as the function grows. Start with a small set of selected functions. Do not enable it for code that requires constant-time execution. See [Function Annotations](FunctionAnnotations_en_US.md) for per-function configuration.

Functions containing `llvm.localescape`, `llvm.gcroot`, or an entry convergence token remain unchanged to preserve LLVM's entry-position requirements. Signatures containing types unsupported by the current bindings (such as Target Extension types), or calls to functions with `noduplicate`, `convergent`, or `returns_twice` restrictions (including alias calls), skip the cloning addition; eligible integer regions may still be processed. Functions containing inline assembly also skip cloning to avoid duplicating assembler labels and state.

Functions in `ExactMatch` / `SameSize` COMDAT groups remain unchanged so that independent randomized transformations across translation units cannot violate the linker's matching-content or matching-size requirements.

## Function Wrapper

Source code: `crates/amice/src/aotu/function_wrapper`

| Variable                           | Description                                                              | Default |
|------------------------------------|--------------------------------------------------------------------------|---------|
| AMICE_FUNCTION_WRAPPER             | Enable function wrapper:<br/>- `true` — enabled<br/>- `false` — disabled | false   |
| AMICE_FUNCTION_WRAPPER_PROBABILITY | Obfuscation probability                                                  | `70`    |
| AMICE_FUNCTION_WRAPPER_TIMES       | Loop iterations                                                          | `3`     |

## Clone Function (Constant Argument Specialization)

Source code: `crates/amice/src/aotu/clone_function`

| Variable             | Description                                                            | Default |
|----------------------|------------------------------------------------------------------------|---------|
| AMICE_CLONE_FUNCTION | Enable clone function:<br/>- `true` — enabled<br/>- `false` — disabled | false   |

## Alias Access

Source code: `crates/amice/src/aotu/alias_access`

| Variable                           | Description                                                                              | Default         |
|------------------------------------|------------------------------------------------------------------------------------------|-----------------|
| AMICE_ALIAS_ACCESS                 | Enable alias access:<br/>- `true` — enabled<br/>- `false` — disabled                     | false           |
| AMICE_ALIAS_ACCESS_MODE            | Working mode:<br/>- `pointer_chain` — random chained pointer access                      | `pointer_chain` |
| AMICE_ALIAS_ACCESS_SHUFFLE_RAW_BOX | Shuffle local variable allocation order:<br/>- `true` — enabled<br/>- `false` — disabled | false           |
| AMICE_ALIAS_ACCESS_LOOSE_RAW_BOX   | Insert phantom data in RawBox:<br/>- `true` — enabled<br/>- `false` — disabled           | false           |

## Custom Calling Convention

Source code: `crates/amice/src/aotu/custom_calling_conv`

| Variable                  | Description                                                                       | Default |
|---------------------------|-----------------------------------------------------------------------------------|---------|
| AMICE_CUSTOM_CALLING_CONV | Enable custom calling convention:<br/>- `true` — enabled<br/>- `false` — disabled | true    |

> The configuration defaults to true, but the calling-convention implementation is currently a placeholder. Even an annotation does not produce a calling-convention transform.

## GEP Offset Obfuscation (Delayed Offset Loading)

Source code: `crates/amice/src/aotu/delay_offset_loading`

| Variable                              | Description                                                                    | Default |
|---------------------------------------|--------------------------------------------------------------------------------|---------|
| AMICE_DELAY_OFFSET_LOADING            | Enable delayed offset loading:<br/>- `true` — enabled<br/>- `false` — disabled | false   |
| AMICE_DELAY_OFFSET_LOADING_XOR_OFFSET | XOR encrypt offset values                                                      | true    |

## Parameter Aggregation (PAO)

Source code: `crates/amice/src/aotu/param_aggregate`

| Variable              | Description                                                                   | Default |
|-----------------------|-------------------------------------------------------------------------------|---------|
| AMICE_PARAM_AGGREGATE | Enable parameter aggregation:<br/>- `true` — enabled<br/>- `false` — disabled | false   |

## Basic Block Outlining

Source code: `crates/amice/src/aotu/basic_block_outlining`

| Variable | Description | Default |
| --- | --- | --- |
| `AMICE_BASIC_BLOCK_OUTLINING` | Enable basic block outlining | false |
| `AMICE_BASIC_BLOCK_OUTLINING_MAX_EXTRACTOR_SIZE` | Extractor size parameter; must be a nonnegative integer; invalid input fails configuration parsing | 16 |

## Shuffle Blocks

Source code: `crates/amice/src/aotu/shuffle_blocks`

| Variable | Description | Default |
| --- | --- | --- |
| `AMICE_SHUFFLE_BLOCKS` | Enable block shuffling | false |
| `AMICE_SHUFFLE_BLOCKS_FLAGS` | Comma-separated `random`, `reverse`, `rotate`; select at least one to reorder blocks; environment flags merge with file flags | Empty |
