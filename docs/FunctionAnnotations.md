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

- 配置覆盖优先级
    1. 默认值（代码内置）
    2. 环境变量（全局影响）
    3. 函数注解（逐函数覆盖）

    - 注：当 `alias_access_mode` 取值非法时，会记录错误日志并回退到默认模式。

- 环境变量（全局）
    - `AMICE_ALIAS_ACCESS=true|false`
    - `AMICE_ALIAS_ACCESS_MODE=pointer_chain|basic|v1`
    - `AMICE_ALIAS_ACCESS_SHUFFLE_RAW_BOX=true|false`
    - `AMICE_ALIAS_ACCESS_LOOSE_RAW_BOX=true|false`

- 函数注解示例
    - 启用并指定模式：
        - `+alias_access alias_access_mode=pointer_chain`
    - 启用并配置 PointerChain 参数：
        -
        `+alias_access alias_access_mode=pointer_chain alias_access_shuffle_raw_box=true alias_access_loose_raw_box=true`

- 常见问题
    - 未指定 `alias_access_mode` 时默认使用 `pointer_chain`。
    - `alias_access_shuffle_raw_box` 与 `alias_access_loose_raw_box` 仅在 PointerChain 模式中有意义；在其他模式下会被忽略。
    - 若同时设置了环境变量与函数注解，函数注解优先生效（逐函数覆盖全局）。

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

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局）
    3. 函数注解（按函数覆盖全局）

- 环境变量（全局）
    - `AMICE_BASIC_BLOCK_OUTLINING=true|false`
    - `AMICE_BASIC_BLOCK_OUTLINING_MAX_EXTRACTOR_SIZE=<十进制整数>`

- 函数注解示例
    - 启用并使用默认规模：
        - `+basic_block_outlining`
    - 指定规模上限（等价写法二选一）：
        - `+basic_block_outlining basic_block_outlining_max_extractor_size=24`
        - `+bb2func bb2func_max_extractor_size=24`

- 常见问题
    - 未显式设置规模参数时，规模上限默认取 16；
    - 函数注解中的 `bb2func`/`bb2func_max_extractor_size` 与 `basic_block_outlining`/
      `basic_block_outlin­ing_max_extractor_size` 等价，可任选其一；
    - 非法或超大数值可能导致构建时间增长或可执行体增大，请结合构建日志逐步校准。

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

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局生效）
    3. 函数注解（按函数覆盖全局）

- 环境变量（全局）
    - `AMICE_BOGUS_CONTROL_FLOW=true|false`
    - `AMICE_BOGUS_CONTROL_FLOW_MODE=basic|v1|polaris-primes|primes|v2`
    - `AMICE_BOGUS_CONTROL_FLOW_PROB=0..100`
    - `AMICE_BOGUS_CONTROL_FLOW_LOOPS=1..`

- 函数注解示例
    - 启用并使用默认参数：
        - `+bogus_control_flow`
    - 指定模式与概率：
        - `+bogus_control_flow bogus_control_flow_mode=polaris-primes bogus_control_flow_prob=60`
    - 使用别名键：
        - `+bcf bcf_mode=primes bcf_prob=50 bcf_loops=2`

- 常见问题
    - 概率取值非法或超过范围时，会记录警告并回退到有效区间或默认值；
    - 循环次数小于 1 或非法时会忽略该设置；
    - 若同时设置了环境变量与函数注解，函数注解优先生效（按函数覆盖全局）。

### 常参特化克隆（Clone Function）

对调用点的“常量实参”进行特化：当一个函数以全部或部分常量参数被调用时，可生成该函数的“特化克隆版本”，将这些常量直接内联到函数体中。

- 开关
    - `+clone_function`
        - 功能：启用常参特化克隆混淆
        - 默认值：false

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局）
    3. 函数注解（按函数覆盖全局）

- 环境变量（全局）
    - `AMICE_CLONE_FUNCTION=true|false`

- 函数注解示例
    - 启用：
        - `+clone_function`

- 注意事项
    - 安卓 NDK 下无效：该混淆在 Android NDK 构建环境中不会生效（受工具链与平台限制）。

