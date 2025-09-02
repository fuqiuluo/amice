# 函数注解（FunctionAnnotation）

源代码内的`annotate`的优先级永远高于环境变量/配置文件，~~但是低于脚本配置~~。

## 配置表达式

### 功能开关

`+flag`表示在当前函数启用某功能, `-flag` 表示在当前函数禁用某功能

### 键值表达式

`^key=value`传递一个键值对，当然你也可以写为`key=value`或者(`+key=value`/`-key=value`)，
最好按照正常情况来编写，否则可能出问题！

> 特殊情况: `+flag` = `+flag=yes` = `+flag=1` = `+flag=true`
> </br> = `^flag=yes` = `^flag=1` = `^flag=true`
> </br> = `flag=yes` = `flag=1` = `flag=true`

## 可用的混淆

### 别名访问（Alias Access）

用于启用基于别名的访问混淆能力，并在指定模式下提供若干可选参数。

- 开关
    - `+alias_access`（别名：`+alias`）
        - 功能：开启别名访问混淆（等价于 `alias_access=true`）
        - 默认值：false

- 模式
    - `alias_access_mode=<mode>`
        - 功能：选择混淆模式
        - 可选值：`pointer_chain`（别名：`basic`, `v1`）
        - 默认值：`pointer_chain`
        - 说明：当前仅支持 PointerChain 模式

- 仅在 PointerChain 模式下生效的参数
    - `alias_access_shuffle_raw_box=<true|false>`
        - 功能：是否打乱编译期产生的局部变量（RawBox）的分配顺序
        - 默认值：false
        - 示例：`alias_access_shuffle_raw_box=true`
    - `alias_access_loose_raw_box=<true|false>`
        - 功能：是否在 MetaBox 中插入无效数据（制造空洞/垃圾填充）
        - 默认值：false
        - 示例：`alias_access_loose_raw_box=true`

### 函数分片（Basic Block Outlining / BB2Func）

将函数中的基础块（Basic Block）提取为独立子函数。

- 开关
    - `+basic_block_outlining`（别名：`+bb2func`）
        - 功能：启用函数分片
        - 默认值：false

- 参数
    - `basic_block_outlining_max_extractor_size=<usize>`（别名：`bb2func_max_extractor_size`）
        - 功能：限制单次“分片提取器”的规模上限（大致可理解为一个基础块在被提取/外提时允许的大小阈值）
        - 默认值：16
        - 影响：数值越小，分片越保守；数值越大，可能提取更大的基础块为独立函数

### 伪造控制流（Bogus Control Flow / BCF）

通过在基本块间插入无效或等价分支。

- 开关
    - `+bogus_control_flow`（别名：`+bcf`）
        - 功能：启用伪造控制流
        - 默认值：false

- 模式
    - `bogus_control_flow_mode=<mode>`（别名：`bcf_mode`）
        - 可选值：
            - `basic`（别名：`v1`）
            - `polaris-primes`（别名：`primes`, `v2`）
        - 默认值：`basic`
        - 说明：不同模式生成伪造路径的策略不同，可根据需求选择

- 参数
    - `bogus_control_flow_prob=<0..100>`（别名：`bcf_prob`）
        - 功能：每个基本块被混淆（应用伪造分支）的概率，单位为百分比
        - 默认值：80
        - 范围与行为：0–100；超过上限将按 100 处理，非法值忽略并保留原值/默认值
    - `bogus_control_flow_loops=<n>`（别名：`bcf_loops`）
        - 功能：对同一函数重复执行该混淆 Pass 的次数
        - 默认值：1
        - 要求：n ≥ 1；小于 1 或非法值将忽略并保留原值/默认值

### 常参特化克隆（Clone Function）

对调用点的“常量实参”进行特化：当一个函数以全部或部分常量参数被调用时，可生成该函数的“特化克隆版本”，将这些常量直接内联到函数体中。

- 开关
    - `+clone_function`
        - 功能：启用常参特化克隆混淆
        - 默认值：false

### 自定义调用约定（Custom Calling Convention / Custom CC）

为目标函数应用自定义调用约定，以改变参数传递与返回值处理的方式。

- 开关
    - `+custom_calling_conv`（别名：`+custom_cc`）
        - 功能：启用自定义调用约定
        - 默认值：true
        - 关闭方式：在相应作用域设置为 false（见下文环境变量与函数注解）

> 当前不可用

### 延时偏移加载（AMA: Delayed Offset Loading）

打散“地址=基址+常量偏移”的固定模式。

- 开关
    - `+delay_offset_loading`（别名：`+ama`）
        - 功能：启用延时偏移加载
        - 默认值：false

- 参数
    - `delay_offset_loading_xor_offset=<true|false>`（别名：`ama_xor_offset`, `ama_xor`）
        - 功能：对偏移值施加 XOR 扰动，再在使用点还原
        - 默认值：true
        - 影响：使偏移不以常量形式直观出现，增加恢复难度；但会引入少量指令开销

