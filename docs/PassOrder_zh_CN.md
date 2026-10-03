# Pass 运行顺序

[English](PassOrder_en_US.md) | 简体中文

## 规则

- 显式 `pass_order.order` / `AMICE_PASS_ORDER` 是允许列表：只有列出的 Pass 会被安装，名称区分大小写。
- 顺序配置不等于启用开关；仍需在配置文件、环境变量或支持的函数注解中启用对应功能。
- 显式顺序优先于 `priority_override`。没有显式顺序时，按覆盖后的优先级从高到低排序，未覆盖的使用默认值。
- `BogusControlFlow` 必须先于 `VmFlatten`；显式顺序或优先级覆盖不能倒置此依赖，冲突时调整并输出警告。允许列表仍决定安装哪些 Pass。
- 顺序只作用于同一个 LLVM 执行阶段，不能把 `OptimizerLast` 的 Pass 移到 `PipelineStart` 之前。同优先级 Pass 不应被视为有固定先后关系。
- 列表重复项不会使 Pass 重复安装；重复名称的位置以最后一次出现为准。一个 Pass 若注册多个阶段，仍可能在不同阶段运行，不能理解为整个编译只运行一次。
- 未知名称不会匹配任何 Pass，且当前不报错。设置顺序时使用下表的准确名称。

## 可直接使用的配置

下面三个文件内容等价，保存其中一个即可。它们同时启用并排序字符串加密、间接调用和基础 Flatten；先验证单独启用各 Pass 的结果，再在实际项目中组合使用。

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

在原编译命令前设置 `AMICE_CONFIG_PATH`，保持原来的插件加载方式：

```bash
export AMICE_CONFIG_PATH="$(pwd)/amice.toml"
```

Windows PowerShell：

```powershell
$env:AMICE_CONFIG_PATH = (Resolve-Path .\amice.toml).Path
```

修改配置后重新编译目标源码；只重跑旧二进制或让构建系统复用旧对象不会应用新配置。不要设置其他冲突的 `AMICE_*` 变量，否则它们会覆盖文件值。

## 用环境变量指定顺序

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

名称之间可以用逗号或分号分隔。显式允许列表会排除其他 Pass，即使对应环境变量或函数注解已开启。

如果只想调整优先级而保留其他 Pass，将配置文件中的 `order` 删除、取消 `AMICE_PASS_ORDER`，然后设置：

```bash
unset AMICE_PASS_ORDER
export AMICE_PASS_PRIORITY_OVERRIDE="IndirectCall=1100,StringEncryption=1000"
```

```powershell
Remove-Item Env:AMICE_PASS_ORDER -ErrorAction SilentlyContinue
$env:AMICE_PASS_PRIORITY_OVERRIDE = 'IndirectCall=1100,StringEncryption=1000'
```

对应的 TOML（不要同时保留 `order`）：

```toml
[pass_order.priority_override]
IndirectCall = 1100
StringEncryption = 1000
```

## 名称、默认优先级与阶段

| 名称 | 默认优先级 | LLVM 注册阶段 |
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

这张表对应当前源码中的注册信息。`CustomCallingConv` 目前是预留实现，不应当作为已生效的保护手段。开关及参数见 [环境变量](EnvConfig_zh_CN.md)，局部配置见 [函数注解](FunctionAnnotations_zh_CN.md)。