### 自定义调用约定（Custom Calling Convention / Custom CC）

为目标函数应用自定义调用约定，以改变参数传递与返回值处理的方式。

- 开关
    - `+custom_calling_conv`（别名：`+custom_cc`）
        - 功能：启用自定义调用约定
        - 默认值：true
        - 关闭方式：在相应作用域设置为 false（见下文环境变量与函数注解）

- 配置覆盖优先级
    1. 默认值（内置，启用）
    2. 环境变量（全局生效）
    3. 函数注解（按函数覆盖全局，优先生效）

- 环境变量（全局）
    - `AMICE_CUSTOM_CALLING_CONV=true|false`

- 函数注解示例
    - 显式启用（通常不需要，因为默认已启用）：
        - `+custom_calling_conv`
    - 使用别名键启用：
        - `+custom_cc`
    - 逐函数禁用：`custom_cc=false`或 `-custom_cc`

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

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局生效）
    3. 函数注解（按函数覆盖全局）

- 环境变量（全局）
    - `AMICE_DELAY_OFFSET_LOADING=true|false`
    - `AMICE_DELAY_OFFSET_LOADING_XOR_OFFSET=true|false`

- 函数注解示例
    - 启用 AMA（使用默认 XOR 扰动）：
        - `+delay_offset_loading`
        - 或 `+ama`
    - 关闭 XOR 扰动：
        - `+delay_offset_loading delay_offset_loading_xor_offset=false`
        - 或 `+ama ama_xor_offset=false`
    - 使用别名键：
        - `+ama ama_xor=false`

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

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局）
    3. 函数注解（逐函数，优先生效）

- 环境变量（全局）
    - `AMICE_FLATTEN=true|false`
    - `AMICE_FLATTEN_MODE=basic|v1|dominator|dominator_enhanced|v2`
    - `AMICE_FLATTEN_FIX_STACK=true|false`
    - `AMICE_FLATTEN_LOWER_SWITCH=true|false`
    - `AMICE_FLATTEN_LOOP_COUNT=<十进制整数>`
    - `AMICE_FLATTEN_SKIP_BIG_FUNCTION=true|false`
    - `AMICE_FLATTEN_ALWAYS_INLINE=true|false`

- 函数注解示例
    - 启用（基础模式，使用默认参数）：
        - `+flatten`
    - 指定 dominator 模式并多次扁平化：
        - `+flatten flatten_mode=dominator flatten_loop_count=2`
    - 兼容别名键的写法：
        - `+fla fla_mode=v2 fla_fix_stack=true fla_lower_switch=true`
    - 跳过大函数并总是内联（dominator 模式强化）：
        - `+flatten flatten_mode=dominator flatten_skip_big_function=true flatten_always_inline=true`

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
    - `function_wrapper_times=<n>`（别名：`func_wrapper_times`, `function_wrapper_time`, `func_wrapper_time`）
        - 功能：对同一调用点重复套用包装的次数（可形成多层跳板）
        - 默认值：3
        - 约束：n ≥ 1；小于 1 将按 1 处理

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局）
    3. 函数注解（逐函数，优先生效）

- 环境变量（全局）
    - `AMICE_FUNCTION_WRAPPER=true|false`
    - `AMICE_FUNCTION_WRAPPER_PROBABILITY=0..100`
    - `AMICE_FUNCTION_WRAPPER_TIMES=1..`

- 函数注解示例
    - 启用（默认 70% 概率、3 次包装）：
        - `+function_wrapper`
    - 指定概率与层数（任选别名键）：
        - `+function_wrapper function_wrapper_probability=50 function_wrapper_times=2`
        - `+func_wrapper func_wrapper_prob=100 func_wrapper_time=4`

### 间接跳转（Indirect Branch / IndirectBr / IndBr / IB）

将直接跳转基本块改写。

- 开关
    - `+indirect_branch`（别名：`+ib`, `+indirectbr`, `+indbr`）
        - 功能：启用间接跳转混淆
        - 默认值：true

