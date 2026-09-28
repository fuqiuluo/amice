# Function Annotations

English | [简体中文](FunctionAnnotations_zh_CN.md)

For passes supporting function annotations, precedence is **function annotation > environment > config file > default**. `AMICE_PASS_ORDER` is an outer pass allowlist: annotations cannot restore an excluded pass.

## Minimal C/C++ Example

```c
__attribute__((noinline, annotate("+flatten,flatten_mode=basic")))
int protected_function(int x) {
    if (x > 10) return x * 3;
    return x + 7;
}

__attribute__((noinline, annotate("-flatten")))
int excluded_function(int x) {
    return x + 1;
}
```

Compile a complete program containing these functions with the plugin as shown in [Quick Start](QuickStart_en_US.md). The first function requests Flatten even without a global switch; the second requests exclusion even when globally enabled. `noinline` helps preserve the demonstration function but does not guarantee that a pass supports it.

String encryption currently uses global configuration and has no working per-function annotation override. For Rust see [Rust Usage](RustUsage_en_US.md); the C attribute syntax above does not apply directly.

## Configuration Expressions

### Feature Switches

`+flag` enables a feature for the current function, `-flag` disables it.

### Key-Value Expressions

`^key=value` passes a key-value pair. You can also write `key=value` or `+key=value` / `-key=value`; prefer the plain `^key=value` or `key=value` forms.

> Special case: `+flag` = `+flag=yes` = `+flag=1` = `+flag=true`,
> equivalently `^flag=yes` = `^flag=1` = `^flag=true`
> and `flag=yes` = `flag=1` = `flag=true`.

## Available Obfuscations

### Alias Access

Enables alias-based access obfuscation with optional parameters for specific modes.

- Switch
    - `+alias_access` (alias: `+alias`)
        - Function: Enable alias access obfuscation (equivalent to `alias_access=true`)
        - Default: false

- Mode
    - `alias_access_mode=<mode>`
        - Function: Select obfuscation mode
        - Options: `pointer_chain` (alias: `basic`, `v1`)
        - Default: `pointer_chain`
        - Note: Currently only PointerChain mode is supported

- Parameters (PointerChain mode only)
    - `alias_access_shuffle_raw_box=<true|false>`
        - Function: Shuffle allocation order of compile-time local variables (RawBox)
        - Default: false
        - Example: `alias_access_shuffle_raw_box=true`
    - `alias_access_loose_raw_box=<true|false>`
        - Function: Insert invalid data in MetaBox (create holes/garbage padding)
        - Default: false
        - Example: `alias_access_loose_raw_box=true`

### Basic Block Outlining (BB2Func)

Extracts basic blocks from functions into independent sub-functions.

- Switch
    - `+basic_block_outlining` (alias: `+bb2func`)
        - Function: Enable function outlining
        - Default: false

- Parameters
    - `basic_block_outlining_max_extractor_size=<usize>` (alias: `bb2func_max_extractor_size`)
        - Function: Limit the maximum size of a single "outlining extractor" (roughly the size threshold for a basic block to be extracted)
        - Default: 16
        - Effect: Smaller values are more conservative; larger values may extract bigger basic blocks

### Bogus Control Flow (BCF)

