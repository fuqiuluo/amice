# Runtime Environment Variables

## String Encryption

Source code: `src/aotu/string_encryption`

| Variable                                 | Description                                                                                                                                                                                                                                           | Default |
|------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------|
| AMICE_STRING_ENCRYPTION                  | Enable string encryption:<br/>- `true` — enabled<br/>- `false` — disabled                                                                                                                                                                             | false   |
| AMICE_STRING_ALGORITHM                   | String encryption algorithm:<br/>- `xor` — XOR encryption<br/>- `simd_xor` — (beta) SIMD-based XOR encryption                                                                                                                                         | xor     |
| AMICE_STRING_DECRYPT_TIMING              | String decryption timing:<br/>- `global` — decrypt all protected strings at global initialization during program startup<br/>- `lazy` — decrypt on-demand before first use (then cache)<br/>Note: Stack-allocated strings do not support this config! | lazy    |
| AMICE_STRING_STACK_ALLOC                 | (beta) Memory allocation for decrypted strings:<br/>- `true` — allocate on stack<br/>- `false` — allocate on heap<br/>Note: Stack allocation only supports `lazy` timing!                                                                             | false   |
| AMICE_STRING_INLINE_DECRYPT_FN           | Inline decrypt function:<br/>- `true` — inline<br/>- `false` — don't inline                                                                                                                                                                           | false   |
| AMICE_STRING_ONLY_DOT_STRING             | Only process strings in `.str` section:<br/>- `true` — only encrypt `.str` strings<br/>- `false` — may encrypt char[] global variables in llvm::Module, possibly causing crashes                                                                      | true    |
| AMICE_STRING_ALLOW_NON_ENTRY_STACK_ALLOC | Allow stack allocation in non-entry blocks (stack decryption mode):<br/>- `true` — allow (many LLVM optimization passes assume all alloca are in entry block)<br/>- `false` — recommended                                                             | false   |

> **Note for Rust**: 
> ```bash
> export AMICE_STRING_ONLY_LLVM_STRING=false  # Rust string globals are named alloc_xxx, not .str
> ```

## Indirect Call Obfuscation

Source code: `src/aotu/indirect_call`

| Variable                    | Description                                                                     | Default |
|-----------------------------|---------------------------------------------------------------------------------|---------|
| AMICE_INDIRECT_CALL         | Enable indirect calls:<br/>- `true` — enabled<br/>- `false` — disabled          | false   |
| AMICE_INDIRECT_CALL_XOR_KEY | XOR key for indirect call index<br/>Note: Input `0` to disable index encryption | Random  |

## Indirect Branch Obfuscation

Source code: `src/aotu/indirect_branch`

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

Source code: `src/aotu/split_basic_block`

| Variable                     | Description                                                                   | Default |
|------------------------------|-------------------------------------------------------------------------------|---------|
| AMICE_SPLIT_BASIC_BLOCK      | Enable basic block splitting:<br/>- `true` — enabled<br/>- `false` — disabled | false   |
| AMICE_SPLIT_BASIC_BLOCk_NUMS | Number of split iterations                                                    | 3       |

## Switch Lowering

Source code: `src/aotu/lower_switch`

| Variable                               | Description                                                                                | Default |
|----------------------------------------|--------------------------------------------------------------------------------------------|---------|
| AMICE_LOWER_SWITCH                     | Enable switch lowering:<br/>- `true` — enabled<br/>- `false` — disabled                    | false   |
| ~~AMICE_LOWER_SWITCH_WITH_DUMMY_CODE~~ | Insert dummy code after lowering (may fail module verification causing `-O1` etc. to fail) | false   |

## VM Flatten

Source code: `src/aotu/vm_flatten`

| Variable         | Description                                                        | Default |
|------------------|--------------------------------------------------------------------|---------|
| AMICE_VM_FLATTEN | Enable VM flatten:<br/>- `true` — enabled<br/>- `false` — disabled | false   |

## Control Flow Flattening

Source code: `src/aotu/flatten`

| Variable                    | Description                                                                              | Default |
|-----------------------------|------------------------------------------------------------------------------------------|---------|
| AMICE_FLATTEN               | Enable flattening:<br/>- `true` — enabled<br/>- `false` — disabled                       | false   |
| AMICE_FLATTEN_MODE          | Obfuscation mode:<br/>- `basic` — basic mode<br/>- `dominator` — dominator-enhanced mode | `basic` |
| AMICE_FLATTEN_FIX_STACK     | Execute `fixStack` to fix phi after obfuscation                                          | true    |
| AMICE_FLATTEN_LOWER_SWITCH  | Automatically lower switch                                                               | true    |
| AMICE_FLATTEN_LOOP_COUNT    | Loop count (recommended <= 7)                                                            | 1       |
| AMICE_FLATTEN_ALWAYS_INLINE | Inline the `key_array` update function in `dominator` mode                               | false   |

## MBA Arithmetic Obfuscation

Source code: `src/aotu/mba`