- 标志位（可组合，使用逗号分隔）
    - 键：`indirect_branch_flags`（别名：`ib_flags`, `indirectbr_flags`, `indbr_flags`）
    - 可选值：
        - `dummy_block`：插入“假”基本块，扰乱控制流分析
        - `chained_dummy_blocks`：串联多个假基本块（包含 dummy_block 的效果）
        - `encrypt_block_index`：加密跳转索引，隐藏真实目标编号
        - `dummy_junk`：在假基本块中插入无意义指令增加噪声
    - 默认值：空（不额外叠加任何标志，仅基础的间接化）
    - 叠加规则：标志按位合并；多来源配置会累计生效

- 配置覆盖与合并策略
    1. 默认值（内置）
    2. 环境变量（全局）：enable 覆盖全局开关；flags 会与已有标志“合并”（累加）
    3. 函数注解（逐函数）：enable 覆盖该函数开关；flags 继续在全局基础上“合并”
    - 说明：由于 flags 采用合并语义，函数级别可在全局基础上再增加特定标志；如需“去除”某标志，请在全局层面控制或避免预先设置它

- 环境变量（全局）
    - `AMICE_INDIRECT_BRANCH=true|false`
    - `AMICE_INDIRECT_BRANCH_FLAGS=dummy_block,encrypt_block_index`（逗号分隔列表）

- 函数注解示例
    - 启用（使用默认标志集）：
        - `+indirect_branch`
    - 指定标志（任选别名键）：
        - `+indirect_branch indirect_branch_flags=dummy_block,encrypt_block_index`
        - `+ib ib_flags=chained_dummy_blocks,dummy_junk`

### 间接调用（Indirect Call / ICall / IndirectCall）

改写直接函数调用。

- 开关
    - `+indirect_call`（别名：`+icall`, `+indirectcall`）
        - 功能：启用间接调用混淆
        - 默认值：true

- 参数
    - `indirect_call_xor_key=<u32>`（别名：`icall_xor_key`, `indirectcall_xor_key`）
        - 功能：对用于间接调用的函数指针应用 XOR 掩码；在使用点还原后再调用
        - 默认行为：未提供时使用随机键（或实现默认策略），以避免固定明文指针
        - 注意：此为轻量级混淆手段，不能视为安全加密

- 配置覆盖优先级
    1. 默认值（内置，开启）
    2. 环境变量（全局）
    3. 函数注解（逐函数覆盖全局，优先生效）

- 环境变量（全局）
    - `AMICE_INDIRECT_CALL=true|false`
    - `AMICE_INDIRECT_CALL_XOR_KEY=<十进制u32>`

- 函数注解示例
    - 显式启用（通常无需，因为默认已启用）：
        - `+indirect_call`
    - 指定 XOR 键（任选别名）：
        - `+indirect_call indirect_call_xor_key=305419896`

### Switch 降级（Lower Switch / Switch→If-Else）

将 switch 语句预先降级为 if-else 链，便于后续混淆变换（如扁平化、伪造控制流）。

- 开关
    - `+lower_switch`（别名：`+lowerswitch`, `+switch_to_if`）
        - 功能：启用 switch→if-else 降级
        - 默认值：false

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局）
    3. 函数注解（逐函数，优先生效）

- 环境变量（全局）
    - `AMICE_LOWER_SWITCH=true|false`

- 函数注解示例
    - 启用降级：
        - `+lower_switch`

### 混合布尔算术（MBA: Mixed Boolean Arithmetic / LinearMBA）

将算术/逻辑运算替换为等价的 MBA 表达式。

- 开关
    - `+mba`（别名：`+linearmba`）
        - 功能：启用 MBA 重写
        - 默认值：false

