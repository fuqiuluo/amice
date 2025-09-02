### Pass 运行顺序与优先级覆盖能力说明

本文档介绍如何通过配置和环境变量控制 Pass 的运行顺序与优先级，以覆盖编译期写死的优先级。

#### 背景与目标
- 默认的运行顺序仅由编译期注解中的 `priority` 决定（数值越大越先执行）。
- 运行时优先级配置：
    - 显式顺序：在配置文件中写出确切顺序，严格按列表运行，**未出现的不运行**。
    - 覆盖优先级：不改动源码注解，按名称为单个 **Pass** 提供新的优先级数值。

> **显式顺序** > **覆盖优先级** >= **编译期注解优先级**，当显式顺序存在的时候，优先级完全失效，严格按照显式顺序运行，不在显式顺序中的不会运行。
>
> 未避免不可预知的问题，每个Pass只会运行一次！

#### 配置结构
```rust
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PassOrderConfig {
    /// 显式运行顺序；若为 None，则按优先级排序
    pub order: Option<Vec<String>>,
    /// 覆盖各 Pass 的优先级（越大越靠前）
    pub priority_override: HashMap<String, i32>,
}
```

环境变量覆盖实现：
```rust
impl EnvOverlay for PassOrderConfig {
    fn overlay_env(&mut self) {
        // AMICE_PASS_ORDER="A,B,C" 或 "A;B;C"
        // AMICE_PASS_PRIORITY_OVERRIDE="A=1200,B=500" 或 "A=1200;B=500"
    }
}
```

#### 生效优先级与规则
运行顺序采用如下规则：
1) 如果配置中提供了显式顺序 `pass_order.order`：
- 列表中的 Pass 严格按给定顺序运行。
- 未出现在列表中的 Pass，**不会运行**。
- 若显式顺序与 `priority_override` 同时存在，`显式order`优先，`priority_override`不执行。

2) 如果未提供显式顺序，但提供了 `pass_order.priority_override`：
- 按覆盖后的优先级从高到低排序；未覆盖的使用默认优先级。

3) 如果都未提供：
- 回退为默认行为：按编译期优先级从高到低排序。

#### 配置文件示例
TOML 示例：TODO
YAML 示例：TODO
JSON 示例：TODO

#### 环境变量覆盖
支持在不改动配置文件的情况下临时覆盖：
- 显式顺序
    - AMICE_PASS_ORDER="StringEncryption,SplitBasicBlock,ShuffleBlocks,IndirectBranch,IndirectCall"
    - 分隔符可用逗号或分号："A,B,C" 或 "A;B;C"
- 覆盖优先级
    - AMICE_PASS_PRIORITY_OVERRIDE="StringEncryption=1200,IndirectBranch=500"
    - 同样支持分号作为分隔符："A=1200;B=500"

#### 不同Pass的名称与默认优先级
因为可能随着更新而改变名称或优先级的数值，wiki这里同步更新不够及时，这里给出一段源代码（如有需要改变运行顺序请自行翻阅源代码）:
```rust
#[amice(priority = 800, name = "IndirectBranch")]
#[derive(Default)]
pub struct IndirectBranch {
    enable: bool,
    flags: IndirectBranchFlags,
    xor_key: Option<[u32; 4]>,
}
```
