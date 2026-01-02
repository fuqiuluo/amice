# 运行时环境变量

## 字符串加密

源代码：`src/aotu/string_encryption`

| 变量名                                      | 说明                                                                                                                             | 默认值   |
|------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------|-------|
| AMICE_STRING_ENCRYPTION                  | 是否启用字符串加密：<br/>• `true` —— 启用；<br/>• `false` —— 关闭;                                                                            | false |
| AMICE_STRING_ALGORITHM                   | 控制字符串的加密算法：<br/>• `xor` —— 使用异或加密字符串。<br/>• `simd_xor` —— (beta) 使用SIMD指令的异或加密字符串。                                             | xor   |
| AMICE_STRING_DECRYPT_TIMING              | 控制字符串的解密时机：<br/>• `global` —— 程序启动时在全局初始化阶段一次性解密所有受保护字符串；<br/>• `lazy` —— 在每个字符串首次被使用前按需解密（随后可缓存）。 <br/>  备注：解密在栈上的字符串不支持这个配置！ | lazy  |
| AMICE_STRING_STACK_ALLOC                 | (beta) 控制解密字符串的内存分配方式：<br/>• `true` —— 将解密的字符串分配到栈上；<br/>• `false` —— 将解密的字符串分配到堆上。<br/>  备注：栈分配模式下仅支持 `lazy` 解密时机！            | false |
| AMICE_STRING_INLINE_DECRYPT_FN           | 控制是否内联解密函数：<br/>• `true` ——内联解密函数；<br/>• `false` —— 不内联解密函数。                                                                   | false |
| AMICE_STRING_ONLY_DOT_STRING             | 控制是否仅处理 `.str` 段中的字符串：<br/>• `true` ——只加密`.str`字符串；<br/>• `false` —— 可能加密了llvm::Module内的类型为char[]全局变量，导致崩溃。                    | true  |
| AMICE_STRING_ALLOW_NON_ENTRY_STACK_ALLOC | 控制是否允许在栈解密模式下，在非基本块分配栈：<br/>• `true` ——允许，许多LLVM优化pass假设所有 alloca 都在入口块；<br/>• `false` —— 推荐                                   | false |
| AMICE_STRING_NULL_TERMINATED             | 控制字符串是否以 null 结尾（C 风格）：<br/>• `true` —— 解密后在 len-1 位置写入 null 终止符（C 模式）；<br/>• `false` —— 不写入 null 终止符，保留原始字符串内容（Rust 模式）。      | true  |

> **Rust 注意**：Rust 字符串不以 null 结尾，如果使用字符串加密功能，需要设置以下环境变量：
> ```bash
> export AMICE_STRING_ONLY_LLVM_STRING=false  # Rust 字符串全局变量名为 alloc_xxx，而非 .str
> export AMICE_STRING_NULL_TERMINATED=false   # Rust 字符串不需要 null 终止符
> ```

## 间接调用混淆

源代码：`src/aotu/indirect_call`

| 变量名                         | 说明                                                 | 默认值   |
|-----------------------------|----------------------------------------------------|-------|
| AMICE_INDIRECT_CALL         | 是否启用间接跳转：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| AMICE_INDIRECT_CALL_XOR_KEY | 间接跳转下标xor密钥<br/>备注：输入`0`关闭间接跳转下标加密                 | 随机数   |

## 间接跳转混淆

源代码：`src/aotu/indirect_branch`

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

源代码：`src/aotu/split_basic_block`

| 变量名                          | 说明                                                  | 默认值   |
|------------------------------|-----------------------------------------------------|-------|
| AMICE_SPLIT_BASIC_BLOCK      | 是否启用切割基本块：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| AMICE_SPLIT_BASIC_BLOCk_NUMS | 切割基本块次数                                             | 3     |

## `switch`降级

源代码：`src/aotu/lower_switch`

| 变量名                                    | 说明                                             | 默认值   |
|----------------------------------------|------------------------------------------------|-------|
| AMICE_LOWER_SWITCH                     | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| ~~AMICE_LOWER_SWITCH_WITH_DUMMY_CODE~~ | 是否开启降级后插入无效代码（开启可能无法通过模块校验导致`-O1`等编译失败）        | false |

## 扁平化控制流 (VM)

源代码：`src/aotu/vm_flatten`

| 变量名              | 说明                                             | 默认值   |
|------------------|------------------------------------------------|-------|
| AMICE_VM_FLATTEN | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |

## 控制流平坦化

源代码：`src/aotu/flatten`

| 变量名                         | 说明                                                       | 默认值     |
|-----------------------------|----------------------------------------------------------|---------|
| AMICE_FLATTEN               | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭;           | false   |
| AMICE_FLATTEN_MODE          | 混淆模式：<br/>• `basic` —— 基本的；<br/>• `dominator` —— 支配树加强版; | `basic` |
| AMICE_FLATTEN_FIX_STACK     | 是否在混淆后执行`fixStack`修复phi                                  | true    |
| AMICE_FLATTEN_LOWER_SWITCH  | 是否自动降级switch                                             | true    |
| AMICE_FLATTEN_LOOP_COUNT    | 循环次数（最好小于等于7）                                            | 1       |
| AMICE_FLATTEN_ALWAYS_INLINE | 是否把`dominator`模式的更新`key_array`的函数给inline了                | false   |