| Variable                             | Description                                                 | Default |
|--------------------------------------|-------------------------------------------------------------|---------|
| AMICE_MBA                            | Enable MBA:<br/>- `true` — enabled<br/>- `false` — disabled | false   |
| AMICE_MBA_AUX_COUNT                  | Number of auxiliary variables                               | `2`     |
| AMICE_MBA_REWRITE_OPS                |                                                             | `24`    |
| AMICE_MBA_REWRITE_DEPTH              |                                                             | `3`     |
| AMICE_MBA_ALLOC_AUX_PARAMS_IN_GLOBAL | Allocate auxiliary variables in global                      | false   |
| AMICE_MBA_FIX_STACK                  | Execute `fixStack` after obfuscation                        | false   |

## Bogus Control Flow

Source code: `src/aotu/bogus_control_flow`

| Variable                       | Description                                                                                                                                                                                                                                             | Default |
|--------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------|
| AMICE_BOGUS_CONTROL_FLOW       | Enable BCF:<br/>- `true` — enabled<br/>- `false` — disabled                                                                                                                                                                                             | false   |
| AMICE_BOGUS_CONTROL_FLOW_MODE  | Obfuscation mode:<br/>- `basic` — default, basic mode, may be optimized away<br/>- `polaris-primes` — variant from [Polaris-Obfuscator](https://github.com/za233/Polaris-Obfuscator/blob/main/src/llvm/lib/Transforms/Obfuscation/BogusControlFlow.cpp) | `basic` |
| AMICE_BOGUS_CONTROL_FLOW_PROB  | Obfuscation probability                                                                                                                                                                                                                                 | `80`    |
| AMICE_BOGUS_CONTROL_FLOW_LOOPS | Loop iterations                                                                                                                                                                                                                                         | `1`     |

## Function Wrapper

Source code: `src/aotu/function_wrapper`

| Variable                           | Description                                                              | Default |
|------------------------------------|--------------------------------------------------------------------------|---------|
| AMICE_FUNCTION_WRAPPER             | Enable function wrapper:<br/>- `true` — enabled<br/>- `false` — disabled | false   |
| AMICE_FUNCTION_WRAPPER_PROBABILITY | Obfuscation probability                                                  | `80`    |
| AMICE_FUNCTION_WRAPPER_TIMES       | Loop iterations                                                          | `1`     |

## Clone Function (Constant Argument Specialization)

Source code: `src/aotu/clone_function`

| Variable             | Description                                                            | Default |
|----------------------|------------------------------------------------------------------------|---------|
| AMICE_CLONE_FUNCTION | Enable clone function:<br/>- `true` — enabled<br/>- `false` — disabled | false   |

## Alias Access

Source code: `src/aotu/alias_access`

| Variable                           | Description                                                                              | Default         |
|------------------------------------|------------------------------------------------------------------------------------------|-----------------|
| AMICE_ALIAS_ACCESS                 | Enable alias access:<br/>- `true` — enabled<br/>- `false` — disabled                     | false           |
| AMICE_ALIAS_ACCESS_MODE            | Working mode:<br/>- `pointer_chain` — random chained pointer access                      | `pointer_chain` |
| AMICE_ALIAS_ACCESS_SHUFFLE_RAW_BOX | Shuffle local variable allocation order:<br/>- `true` — enabled<br/>- `false` — disabled | false           |
| AMICE_ALIAS_ACCESS_LOOSE_RAW_BOX   | Insert phantom data in RawBox:<br/>- `true` — enabled<br/>- `false` — disabled           | false           |

## Custom Calling Convention

Source code: `src/aotu/custom_calling_conv`

| Variable                  | Description                                                                       | Default |
|---------------------------|-----------------------------------------------------------------------------------|---------|
| AMICE_CUSTOM_CALLING_CONV | Enable custom calling convention:<br/>- `true` — enabled<br/>- `false` — disabled | true    |

> Enabled by default, but won't modify any function's calling convention unless annotated:
> ```cpp
> #define OBFUSCATE_CC __attribute__((annotate("+custom_calling_conv")))
>
> OBFUSCATE_CC
> int add(int a, int b) {
>      return a + b;
> }
> ```
> Only functions marked with the `custom_calling_conv` annotation will be processed!

## GEP Offset Obfuscation (Delayed Offset Loading)

Source code: `src/aotu/delay_offset_loading`

| Variable                              | Description                                                                    | Default |
|---------------------------------------|--------------------------------------------------------------------------------|---------|
| AMICE_DELAY_OFFSET_LOADING            | Enable delayed offset loading:<br/>- `true` — enabled<br/>- `false` — disabled | false   |
| AMICE_DELAY_OFFSET_LOADING_XOR_OFFSET | XOR encrypt offset values                                                      | true    |

## Parameter Aggregation (PAO)

Source code: `src/aotu/param_aggregate`

| Variable              | Description                                                                   | Default |
|-----------------------|-------------------------------------------------------------------------------|---------|
| AMICE_PARAM_AGGREGATE | Enable parameter aggregation:<br/>- `true` — enabled<br/>- `false` — disabled | false   |
