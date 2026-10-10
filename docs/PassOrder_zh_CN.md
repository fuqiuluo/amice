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

## 编译时间与强度的权衡

混淆的开销主要不是 Pass 自身，而是**膨胀后的 IR 让后续优化管线做更多工作**。在 300 个函数 / 约 5400 行的合成工程（`opt -passes=default<O2>`，LLVM 20.1.1，x86_64 Windows，数值随机器与工程形态变化，只看相对量级）实测：

| 配置 | opt 耗时 | 相对无混淆 |
| --- | --- | --- |
| 无混淆 | 640 ms | 1.0× |
| 轻量组合：StringEncryption + IndirectCall + IndirectBranch + SplitBasicBlock | 825 ms | ≈1.3× |
| 仅 `Flatten` | 1517 ms | ≈2.4× |
| 仅 `VmFlatten` | 1514 ms | ≈2.4× |
| 仅 `StringEncryption` | 1142 ms | ≈1.8× |
| 重混淆全开（BCF+Flatten+VmFlatten+MBA+IndirectCall+IndirectBranch+Split+StringEncryption） | 2484 ms | ≈3.9× |

Pass 自身的执行时间只占小头（全开时合计约 300 ms，其中 IndirectBranch 约 107 ms）；其余都是后续优化管线在消化膨胀后的 IR。因此控制编译时间最有效的手段按顺序是：

1. **少开重型 Pass**：`Flatten` / `VmFlatten` 单开就有 2.4×；仅需要字符串与调用混淆时用轻量组合（约 1.3×）。
2. **按函数启用**：用 [函数注解](FunctionAnnotations_zh_CN.md) 只混淆安全敏感函数，混淆范围线性决定编译时间。
3. **调低强度参数**：如 `AMICE_MBA_MAX_ADDED_INSTRUCTIONS`（默认 2048，对小函数接近无上限）、`AMICE_BOGUS_CONTROL_FLOW_MAX_REGIONS` 等，生成量减少会同时降低 Pass 内部与后续管线的成本。
4. `SplitBasicBlock` 在 `PipelineStart` 与 `OptimizerLast` 各运行一次是有意设计（防止后续优化把切开的块重新合并），不要因为耗时移除其中一段。