- 参数
    - `mba_aux_count=<u32>`（别名：`mba_aux`）
        - 功能：MBA 表达式中使用的辅助参数数量
        - 默认值：2
        - 影响：更高的辅助参数可增加自由度和扰动，但也可能放大体积
    - `mba_rewrite_ops=<u32>`（别名：`mba_ops`）
        - 功能：计划用 MBA 表达式改写的操作数量（近似上限）
        - 默认值：24
        - 影响：数值越大，改写覆盖面越广，开销也越大
    - `mba_rewrite_depth=<u32>`（别名：`mba_depth`）
        - 功能：MBA 表达式嵌套的最大深度
        - 默认值：3
        - 影响：更深的表达式树更难化简，但会显著增加 IR 复杂度
    - `mba_alloc_aux_params_in_global=<true|false>`（别名：`mba_alloc_global`）
        - 功能：将辅助参数分配为全局变量参与表达式，以抵抗编译器（如 LLVM）的常量折叠/优化
        - 默认值：false
        - 影响：更抗优化，但会引入全局状态与潜在可见性，需要斟酌
    - `mba_fix_stack=<true|false>`（别名：`mba_stack_fix`）
        - 功能：在混淆中进行栈修复，避免异常或崩溃
        - 默认值：false
    - `mba_opt_none=<true|false>`（别名：`mba_no_opt`）
        - 功能：对相关函数应用“禁止优化”属性，防止 MBA 被过度优化消解
        - 默认值：false
        - 提示：启用后会抑制编译器优化，可能影响性能与体积

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局）
    3. 函数注解（逐函数覆盖全局，优先生效）

- 环境变量（全局）
    - `AMICE_MBA=true|false`
    - `AMICE_MBA_AUX_COUNT=<u32>`
    - `AMICE_MBA_REWRITE_OPS=<u32>`
    - `AMICE_MBA_REWRITE_DEPTH=<u32>`
    - `AMICE_MBA_ALLOC_AUX_PARAMS_IN_GLOBAL=true|false`
    - `AMICE_MBA_FIX_STACK=true|false`
    - `AMICE_MBA_OPT_NONE=true|false`

- 函数注解示例
    - 启用（使用默认参数）：
        - `+mba`
    - 指定参数（任选别名键）：
        - `+mba mba_aux_count=3 mba_rewrite_ops=32 mba_rewrite_depth=4`
        - `+linearmba mba_alloc_global=true mba_stack_fix=true mba_no_opt=true`

### 参数结构化访问（Param Aggregate）

将函数的多个形参聚合为单一结构体进行传递。

- 开关
    - `+param_aggregate`
        - 功能：启用参数结构化访问
        - 默认值：false

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局生效）
    3. 函数注解（按函数覆盖全局，优先生效）

- 环境变量（全局）
    - `AMICE_PARAM_AGGREGATE=true|false`

- 函数注解示例
    - 启用：
        - `+param_aggregate`

### 基本块乱序（Shuffle Blocks）

对函数内的基本块顺序进行扰动。

- 开关
    - `+shuffle_blocks`
        - 功能：启用基本块乱序
        - 默认值：false

- 标志位（可组合，逗号分隔）
    - 键：`shuffle_blocks_flags`
    - 可选值与含义：
        - `reverse`（别名：`flip`）：反转基本块顺序
        - `random`（别名：`shuffle`）：随机打乱基本块顺序
        - `rotate`（别名：`rotate_left`）：旋转基本块（左移一位）
    - 默认值：空（不叠加任何策略）
    - 合并规则：标志按位合并；来自环境变量与函数注解的标志会在现有基础上累加生效

- 配置覆盖与合并顺序
    1. 默认值（内置）
    2. 环境变量（全局）：enable 覆盖开关；flags 与现有标志合并
    3. 函数注解（逐函数）：enable 覆盖该函数开关；flags 继续在基础上合并

- 环境变量（全局）
    - `AMICE_SHUFFLE_BLOCKS=true|false`
    - `AMICE_SHUFFLE_BLOCKS_FLAGS=reverse,random`（逗号分隔列表）

- 函数注解示例
    - 启用（默认不加标志）：
        - `+shuffle_blocks`
    - 指定标志（字符串形式，支持别名）：
        - `+shuffle_blocks shuffle_blocks_flags=reverse,random`

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

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局）
    3. 函数注解（逐函数，优先生效）

