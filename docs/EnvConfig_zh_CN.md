# 运行时环境变量

[English](EnvConfig_en_US.md) | 简体中文

环境变量由加载插件的编译器进程读取。Bash 使用 `export AMICE_STRING_ENCRYPTION=true` 或单条命令前缀，PowerShell 使用 `$env:AMICE_STRING_ENCRYPTION = 'true'`。

全局优先级为 **环境变量 > 配置文件 > 默认值**；支持函数注解的 Pass 会再应用 [函数注解](FunctionAnnotations_zh_CN.md)。环境布尔值接受 `true/false`、`1/0`、`on/off`（不区分大小写）；不要把注解支持的 `yes` 当作环境变量的 true。

## 支持矩阵

| Pass | 环境变量开关 | C/C++ | Rust | ObjC | 说明 |
|:---|:---|:---:|:---:|:---:|:---|
| String Encryption | `AMICE_STRING_ENCRYPTION` | ✅ | ✅ | ⏳ | 字符串加密，支持 `xor` / `simd_xor`、lazy/global 解密、栈/堆解密配置 |
| Indirect Call | `AMICE_INDIRECT_CALL` | ✅ | ✅ | ❌ | 将直接调用改写为函数表/索引形式的间接调用 |
| Indirect Branch | `AMICE_INDIRECT_BRANCH` | ✅ | ✅ | ❌ | 将分支改写为 `indirectbr`，支持 dummy block、表重排、索引加密等 flags |
| Split Basic Block | `AMICE_SPLIT_BASIC_BLOCK` | ✅ | ✅ | ❌ | 按配置切割基本块 |
| Lower Switch | `AMICE_LOWER_SWITCH` | ✅ | ✅ | ❌ | 降级 LLVM `switch` 指令 |
| VM Flatten | `AMICE_VM_FLATTEN` | ✅ | ✅ | ❌ | VM控制流平坦化 |
| VM Virtualize | `AMICE_VM_VIRTUALIZE` | ✅ | ✅ | ❌ | 指令级 VMP 虚拟化，支持全局开启或通过函数注解按函数启用 |
| Flatten | `AMICE_FLATTEN` | ✅ | ✅ | ❌ | 控制流平坦化，支持 `basic` / `dominator` 模式 |
| MBA | `AMICE_MBA` | ✅ | ✅ | ❌ | 混合布尔算术表达式重写 |
| Bogus Control Flow | `AMICE_BOGUS_CONTROL_FLOW` | ✅ | ✅ | ❌ | 混淆程序控制流，增加逆向分析难度 |
| Function Wrapper | `AMICE_FUNCTION_WRAPPER` | ✅ | ✅ | ❌ | 生成包装函数并替换调用点 |
| Clone Function | `AMICE_CLONE_FUNCTION` | ✅ | ✅ | ❌ | 常量参数特化克隆 |
| Alias Access | `AMICE_ALIAS_ACCESS` | ✅ | ✅ | ❌ | 基于指针链的别名访问混淆 |
| Custom Calling Conv | `AMICE_CUSTOM_CALLING_CONV` | ⏳ | ⏳ | ❌ | 预留实现，目前不执行调用约定变换 |
| Delay Offset Loading | `AMICE_DELAY_OFFSET_LOADING` | ✅ | ⏳ | ❌ | GEP 偏移延迟加载/可选 XOR 保护 |
| Param Aggregate | `AMICE_PARAM_AGGREGATE` | ✅ | ⏳ | ❌ | 参数结构化聚合混淆 |
| Basic Block Outlining | `AMICE_BASIC_BLOCK_OUTLINING` | ✅ | ⏳ | ❌ | 将基础块提取为独立子函数，亦称 BB2Func |
| Shuffle Blocks | `AMICE_SHUFFLE_BLOCKS` | ✅ | ⏳ | ❌ | 基本块重排 |

> 图例：✅ 已支持；⏳ 进行中 / 计划中 / 未测试；❌ 暂未规划。

各 Pass 的完整参数见下文分节；按函数启用/禁用见 [函数注解](FunctionAnnotations_zh_CN.md)。

## 配置文件、顺序和日志