### 控制流扁平化（Flatten / Flattening / FLA）

把原有基本块的顺序与分支结构打散。

- 开关
    - `+flatten`（别名：`+flattening`, `+fla`）
        - 功能：启用控制流扁平化
        - 默认值：false

- 模式
    - `flatten_mode=<mode>`（别名：`flattening_mode`, `fla_mode`）
        - 可选值：
            - `basic`（别名：`v1`）
            - `dominator`（别名：`dominator_enhanced`, `v2`）
        - 默认值：`basic`
        - 说明：dominator 模式在关键路径上引入支配关系增强的构造，并可配合“总是内联”策略强化混淆。

- 参数
    - `flatten_fix_stack=<true|false>`（别名：`flattening_fix_stack`, `fla_fix_stack`）
        - 功能：在混淆过程中进行栈修复，避免异常或崩溃
        - 默认值：true
    - `flatten_lower_switch=<true|false>`（别名：`flattening_lower_switch`, `fla_lower_switch`）
        - 功能：将 switch 预降级为 if-else 链，便于后续处理
        - 默认值：true
    - `flatten_loop_count=<n>`（别名：`flattening_loop_count`, `fla_loop_count`）
        - 功能：对同一函数重复执行扁平化 Pass 的次数
        - 默认值：1
        - 建议：从 1 起步，逐步增大以平衡体积与性能
    - `flatten_skip_big_function=<true|false>`（别名：`flattening_skip_big_function`, `fla_skip_big_function`）
        - 功能：跳过体量过大的函数以避免极端性能开销
        - 默认值：false
    - `flatten_always_inline=<true|false>`（别名：`flattening_always_inline`, `fla_always_inline`）
        - 功能：在 dominator 模式下，总是内联关键更新逻辑（例如状态/密钥数组更新）以增强混淆
        - 默认值：false
        - 说明：仅在 dominator 模式下具有实际意义

### 函数包装（Function Wrapper / Func Wrapper）

在调用点外包一层或多层“代理函数/封装跳板”。

- 开关
    - `+function_wrapper`（别名：`+func_wrapper`）
        - 功能：启用函数包装混淆
        - 默认值：false

- 参数
    - `function_wrapper_probability=<0..100>`（别名：`func_wrapper_probability`, `function_wrapper_prob`, `func_wrapper_prob`）
        - 功能：每个调用点被包装的概率（百分比）
        - 默认值：70
        - 约束：超过 100 将按 100 处理

### 间接跳转（Indirect Branch / IndirectBr / IndBr / IB）

将直接跳转基本块改写。

- 开关
    - `+indirect_branch`（别名：`+ib`, `+indirectbr`, `+indbr`）
        - 功能：启用间接跳转混淆
        - 默认值：false

### 间接调用（Indirect Call / ICall / IndirectCall）

改写直接函数调用。

- 开关
    - `+indirect_call`（别名：`+icall`, `+indirectcall`）
        - 功能：启用间接调用混淆
        - 默认值：false

### Switch 降级（Lower Switch / Switch→If-Else）

将 switch 语句预先降级为 if-else 链，便于后续混淆变换（如扁平化、伪造控制流）。

- 开关
    - `+lower_switch`（别名：`+lowerswitch`, `+switch_to_if`）
        - 功能：启用 switch→if-else 降级
        - 默认值：false

### 混合布尔算术（MBA: Mixed Boolean Arithmetic / LinearMBA）

将算术/逻辑运算替换为等价的 MBA 表达式。

- 开关
    - `+mba`（别名：`+linearmba`）
        - 功能：启用 MBA 重写
        - 默认值：false

### 参数结构化访问（Param Aggregate）

将函数的多个形参聚合为单一结构体进行传递。

- 开关
    - `+param_aggregate`
        - 功能：启用参数结构化访问
        - 默认值：false

### 基本块乱序（Shuffle Blocks）

对函数内的基本块顺序进行扰动。

- 开关
    - `+shuffle_blocks`
        - 功能：启用基本块乱序
        - 默认值：false

### 基本块拆分（Split Basic Block）

将单个基本块拆分为多个更小的基本块。

- 开关
    - `+split_basic_block`
        - 功能：启用基本块拆分
        - 默认值：false

- 参数
    - `split_basic_block_num=<u32>`
        - 功能：对每个可拆分的基本块重复执行的拆分次数（一次可能引入多个新块）
        - 默认值：3
        - 建议：值越大，块数量与跳转增多，体积与编译/优化时间也会上升

### 虚拟机扁平化（VM Flatten / VMF）

将目标函数改写为由“虚拟机解释器 + 字节码/指令表”驱动的控制流形式。

- 开关
    - `+vm_flatten`（别名：`+vmf`）
        - 功能：启用基于虚拟机的控制流扁平化
        - 默认值：false