Use these annotations to enable BCF, adjust budgets, or override global settings per function. Whole-function cloning and forced inlining can be disabled independently while retaining integer region rewriting. See [Environment Configuration](EnvConfig_en_US.md#bogus-control-flow) for supported cases and limitations.

| Annotation | Alias | Default and meaning |
|---|---|---|
| `+bogus_control_flow` | `+bcf`, `+boguscfg` | Disabled by default; `-bcf` disables per function |
| `bogus_control_flow_prob=<n>` | `bcf_prob` | 80; selection percentage for whole-function cloning and candidate regions, capped at 100 |
| `+bogus_control_flow_clone` | `+bcf_clone` | Enabled by default; `-bcf_clone` disables only the whole-function cloning addition |
| `bogus_control_flow_max_regions=<n>` | `bcf_max_regions` | 2; regions per function, 0 disables, hard cap 16 |
| `bogus_control_flow_max_region_instructions=<n>` | `bcf_max_region_instructions` | 8; original instructions per region, below 2 disables, hard cap 16 |
| `bogus_control_flow_seed=<n>` | `bcf_seed` | Inherits the global seed, random when unconfigured; decimal u64 seed |

Example: `__attribute__((annotate("+bcf bcf_prob=100 bcf_max_regions=1 bcf_max_region_instructions=6 bcf_seed=42")))`.

### Clone Function (Constant Argument Specialization)

Specializes functions for constant arguments: when a function is called with all or some constant parameters, generates a specialized clone with those constants inlined.

- Switch
    - `+clone_function`
        - Function: Enable constant argument specialization
        - Default: false

### Custom Calling Convention

Applies custom calling conventions to target functions, changing parameter passing and return value handling.

- Switch
    - `+custom_calling_conv` (alias: `+custom_cc`)
        - Function: Enable custom calling convention
        - Default: true
        - Note: Set to false to disable

> Currently unavailable

### Delayed Offset Loading (AMA)

Breaks the fixed pattern of "address = base + constant offset".

- Switch
    - `+delay_offset_loading` (alias: `+ama`)
        - Function: Enable delayed offset loading
        - Default: false

- Parameters
    - `delay_offset_loading_xor_offset=<true|false>` (alias: `ama_xor_offset`, `ama_xor`)
        - Function: Apply XOR perturbation to offset values, restored at use site
        - Default: true
        - Effect: Prevents offsets from appearing as plain constants, increasing recovery difficulty

### Control Flow Flattening (Flatten / FLA)

Scrambles the original basic block order and branch structure.

- Switch
    - `+flatten` (alias: `+flattening`, `+fla`)
        - Function: Enable control flow flattening
        - Default: false

- Mode
    - `flatten_mode=<mode>` (alias: `flattening_mode`, `fla_mode`)
        - Options:
            - `basic` (alias: `v1`)
            - `dominator` (alias: `dominator_enhanced`, `v2`)
        - Default: `basic`
        - Note: Dominator mode introduces enhanced dominator-based constructs on critical paths

- Parameters
    - `flatten_fix_stack=<true|false>` (alias: `flattening_fix_stack`, `fla_fix_stack`)
        - Function: Perform stack repair during obfuscation to prevent crashes
        - Default: true
    - `flatten_lower_switch=<true|false>` (alias: `flattening_lower_switch`, `fla_lower_switch`)
        - Function: Pre-lower switch statements to if-else chains
        - Default: true
    - `flatten_loop_count=<n>` (alias: `flattening_loop_count`, `fla_loop_count`)
        - Function: Number of times to repeat flattening on the same function
        - Default: 1
        - Suggestion: Start from 1 and increase gradually to balance size and performance
    - `flatten_skip_big_function=<true|false>` (alias: `flattening_skip_big_function`, `fla_skip_big_function`)
        - Function: Skip excessively large functions to avoid extreme performance overhead
        - Default: false
    - `flatten_always_inline=<true|false>` (alias: `flattening_always_inline`, `fla_always_inline`)
        - Function: In dominator mode, always inline critical update logic (e.g., state/key array updates)
        - Default: false
        - Note: Only meaningful in dominator mode

### Function Wrapper

Wraps call sites with one or more proxy/trampoline functions.

- Switch
    - `+function_wrapper` (alias: `+func_wrapper`)
        - Function: Enable function wrapper obfuscation
        - Default: false

- Parameters
    - `function_wrapper_probability=<0..100>` (alias: `func_wrapper_probability`, `function_wrapper_prob`, `func_wrapper_prob`)
        - Function: Probability (percentage) of each call site being wrapped
        - Default: 70
        - Constraint: Values above 100 are treated as 100

### Indirect Branch (IndirectBr / IB)

Rewrites direct basic block jumps.

- Switch
    - `+indirect_branch` (alias: `+ib`, `+indirectbr`, `+indbr`)
        - Function: Enable indirect branch obfuscation
        - Default: false

### Indirect Call (ICall)

Rewrites direct function calls.

- Switch
    - `+indirect_call` (alias: `+icall`, `+indirectcall`)
        - Function: Enable indirect call obfuscation
        - Default: false

### Lower Switch (Switch to If-Else)

Pre-lowers switch statements to if-else chains, facilitating subsequent obfuscation transformations.

- Switch
    - `+lower_switch` (alias: `+lowerswitch`, `+switch_to_if`)
        - Function: Enable switch lowering
        - Default: false

### Mixed Boolean Arithmetic (MBA / LinearMBA)

Replaces arithmetic/logical operations with equivalent MBA expressions.

- Switch
    - `+mba`; optional `-mba_float_regions`, `-mba_pre_expand`, `-mba_opaque_guard`, `mba_max_instructions=32`, `mba_max_added_instructions=512`
        - Function: Enable MBA rewriting
        - Default: false

### Param Aggregate

Aggregates multiple function parameters into a single structure for passing.

- Switch
    - `+param_aggregate`
        - Function: Enable parameter aggregation
        - Default: false

### Shuffle Blocks

Scrambles the order of basic blocks within a function.

- Switch
    - `+shuffle_blocks`
        - Function: Enable basic block shuffling
        - Default: false

### Split Basic Block

Splits a single basic block into multiple smaller basic blocks.

- Switch
    - `+split_basic_block`
        - Function: Enable basic block splitting
        - Default: false

- Parameters
    - `split_basic_block_num=<u32>`
        - Function: Number of split iterations per splittable basic block
        - Default: 3
        - Note: Larger values increase block count and jumps, also increasing size and compile time

### VM Flatten (VMF)

Rewrites the target function as a control flow driven by a "virtual machine interpreter + bytecode/instruction table".

- Switch
    - `+vm_flatten` (alias: `+vmf`)
        - Function: Enable VM-based control flow flattening
        - Default: false

### Instruction-Level VMP (VmVirtualize)

- `+vm_virtualize` (alias `+vmp`): enable VMP for this function; default false.
- `vm_profile_path=/absolute/path/to/profile` (alias `vm_profile`): select a profile package; defaults to the built-in `amice-simple-vmp`.
- `vm_runtime_scope=func` or `module`: override the profile's runtime scope.

```c
__attribute__((noinline, annotate("+vm_virtualize,vm_runtime_scope=func")))
int sensitive(int x) {
    return (x * 7) ^ 0x55;
}
```

Unsupported functions keep their original implementation. Use `RUST_LOG=amice=debug` to inspect skip reasons. Validate one function before expanding coverage; enabling VMP does not guarantee every function is virtualized. Profile constraints are documented in [VMP Design (Chinese)](VMPDesign_zh_CN.md).