## MBA算术混淆

源代码：`src/aotu/mba`

| 变量名                                  | 说明                                             | 默认值   |
|--------------------------------------|------------------------------------------------|-------|
| AMICE_MBA                            | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| AMICE_MBA_AUX_COUNT                  | 算术混淆辅助变量个数                                     | `2`   |
| AMICE_MBA_REWRITE_OPS                |                                                | `24`  |
| AMICE_MBA_REWRITE_DEPTH              |                                                | `3`   |
| AMICE_MBA_ALLOC_AUX_PARAMS_IN_GLOBAL | 是否将辅助变量分配到全局变量                                 | false |
| AMICE_MBA_FIX_STACK                  | 混淆后执行`fixStack`                                | false |

## 虚假控制流混淆

源代码：`src/aotu/bogus_control_flow`

| 变量名                            | 说明                                                                                                                                                                                                             | 默认值     |
|--------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------|
| AMICE_BOGUS_CONTROL_FLOW       | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭;                                                                                                                                                                 | false   |
| AMICE_BOGUS_CONTROL_FLOW_MODE  | 混淆模式：<br/>• `basic` —— 默认的，基础版，甚至可能被优化；<br/>• `polaris-primes` —— 从[Polaris-Obfuscator](https://github.com/za233/Polaris-Obfuscator/blob/main/src/llvm/lib/Transforms/Obfuscation/BogusControlFlow.cpp)抄的一个变种； | `basic` | 
| AMICE_BOGUS_CONTROL_FLOW_PROB  | 混淆概率                                                                                                                                                                                                           | `80`    |
| AMICE_BOGUS_CONTROL_FLOW_LOOPS | 循环执行次数                                                                                                                                                                                                         | `1`     |

## 函数包装

源代码：`src/aotu/function_wrapper`

| 变量名                                | 说明                                             | 默认值   |
|------------------------------------|------------------------------------------------|-------|
| AMICE_FUNCTION_WRAPPER             | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| AMICE_FUNCTION_WRAPPER_PROBABILITY | 混淆概率                                           | `80`  |
| AMICE_FUNCTION_WRAPPER_TIMES       | 循环执行次数                                         | `1`   |

## 常参特化克隆混淆

源代码：`src/aotu/clone_function`

| 变量名                  | 说明                                             | 默认值   |
|----------------------|------------------------------------------------|-------|
| AMICE_CLONE_FUNCTION | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |

## 别名访问混淆

源代码：`src/aotu/alias_access`

| 变量名                                | 说明                                                         | 默认值             |
|------------------------------------|------------------------------------------------------------|-----------------|
| AMICE_ALIAS_ACCESS                 | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭;             | false           |
| AMICE_ALIAS_ACCESS_MODE            | 工作模式：<br/>• `pointer_chain` —— 随机链式指针访问；                   | `pointer_chain` |
| AMICE_ALIAS_ACCESS_SHUFFLE_RAW_BOX | 是否打乱局部变量分配顺序：<br/>• `true` —— 启用；<br/>• `false` —— 关闭;     | false           |
| AMICE_ALIAS_ACCESS_LOOSE_RAW_BOX   | 是否在RawBox内插入幻象数据：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false           |

## 自定义调用约定

源代码：`src/aotu/custom_calling_conv`

| 变量名                       | 说明                                             | 默认值  |
|---------------------------|------------------------------------------------|------|
| AMICE_CUSTOM_CALLING_CONV | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | true |

> 默认情况下开启，但是不会修改任何函数的调用约定，
> ```cpp
> #define OBFUSCATE_CC __attribute__((annotate("+custom_calling_conv")))
> 
> OBFUSCATE_CC
> int add(int a, int b) {
>      return a + b;
> }
> ```
> 只有当函数标记了`custom_calling_conv`注解的时候，才会对这个函数执行该混淆！

## GEP偏移量混淆（延迟偏移加载）

源代码：`src/aotu/delay_offset_loading`

| 变量名                                   | 说明                                             | 默认值   |
|---------------------------------------|------------------------------------------------|-------|
| AMICE_DELAY_OFFSET_LOADING            | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
| AMICE_DELAY_OFFSET_LOADING_XOR_OFFSET | 是否xor加密偏移量                                     | true  |

## 参数结构化混淆（PAO）

源代码：`src/aotu/param_aggregate`

| 变量名                   | 说明                                             | 默认值   |
|-----------------------|------------------------------------------------|-------|
| AMICE_PARAM_AGGREGATE | 是否开启：<br/>• `true` —— 启用；<br/>• `false` —— 关闭; | false |