- 环境变量（全局）
    - `AMICE_SPLIT_BASIC_BLOCK=true|false`
    - `AMICE_SPLIT_BASIC_BLOCK_NUM=<u32>`

- 函数注解示例
    - 启用（使用默认 3 次拆分）：
        - `+split_basic_block`
    - 指定拆分次数：
        - `+split_basic_block split_basic_block_num=5`

### 字符串加密（String Encryption / StrEnc / GVEnc）

对字符串常量进行加密存储，并在运行时按策略解密。

- 开关
    - `+string_encryption`（别名：`+strenc`, `+gvenc`）
        - 功能：启用字符串加密混淆
        - 默认值：false

- 算法
    - `string_algorithm=<algo>`（别名：`strenc_algorithm`, `gvenc_algorithm`）
        - 可选值：
            - `xor`
            - `simd_xor`（别名：`xorsimd`, `xor_simd`, `simdxor`）
        - 默认值：`xor`
        - 说明：SIMD 方案在支持的架构上可带来更高扰动与吞吐，代价是更复杂的指令序列与体积开销

- 解密时机
    - `string_decrypt_timing=<timing>`（别名：`strenc_decrypt_timing`, `gvenc_decrypt_timing`）
        - 可选值：`lazy` | `global`
        - 默认值：`lazy`
        - 含义：
            - lazy：按需解密（接近使用点），隐藏解密聚集点，提升逆向难度
            - global：集中式预解密，便于管理，但更易被定位

- 参数
    - `string_stack_alloc=<true|false>`（别名：`strenc_stack_alloc`, `gvenc_stack_alloc`）
        - 功能：启用基于栈的解密数据承载，减少长生命周期痕迹
          默认值：false
    - `string_inline_decrypt_fn=<true|false>`（别名：`strenc_inline_decrypt_fn`, `gvenc_inline_decrypt_fn`）
        - 功能：将解密函数标记为可内联，打散固定的调用点特征
          默认值：false
    - `string_only_dot_str=<true|false>`（别名：`strenc_only_dot_str`, `gvenc_only_dot_str`）
        - 功能：仅处理来自“.str”或等价字符串区的字符串
          默认值：true
    - `string_allow_non_entry_stack_alloc=<true|false>`（别名：`strenc_allow_non_entry_stack_alloc`, `gvenc_allow_non_entry_stack_alloc`）
        - 功能：允许在非入口基本块也进行栈上分配承载解密数据
          默认值：false（仅限入口块，利于优化与结构稳定）

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局）
    3. 函数注解（逐函数，优先生效）

- 环境变量（全局）
    - `AMICE_STRING_ENCRYPTION=true|false`
    - `AMICE_STRING_ALGORITHM=xor|simd_xor|xorsimd|xor_simd|simdxor`
    - `AMICE_STRING_DECRYPT_TIMING=lazy|global`
    - `AMICE_STRING_STACK_ALLOC=true|false`
    - `AMICE_STRING_INLINE_DECRYPT_FN=true|false`
    - 选择来源限制（只处理字符串区）：
        - `AMICE_STRING_ONLY_LLVM_STRING=true|false`
        - `AMICE_STRING_ONLY_DOT_STRING=true|false`
    - 允许非入口块栈分配：
        - `AMICE_STRING_ALLOW_NON_ENTRY_STACK_ALLOC=true|false`

- 函数注解示例
    - 启用（默认 XOR、lazy 时机，仅 .str）：
        - `+string_encryption`

### 虚拟机扁平化（VM Flatten / VMF）

将目标函数改写为由“虚拟机解释器 + 字节码/指令表”驱动的控制流形式。

- 开关
    - `+vm_flatten`（别名：`+vmf`）
        - 功能：启用基于虚拟机的控制流扁平化
        - 默认值：false

- 配置覆盖优先级
    1. 默认值（内置）
    2. 环境变量（全局）
    3. 函数注解（逐函数覆盖全局，优先生效）

- 环境变量（全局）
    - `AMICE_VM_FLATTEN=true|false`

- 函数注解示例
    - 启用：
        - `+vm_flatten`
        - 或 `+vmf`