| 变量 | 用法 | 默认 |
| --- | --- | --- |
| `AMICE_CONFIG_PATH` | TOML/YAML/JSON 文件路径，推荐绝对路径；完整示例见 [Pass 运行顺序](PassOrder_zh_CN.md) | 不读取文件 |
| `AMICE_PASS_ORDER` | 逗号或分号分隔的 Pass 名称；只安装列表中的 Pass，不会自动开启它们 | 按优先级排序 |
| `AMICE_PASS_PRIORITY_OVERRIDE` | 例如 `IndirectCall=1100,StringEncryption=1000`；有显式顺序时不生效 | 不覆盖 |
| `RUST_LOG` | `amice=info` 查看加载/执行日志；`amice=debug` 查看更细的诊断 | 未显式配置 |

配置读取失败目前会回退默认值；设置了路径不代表文件已成功应用。用下面的开关验证目标产物，排查步骤见 [故障排除](Troubleshooting_zh_CN.md)。

## 字符串加密

源代码：`crates/amice/src/aotu/string_encryption`

| 变量名                                      | 说明                                                                                                                             | 默认值   |
|------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------|-------|
| AMICE_STRING_ENCRYPTION                  | 是否启用字符串加密：<br/>• `true` —— 启用；<br/>• `false` —— 关闭;                                                                            | false |
| AMICE_STRING_ALGORITHM                   | 控制字符串的加密算法：<br/>• `xor` —— 使用异或加密字符串。<br/>• `simd_xor` —— (beta) 使用SIMD指令的异或加密字符串。                                             | xor   |
| AMICE_STRING_DECRYPT_TIMING              | 控制字符串的解密时机：<br/>• `global` —— 程序启动时在全局初始化阶段一次性解密所有受保护字符串；<br/>• `lazy` —— 在每个字符串首次被使用前按需解密（随后可缓存）。 <br/>  备注：解密在栈上的字符串不支持这个配置！ | lazy  |
| AMICE_STRING_STACK_ALLOC                 | (beta) 控制解密字符串的内存分配方式：<br/>• `true` —— 将解密的字符串分配到栈上；<br/>• `false` —— 将解密的字符串分配到堆上。<br/>  备注：栈分配模式下仅支持 `lazy` 解密时机！            | false |
| AMICE_STRING_INLINE_DECRYPT_FN           | 控制是否内联解密函数：<br/>• `true` ——内联解密函数；<br/>• `false` —— 不内联解密函数。                                                                   | false |
| AMICE_STRING_ONLY_DOT_STRING             | 控制是否仅处理 `.str` 段中的字符串：<br/>• `true` ——只加密`.str`字符串；<br/>• `false` —— 可能加密了llvm::Module内的类型为char[]全局变量，导致崩溃。                    | true  |
| AMICE_STRING_ALLOW_NON_ENTRY_STACK_ALLOC | 控制是否允许在栈解密模式下，在非基本块分配栈：<br/>• `true` ——允许，许多LLVM优化pass假设所有 alloca 都在入口块；<br/>• `false` —— 推荐                                   | false |
| AMICE_STRING_MAX_ENCRYPTION_COUNT | 非 global 解密模式的加密次数，限制到 1–100000                                                                              | 1     |

> **Rust 注意**
> ```bash
> export AMICE_STRING_ONLY_DOT_STRING=false  # Rust 字符串全局变量名为 alloc_xxx，而非 .str
> ```

> `AMICE_STRING_ONLY_LLVM_STRING` 是 `AMICE_STRING_ONLY_DOT_STRING` 的兼容旧名；两者都设置时以后者为准。

## 间接调用混淆

源代码：`crates/amice/src/aotu/indirect_call`

| 变量名                         | 说明                                                 | 默认值   |
|-----------------------------|----------------------------------------------------|-------|
| AMICE_INDIRECT_CALL         | 是否启用间接调用：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| AMICE_INDIRECT_CALL_XOR_KEY | 间接跳转下标xor密钥<br/>备注：输入`0`关闭间接跳转下标加密                 | 随机数   |

## 间接跳转混淆

源代码：`crates/amice/src/aotu/indirect_branch`

