# Pass Execution Order

English | [简体中文](PassOrder_zh_CN.md)

## Rules

- Explicit `pass_order.order` / `AMICE_PASS_ORDER` is an allowlist: only listed passes are installed. Names are case-sensitive.
- Ordering does not enable a pass. Enable it separately through configuration, environment variables or supported function annotations.
- Explicit order overrides `priority_override`. Otherwise, passes are sorted by descending priority, using defaults for entries without an override.
- `BogusControlFlow` must precede `VmFlatten`. Explicit orders and priority overrides cannot invert this dependency; conflicts are adjusted with a warning. The allowlist still controls which passes are installed.
- Order applies within the same LLVM extension point. It cannot move an `OptimizerLast` pass ahead of `PipelineStart`. Do not rely on an ordering between equal-priority passes.
- Duplicate list entries do not install a pass repeatedly; the last occurrence determines its position. A pass registered at multiple stages may still execute at multiple stages, so this is not a once-per-compilation guarantee.
- Unknown names currently match nothing and do not report an error. Use the exact names in the table below.

## Complete Configuration Examples

These three files are equivalent; save one. They both enable and order string encryption, indirect calls and basic Flatten. Validate passes individually before combining them in a real project.

TOML (`amice.toml`):

```toml
[pass_order]
order = ["StringEncryption", "IndirectCall", "Flatten"]

[string_encryption]
enable = true

[indirect_call]
enable = true

[flatten]
enable = true
mode = "basic"
```

YAML (`amice.yaml`):

```yaml
pass_order:
  order: [StringEncryption, IndirectCall, Flatten]
string_encryption:
  enable: true
indirect_call:
  enable: true
flatten:
  enable: true
  mode: basic
```

JSON (`amice.json`):

```json
{
  "pass_order": { "order": ["StringEncryption", "IndirectCall", "Flatten"] },
  "string_encryption": { "enable": true },
  "indirect_call": { "enable": true },
  "flatten": { "enable": true, "mode": "basic" }
}
```

Set `AMICE_CONFIG_PATH` before your existing compile command, keeping the same plugin-loading method:

```bash
export AMICE_CONFIG_PATH="$(pwd)/amice.toml"
```

Windows PowerShell:

```powershell
$env:AMICE_CONFIG_PATH = (Resolve-Path .\amice.toml).Path
```

Recompile source after changing configuration. Running an old binary or reusing cached objects does not apply new settings. Other `AMICE_*` variables override corresponding file values.

## Environment Overrides

```bash
export AMICE_PASS_ORDER="StringEncryption,IndirectCall,Flatten"
export AMICE_STRING_ENCRYPTION=true
export AMICE_INDIRECT_CALL=true
export AMICE_FLATTEN=true
```

```powershell
$env:AMICE_PASS_ORDER = 'StringEncryption,IndirectCall,Flatten'
$env:AMICE_STRING_ENCRYPTION = 'true'
$env:AMICE_INDIRECT_CALL = 'true'
$env:AMICE_FLATTEN = 'true'
```

Separate names with commas or semicolons. The allowlist excludes other passes even if their environment variables or function annotations enable them.

To change priority while retaining other passes, delete `order` from the config file, unset `AMICE_PASS_ORDER`, then use:

```bash
unset AMICE_PASS_ORDER
export AMICE_PASS_PRIORITY_OVERRIDE="IndirectCall=1100,StringEncryption=1000"
```

```powershell
Remove-Item Env:AMICE_PASS_ORDER -ErrorAction SilentlyContinue
$env:AMICE_PASS_PRIORITY_OVERRIDE = 'IndirectCall=1100,StringEncryption=1000'
```

Equivalent TOML (do not keep `order` alongside it):

```toml
[pass_order.priority_override]
IndirectCall = 1100
StringEncryption = 1000
```

## Names, Default Priorities and Stages

| Name | Default priority | LLVM extension point |
| --- | --- | --- |
| `DelayOffsetLoading` | 1150 | `PipelineStart` |
| `CustomCallingConv` | 1121 | `PipelineStart` |
| `ParamAggregate` | 1120 | `PipelineStart` |
| `AliasAccess` | 1112 | `PipelineStart` |
| `CloneFunction` | 1111 | `PipelineStart` |
| `FunctionWrapper` | 1010 | `PipelineStart` |
| `StringEncryption` | 1000 | `PipelineStart` |
| `IndirectCall` | 990 | `PipelineStart` |
| `BasicBlockOutlining` | 979 | `PipelineStart` |
| `ShuffleBlocks` | 970 | `PipelineStart` |
| `BogusControlFlow` | 970 | `OptimizerLast, FullLtoLast` |
| `LowerSwitch` | 961 | `PipelineStart` |
| `VmFlatten` | 960 | `OptimizerLast` / `FullLtoLast` |
| `Flatten` | 959 | `PipelineStart` |
| `SplitBasicBlock` | 958 | `PipelineStart, OptimizerLast` |
| `Mba` | 955 | `OptimizerLast` |
| `VmVirtualize` | 955 | `OptimizerLast` |
| `IndirectBranch` | 800 | `PipelineStart` |

This table reflects current source registrations. `CustomCallingConv` is currently a placeholder and should not be treated as effective protection. See [Environment Variables](EnvConfig_en_US.md) for switches and parameters, and [Function Annotations](FunctionAnnotations_en_US.md) for local overrides.