| 变量名                         | 说明                                                               | 默认值                |
|-----------------------------|------------------------------------------------------------------|--------------------|
| AMICE_INDIRECT_BRANCH       | 是否启用间接指令：<br/>• `true` —— 启用；<br/>• `false` —— 关闭;               | false              |
| AMICE_INDIRECT_BRANCH_FLAGS | 间接指令的额外混淆扩展功能，以逗号分隔的字符串形式指定。[可选扩展](#amice_indirect_branch_flags) | `""`（空字符串，表示无额外扩展） |

### AMICE_INDIRECT_BRANCH_FLAGS

- `dummy_block` —— 在无条件跳转（`br label`）转换为`indirectbr`时，插入一个或多个虚假的基本块（dummy block），执行 1~3
  条无意义的计算指令（如空加法、位运算等），再跳转至真实目标块；
- `chained_dummy_blocks` —— 增强 `dummy_block`，支持插入多个连续的虚假块，形成跳转链，显著增加控制流复杂度;
- `encrypt_block_index` —— 加密基本块在跳转表的下标;
- ~~`dummy_junk` —— 虚假块里面塞干扰性指令;~~
- `shuffle_table` —— 打乱跳转表顺序，随机化基本块在表中的排列（默认关闭）;

## 切割基本块

源代码：`crates/amice/src/aotu/split_basic_block`

| 变量名                          | 说明                                                  | 默认值   |
|------------------------------|-----------------------------------------------------|-------|
| AMICE_SPLIT_BASIC_BLOCK      | 是否启用切割基本块：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| AMICE_SPLIT_BASIC_BLOCK_NUM | 切割基本块次数                                             | 3     |

## `switch`降级

源代码：`crates/amice/src/aotu/lower_switch`

| 变量名                                    | 说明                                             | 默认值   |
|----------------------------------------|------------------------------------------------|-------|
| AMICE_LOWER_SWITCH                     | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| ~~AMICE_LOWER_SWITCH_WITH_DUMMY_CODE~~ | 是否开启降级后插入无效代码（开启可能无法通过模块校验导致`-O1`等编译失败）        | false |

## VM控制流平坦化

源代码：`crates/amice/src/aotu/vm_flatten`

| 变量名              | 说明                                             | 默认值   |
|------------------|------------------------------------------------|-------|
| AMICE_VM_FLATTEN | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |

## 指令级 VMP 虚拟化

源代码：`crates/amice/src/aotu/vm_virtualize`

| 变量名                    | 说明                                                                                                         | 默认值                       |
|------------------------|------------------------------------------------------------------------------------------------------------|---------------------------|
| AMICE_VM_VIRTUALIZE    | 是否启用指令级 VMP 虚拟化：<br/>• `true` —— 启用；<br/>• `false` —— 关闭;                                                | false                     |
| AMICE_VM_PROFILE_PATH  | VM profile package 路径；为空时使用内置 `amice-simple-vmp` profile package                                           | 内置 `amice-simple-vmp`      |
| AMICE_VM_RUNTIME_SCOPE | 覆盖 profile 中的 runtime scope：<br/>• `func` —— 每个函数独立 runtime；<br/>• `module` —— 模块共享 runtime                  | profile 声明的 runtime scope |
| AMICE_VM_EMIT_MARKERS  | 生成 `AMICEVMP` bytecode magic、`AMICE_VMP_RUNTIME_BYTECODE`、`.amice.vm.meta.*` 和可读 bytecode 符号等稳定 VMP 测试/调试 marker；生产环境保持关闭。 | false                     |
| AMICE_VM_DUMP_BYTECODE | 通过 debug 日志输出编码后的 VM bytecode                                                                          | false                     |
| AMICE_VM_DUMP_LOWERING | 通过 debug 日志输出 LLVM IR 到 VM IR 的 lowering 结果                                                            | false                     |

## 控制流平坦化

源代码：`crates/amice/src/aotu/flatten`

| 变量名                         | 说明                                                       | 默认值     |
|-----------------------------|----------------------------------------------------------|---------|
| AMICE_FLATTEN               | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭;           | false   |
| AMICE_FLATTEN_MODE          | 混淆模式：<br/>• `basic` —— 基本的；<br/>• `dominator` —— 支配树加强版; | `basic` |
| AMICE_FLATTEN_FIX_STACK     | 是否在混淆后执行`fixStack`修复phi                                  | true    |
| AMICE_FLATTEN_LOWER_SWITCH  | 是否自动降级switch                                             | true    |
| AMICE_FLATTEN_LOOP_COUNT    | 循环次数（最好小于等于7）                                            | 1       |
| AMICE_FLATTEN_ALWAYS_INLINE | 是否把`dominator`模式的更新`key_array`的函数给inline了                | false   |
| `AMICE_FLATTEN_SKIP_BIG_FUNCTION` | 跳过过大的函数 | false |

## MBA算术混淆

源代码：`crates/amice/src/aotu/mba`

| 变量名                                  | 说明                                             | 默认值   |
|--------------------------------------|------------------------------------------------|-------|
| AMICE_MBA                            | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| AMICE_MBA_FLOAT_REGIONS              | 在受支持的硬件浮点目标上启用精确 binary64 SSA 区域             | true  |
| AMICE_MBA_PRE_EXPAND                 | 将整数等价 DAG 直接展开到浮点区域内                          | true  |
| AMICE_MBA_OPAQUE_GUARD               | 每次调用 volatile 读取私有全局种子，生成有界浮点载体           | true  |
| AMICE_MBA_MAX_INSTRUCTIONS           | 每函数最多改写的原始指令数，上限 512；0 表示跳过                | `128` |
| AMICE_MBA_MAX_ADDED_INSTRUCTIONS     | 每函数生成指令数的保守预算                                  | `2048` |

MBA 现在沿算术、位运算、select 和 phi 改变 SSA 值的表示。旧的辅助变量、重写深度、
fixStack 和 optnone 配置已移除。支持范围、语义约束和测试方法见 [MBA 区域设计](MbaRegions.md)。

## 虚假控制流混淆

源代码：`crates/amice/src/aotu/bogus_control_flow`。

BCF 混淆程序控制流，增加逆向分析难度，在 OptimizerLast / FullLtoLast 执行。默认启用完整原始函数的克隆与强制内联补充；可单独关闭此补充，继续使用整数区域变换。整数区域支持 i8/i16/i32/i64 的 add/sub/and/or/xor，跳过已有循环中的区域和异常处理函数。

| 变量名 | 说明 | 默认值 |
|---|---|---|
| AMICE_BOGUS_CONTROL_FLOW | 启用 BCF | `false` |
| AMICE_BOGUS_CONTROL_FLOW_PROB | 整函数克隆及候选区域的选择概率，0–100，超过 100 按 100 处理 | `80` |
| AMICE_BOGUS_CONTROL_FLOW_CLONE | 启用完整原始函数克隆与强制内联；关闭后保留整数区域变换 | `true` |
| AMICE_BOGUS_CONTROL_FLOW_MAX_REGIONS | 每函数最多转换的区域数；0 禁用，硬上限 16 | `2` |
| AMICE_BOGUS_CONTROL_FLOW_MAX_REGION_INSTRUCTIONS | 每区域原始指令上限；小于 2 不转换，超过 16 按 16 处理 | `8` |
| AMICE_BOGUS_CONTROL_FLOW_SEED | u64 十进制种子；未指定时随机生成，显式指定时可复现 | 随机数 |

区域预算不限制整函数克隆的大小；`MAX_REGIONS=0` 或 `MAX_REGION_INSTRUCTIONS<2` 会禁用 BCF。变参、直接递归、异常处理、基本块地址逃逸及其他不满足内联条件的函数跳过克隆补充；内联失败时仍按整数区域规则处理。指针参数保持透传。整数参数仅在不破坏运算前提时扰动。后续优化可能合并等价分支，需检查最终产物中的保留情况。

配置文件中的补充开关为 `bogus_control_flow.clone`。启用 BCF 会增加运行时间和代码体积，完整克隆的成本随函数大小增长，建议从少量目标函数开始。不要用于要求恒定执行时间的代码。逐函数配置见 [函数注解](FunctionAnnotations_zh_CN.md)。

含 `llvm.localescape`、`llvm.gcroot` 或入口收敛令牌的函数保持原样，以保留 LLVM 对入口位置的要求。函数签名含当前绑定不支持的类型（如 Target Extension 类型），或调用具有 `noduplicate`、`convergent`、`returns_twice` 限制的函数（包括别名调用）时，跳过克隆补充；满足条件的整数区域仍可处理。含内联汇编的函数也跳过克隆补充，避免复制汇编标签和状态。

`ExactMatch` / `SameSize` COMDAT 中的函数保持原样，避免不同编译单元的随机变换破坏链接器对副本内容或大小一致的要求。

## 函数包装

源代码：`crates/amice/src/aotu/function_wrapper`

| 变量名                                | 说明                                             | 默认值   |
|------------------------------------|------------------------------------------------|-------|
| AMICE_FUNCTION_WRAPPER             | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| AMICE_FUNCTION_WRAPPER_PROBABILITY | 混淆概率                                           | `70`  |
| AMICE_FUNCTION_WRAPPER_TIMES       | 循环执行次数                                         | `3`   |

## 常参特化克隆混淆

源代码：`crates/amice/src/aotu/clone_function`

| 变量名                  | 说明                                             | 默认值   |
|----------------------|------------------------------------------------|-------|
| AMICE_CLONE_FUNCTION | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |

## 别名访问混淆

源代码：`crates/amice/src/aotu/alias_access`

| 变量名                                | 说明                                                         | 默认值             |
|------------------------------------|------------------------------------------------------------|-----------------|
| AMICE_ALIAS_ACCESS                 | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭;             | false           |
| AMICE_ALIAS_ACCESS_MODE            | 工作模式：<br/>• `pointer_chain` —— 随机链式指针访问；                   | `pointer_chain` |
| AMICE_ALIAS_ACCESS_SHUFFLE_RAW_BOX | 是否打乱局部变量分配顺序：<br/>• `true` —— 启用；<br/>• `false` —— 关闭;     | false           |
| AMICE_ALIAS_ACCESS_LOOSE_RAW_BOX   | 是否在RawBox内插入幻象数据：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false           |

## 自定义调用约定

源代码：`crates/amice/src/aotu/custom_calling_conv`

| 变量名                       | 说明                                             | 默认值  |
|---------------------------|------------------------------------------------|------|
| AMICE_CUSTOM_CALLING_CONV | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | true |

> 配置默认值为 true，但当前调用约定实现仍是占位代码；即使添加注解也不会产生调用约定变换。

## GEP偏移量混淆（延迟偏移加载）

源代码：`crates/amice/src/aotu/delay_offset_loading`

| 变量名                                   | 说明                                             | 默认值   |
|---------------------------------------|------------------------------------------------|-------|
| AMICE_DELAY_OFFSET_LOADING            | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| AMICE_DELAY_OFFSET_LOADING_XOR_OFFSET | 是否xor加密偏移量                                     | true  |

## 参数结构化混淆（PAO）

源代码：`crates/amice/src/aotu/param_aggregate`

| 变量名                   | 说明                                             | 默认值   |
|-----------------------|------------------------------------------------|-------|
| AMICE_PARAM_AGGREGATE | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |

## 函数分片（Basic Block Outlining）

源代码：`crates/amice/src/aotu/basic_block_outlining`

| 变量名 | 说明 | 默认值 |
| --- | --- | --- |
| `AMICE_BASIC_BLOCK_OUTLINING` | 开启基本块外提 | false |
| `AMICE_BASIC_BLOCK_OUTLINING_MAX_EXTRACTOR_SIZE` | 提取器规模参数，必须是非负整数；非法值会导致配置解析失败 | 16 |

## 基本块重排（Shuffle Blocks）

源代码：`crates/amice/src/aotu/shuffle_blocks`

| 变量名 | 说明 | 默认值 |
| --- | --- | --- |
| `AMICE_SHUFFLE_BLOCKS` | 开启基本块重排 | false |
| `AMICE_SHUFFLE_BLOCKS_FLAGS` | 逗号分隔的 `random`、`reverse`、`rotate`；至少选一个才能产生重排，环境 flags 与文件 flags 合并 | 空 |
