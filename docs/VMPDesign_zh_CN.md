# VMP 虚拟化实现规范

本文定义 AMICE VMP 的实现与架构。该 pass 的职责是把目标函数的 LLVM IR 翻译成 profile 指定的 VM bytecode，并在当前 LLVM `Module` 中生成能够解释执行这些 bytecode 的 VM runtime。

## 目标

- VM 的 ISA、ABI、字节码格式、解码器流水线和 lowering 规则由外部 profile 描述。
- VM 执行模型固定为寄存器虚拟机：LLVM SSA value lowering 后进入虚拟寄存器，handler 通过 register file 读写状态。
- AMICE 插件只内置 profile 解析、校验、LLVM IR 翻译框架、bytecode encoder 和 runtime emitter。
- runtime 不能是旧 `vm_flatten` 那类写死解释器；它必须从 profile 解析出的 ISA semantic、ABI、decoder pipeline 和 bytecode layout 生成 LLVM IR。AMICE 允许的 handler semantic 是受限 typed DSL 模板，超出模板的 profile 必须在 verifier 阶段被拒绝。
- 默认使用模块级 runtime 和 descriptor table：同一个 LLVM `Module` 内共享 runtime、紧凑 bytecode blob、native thunk table 和加密 descriptor table；wrapper 只传 opaque `fn_token`、`ret_slots` 和 `arg_slots`。
- profile 必须可验证。插件要能在编译期检查 handler 读写、operand 类型、decoder 可逆性、ABI 映射、LLVM lowering 覆盖范围。

## VM Profile

profile 必须是一个 package，而不是单个配置文件。

```text
my-vm-profile/        # 一个完整 VM profile package
  manifest.toml      # profile 元信息和各 DSL 文件入口
  abi.vm             # Host ABI 与 VM ABI 映射
  isa.vm             # VM 指令集和 handler 语义
  lowering.vm        # LLVM IR 到 VM 指令的翻译规则
  bytecode.vm        # 字节码段、record layout 和 reloc 规则
  decoder.vm         # 字节码解码器流水线
  runtime.vm         # runtime scope、dispatch、register file 和 state layout
```

`manifest.toml` 只负责声明 profile 元数据和入口文件：

```toml
version = 1                         # profile 格式版本，用于兼容性检查
name = "amice-simple-vmp"           # profile 名称，会出现在诊断和 dump 中
[target]                            # 目标平台约束
pointer_bits = 64                   # 目标指针宽度，必须匹配当前 LLVM Module
endian = "little"                   # 目标字节序，影响 bytecode 和 memory lowering
[profile]                           # 各 profile DSL 文件入口
abi = "abi.vm"                      # ABI 规则文件
isa = "isa.vm"                      # 指令集规则文件
lowering = "lowering.vm"            # LLVM IR lowering 规则文件
bytecode = "bytecode.vm"            # 字节码布局规则文件
decoder = "decoder.vm"              # 解码器流水线规则文件
runtime = "runtime.vm"              # runtime 生成规则文件
```

## Profile 示例注释约定

项目中的 profile 示例必须每行都有注释。这个约定适用于 `manifest.toml`、`*.vm` DSL 示例文件，以及文档中展示的 profile 片段。

DSL 注释统一使用 `#`。解析器必须支持整行注释和行尾注释：

- 整行注释用于解释一个小节的意图。
- 行尾注释用于解释当前语句的输入、输出、约束或副作用。
- 示例 profile 不允许出现无注释的有效语句行。
- 空行不承载语义；为了避免歧义，示例 profile 中应尽量不用空行，确实需要分隔时用注释行代替。

## 寄存器模型

AMICE VMP 的 VM 模型固定为寄存器虚拟机，不设计为 operand stack 驱动的栈虚拟机。VM 的物理寄存器模型固定为 `x0` 到 `x31` 和 `q0` 到 `q64`。Profile 只能定义寄存器别名、调用约定和 ABI 映射，不能改变基础寄存器组，也不能把执行模型切换成 stack VM。

这带来几个硬约束：

- VM state 必须包含 `x` 和 `q` 两组 register file，handler 通过显式寄存器名读写 VM value。
- 字节码 operand 应编码 `vreg`、`imm`、`label`、`const_pool_index` 等显式操作数，不依赖隐式 push/pop 栈。
- LLVM SSA value lowering 后必须绑定到虚拟寄存器；`bind %llvm_value = %vreg` 是主要数据流记录方式。
- 常量以内联 immediate 或 const pool entry materialize，进入运算前必须表现为 VM operand 或 VM register。
- 内存访问通过寄存器中的 pointer/address value 加显式 `load` / `store` handler 表达。
- 函数参数和返回值通过 ABI 指定的 VM register marshal/unmarshal；多返回值也是 `ret0..retN` 到 register file 的映射。
- verifier 需要检查每条 handler 的 register read/write set，避免隐式状态修改。

`runtime.vm` 负责声明寄存器组、控制状态和寄存器别名。`lr` 这类名字不能散落在 lowering rule 中，必须在 `runtime.vm` 里定义为固定 register alias，然后由 `abi.vm` 的调用约定引用。

```text
registers {                            # 定义 VM register file 和寄存器别名
  bank x range x0..x31 type u64         # x0 到 x31 是 64 位通用/指针寄存器
  bank q range q0..q64 type v128        # q0 到 q64 是 128 位宽寄存器
  q.lowering = disabled                 # 若实现尚未支持宽值 lowering，profile 必须显式声明禁用
  alias lr = x30                        # lr 是 link register，默认绑定到 x30
  alias sp = x31                        # sp 是 VM stack/base 指针别名，默认绑定到 x31
}                                       # 结束寄存器模型定义
# control_state 保存不属于通用数据寄存器的解释器控制字段
control_state {                         # 定义 VM runtime 控制状态
  pc: label                             # pc 保存当前 bytecode instruction 位置
  flags: u64                            # flags 保存比较、溢出等 handler 可见状态
}                                       # 结束控制状态定义
```

如果某个 profile 不需要 `sp`，允许不在 ABI 中使用它；但 `lr` 如果参与 VM 内部 call/ret，必须在 `runtime.vm` 中有明确 alias。Verifier 必须拒绝未定义 `lr` 却声明 VM call/ret 规则的 profile。

当前实现如果尚未提供 `q0..q64` 的宽值 lowering，profile 必须用 `q.lowering = disabled` 显式声明该限制；verifier 必须在这种模式下拒绝任何依赖 `q` 寄存器的 ABI、lowering rule 或 handler 语义，避免把宽寄存器组伪装成完整可用能力。

translator 的 `x` 寄存器分配不能退化成“1 个 LLVM SSA 永久占 1 个 VM 寄存器”。函数参数、返回槽、native-call ABI 槽、phi 结果以及跨 basic block 使用的值会被保守固定；只在同一 basic block 内使用的标量 SSA 临时值必须在最后一次使用后释放，后续 lowering 可以复用该 `x` 寄存器。直接 `struct`、固定数组和 fixed vector 的 `x`-slot 聚合绑定按叶子字段或 lane 应用同一规则。若某个函数的同时活跃固定值、ABI 槽和当前指令临时 scratch 合计仍超过 `x0..x31`，pass 必须安全跳过该函数并输出 debug 日志。

## 总体架构

新的 pass 命名为 `vm_virtualize`。

```text
LLVM Module
  -> profile loader
  -> profile verifier
  -> function selector
  -> LLVM IR normalizer
  -> LLVM IR -> VM IR translator
  -> VM IR -> bytecode encoder
  -> runtime emitter
  -> function wrapper rewriter
```

实现拆分如下，profile 和 VM 编译逻辑由独立 crate 承载：

```text
crates/amice-vm/
  src/profile.rs    # profile package 解析、版本检查、入口文件解析
  src/abi.rs        # VM ABI、host ABI、参数/返回值映射
  src/isa.rs        # VM 指令、operand、handler semantic typed AST
  src/lowering.rs   # VM IR、label 和 native-call 返回槽
  src/bytecode.rs   # layout、reloc、encoder、decoder inverse
  src/runtime.rs    # runtime profile 和增强开关
  src/verify.rs     # profile verifier 和 lowering coverage checker

crates/amice/src/aotu/vm_virtualize/
  mod.rs            # pass 入口，只负责调度
```

## Runtime Scope

AMICE pass 一次处理的是一个 LLVM `Module`。在同一个 `Module` 中创建的 `private`/`internal` function 和 global 允许被同一个模块内的多个函数共享，但不会自动跨编译单元共享。

默认策略应为：

```text
runtime.scope = module       # 当前 LLVM Module 内共享一套 runtime
runtime.entry = call         # 默认 wrapper 调用 private dispatcher；可显式改为 inline
bytecode.scope = func        # 每个被保护函数生成自己的 bytecode
polymorph.scope = func       # 每个函数允许独立 opcode/key/layout 多态化
```

也就是：

- 每个 LLVM `Module` 生成一套 VM runtime。
- pass 默认把每个被保护函数的 const_pool/code segment 拼入模块级紧凑 bytecode blob，即使 profile 写的是 `bytecode.scope = func`，wrapper 也不能直接引用 per-function bytecode global。
- `runtime.scope = module` 的共享边界是同一个 descriptor group：同 profile 组内共享一套 module-scope runtime，跨 profile 或跨 descriptor table/seed 的函数必须生成不同 runtime helper 名，不能复用同一个 dispatcher。
- 所有 native-call thunk pointer 进入同一个模块级 native table；每个函数的 descriptor 记录自己的 `native_base` 和 `native_count`。
- 每个被保护函数拥有自己的 function key、opcode permutation、bytecode layout salt。
- 原始函数 body 被替换成 wrapper：负责 marshal 参数到 `arg_slots`、构造混淆后的 `fn_token`，再按 `runtime.entry` 进入 VM；`call` 模式调用 `dispatch(fn_token, ret_slots, arg_slots)`，`inline` 模式把 descriptor decode、VM loop 和 handler CFG 直接嵌入 wrapper，最后统一跳到返回 marshal block。

生成后的形态类似：

```text
.L__<hash>                # 生产默认使用 private dispatcher / reader helper，不暴露 AMICE/VMP 可搜索符号名
.L__<hash>                # 生产默认使用 private compact bytecode blob，不保留 AMICEVMP package magic
.L__<hash>                # 生产默认使用 private encrypted descriptor table
.L__<hash>                # 生产默认使用 private module native table

.amice.vm.bytecode.foo    # 仅 runtime.emit_markers=true 或 AMICE_VM_EMIT_MARKERS=true 的测试/调试输出
.amice.vm.meta.foo        # 仅 runtime.emit_markers=true 或 AMICE_VM_EMIT_MARKERS=true 的测试/调试输出
.amice.vm.descriptor_table.foo # 仅 runtime.emit_markers=true 或 AMICE_VM_EMIT_MARKERS=true 的测试/调试输出
foo(...) {
  ret_slots = alloca(...)
  arg_slots = marshal_args(...)
  fn_token = obfuscated_token(fn_index)
  call .L__<hash>(fn_token, ret_slots, arg_slots)
  return marshal_ret(ret_slots)
}

foo_inline(...) {
  ret_slots = alloca(...)
  arg_slots = marshal_args(...)
  fn_token = obfuscated_token(fn_index)
  descriptor.decode / descriptor.ready / loop.check / execute.decode / handler.*  # 由 runtime CFG emitter 直接写入 wrapper
  after_vm:
  return marshal_ret(ret_slots)
}
```

合法 scope 只允许 `func` 和 `module`。其他字符串都是非法 profile 值。

| Scope    | 行为                   | 适用场景        | 风险                |
|----------|----------------------|-------------|-------------------|
| `func`   | 每个函数生成独立 runtime     | 最大多态性       | 体积膨胀明显            |
| `module` | 每个 Module 共享 runtime | 默认选择        | 非 LTO 下每个目标文件各有一份 |

## Profile DSL

Profile DSL 是受限、声明式、可校验的语言。DSL 采用 SSA 风格表达数据流，禁止执行任意宿主代码。

### ISA 定义

`isa.vm` 描述 VM 指令、operand 和 handler 语义。语义块不是任意脚本；当前 AMICE 支持赋值、`pc` 赋值、`store_width`、`volatile_store_width`、`atomic_store_width`、`volatile_atomic_store_width`、`state = unchanged`、寄存器引用、常量池引用、整数二元运算、整数溢出标志、整数比较、标量浮点二元运算、标量浮点/整数混合二元运算、标量浮点一元运算、标量浮点到整数取整 intrinsic、标量浮点比较、标量浮点分类、宽度转换、`stack_alloc`、`stack_save`、`stack_restore`、`clear_cache`、`pseudo_probe`、`prefetch`、`load_width`、`volatile_load_width`、`atomic_load_width`、`volatile_atomic_load_width`、`atomic_rmw`、`volatile_atomic_rmw`、`memcpy_dynamic`、`memmove_dynamic`、`memset_dynamic`、`volatile_memcpy_dynamic`、`volatile_memmove_dynamic`、`volatile_memset_dynamic`、`cmpxchg`、`fence` 和 `call_table` 返回槽读取。Verifier 会把这些语句解析成 typed AST，并匹配到 AMICE 已实现的有限 handler 模板；不能匹配的 semantic 会被拒绝。

同一个 opcode alias 在整个 `isa.vm` 指令集中必须唯一；同一条 `instr` 的 alias 列表内部也不能重复。Verifier 会在冲突时报告重复 opcode 和关联指令名，避免 runtime 分发表出现不可判定的 handler 目标。

```text
instr iadd32(dst: vreg<i32>, lhs: vreg<i32>, rhs: vreg<i32>) { # 定义 32 位整数加法 VM 指令
  opcode alias [0x31, 0xa7]                                    # 同一语义可随机选择多个 opcode
  semantic {                                                    # handler 语义块，必须可被 verifier 静态分析
    reg[dst] = trunc_width(reg[lhs] + reg[rhs], width)           # 读取 lhs/rhs 虚拟寄存器，相加后按 width 截断写入 dst
    pc = next                                                   # 执行完成后进入下一条 VM 指令
  }                                                             # 结束 iadd32 语义块
}                                                               # 结束 iadd32 指令定义
# 下一个指令定义用于条件分支
instr br_if(cond: vreg<i1>, then_pc: label, else_pc: label) {   # 定义条件跳转 VM 指令
  opcode alias [0x52]                                           # 条件跳转当前只有一个 opcode
  semantic {                                                    # handler 语义块
    pc = select(reg[cond], then_pc, else_pc)                    # 根据 cond 选择 then 或 else 的字节码 PC
  }                                                             # 结束 br_if 语义块
}                                                               # 结束 br_if 指令定义
```

Verifier 需要从 semantic AST 中推导：

- 读哪些 VM register。
- 写哪些 VM register。
- 是否读写 memory。
- 是否修改 `pc`。
- 是否可能调用 native function。
- operand 类型和字节码 layout 是否匹配。

### Lowering 规则

`lowering.vm` 描述 LLVM IR 到 VM 指令的翻译。实现必须结构化解析 `rule`、`match`、`lower`、`materialize`、`vreg`、`emit`、`bind`，translator 按 action 顺序执行：`materialize` 把 LLVM value 或立即数变成 VM value，`vreg` 分配目标 VM 寄存器，`emit` 必须使用 profile ISA 中的具名指令，`bind` 把 LLVM result 绑定到已定义 VM value。缺少 rule、emit 指令不存在、emit operand 不符合 ISA、result rule 缺少 bind、bind 或 emit 引用未定义 VM value 时，profile verifier 或 pass 会拒绝该函数并输出 debug 诊断。

```text
rule llvm.add.integer {                      # 定义 LLVM 整数 add 的 lowering 规则
  match %r = llvm.add integer %a, %b         # 匹配 LLVM IR：%r = add integer %a, %b
  lower {                                    # lowering 动作块
    %va = materialize %a as integer          # 把 LLVM value %a 映射到 VM 整数值
    %vb = materialize %b as integer          # 把 LLVM value %b 映射到 VM 整数值
    %vr = vreg integer                       # 分配一个 VM x 寄存器保存结果
    emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中定义的 iadd 指令
    bind %r = %vr                            # 记录 LLVM result %r 与 VM value %vr 的绑定关系
  }                                          # 结束 lowering 动作块
}                                            # 结束 llvm.add.integer 规则
```

LLVM IR lowering 必须覆盖的基础子集：

- 整数运算：`add`、`sub`、`mul`、`udiv`、`sdiv`、`urem`、`srem`、`xor`、`and`、`or`、`shl`、`lshr`、`ashr`，以及 i1/i8/i16/i32/i64 的 `llvm.ctpop`、`llvm.ctlz`、`llvm.cttz`、`llvm.abs`、`llvm.smax`、`llvm.smin`、`llvm.umax`、`llvm.umin`、`llvm.uadd.sat`、`llvm.usub.sat`、`llvm.sadd.sat`、`llvm.ssub.sat`、`llvm.ushl.sat`、`llvm.sshl.sat`、`llvm.uadd.with.overflow`、`llvm.sadd.with.overflow`、`llvm.usub.with.overflow`、`llvm.ssub.with.overflow`、`llvm.umul.with.overflow`、`llvm.smul.with.overflow`、`llvm.bitreverse`、`llvm.fshl`、`llvm.fshr`、`llvm.set.loop.iterations`、`llvm.start.loop.iterations`、`llvm.test.set.loop.iterations`、`llvm.test.start.loop.iterations`、`llvm.loop.decrement`、`llvm.loop.decrement.reg` intrinsic，i16/i32/i64 的 `llvm.bswap` intrinsic，以及 i1/i8/i16/i32/i64 的 `llvm.expect` / `llvm.expect.with.probability` 值保持 intrinsic；`llvm.ctlz` / `llvm.cttz` 接受 `is_zero_undef=true|false`，`llvm.abs` 接受 `is_int_min_poison=true|false`，true flag 触发的 poison 输入沿用 LLVM 未定义边界。
- 标量浮点运算和分类：普通 `fadd`、`fsub`、`fmul`、`fdiv`、`frem`、`fneg` 和 `fcmp` 支持 LLVM `half`、`float` 和 `double`，VM x 寄存器保存 IEEE 原始 bit；其中 half runtime 通过 f16 bit 扩展到 f32 计算后再截回 f16 bit。`half` / `float` / `double` 标量 `llvm.fabs`、`llvm.sqrt`、`llvm.canonicalize`、`llvm.floor`、`llvm.ceil`、`llvm.trunc`、`llvm.rint`、`llvm.nearbyint`、`llvm.round`、`llvm.roundeven`、`llvm.lrint`、`llvm.llrint`、`llvm.lround`、`llvm.llround`、`llvm.sin`、`llvm.cos`、`llvm.exp`、`llvm.exp2`、`llvm.log`、`llvm.log10`、`llvm.log2`、`llvm.pow`、`llvm.powi`、`llvm.fma`、`llvm.fmuladd`、`llvm.minnum`、`llvm.maxnum`、`llvm.minimum`、`llvm.maximum`、`llvm.copysign` 和 `llvm.is.fpclass` 支持位级或 profile handler 处理；`llvm.fabs` 清除符号位并保留其它 bit，`llvm.sqrt` 和数学类一元 intrinsic 通过 profile 中的 `float_unary(...)` semantic 进入 runtime，其中 f16 一元路径先扩展到 f32 执行再截回 f16 bit，`llvm.lrint` / `llvm.llrint` / `llvm.lround` / `llvm.llround` 通过 profile 中的 `float_round_to_int(...)` semantic 进入 runtime 并调用对应 LLVM intrinsic，`llvm.pow` 通过 profile 中的 `float_bin(fpow, ...)` semantic 进入 runtime 并调用 LLVM `pow` intrinsic，`llvm.powi` 通过 profile 中的 `float_int_bin(fpowi, ...)` semantic 进入 runtime 并调用 LLVM `powi` intrinsic，指数 operand 必须是 i32，`llvm.fma` 通过 profile 中的 `float_ternary(fma, ...)` semantic 进入 runtime 并调用 LLVM `fmuladd` intrinsic，`llvm.fmuladd` 通过 profile 中的 `float_ternary(fmuladd, ...)` semantic 进入 runtime 并调用 LLVM `fmuladd` intrinsic，`llvm.minnum` / `llvm.maxnum` 通过 profile 中的 `float_bin(fminnum/fmaxnum, ...)` semantic 进入 runtime 并调用 LLVM `minnum` / `maxnum` intrinsic，`llvm.minimum` / `llvm.maximum` 通过 profile 中的 `float_bin(fminimum/fmaximum, ...)` semantic 进入 runtime 并调用 LLVM `minimum` / `maximum` intrinsic，`llvm.copysign` 保留第一个 operand 的数值位并复制第二个 operand 的符号位，`llvm.is.fpclass` 的 mask 按 LLVM `FPClassTest` 位定义解释。
- 标量浮点取整和数学一元 intrinsic：`llvm.floor`、`llvm.ceil`、`llvm.trunc`、`llvm.rint`、`llvm.nearbyint`、`llvm.round`、`llvm.roundeven`、`llvm.sin`、`llvm.cos`、`llvm.exp`、`llvm.exp2`、`llvm.log`、`llvm.log10` 和 `llvm.log2` 支持 `half`、`float` 和 `double`，必须由 profile 中的 `llvm.*.float` lowering rule 发射到 `ffloor`、`fceil`、`ftrunc`、`frint`、`fnearbyint`、`fround`、`froundeven`、`fsin`、`fcos`、`fexp`、`fexp2`、`flog`、`flog10` 和 `flog2`，再由对应 `float_unary(...)` semantic 驱动 runtime 调用 LLVM intrinsic；f16 路径先扩展为 f32 执行，再截回 f16 bit。
- 标量浮点二元/三元数学 intrinsic：`llvm.pow` 支持 `half`、`float` 和 `double`，必须由 profile 中的 `llvm.pow.float` lowering rule 发射到 `fpow`，再由 `float_bin(fpow, ...)` semantic 驱动 runtime 调用 LLVM `pow` intrinsic；`llvm.powi` 支持 `half`、`float` 和 `double` 底数加 i32 指数，必须由 profile 中的 `llvm.powi.float` lowering rule 发射到 `fpowi`，再由 `float_int_bin(fpowi, ...)` semantic 驱动 runtime 调用 LLVM `powi` intrinsic；`llvm.fma`、`llvm.fmuladd`、`llvm.minnum`、`llvm.maxnum`、`llvm.minimum` 和 `llvm.maximum` 同样支持 `half`、`float` 和 `double`，f16 路径先扩展为 f32 执行，再截回 f16 bit。
- 受限浮点 intrinsic：`llvm.experimental.constrained.fadd`、`llvm.experimental.constrained.fsub`、`llvm.experimental.constrained.fmul`、`llvm.experimental.constrained.fdiv` 和 `llvm.experimental.constrained.frem` 当前支持 `half`、`float` 和 `double` 标量，以及同 lane 数同 lane 宽的 fixed vector 形态，metadata 必须是 `round.tonearest` 与 `fpexcept.ignore` 的保守子集。translator 会先校验 rounding 和 exception metadata；标量分别通过 profile 中的 `llvm.constrained.fadd.float`、`llvm.constrained.fsub.float`、`llvm.constrained.fmul.float`、`llvm.constrained.fdiv.float` 和 `llvm.constrained.frem.float` lowering rule 发射到普通 `fadd` / `fsub` / `fmul` / `fdiv` / `frem` handler，fixed vector 则通过 `llvm.constrained.vector.{fadd,fsub,fmul,fdiv,frem}.float` rule 逐 lane 发射同一组 handler。
  `llvm.experimental.constrained.sqrt`、`llvm.experimental.constrained.rint`、`llvm.experimental.constrained.nearbyint`、`llvm.experimental.constrained.sin`、`llvm.experimental.constrained.cos`、`llvm.experimental.constrained.exp`、`llvm.experimental.constrained.exp2`、`llvm.experimental.constrained.log`、`llvm.experimental.constrained.log10` 和 `llvm.experimental.constrained.log2` 当前支持 `half`、`float` 和 `double` 标量，以及同 lane 数同 lane 宽的 fixed vector 形态，metadata 必须是 `round.tonearest` 与 `fpexcept.ignore`；`llvm.experimental.constrained.fabs`、`llvm.experimental.constrained.canonicalize`、`llvm.experimental.constrained.floor`、`llvm.experimental.constrained.ceil`、`llvm.experimental.constrained.trunc`、`llvm.experimental.constrained.round` 和 `llvm.experimental.constrained.roundeven` 当前支持 `half`、`float` 和 `double` 标量，以及同 lane 数同 lane 宽的 fixed vector 形态，metadata 必须是 `fpexcept.ignore`。这些 constrained unary 的标量形态必须分别通过 profile 中的 `llvm.constrained.{fabs,sqrt,canonicalize,floor,ceil,trunc,rint,nearbyint,round,roundeven,sin,cos,exp,exp2,log,log10,log2}.float` lowering rule 发射到普通浮点一元 handler；fixed vector 形态必须通过 `llvm.constrained.vector.{fabs,sqrt,canonicalize,floor,ceil,trunc,rint,nearbyint,round,roundeven,sin,cos,exp,exp2,log,log10,log2}.float` rule 逐 lane 发射同一组 handler，并沿用普通一元 intrinsic 的宽度约束。
  `llvm.experimental.constrained.pow` 当前支持 `half`、`float` 和 `double` 标量，以及同 lane 数同 lane 宽 fixed vector，metadata 必须是 `round.tonearest` 与 `fpexcept.ignore`；`llvm.experimental.constrained.copysign`、`llvm.experimental.constrained.minnum`、`llvm.experimental.constrained.maxnum`、`llvm.experimental.constrained.minimum` 和 `llvm.experimental.constrained.maximum` 当前支持 `half`、`float` 和 `double` 标量，以及同 lane 数同 lane 宽 fixed vector，metadata 必须是 `fpexcept.ignore`；`llvm.experimental.constrained.powi` 当前支持 `half`、`float` 和 `double` 标量，以及 fixed vector 浮点底数配 i32 标量指数，metadata 必须是 `round.tonearest` 与 `fpexcept.ignore`；`llvm.experimental.constrained.fma` 和 `llvm.experimental.constrained.fmuladd` 当前支持 `half`、`float` 和 `double` 标量，以及三个源和结果同 lane 数同 lane 宽 fixed vector，metadata 必须是 `round.tonearest` 与 `fpexcept.ignore`；`llvm.experimental.constrained.lrint` 和 `llvm.experimental.constrained.llrint` 当前只支持 `half`、`float` 和 `double` 标量输入、i32/i64 标量整数返回、metadata `round.tonearest` 与 `fpexcept.ignore`；`llvm.experimental.constrained.lround` 和 `llvm.experimental.constrained.llround` 当前只支持同一类型子集与 `fpexcept.ignore`。这些 constrained binary math 的标量形态必须分别通过 profile 中的 `llvm.constrained.{copysign,pow,minnum,maxnum,minimum,maximum}.float` lowering rule 发射到普通 math handler，fixed vector 形态必须通过 `llvm.constrained.vector.{copysign,pow,minnum,maxnum,minimum,maximum}.float` rule 逐 lane 发射同一组 handler；constrained `powi/fma/fmuladd` 的标量形态必须通过 profile 中的 `llvm.constrained.{powi,fma,fmuladd}.float` lowering rule 发射到普通 math handler，fixed vector 形态必须通过 `llvm.constrained.vector.{powi,fma,fmuladd}.float` rule 逐 lane 发射同一组 handler；constrained round-to-int 标量形态必须通过 profile 中的 `llvm.constrained.{lrint,llrint,lround,llround}.float` lowering rule 发射到普通 round-to-int handler。
  `llvm.experimental.constrained.fcmp` 和 `llvm.experimental.constrained.fcmps` 当前支持 `half`、`float` 和 `double` 标量，以及同 lane 数同 lane 宽的 fixed vector 形态，metadata 必须是 LLVM fcmp 谓词和 `fpexcept.ignore`；标量必须分别通过 profile 中的 `llvm.constrained.fcmp.float` 和 `llvm.constrained.fcmps.float` lowering rule 发射到普通 `fcmp` handler，fixed vector 必须分别通过 `llvm.constrained.vector.fcmp.float` 和 `llvm.constrained.vector.fcmps.float` rule 逐 lane 发射同一 handler。
  `llvm.experimental.constrained.sitofp`、`llvm.experimental.constrained.uitofp` 和 `llvm.experimental.constrained.fptrunc` 当前支持标量和同 lane 数 fixed vector，metadata 必须是 `round.tonearest` 与 `fpexcept.ignore`；`llvm.experimental.constrained.fptosi`、`llvm.experimental.constrained.fptoui` 和 `llvm.experimental.constrained.fpext` 当前支持标量和同 lane 数 fixed vector，metadata 必须是 `fpexcept.ignore`。这些 constrained cast 的标量形态必须分别通过 profile 中的 `llvm.constrained.sitofp.float`、`llvm.constrained.uitofp.float`、`llvm.constrained.fptosi.float`、`llvm.constrained.fptoui.float`、`llvm.constrained.fptrunc.float` 和 `llvm.constrained.fpext.float` lowering rule 发射到普通 float-cast handler；fixed vector 形态必须分别通过 `llvm.constrained.vector.sitofp.float`、`llvm.constrained.vector.uitofp.float`、`llvm.constrained.vector.fptosi.float`、`llvm.constrained.vector.fptoui.float`、`llvm.constrained.vector.fptrunc.float` 和 `llvm.constrained.vector.fpext.float` rule 逐 lane 发射同一组 handler，并沿用普通 cast 的整数宽度、浮点端 `half` / `float` / `double`、lane 数一致以及 fptrunc/fpext 宽度方向约束；其它 rounding mode、exception behavior、scalable vector、q ABI、未列入本条的 vector constrained intrinsic、未列入上述白名单的 constrained math intrinsic 或非受支持类型必须安全跳过目标函数。因为异常行为被忽略，signaling fcmps 在当前子集内只保留比较布尔结果，不模拟浮点异常状态。
- 标量整数/浮点转换：`sitofp`、`uitofp`、`fptosi` 和 `fptoui` 的浮点端支持 LLVM `half`、`float` 和 `double`；`fptrunc` / `fpext` 支持 `half`、`float` 和 `double` 三者之间的合法窄化或扩宽转换，VM record 必须同时携带 `from_width` 和 `to_width` 并由 runtime 按宽度组合还原 LLVM cast；`llvm.convert.to.fp16` 必须通过 profile lowering 复用 `fptrunc` 语义把 f32/f64 转成 i16 half bit，`llvm.convert.from.fp16` 必须复用 `fpext` 语义把 i16 half bit 转成 f32/f64；标量和固定向量 `llvm.fptosi.sat`、`llvm.fptoui.sat` 的浮点端支持 LLVM `half`、`float` 和 `double`，目标整数宽度支持 i1/i8/i16/i32/i64。
- 标量浮点到整数取整 intrinsic：`llvm.lrint`、`llvm.llrint`、`llvm.lround` 和 `llvm.llround` 支持 `half`、`float` 和 `double` 输入，目标整数宽度支持 i32/i64；profile 必须通过 `llvm.lrint.float`、`llvm.llrint.float`、`llvm.lround.float` 或 `llvm.llround.float` lowering rule 发射到 `flrint`、`fllrint`、`flround` 或 `fllround`，再由 `float_round_to_int(lrint|llrint|lround|llround, ...)` semantic 驱动 runtime 调用对应 LLVM intrinsic。
- 比较：整数/指针标量 `icmp`、`half`/`float`/`double` 标量 `fcmp`。
- 选择：`select`，当前支持整数、指针、`half`、`float` 和 `double` 标量，以及直接 struct / 固定数组小聚合；标量按 x 寄存器 bit 复制，聚合按叶子字段展开为 profile `llvm.select.aggregate` 中的 `br_if` / `mov` / `br` 序列。聚合 then/else operand 可以来自已支持聚合 lowering，也可以是无 `undef` / `poison` 字段的 LLVM 常量 struct / 固定数组；含 `undef` / `poison` 字段的常量聚合必须先经过 `freeze`。
- 类型转换：`zext`、`sext`、`trunc`、同宽整数/`half`/`float`/`double` 标量 `bitcast`、`ptrtoint`、`inttoptr`、`addrspacecast`；`addrspacecast` 当前只按 64 位指针 bit 模式保留。
- 值稳定化：`freeze`，当前支持整数、指针、`half`、`float` 或 `double` 标量、直接 struct / 固定数组小聚合，以及固定向量内部临时值；聚合 `freeze` 可以直接消费 LLVM 常量 struct / 固定数组并按 leaf 字段展开，固定向量 `freeze` 可以直接消费 LLVM 常量向量，常量聚合字段或常量向量 lane 中的 `undef` / `poison` 在 VM 中稳定为 0 bit pattern。
- 指针值保持、指针 mask、编译期查询和注释类 intrinsic：`llvm.launder.invariant.group`、`llvm.strip.invariant.group`、`llvm.preserve.array.access.index`、`llvm.preserve.union.access.index`、`llvm.preserve.struct.access.index`、`llvm.preserve.static.offset`、`llvm.invariant.start`、`llvm.threadlocal.address`、`llvm.thread.pointer`、`llvm.ptrmask`、`llvm.is.constant`、`llvm.objectsize`、`llvm.experimental.widenable.condition`、`llvm.allow.runtime.check`、`llvm.allow.ubsan.check`、`llvm.annotation`、`llvm.ptr.annotation`，当前只保留整数或 64 位指针 bit，`llvm.is.constant` 在 translator 中按整数、指针或 `half` / `float` / `double` 标量 operand，以及这些 x 寄存器可承载 lane 组成的固定向量 operand 是否为 LLVM 常量折叠成 i1 并通过 profile 常量物化进入 VM；`llvm.experimental.widenable.condition` 是无参数 i1 优化提示，translator 通过 profile `llvm.widenable.condition.integer` lowering rule 保守物化为 true；`llvm.allow.runtime.check` 和 `llvm.allow.ubsan.check` 是运行时检查开关，translator 通过 profile `llvm.allow.runtime.check.integer` / `llvm.allow.ubsan.check.integer` lowering rule 保守物化为 true，避免删除可选运行时检查；`llvm.preserve.*.access.index` 和 `llvm.preserve.static.offset` 只保留第一个 pointer operand 的运行时地址，访问索引、debug type 和静态偏移信息不进入 VM 状态；`llvm.objectsize` 当前优先接受静态可确定的 alloca、GlobalVariable、可剥离 pointer `bitcast` / `addrspacecast` 之后仍回到静态对象的基址，以及常量偏移 GEP，并按剩余对象字节数通过 `llvm.objectsize.integer` lowering rule 物化为 VM 常量；如果基址是函数参数或其它普通未知指针，translator 按 LLVM unknown object size 规则折叠：`min=true` 为 0，`min=false` 为结果位宽全 1，该规则不受 `dynamic` 标志影响；直接动态 alloca 或只经过 pointer `bitcast` / `addrspacecast` 回到动态 alloca 的形态在 `dynamic=true` 时通过 `llvm.objectsize.dynamic_alloca` lowering rule 发射 `imul` 运行时计算 `count * elem_size`，动态 alloca 在 `dynamic=false` 时仍使用 unknown fallback；动态 GEP 这类对象大小还依赖运行时偏移的形态只在 `dynamic=false` 时使用 unknown fallback；`nullunknown=false` 的 null 指针折叠为 0，`nullunknown=true` 的 null 指针使用同一 unknown 规则；invariant-group、preserve 索引、invariant 描述符长度和 annotation metadata 不进入 VM 状态；`llvm.threadlocal.address` 的 TLS GlobalValue operand 必须留在私有 native thunk 内，VM bytecode 通过 `call_native` + `tls_addr` 记录该边界；`llvm.thread.pointer` 通过 profile `read_thread_pointer` handler 读取当前线程指针并以 64 位 pointer bit 写入 x 寄存器。
  注：上述动态 GEP safe-skip 不包括“由静态对象或动态 alloca 起始、每段 GEP 都带 `inbounds`、并且 translator 能在每段 GEP lowering 时记录或累加运行时 byte offset”的子集；静态对象子集在 `dynamic=true` 时通过 `llvm.objectsize.static_gep` 发射 `isub` 计算 `total_size - offset`，动态 alloca 子集通过 `llvm.objectsize.dynamic_gep_offset` 记录当前段 `gep_ptr - base_ptr`，链式 GEP 再通过 `llvm.objectsize.dynamic_gep_accumulate` 累加总偏移，最后由 `llvm.objectsize.dynamic_alloca_gep` 发射 `imul` + `isub` 计算 `count * elem_size - offset`。
- SSA 和浮点优化屏障类值保持 intrinsic：`llvm.ssa.copy` 当前支持整数、指针、`half`、`float` 和 `double` 标量，以及这些 x 寄存器可承载 lane 的固定向量，必须由 profile 中的 `llvm.ssa.copy.scalar` 或 `llvm.vector.ssa.copy` lowering rule 发射到 `mov`，只复制 x 寄存器 bit，不生成专用 runtime handler；`llvm.arithmetic.fence.*` 当前支持 `half`、`float` 和 `double` 标量以及固定浮点向量，必须由 profile 中的 `llvm.arithmetic.fence.scalar` 或 `llvm.vector.arithmetic.fence` lowering rule 发射到 `mov`，VM 保留原始浮点 bit，优化屏障语义由原 LLVM intrinsic 边界和 lowering 规则共同声明。
- 固定向量 x 槽 lowering：在 `q.lowering = disabled` 时，当前只支持函数体内部固定向量的常量或动态 lane `insertelement` / `extractelement`、固定向量 `freeze`、固定向量 `phi`、scalar i1 或 fixed `<N x i1>` 条件的固定向量 `select`、常量 mask 的固定向量 `shufflevector`、固定向量 `llvm.vector.reverse` / `llvm.vector.splice` / `llvm.experimental.vp.splice` / `llvm.vector.insert` / `llvm.vector.extract` / `llvm.experimental.vector.compress`、同 lane 数且同 lane 位宽的固定向量 `bitcast`、固定整数向量 `zext/sext/trunc`、固定指针向量 `ptrtoint/inttoptr/addrspacecast`、固定整数向量 `add/sub/mul/udiv/sdiv/urem/srem/xor/and/or/shl/lshr/ashr`、固定整数向量 `icmp`、固定浮点向量 `fcmp`、固定浮点向量 unary intrinsic `llvm.fabs/sqrt/canonicalize/floor/ceil/trunc/rint/nearbyint/round/roundeven`、固定向量整数/浮点互转 `sitofp/uitofp/fptosi/fptoui`、固定向量饱和浮点转整数 intrinsic `llvm.fptosi.sat/fptoui.sat`、固定浮点向量 `fptrunc/fpext`，以及固定浮点向量 `fneg/fadd/fsub/fmul/fdiv/frem`；元素必须是 x 寄存器可承载的整数、指针、`half`、`float` 或 `double` 标量，动态 lane 下标必须是 x 寄存器可承载的整数，动态 `insertelement` 的基础向量可以是全量常量向量、`undef` / `poison` 或已由受支持 lowering 写入的 lane，动态 `extractelement` 的源向量可以是无 `undef` / `poison` lane 的常量向量或已由受支持 lowering 写入的 lane，固定向量 `freeze` 的源向量可以是常量向量、`undef` / `poison` 或已由受支持 lowering 写入的 lane，常量向量中的 `undef` / `poison` lane 只允许被 `freeze` 稳定为 0 bit pattern，固定向量 `llvm.vector.insert` / `llvm.vector.extract` 只支持 fixed vector、i64 非负常量 offset 且 subvector 范围完整落在 base/source lane 范围内，固定向量 `llvm.experimental.vector.compress` 只支持 fixed vector、常量 `<N x i1>` mask、value / passthru / result lane 数和 lane 类型一致，固定向量 `bitcast` 只逐 lane 保留 bit pattern，不支持跨 lane 重新打包，固定整数向量 `zext/sext/trunc` 的源和结果 lane 必须全部是整数 lane 且 lane 数相同，固定指针向量 `ptrtoint/inttoptr/addrspacecast` 的源和结果 lane 必须在整数 lane 与 64 位 pointer lane 之间逐 lane 转换且 lane 数相同，固定整数向量二元运算的 lane 必须全部是整数 lane，固定整数向量 `icmp` 的 operand lane 必须全部是整数 lane 且结果 lane 是 `i1`，固定向量 `select` 的 vector condition 必须是同 lane 数的 fixed `<N x i1>`，固定向量 `sitofp/uitofp` 的源 lane 必须全部是整数且结果 lane 必须全部是 `half` / `float` / `double`，固定向量 `fptosi/fptoui` 和固定向量 `llvm.fptosi.sat/fptoui.sat` 的源 lane 必须全部是 `half` / `float` / `double` 且结果 lane 必须全部是整数，固定浮点向量 `fneg`、固定浮点向量二元运算、固定浮点向量 `fcmp`、固定浮点向量 `fptrunc/fpext` 和固定浮点向量 unary intrinsic 的 operand lane 必须全部是 `half` / `float` / `double` lane，`fptrunc` 只能逐 lane 窄化，`fpext` 只能逐 lane 扩宽，`fcmp` 结果 lane 必须是 `i1`；profile 必须分别通过 `llvm.vector.insert.element`、`llvm.vector.insert.dynamic_element`、`llvm.vector.extract.element`、`llvm.vector.extract.dynamic_element`、`llvm.vector.freeze`、`llvm.vector.phi.edge_move`、`llvm.select.vector`、`llvm.select.vector_condition`、`llvm.vector.shuffle.element`、`llvm.vector.reverse.element`、`llvm.vector.splice.element`、`llvm.experimental.vp.splice.element`、`llvm.vector.insert.subvector.element`、`llvm.vector.extract.subvector.element`、`llvm.experimental.vector.compress.element`、`llvm.vector.bitcast.element`、`llvm.vector.cast.integer`、`llvm.vector.cast.pointer`、`llvm.vector.{add,sub,mul,udiv,sdiv,urem,srem}.integer`、`llvm.vector.bitops.integer`、`llvm.vector.shift.integer`、`llvm.vector.icmp.integer`、`llvm.vector.fcmp.float`、`llvm.vector.{fabs,sqrt,canonicalize,floor,ceil,trunc,rint,nearbyint,round,roundeven}.float`、`llvm.vector.fneg.float`、`llvm.vector.{sitofp,uitofp,fptosi,fptoui}.float`、`llvm.vector.{fptosi.sat,fptoui.sat}.float`、`llvm.vector.{fptrunc,fpext}.float` 和 `llvm.vector.{fadd,fsub,fmul,fdiv,frem}.float` lowering rule 发射 `mov`/`br_if`/`br` 或对应 ALU handler。固定向量函数参数/返回以及 direct/indirect native call 的固定向量参数/返回也使用同一套 x 槽逐 lane 展开机制；这不是 q 寄存器 ABI、scalable vector、direct varargs vector 变参、packed 或 data-layout 不能证明为逐 lane 字节连续布局的向量 load/store、scalable vector condition select 或未列入固定向量白名单的其它向量浮点 intrinsic 支持。
- 固定整数向量 `llvm.stepvector` 通过 `llvm.vector.step` lowering rule 展开；translator 对每个 lane 发射一次 profile `mov_imm`，立即数为该 lane 的 0-based 编号并按 lane 宽度截断。LLVM 21 verifier 只允许 lane bitwidth 至少为 8，因此该路径只支持 fixed integer vector，lane 宽度必须是 i8/i16/i32/i64，intrinsic 参数数量必须为 0；scalable vector、i1 lane、非整数 lane、q ABI 场景或签名不匹配时必须安全跳过。
- 固定 mask 向量 `llvm.get.active.lane.mask` 通过 `llvm.vector.get.active.lane.mask` lowering rule 展开；translator 对 fixed `<N x i1>` 返回的每个 lane 发射 `mov_imm` 物化 lane 编号，再用 `iadd` 计算 `start + lane`，最后用 `icmp ult` 与 `end` 比较得到当前 lane 的 i1 mask。该路径只支持两个同宽整数 `start` / `end` 参数，宽度必须是 i1/i8/i16/i32/i64；返回必须是 fixed `<N x i1>`；scalable vector、非 i1 结果 lane、非整数或宽度不一致的 start/end、q ABI 场景或签名不匹配时必须安全跳过。
- 固定 mask 向量 `llvm.experimental.cttz.elts` 通过 `llvm.experimental.cttz.elts` lowering rule 展开；translator 支持 `zero_is_poison=false/true` 的 fixed `<N x i1>` mask 子集，先把结果初始化为 lane 总数，再按高 lane 到低 lane 的顺序对每个 true lane 发射 `br_if`、`mov_imm` 和 `mov` 覆盖结果，因此最终得到从 lane0 开始连续 false lane 的数量。`zero_is_poison=true` 且 mask 全零时 LLVM 结果是 poison，VM 选择 lane 总数作为确定代表值。结果整数宽度必须是 i1/i8/i16/i32/i64 且能够表示 lane 总数；mask lane 必须全是 i1 且已经由受支持 lowering 写入或是无 undef/poison 的常量向量；scalable vector、非 i1 mask lane、结果宽度不足、未冻结 undef/poison lane 或 q ABI 场景必须安全跳过。
- 固定 mask 向量 `llvm.vp.cttz.elts` 通过 `llvm.vp.cttz.elts` lowering rule 展开；translator 支持 `zero_is_poison=false/true`、fixed `<N x i1>` 计数 mask、常量 `<N x i1>` VP mask 和常量 i32 EVL 子集，先把结果初始化为 `min(EVL, lane_count)`，再按高 lane 到低 lane 的顺序只对 `vp_mask[lane] == true && lane < EVL` 的激活 lane 检查计数 mask 并发射 `br_if`、`mov_imm` 和 `mov` 覆盖结果。`zero_is_poison=true` 且激活 lane 全零时 LLVM 结果是 poison，VM 选择激活 lane 数作为确定代表值。结果整数宽度必须是 i1/i8/i16/i32/i64 且能够表示 `min(EVL, lane_count)`；计数 mask 的激活 lane 必须全是 i1 且已经由受支持 lowering 写入或是无 undef/poison 的常量向量；动态 VP mask、动态 EVL、scalable vector、非 i1 mask lane、结果宽度不足、激活 lane 未冻结 undef/poison 或 q ABI 场景必须安全跳过。
- 非 scalable 向量长度查询 `llvm.experimental.get.vector.length` 通过 `llvm.experimental.get.vector.length.integer` lowering rule 展开；translator 支持 `isScalable=false`、返回 i32、AVL 为 i1/i8/i16/i32/i64 整数且 VF 为 i32 immarg 的子集，语义为无符号 `min(AVL, VF)` 后返回 i32。profile 必须发射 `zext`（仅窄 AVL 实际使用）、`mov_imm`、`icmp ult`、`trunc`、`br_if`、`mov` 和 `br` 组合；`isScalable=true`、动态 VF/scalable flag、AVL 非 x 寄存器整数、返回不是 i32 或 q ABI 场景必须安全跳过。
- 固定整数向量 VP 二元 intrinsic：`llvm.vp.add/sub/mul/udiv/sdiv/urem/srem/xor/and/or/shl/lshr/ashr` 当前支持 fixed integer vector、常量 `<N x i1>` mask、常量 i32 EVL、i8/i16/i32/i64 lane。translator 只对 `mask[lane] == true && lane < EVL` 的激活 lane 执行 lowering，并分别通过 `llvm.vp.vector.add/sub/mul/udiv/sdiv/urem/srem.integer`、`llvm.vp.vector.bitops.integer` 和 `llvm.vp.vector.shift.integer` rule 发射现有整数 ALU handler；未激活 lane 保持 poison/未绑定状态，后续只有经过受支持 `freeze` 后才能被稳定读取。动态 mask、动态 EVL、scalable vector、非整数 lane、lane 数或 lane 宽不匹配、q ABI 场景必须安全跳过。
- 固定整数向量 VP 一元 intrinsic：`llvm.vp.ctpop/ctlz/cttz/abs/bswap/bitreverse` 当前支持 fixed integer vector、常量 `<N x i1>` mask、常量 i32 EVL、i8/i16/i32/i64 lane（其中 `bswap` 仍只允许 i16/i32/i64 lane）。`llvm.vp.ctlz/cttz` 的 `is_zero_undef` 和 `llvm.vp.abs` 的 `is_int_min_poison` 必须是编译期 i1 常量；这些 flag 只收窄 LLVM 定义域，VM handler 复用同一套逐 lane 计算。translator 只对激活 lane 执行 lowering，并分别通过 `llvm.vp.vector.ctpop.integer`、`llvm.vp.vector.ctlz.integer`、`llvm.vp.vector.cttz.integer`、`llvm.vp.vector.abs.integer`、`llvm.vp.vector.bswap.integer` 和 `llvm.vp.vector.bitreverse.integer` rule 发射现有整数一元 handler；未激活 lane 保持 poison/未绑定状态。动态 mask、动态 EVL、动态 flag、scalable vector、非整数 lane、lane 数或 lane 宽不匹配、q ABI 场景必须安全跳过。
- 固定向量 VP select/merge intrinsic：`llvm.vp.select` 当前支持 fixed vector、常量 i32 EVL 和 fixed `<N x i1>` condition，then/else/result lane 必须是 x 寄存器可承载且同类型同宽的整数、指针、`half`、`float` 或 `double`。translator 只对 `lane < EVL` 的激活 lane 执行 lowering，并通过 `llvm.vp.select.vector_condition` rule 发射现有 `br_if` / `mov` / `br` handler；未激活 lane 保持 poison/未绑定状态。`llvm.vp.merge` 支持 fixed vector、fixed `<N x i1>` condition 和 i32 pivot，pivot 可以是运行时值；translator 对每个 lane 先通过 `mov_imm` / `icmp ult` 计算 `lane < pivot`，再通过 `llvm.vp.merge.vector_condition` rule 发射两级 `br_if`，只有 `cond && lane < pivot` 时复制 then lane，其它 lane 都复制 else lane。动态 EVL、select condition lane 不是 i1、merge pivot 不是整数、then/else/result lane 数或类型不匹配、scalable vector、q ABI 场景必须安全跳过。
  固定向量 `llvm.experimental.vp.reverse` 和 `llvm.experimental.vp.splat` 当前支持 fixed vector、常量 fixed `<N x i1>` mask 和常量 i32 EVL；`reverse` 的 source/result lane 数、lane 类型和 lane 位宽必须一致，`splat` 的标量 operand 必须与 result lane 类型和位宽一致。translator 只对 `mask[lane] && lane < EVL` 的激活 lane 通过 `llvm.experimental.vp.reverse.element` / `llvm.experimental.vp.splat.element` rule 发射现有 `mov` handler；未激活 lane 保持 poison/未绑定状态。动态 mask、scalar i1 mask、动态 EVL、lane 类型不匹配、scalable vector、q ABI 场景必须安全跳过。
- 固定向量 VP 指针转换 intrinsic：`llvm.vp.ptrtoint` 和 `llvm.vp.inttoptr` 当前支持 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL，以及逐 lane 的 64 位 pointer bit 与整数 bit 之间转换。translator 只对 `mask[lane] == true && lane < EVL` 的激活 lane 执行 lowering，并通过 `llvm.vp.vector.cast.pointer` rule 发射现有 `zext` / `trunc` / `bitcast` handler；未激活 lane 保持 poison/未绑定状态。动态 mask、动态 EVL、lane 数不匹配、非 pointer/int 合法转换、scalable vector、q ABI 场景必须安全跳过。LLVM 21 当前没有对应的 `llvm.vp.addrspacecast` intrinsic；普通 fixed vector `addrspacecast` 仍走 `llvm.vector.cast.pointer`。
- 固定向量 VP 整数/指针比较 intrinsic：`llvm.vp.icmp` 当前支持 fixed integer vector 和 fixed pointer vector、metadata 字符串谓词、常量 `<N x i1>` mask、常量 i32 EVL。translator 只对激活 lane 执行 lowering，并通过 `llvm.vp.vector.icmp.integer` / `llvm.vp.vector.icmp.pointer` rule 发射现有 `icmp` handler；结果 lane 必须是 `i1`，两个源 operand 的 lane 数、lane 类型与 lane bit width 必须一致，未激活 lane 保持 poison/未绑定状态。
- 固定向量 VP 浮点二元 intrinsic：`llvm.vp.fadd`、`llvm.vp.fsub`、`llvm.vp.fmul`、`llvm.vp.fdiv`、`llvm.vp.frem`、`llvm.vp.minnum`、`llvm.vp.maxnum`、`llvm.vp.minimum`、`llvm.vp.maximum` 和 `llvm.vp.copysign` 当前支持 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL，以及 `half` / `float` / `double` lane。translator 只对激活 lane 执行 lowering，并通过 `llvm.vp.vector.fadd.float` / `llvm.vp.vector.fsub.float` / `llvm.vp.vector.fmul.float` / `llvm.vp.vector.fdiv.float` / `llvm.vp.vector.frem.float` / `llvm.vp.vector.minnum.float` / `llvm.vp.vector.maxnum.float` / `llvm.vp.vector.minimum.float` / `llvm.vp.vector.maximum.float` / `llvm.vp.vector.copysign.float` rule 发射现有浮点 ALU handler；未激活 lane 保持 poison/未绑定状态。动态 mask、动态 EVL、lane 数或 lane 类型不匹配、非 `half` / `float` / `double` lane、scalable vector、q ABI 场景必须安全跳过。
- 固定向量 VP 浮点一元 intrinsic：`llvm.vp.fneg`、`llvm.vp.fabs`、`llvm.vp.sqrt`、`llvm.vp.canonicalize`、`llvm.vp.floor`、`llvm.vp.ceil`、`llvm.vp.roundtozero`、`llvm.vp.rint`、`llvm.vp.nearbyint`、`llvm.vp.round`、`llvm.vp.roundeven`、`llvm.vp.sin`、`llvm.vp.cos`、`llvm.vp.exp`、`llvm.vp.exp2`、`llvm.vp.log`、`llvm.vp.log10` 和 `llvm.vp.log2` 当前支持 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL，以及 `half` / `float` / `double` lane。translator 只对激活 lane 执行 lowering，并通过 `llvm.vp.vector.{fneg,fabs,sqrt,canonicalize,floor,ceil,roundtozero,rint,nearbyint,round,roundeven,sin,cos,exp,exp2,log,log10,log2}.float` rule 发射现有浮点一元 handler；其中 `roundtozero` 复用 `ftrunc` 语义。未激活 lane 保持 poison/未绑定状态。LLVM 21 中 `llvm.vp.trunc` 是整数向量截断 intrinsic，不归入这里的浮点 unary 子集；动态 mask、动态 EVL、lane 数或 lane 类型不匹配、非 `half` / `float` / `double` lane、scalable vector、q ABI 场景必须安全跳过。
- 固定向量 VP 浮点到整数取整 intrinsic：`llvm.vp.lrint` 和 `llvm.vp.llrint` 当前支持 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL，source lane 为 `half` / `float` / `double`，result lane 为 i32/i64。translator 只对激活 lane 执行 lowering，并通过 `llvm.vp.vector.lrint.float` / `llvm.vp.vector.llrint.float` rule 发射现有 `flrint` / `fllrint` handler；未激活 lane 保持 poison/未绑定状态。`llvm.vp.lround` / `llvm.vp.llround` 在 LLVM 21 当前没有对应 intrinsic；动态 mask、动态 EVL、lane 数不一致、source lane 不是 `half` / `float` / `double`、result lane 不是 i32/i64、scalable vector、q ABI 场景必须安全跳过。
- 固定向量 VP 浮点三元 intrinsic：`llvm.vp.fma` 和 `llvm.vp.fmuladd` 当前支持 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL，以及 `half` / `float` / `double` lane。translator 只对激活 lane 执行 lowering，并通过 `llvm.vp.vector.fma.float` / `llvm.vp.vector.fmuladd.float` rule 发射现有 `ffma` / `ffmuladd` handler；三个源 operand 和结果的 lane 数、lane 类型与 lane bit width 必须一致，未激活 lane 保持 poison/未绑定状态。
- 固定向量 VP 浮点比较 intrinsic：`llvm.vp.fcmp` 当前支持 fixed vector、metadata 字符串谓词、常量 `<N x i1>` mask、常量 i32 EVL，以及 `half` / `float` / `double` lane。translator 只对激活 lane 执行 lowering，并通过 `llvm.vp.vector.fcmp.float` rule 发射现有 `fcmp` handler；结果 lane 必须是 `i1`，两个源 operand 的 lane 数、lane 类型与 lane bit width 必须一致，未激活 lane 保持 poison/未绑定状态。
- 固定向量 VP 浮点分类 intrinsic：`llvm.vp.is.fpclass` 当前支持 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL，以及 `half` / `float` / `double` lane。translator 只对激活 lane 执行 lowering，并通过 `llvm.vp.vector.is.fpclass.float` rule 发射现有 `fpclass` handler；结果 lane 必须是 `i1`，source 和 result 的 lane 数必须一致，class mask 必须是 LLVM `FPClassTest` 当前 `0x03ff` 范围内的 immarg，未激活 lane 保持 poison/未绑定状态。
  本条中的“固定向量白名单”还包括后文列出的 fixed vector intrinsic 补充项：`llvm.vector.interleave2..8` / `llvm.vector.deinterleave2..8`，整数 unary/binary/overflow-pair/ternary/value-hint/reduction，浮点 `copysign/minnum/maxnum/minimum/maximum`、`pow/powi`、`is.fpclass`、`fma/fmuladd`、`sin/cos/exp/exp2/log/log10/log2`、`lrint/llrint/lround/llround`、`fptosi.sat/fptoui.sat` 和浮点 reduction；这些补充项与本条共享同一套 x 寄存器 lane 约束。
  固定向量 `phi` 的 incoming 可以是已由受支持 lowering 写入的固定向量绑定，也可以是无 `undef` / `poison` lane 的 LLVM 常量固定向量；含 `undef` / `poison` lane 的常量向量必须先经过受支持的 `freeze`。
  固定指针向量 `icmp` 也属于当前固定向量白名单；profile 必须通过 `llvm.vector.icmp.pointer` rule 逐 lane 发射 `icmp` handler，左右 operand lane 必须都是 64 位 pointer bit，结果 lane 必须是 `i1`，并且源 pointer vector 必须由受支持的 `insertelement` / `phi` / `select` / `shufflevector` / fixed vector memory / masked gather 等 lowering 构造。
  固定整数向量 unary intrinsic `llvm.ctpop/ctlz/cttz/abs/bswap/bitreverse` 也属于当前固定向量白名单；profile 必须分别通过 `llvm.vector.{ctpop,ctlz,cttz,abs,bswap,bitreverse}.integer` rule 逐 lane 发射 `ctpop` / `ctlz` / `cttz` / `iabs` / `bswap` / `bitreverse` handler，其中 `bswap` 只允许 i16/i32/i64 lane，其余只允许 i1/i8/i16/i32/i64 lane。
  固定整数向量 binary intrinsic `llvm.smax/smin/umax/umin/uadd.sat/usub.sat/sadd.sat/ssub.sat/ushl.sat/sshl.sat` 也属于当前固定向量白名单；profile 必须分别通过 `llvm.vector.{smax,smin,umax,umin,uadd.sat,usub.sat,sadd.sat,ssub.sat,ushl.sat,sshl.sat}.integer` rule 逐 lane 发射 `ismax` / `ismin` / `iumax` / `iumin` / `iuadd_sat` / `iusub_sat` / `isadd_sat` / `issub_sat` / `iushl_sat` / `isshl_sat` handler，只允许 lane 数相同且 lane 宽度为 i1/i8/i16/i32/i64 的固定整数向量。
  固定整数向量 overflow intrinsic `llvm.uadd.with.overflow/sadd.with.overflow/usub.with.overflow/ssub.with.overflow/umul.with.overflow/smul.with.overflow` 也属于当前固定向量白名单；LLVM 返回类型必须是 `{ <N x iW>, <N x i1> }`，两个输入和第一个返回字段必须是 lane 数相同且 lane 宽度为 i1/i8/i16/i32/i64 的固定整数向量，第二个返回字段必须是同 lane 数的固定 `i1` 向量。profile 必须分别通过 `llvm.vector.{uadd,sadd,usub,ssub,umul,smul}.with.overflow.integer` rule 逐 lane 发射 `iuadd_overflow` / `isadd_overflow` / `iusub_overflow` / `issub_overflow` / `iumul_overflow` / `ismul_overflow` handler；translator 会把每个 lane 的结果值和溢出标志重新绑定为 LLVM 的两个聚合字段。
  固定整数向量 ternary intrinsic `llvm.fshl/fshr` 也属于当前固定向量白名单；profile 必须分别通过 `llvm.vector.{fshl,fshr}.integer` rule 逐 lane 发射 `fshl` / `fshr` handler，只允许三个源和结果 lane 数相同且 lane 宽度为 i1/i8/i16/i32/i64 的固定整数向量。
  固定向量 VP 整数 ternary intrinsic `llvm.vp.fshl/fshr` 也属于当前固定向量白名单；profile 必须分别通过 `llvm.vp.vector.{fshl,fshr}.integer` rule 只对 `mask[lane] == true && lane < EVL` 的激活 lane 发射 `fshl` / `fshr` handler，未激活 lane 保持 poison/未绑定状态。该子集只允许三个源和结果 lane 数相同、lane 宽度为 i1/i8/i16/i32/i64、mask 为常量 `<N x i1>` 且 EVL 为编译期整数常量；动态 mask、动态 EVL、scalable vector、q ABI 或读取未绑定 inactive lane 时必须安全跳过。
  固定整数向量值保持 intrinsic `llvm.expect/llvm.expect.with.probability` 也属于当前固定向量白名单；profile 必须分别通过 `llvm.vector.expect.integer` / `llvm.vector.expect.with_probability.integer` rule 逐 lane 发射 `mov`，只保留第一个 value operand，`expected` 和 `probability` 参数不进入 VM 状态。
  固定整数向量 reduction intrinsic `llvm.vector.reduce.add/mul/and/or/xor/smax/smin/umax/umin` 也属于当前固定向量白名单；profile 必须分别通过 `llvm.vector.reduce.{add,mul,and,or,xor,smax,smin,umax,umin}.integer` rule 把已绑定 fixed vector lane 按源码顺序折叠为标量结果，每一步复用 `iadd` / `imul` / `iand` / `ior` / `ixor` / `ismax` / `ismin` / `iumax` / `iumin` handler，只允许 lane 宽度和标量返回宽度相同且宽度为 i1/i8/i16/i32/i64 的固定整数向量。
  固定浮点向量 reduction intrinsic `llvm.vector.reduce.fadd/fmul/fmin/fmax/fminimum/fmaximum` 也属于当前固定向量白名单；profile 必须分别通过 `llvm.vector.reduce.{fadd,fmul,fmin,fmax,fminimum,fmaximum}.float` rule 把已绑定 fixed vector lane 按源码顺序折叠为标量结果，`fadd/fmul` 从 LLVM intrinsic 的标量 accumulator 开始，`fmin/fmax/fminimum/fmaximum` 从第一个 lane 开始，每一步复用 `fadd` / `fmul` / `fminnum` / `fmaxnum` / `fminimum` / `fmaximum` handler，只允许 accumulator、lane 和标量返回宽度相同且类型为 `half` / `float` / `double` 的固定浮点向量。
  固定向量 VP reduction intrinsic `llvm.vp.reduce.add/mul/and/or/xor/smax/smin/umax/umin` 和 `llvm.vp.reduce.fadd/fmul/fmin/fmax/fminimum/fmaximum` 当前支持 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL 以及标量 start value。translator 从 start value 开始，只按源码 lane 顺序折叠 `mask[lane] == true && lane < EVL` 的激活 lane；所有 lane inactive 时结果保持 start value，不发射 reduction handler。profile 必须分别通过 `llvm.vp.reduce.{add,mul,and,or,xor,smax,smin,umax,umin}.integer` 与 `llvm.vp.reduce.{fadd,fmul,fmin,fmax,fminimum,fmaximum}.float` rule 发射现有标量整数/浮点 handler；动态 mask、动态 EVL、scalable vector、start/result/lane 宽度不一致、激活 lane 未由受支持 lowering 写入或 q ABI 场景必须安全跳过。
  固定向量 `llvm.experimental.vector.extract.last.active` 当前支持 fixed vector、常量 `<N x i1>` mask 和 x 寄存器可承载的 passthru 标量。translator 选择最高编号 active lane；如果 mask 全 false，则选择 passthru。profile 必须通过 `llvm.experimental.vector.extract.last.active` rule 发射 `mov` handler，把被选中的 lane 或 passthru 复制为标量结果。动态 mask、scalable vector、result/lane/passthru 类型或宽度不一致、选中 lane 未由受支持 lowering 写入、passthru 是未冻结的 undef/poison 或 q ABI 场景必须安全跳过。
- 固定浮点向量数学 unary intrinsic 补充：`llvm.sin`、`llvm.cos`、`llvm.exp`、`llvm.exp2`、`llvm.log`、`llvm.log10` 和 `llvm.log2` 的 fixed vector 形式也属于当前内部临时值白名单，必须通过 `llvm.vector.{sin,cos,exp,exp2,log,log10,log2}.float` lowering rule 逐 lane 发射 `fsin` / `fcos` / `fexp` / `fexp2` / `flog` / `flog10` / `flog2` handler；operand 和结果的 lane 数必须相同，每个 lane 都必须是 `half` / `float` / `double`。
- 固定浮点向量 binary intrinsic 补充：`llvm.copysign`、`llvm.minnum`、`llvm.maxnum`、`llvm.minimum` 和 `llvm.maximum` 也属于当前 fixed vector 内部临时值白名单，必须通过 `llvm.vector.{copysign,minnum,maxnum,minimum,maximum}.float` lowering rule 逐 lane 发射 `fcopysign` / `fminnum` / `fmaxnum` / `fminimum` / `fmaximum` handler；两个 operand 和结果的 lane 数必须相同，每个 lane 都必须是 `half` / `float` / `double`。
- 固定浮点向量 pow intrinsic 补充：`llvm.pow` 的 fixed vector 形式也属于当前内部临时值白名单，必须通过 `llvm.vector.pow.float` lowering rule 逐 lane 发射 `fpow` handler；两个 operand 和结果的 lane 数必须相同，每个 lane 都必须是 `half` / `float` / `double`。`llvm.powi` 的 fixed vector 形式必须通过 `llvm.vector.powi.float` lowering rule 逐 lane 发射 `fpowi` handler；第一个 operand 和结果的 lane 数必须相同，每个 lane 都必须是 `half` / `float` / `double`，第二个指数 operand 必须是所有 lane 共享的标量 i32。
- 固定浮点向量分类 intrinsic 补充：`llvm.is.fpclass` 的 fixed vector 形式也属于当前内部临时值白名单，必须通过 `llvm.vector.is.fpclass.float` lowering rule 逐 lane 发射 `fpclass` handler；源 operand 和结果的 lane 数必须相同，源 lane 必须是 `half` / `float` / `double`，结果 lane 必须是 `i1`，mask 必须是 LLVM `FPClassTest` 当前 `0x03ff` 范围内的 immarg。
  固定浮点向量 VP 分类 intrinsic 补充：`llvm.vp.is.fpclass` 的 fixed vector + 常量 VP mask/EVL 子集也属于当前内部临时值白名单，必须通过 `llvm.vp.vector.is.fpclass.float` lowering rule 对 `mask[lane] && lane < EVL` 的激活 lane 发射 `fpclass` handler；source/result lane 数必须相同，source lane 必须是 `half` / `float` / `double`，result lane 必须是 `i1`，class mask 必须是 LLVM `FPClassTest` 当前 `0x03ff` 范围内的 immarg。动态 VP mask、scalar i1 VP mask、动态 EVL、scalable vector、q ABI 或读取未冻结 inactive lane 时必须安全跳过。
- 固定浮点向量 ternary intrinsic 补充：`llvm.fma` 和 `llvm.fmuladd` 的 fixed vector 形式也属于当前内部临时值白名单，必须通过 `llvm.vector.fma.float` / `llvm.vector.fmuladd.float` lowering rule 逐 lane 发射 `ffma` / `ffmuladd` handler；三个 operand 和结果的 lane 数必须相同，每个 lane 都必须是 `half` / `float` / `double`。
- 计数器、目标运行时查询、栈状态、指令缓存和伪探针 intrinsic：`llvm.readcyclecounter` 和 `llvm.readsteadycounter` 当前支持无参数 `i64` 返回形式，必须由 profile 中的 `llvm.readcyclecounter.integer` / `llvm.readsteadycounter.integer` lowering rule 发射到 `read_cycle` / `read_steady`，再由 `read_counter(cycle|steady)` semantic 驱动 runtime 调用对应 LLVM intrinsic；`llvm.vscale.*` 当前支持无参数整数标量返回形式，必须由 profile 中的 `llvm.vscale.integer` lowering rule 发射到 `read_vscale`，再由 `read_vscale()` semantic 驱动 runtime 调用 LLVM vscale intrinsic 并按 LLVM 返回位宽截断，最终是否可 codegen 仍取决于目标后端是否支持该 intrinsic；`llvm.get.rounding` 当前支持无参数 `i32` 返回形式，`llvm.flt.rounds` 在 LLVM 21 IR 解析后会规范化为同一类 rounding 查询，`llvm.set.rounding` 当前支持单个 `i32` 参数和 void 返回形式，必须分别由 profile 中的 `llvm.get.rounding.integer` / `llvm.set.rounding.integer` lowering rule 发射到 `read_rounding` / `write_rounding`，再由 `read_rounding()` / `write_rounding(...)` semantic 驱动 runtime 调用对应 LLVM rounding intrinsic；如果前端保留 `llvm.flt.rounds` callee 名称，profile 也可以用 `llvm.flt.rounds.integer` 发射到 `read_flt_rounds` 并由 `read_flt_rounds()` semantic 调用 LLVM `flt.rounds` intrinsic；`llvm.get.fpenv.*` / `llvm.set.fpenv.*` 和 `llvm.get.fpmode.*` / `llvm.set.fpmode.*` 当前支持 i32/i64 状态宽度，`llvm.reset.fpenv` / `llvm.reset.fpmode` 当前支持无参数 void 返回形式，必须由 profile 中的对应 lowering rule 发射到 `read_fpenv` / `write_fpenv` / `reset_fpenv` / `read_fpmode` / `write_fpmode` / `reset_fpmode`，再由 `read_fpenv()`、`write_fpenv(...)`、`reset_fpenv()`、`read_fpmode()`、`write_fpmode(...)` 和 `reset_fpmode()` semantic 驱动 runtime 调用 LLVM FP 状态 intrinsic；`llvm.stacksave` / `llvm.stackrestore` 当前支持无参数 pointer 返回和单 pointer 参数 void 返回形式，必须由 profile 中的 `llvm.stacksave.pointer` / `llvm.stackrestore` lowering rule 发射到 `stacksave` / `stackrestore`，再由 `stack_save()` / `stack_restore(...)` semantic 驱动 runtime 调用对应 LLVM intrinsic；`llvm.clear_cache` 当前支持两个 pointer 参数 void 返回形式，必须由 profile 中的 `llvm.clear_cache` lowering rule 发射到 `clear_cache`，再由 `clear_cache(...)` semantic 驱动 runtime 调用 LLVM `clear_cache` intrinsic；`llvm.pseudoprobe` 当前支持四个编译期整数参数和 void 返回形式，必须由 profile 中的 `llvm.pseudoprobe` lowering rule 发射到 `pseudoprobe`，再由 `pseudo_probe(...)` semantic 驱动 runtime 调用 LLVM `pseudoprobe` intrinsic；`llvm.prefetch` 当前支持一个 pointer 参数加三个编译期整数 immarg 参数和 void 返回形式，必须由 profile 中的 `llvm.prefetch` lowering rule 发射到 `prefetch`，再由 `prefetch(...)` semantic 驱动 runtime 调用 LLVM `prefetch` intrinsic。
- 栈内省 intrinsic：`llvm.returnaddress`、`llvm.frameaddress.*`、`llvm.addressofreturnaddress.*`、`llvm.localaddress` 和 `llvm.sponentry.*` 当前必须安全跳过。它们的语义绑定到“原函数调用栈/帧”的身份；如果在 VM dispatcher handler 内直接调用对应 LLVM intrinsic，会读到解释器自身的栈帧或返回点，生成错误语义。后续只有在 VM ABI 显式携带原函数栈内省状态时才能转为支持。
- 目标寄存器、GC 栈和栈图 intrinsic：`llvm.read_register.*`、`llvm.write_register.*`、`llvm.gcroot`、`llvm.gcread`、`llvm.gcwrite`、`llvm.experimental.stackmap`、`llvm.experimental.patchpoint.*` 和 `llvm.experimental.gc.statepoint.*` 当前必须安全跳过。它们依赖后端目标寄存器、GC root map、stackmap/patchpoint 位置或 statepoint relocation 信息；当前 VM native bridge 只保留普通函数调用 ABI，不能证明这些 call-site 状态搬到 dispatcher 后仍等价。
- 目标架构专用 intrinsic：`llvm.x86.*`、`llvm.aarch64.*`、`llvm.arm.*`、`llvm.amdgcn.*`、`llvm.r600.*`、`llvm.ppc.*`、`llvm.riscv.*`、`llvm.bpf.*`、`llvm.nvvm.*`、`llvm.spv.*`、`llvm.dx.*`、`llvm.wasm.*`、`llvm.mips.*`、`llvm.loongarch.*`、`llvm.s390.*`、`llvm.hexagon.*`、`llvm.ve.*`、`llvm.xcore.*` 和 `llvm.spu.*` 当前必须安全跳过。它们的语义通常绑定目标 ISA、向量寄存器、状态寄存器、地址空间或后端 builtin lowering；当前 profile 描述的是目标无关的 VM IR，不能把这类 intrinsic 自动降成普通 `x` / `q` 寄存器操作。
- 内存、陷阱和无运行时语义 intrinsic：固定大小和运行时元素个数 `alloca`、`load`、`store`、可由 data layout 证明为逐 lane 字节连续布局的普通和 volatile fixed vector `load` / `store`、常量 `<N x i1>` mask 的非 volatile fixed vector `llvm.masked.load` / `llvm.masked.store` / `llvm.masked.expandload` / `llvm.masked.compressstore`、常量 `<N x i1>` mask 且 fixed pointer vector 的非 volatile `llvm.masked.gather` / `llvm.masked.scatter`、常量 `<N x i1>` mask 和常量 EVL 的 fixed vector `llvm.vp.load` / `llvm.vp.store` / `llvm.experimental.vp.strided.load` / `llvm.experimental.vp.strided.store`，以及 fixed pointer vector 的 `llvm.vp.gather` / `llvm.vp.scatter`、默认 system 或 `syncscope("singlethread")` 的自然对齐整数/指针/`half`/`float`/`double` 标量 atomic `load`/`store` 和 volatile atomic `load`/`store`、默认 system 或 `syncscope("singlethread")` 的自然对齐整数标量 `atomicrmw xchg/add/sub/and/or/xor/nand/min/max/umin/umax/uinc_wrap/udec_wrap/usub_cond/usub_sat` 及其 volatile 形式、默认 system 或 `syncscope("singlethread")` 的自然对齐指针标量 `atomicrmw xchg`、默认 system 或 `syncscope("singlethread")` 的自然对齐 `half`/`float`/`double` 标量 `atomicrmw fadd/fsub/fmax/fmin/fmaximum/fminimum` 及其 volatile 形式、默认 system 或 `syncscope("singlethread")` 的自然对齐整数/指针标量 `cmpxchg`（weak 按 strong 形式发射）、默认 system 或 `syncscope("singlethread")` 的 acquire/release/acq_rel/seq_cst `fence`、`getelementptr`、`llvm.stacksave` / `llvm.stackrestore` / `llvm.clear_cache` / `llvm.pseudoprobe` / `llvm.prefetch`、固定小长度内联展开的非 volatile `llvm.memcpy` / `llvm.memmove` / `llvm.memset` intrinsic、固定大长度或动态长度的非 volatile `llvm.memcpy` / `llvm.memmove` / `llvm.memset` intrinsic、常量长度和动态长度的 volatile `llvm.memcpy` / `llvm.memmove` / `llvm.memset` intrinsic、`llvm.trap` / `llvm.debugtrap` / `llvm.ubsantrap`、`llvm.sideeffect`，以及 `llvm.invariant.end`、`llvm.experimental.noalias.scope.decl`、`llvm.donothing`、`llvm.fake.use`、`llvm.lifetime.start` / `llvm.lifetime.end`、`llvm.assume`、`llvm.dbg.*`、`llvm.var.annotation`、`llvm.codeview.annotation` 这类不改变运行时状态的 intrinsic。
- 控制流：`br`、条件 `br`、`switch`、`ret`。
- 调用：direct call 通过 `native_call` 规则重新生成 LLVM call；indirect call 通过 AMICE 生成的 adapter thunk 把运行时 callee 指针作为第 0 个 native 参数传入，再由同一套 `call_native` VM handler 调用；被调函数是否虚拟化由函数选择器单独决定，call lowering 不隐式递归虚拟化被调函数。direct/indirect native call 当前接受整数、指针、`half` / `float` / `double` 标量、直接 struct / 固定数组小聚合，以及 lane 可由 x 寄存器承载的固定向量参数/返回；聚合按 leaf 字段展开，固定向量按 lane 展开，展开后的槽位必须能放入 `native_call` ABI 的 `args` / `returns` 列表。超过 native call ABI 槽位、scalable vector、q ABI 宽向量、direct varargs call 中的 vector 变参，或 call-site ABI 属性无法复制到 native thunk / indirect adapter 的情况必须安全跳过。

`phi` 不得作为普通指令进入 VM lowering。translator 在 predecessor edge 上使用 `llvm.phi.edge_move` 的 profile `emit mov` 形态生成标量 VM move，并把 result 绑定到 phi 的目标 VM 寄存器；直接 struct / 固定数组小聚合 phi 使用 `llvm.aggregate.phi.edge_move` 按叶子字段在前驱边逐个 `mov` 到预分配的 aggregate result 字段寄存器；固定向量 phi 使用 `llvm.vector.phi.edge_move` 按 lane 在前驱边逐个 `mov` 到预分配的固定向量结果 lane 寄存器。aggregate / fixed vector phi incoming 可以来自已支持 lowering 产生的绑定，也可以是无 `undef` / `poison` 的 LLVM 常量 struct / 固定数组 / 固定向量；常量字段或 lane 含 `undef` / `poison` 时必须先经过受支持的 `freeze`。

`select`、`switch`、动态 GEP、aggregate parameter、aggregate return、`sret`、direct/indirect native call、direct varargs native call 和 multi-block phi 需要 host context 才能计算 label、field、native call id、indirect adapter、call-site 变参类型或 ABI 参数/返回槽；这些路径的控制结构由 Rust translator 保守生成，但每条实际 VM instruction 仍从对应 lowering rule 中按 operand shape 选取具名 `emit`。同一 handler semantic 有多条 profile 指令时，普通 lowering 以 `emit` 指令名为准；host-context helper 只有在该 semantic 唯一时才允许按 semantic 选择，否则必须由 lowering rule 的具名 `emit` 消解。

### 超级指令

`isa.vm` 可以声明受限超级指令，`lowering.vm` 必须同时声明对应 `fusion` 模板。当前已实现的超级指令模板是 `iadd_xor`、`icmp_br_if`、`gep_load`、`load_iadd`、`load_imul`、`load_iudiv`、`load_isdiv`、`load_iurem`、`load_isrem`、`load_ishl`、`load_ilshr`、`load_iashr`、`load_ismax`、`load_ismin`、`load_iumax`、`load_iumin`、`load_iuadd_sat`、`load_iusub_sat`、`load_isadd_sat`、`load_issub_sat`、`load_iushl_sat`、`load_isshl_sat`、`load_iand`、`load_ior`、`load_isub` 和 `load_ixor`。超级指令仍然使用普通 `instr` 语法，profile 通过 semantic AST 声明组合语义；translator 只有在 profile 存在对应 `Super(...)` semantic 且 `lowering.vm` 声明了同名 target fusion 时才启用融合，否则保留普通 VM 指令序列。

`iadd_xor` 语义为先计算 `lhs + rhs`，再与 `xor_rhs` 做 `xor`，最后按 `width` 截断：

```text
instr iadd_xor(dst: vreg<i64>, lhs: vreg<i64>, rhs: vreg<i64>, xor_rhs: vreg<i64>, width: imm<u7>) { # 超级指令：整数加法后立即异或
  opcode alias [0x10b, 0x10c]               # 超级指令也支持 opcode alias 和 varint opcode
  decoded_width = 48                        # 超级指令可以声明自己的 decoded record 宽度
  semantic {                                # 语义由 verifier 解析成受限 AST
    reg[dst] = trunc_width(reg[lhs] + reg[rhs] xor reg[xor_rhs], width) # 组合 add 与 xor 两个基础语义
    pc = next                               # 执行后按当前 decoded_width 前进
  }                                         # 结束语义块
}                                           # 结束 iadd_xor
```

`icmp_br_if` 语义为先执行整数比较，再直接选择两个 bytecode 目标，不写中间 `cond` 寄存器：

```text
instr icmp_br_if(pred: imm<u7>, lhs: vreg<i64>, rhs: vreg<i64>, width: imm<u7>, then_pc: label, else_pc: label) { # 超级指令：整数比较后直接条件跳转
  opcode alias [0x10d, 0x10e]               # 超级分支同样支持 opcode alias
  decoded_width = 32                        # 两个 label PC 最坏各占 64 位 bitpacked operand
  semantic {                                # 语义由 verifier 解析成受限 AST
    pc = select(compare(pred, reg[lhs], reg[rhs], width), then_pc, else_pc) # 比较结果直接驱动 PC
  }                                         # 结束语义块
}                                           # 结束 icmp_br_if
```

`gep_load` 语义为先执行常量字节偏移 GEP，再直接从计算出的地址读取标量，不写中间指针寄存器：

```text
instr gep_load(dst: vreg<i64>, base: vreg<ptr>, offset: imm<u64>, width: imm<u7>) { # 超级指令：常量字节偏移 GEP 后立即读取标量
  opcode alias [0x10f, 0x110]               # 超级内存指令同样支持 varint opcode
  decoded_width = 32                        # offset 是 64 位 operand，需要较宽 record
  semantic {                                # 语义由 verifier 解析成受限 AST
    reg[dst] = load_width(reg[base] + offset, width) # 先计算地址，再按 width 读标量
    pc = next                               # 执行后按当前 decoded_width 前进
  }                                         # 结束语义块
}                                           # 结束 gep_load
```

`load_iadd` 语义为先从指针读取标量，再立即与寄存器加数做整数加法，不写中间 loaded 临时寄存器：

```text
instr load_iadd(dst: vreg<i64>, ptr: vreg<ptr>, addend: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行整数加法
  opcode alias [0x111, 0x112]              # 超级内存算术指令同样支持 varint opcode
  decoded_width = 32                       # 三个寄存器操作数和位宽操作数使用 32 字节 record
  semantic {                               # 语义由 verifier 解析成受限 AST
    reg[dst] = trunc_width(load_width(reg[ptr], width) + reg[addend], width) # 读内存后立即做整数加法
    pc = next                              # 执行后按当前 decoded_width 前进
  }                                        # 结束语义块
}                                          # 结束 load_iadd
```

`load_imul` 语义为先从指针读取标量，再立即与寄存器因子做整数乘法，不写中间 loaded 临时寄存器：

```text
instr load_imul(dst: vreg<i64>, ptr: vreg<ptr>, factor: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行整数乘法
  opcode alias [0x1d8, 0x1d9]              # load_imul 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、factor、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadMul)
    reg[dst] = trunc_width(load_width(reg[ptr], width) * reg[factor], width) # load 后做整数乘法
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_imul
```

`load_iudiv` 语义为先从指针读取标量，再立即除以寄存器无符号除数，不写中间 loaded 临时寄存器。因为无符号除法不可交换，fuse pass 只允许 `load tmp` 的结果位于 `iudiv` 左操作数：

```text
instr load_iudiv(dst: vreg<i64>, ptr: vreg<ptr>, divisor: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行无符号整数除法
  opcode alias [0x1da, 0x1db]              # load_iudiv 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、divisor、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadUDiv)
    reg[dst] = trunc_width(load_width(reg[ptr], width) /u reg[divisor], width) # load 后做无符号整数除法
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_iudiv
```

`load_isdiv`、`load_iurem` 和 `load_isrem` 分别对应有符号除法、无符号取余和有符号取余。它们和 `load_iudiv` 一样都是非交换融合，只允许 loaded 临时值作为左操作数：

```text
instr load_isdiv(dst: vreg<i64>, ptr: vreg<ptr>, divisor: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行有符号整数除法
  opcode alias [0x1dc, 0x1dd]              # load_isdiv 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、divisor、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadSDiv)
    reg[dst] = trunc_width(load_width(reg[ptr], width) /s reg[divisor], width) # load 后做有符号整数除法
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_isdiv

instr load_iurem(dst: vreg<i64>, ptr: vreg<ptr>, divisor: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行无符号整数取余
  opcode alias [0x1de, 0x1df]              # load_iurem 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、divisor、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadURem)
    reg[dst] = trunc_width(load_width(reg[ptr], width) %u reg[divisor], width) # load 后做无符号整数取余
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_iurem

instr load_isrem(dst: vreg<i64>, ptr: vreg<ptr>, divisor: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行有符号整数取余
  opcode alias [0x1e0, 0x1e1]              # load_isrem 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、divisor、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadSRem)
    reg[dst] = trunc_width(load_width(reg[ptr], width) %s reg[divisor], width) # load 后做有符号整数取余
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_isrem
```

`load_ishl`、`load_ilshr` 和 `load_iashr` 分别对应左移、逻辑右移和算术右移。它们也只允许 loaded 临时值作为被移位值；`load_iashr` 必须先按 `width` 做符号扩展，再执行算术右移：

```text
instr load_ishl(dst: vreg<i64>, ptr: vreg<ptr>, shift: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行整数左移
  opcode alias [0x1e2, 0x1e3]              # load_ishl 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、shift、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadShl)
    reg[dst] = trunc_width(load_width(reg[ptr], width) << reg[shift], width) # load 后做整数左移
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_ishl

instr load_ilshr(dst: vreg<i64>, ptr: vreg<ptr>, shift: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行整数逻辑右移
  opcode alias [0x1e4, 0x1e5]              # load_ilshr 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、shift、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadLShr)
    reg[dst] = trunc_width(load_width(reg[ptr], width) >>u reg[shift], width) # load 后做逻辑右移
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_ilshr

instr load_iashr(dst: vreg<i64>, ptr: vreg<ptr>, shift: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行整数算术右移
  opcode alias [0x1e6, 0x1e7]              # load_iashr 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、shift、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadAShr)
    reg[dst] = trunc_width(sign_extend(load_width(reg[ptr], width), width) >>s reg[shift], width) # load 后符号扩展再算术右移
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_iashr
```

`load_ismax`、`load_ismin`、`load_iumax` 和 `load_iumin` 分别对应有符号最大值、有符号最小值、无符号最大值和无符号最小值。它们是交换律融合，允许 loaded 临时值位于 min/max 的任一操作数；融合后的 profile semantic 固定把 loaded 值放在 `int_bin(...)` 左操作数、另一侧寄存器放在 `rhs`：

```text
instr load_ismax(dst: vreg<i64>, ptr: vreg<ptr>, rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行有符号最大值
  opcode alias [0x1e8, 0x1e9]              # load_ismax 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、rhs、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadSMax)
    reg[dst] = trunc_width(int_bin(smax, load_width(reg[ptr], width), reg[rhs]), width) # load 后按有符号整数选择较大值
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_ismax

instr load_ismin(dst: vreg<i64>, ptr: vreg<ptr>, rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行有符号最小值
  opcode alias [0x1ea, 0x1eb]              # load_ismin 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、rhs、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadSMin)
    reg[dst] = trunc_width(int_bin(smin, load_width(reg[ptr], width), reg[rhs]), width) # load 后按有符号整数选择较小值
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_ismin

instr load_iumax(dst: vreg<i64>, ptr: vreg<ptr>, rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行无符号最大值
  opcode alias [0x1ec, 0x1ed]              # load_iumax 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、rhs、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadUMax)
    reg[dst] = trunc_width(int_bin(umax, load_width(reg[ptr], width), reg[rhs]), width) # load 后按无符号整数选择较大值
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_iumax

instr load_iumin(dst: vreg<i64>, ptr: vreg<ptr>, rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行无符号最小值
  opcode alias [0x1ee, 0x1ef]              # load_iumin 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、rhs、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadUMin)
    reg[dst] = trunc_width(int_bin(umin, load_width(reg[ptr], width), reg[rhs]), width) # load 后按无符号整数选择较小值
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_iumin
```

`load_iuadd_sat`、`load_iusub_sat`、`load_isadd_sat`、`load_issub_sat`、`load_iushl_sat` 和 `load_isshl_sat` 分别对应无符号饱和加法、无符号饱和减法、有符号饱和加法、有符号饱和减法、无符号饱和左移和有符号饱和左移。`uadd_sat` / `sadd_sat` 是交换律融合，允许 loaded 临时值位于任一操作数；`usub_sat` / `ssub_sat` / `ushl_sat` / `sshl_sat` 只允许 loaded 临时值位于左操作数：

```text
instr load_iuadd_sat(dst: vreg<i64>, ptr: vreg<ptr>, rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行无符号饱和加法
  opcode alias [0x1f0, 0x1f1]              # load_iuadd_sat 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、rhs、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadUAddSat)
    reg[dst] = trunc_width(int_bin(uadd_sat, load_width(reg[ptr], width), reg[rhs]), width) # load 后执行无符号饱和加法
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_iuadd_sat

instr load_iusub_sat(dst: vreg<i64>, ptr: vreg<ptr>, rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行无符号饱和减法
  opcode alias [0x1f2, 0x1f3]              # load_iusub_sat 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、rhs、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadUSubSat)
    reg[dst] = trunc_width(int_bin(usub_sat, load_width(reg[ptr], width), reg[rhs]), width) # load 后执行无符号饱和减法
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_iusub_sat

instr load_isadd_sat(dst: vreg<i64>, ptr: vreg<ptr>, rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行有符号饱和加法
  opcode alias [0x1f4, 0x1f5]              # load_isadd_sat 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、rhs、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadSAddSat)
    reg[dst] = trunc_width(int_bin(sadd_sat, load_width(reg[ptr], width), reg[rhs]), width) # load 后执行有符号饱和加法
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_isadd_sat

instr load_issub_sat(dst: vreg<i64>, ptr: vreg<ptr>, rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行有符号饱和减法
  opcode alias [0x1f6, 0x1f7]              # load_issub_sat 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、rhs、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadSSubSat)
    reg[dst] = trunc_width(int_bin(ssub_sat, load_width(reg[ptr], width), reg[rhs]), width) # load 后执行有符号饱和减法
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_issub_sat

instr load_iushl_sat(dst: vreg<i64>, ptr: vreg<ptr>, rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行无符号饱和左移
  opcode alias [0x1f8, 0x1f9]              # load_iushl_sat 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、rhs、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadUShlSat)
    reg[dst] = trunc_width(int_bin(ushl_sat, load_width(reg[ptr], width), reg[rhs]), width) # load 后执行无符号饱和左移
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_iushl_sat

instr load_isshl_sat(dst: vreg<i64>, ptr: vreg<ptr>, rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行有符号饱和左移
  opcode alias [0x1fa, 0x1fb]              # load_isshl_sat 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、rhs、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadSShlSat)
    reg[dst] = trunc_width(int_bin(sshl_sat, load_width(reg[ptr], width), reg[rhs]), width) # load 后执行有符号饱和左移
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_isshl_sat
```

`load_iand` 语义为先从指针读取标量，再立即与寄存器操作数做整数与，不写中间 loaded 临时寄存器：

```text
instr load_iand(dst: vreg<i64>, ptr: vreg<ptr>, and_rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行整数与
  opcode alias [0x1d4, 0x1d5]              # load_iand 使用独立多字节 opcode
  decoded_width = 32                       # dst/ptr/and_rhs/width 使用 32 字节 decoded record
  semantic {                               # 处理语义由 AMICE 校验器静态识别
    reg[dst] = trunc_width(load_width(reg[ptr], width) and reg[and_rhs], width) # 先读取 ptr，再与 and_rhs 做整数与
    pc = next                              # 执行继续到下一条字节码指令
  }                                        # 结束语义块
}                                          # 结束 load_iand
```

`load_ior` 语义为先从指针读取标量，再立即与寄存器操作数做整数或，不写中间 loaded 临时寄存器：

```text
instr load_ior(dst: vreg<i64>, ptr: vreg<ptr>, or_rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行整数或
  opcode alias [0x1d6, 0x1d7]              # load_ior 使用独立多字节 opcode
  decoded_width = 32                       # dst、ptr、or_rhs、width 四个字段
  semantic {                               # 语义块必须被 profile parser 识别为 Super(LoadOr)
    reg[dst] = trunc_width(load_width(reg[ptr], width) or reg[or_rhs], width) # load 后做整数或
    pc = next                              # 正常顺序执行下一条 VM 指令
  }                                        # 结束语义块
}                                          # 结束 load_ior
```

`load_isub` 语义为先从指针读取标量，再立即减去寄存器操作数，不写中间 loaded 临时寄存器。因为整数减法不可交换，fuse pass 只允许 `load tmp` 的结果位于 `isub` 左操作数：

```text
instr load_isub(dst: vreg<i64>, ptr: vreg<ptr>, subtrahend: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行整数减法
  opcode alias [0x1d2, 0x1d3]              # load_isub 使用独立多字节 opcode
  decoded_width = 32                       # dst/ptr/subtrahend/width 使用 32 字节 decoded record
  semantic {                               # 处理语义由 AMICE 校验器静态识别
    reg[dst] = trunc_width(load_width(reg[ptr], width) - reg[subtrahend], width) # 先读取 ptr，再减去 subtrahend
    pc = next                              # 执行继续到下一条字节码指令
  }                                        # 结束语义块
}                                          # 结束 load_isub
```

`load_ixor` 语义为先从指针读取标量，再立即与寄存器操作数做整数异或，不写中间 loaded 临时寄存器：

```text
instr load_ixor(dst: vreg<i64>, ptr: vreg<ptr>, xor_rhs: vreg<i64>, width: imm<u7>) { # 超级指令：标量 load 后立即执行整数异或
  opcode alias [0x0fc, 0x0fd]              # 超级内存异或指令同样支持 varint opcode
  decoded_width = 32                       # dst/ptr/xor_rhs/width 放入 32 字节 decoded record
  semantic {                               # 处理语义由 AMICE verifier 静态识别
    reg[dst] = trunc_width(load_width(reg[ptr], width) xor reg[xor_rhs], width) # 读取内存后立即异或
    pc = next                              # 执行继续到下一条 bytecode 指令
  }                                        # 结束语义块
}                                          # 结束 load_ixor
```

VM IR fuse pass 对 `iadd_xor` 只融合线性相邻的 `iadd tmp, lhs, rhs` 与 `ixor dst, tmp, xor_rhs`，并且必须满足：两条指令之间没有 label target，`tmp` 没有除紧邻 `ixor` 之外的其它读取，两个操作位宽相同，没有 memory/native side effect。对 `icmp_br_if` 只融合线性相邻的 `icmp tmp, lhs, rhs` 与使用该 `tmp` 的 `br_if`，并且两条指令之间没有 label target、`tmp` 没有其它读取。对 `gep_load` 只融合线性相邻的 `gep tmp, base, offset` 与使用该 `tmp` 的 `load dst, tmp, width`，并且两条指令之间没有 label target、`tmp` 没有其它读取。对 `load_iadd` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 的 `iadd dst, tmp, addend, width` 或 `iadd dst, addend, tmp, width`，并且两条指令之间没有 label target、`tmp` 没有其它读取、load 位宽和 add 位宽一致。对 `load_imul` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 的 `imul dst, tmp, factor, width` 或 `imul dst, factor, tmp, width`，并且两条指令之间没有 label target、`tmp` 没有其它读取、load 位宽和 mul 位宽一致。对 `load_iudiv`、`load_isdiv`、`load_iurem` 和 `load_isrem` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 作为左操作数的 `iudiv` / `isdiv` / `iurem` / `isrem`，并且两条指令之间没有 label target、`tmp` 没有其它读取、load 位宽和 div/rem 位宽一致；`iudiv dst, lhs, tmp, width`、`isdiv dst, lhs, tmp, width`、`iurem dst, lhs, tmp, width` 和 `isrem dst, lhs, tmp, width` 不允许融合。对 `load_ishl`、`load_ilshr` 和 `load_iashr` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 作为左操作数的 `ishl` / `ilshr` / `iashr`，并且两条指令之间没有 label target、`tmp` 没有其它读取、load 位宽和 shift 结果位宽一致；`ishl dst, lhs, tmp, width`、`ilshr dst, lhs, tmp, width` 和 `iashr dst, lhs, tmp, width` 不允许融合。对 `load_ismax`、`load_ismin`、`load_iumax` 和 `load_iumin` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 的 `ismax` / `ismin` / `iumax` / `iumin`，允许 loaded 临时值位于左操作数或右操作数，并且两条指令之间没有 label target、`tmp` 没有其它读取、load 位宽和 min/max 结果位宽一致。对 `load_iuadd_sat` 和 `load_isadd_sat` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 的 `iuadd_sat` / `isadd_sat`，允许 loaded 临时值位于左操作数或右操作数；对 `load_iusub_sat`、`load_issub_sat`、`load_iushl_sat` 和 `load_isshl_sat` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 作为左操作数的 `iusub_sat` / `issub_sat` / `iushl_sat` / `isshl_sat`；这些饱和运算融合都要求两条指令之间没有 label target、`tmp` 没有其它读取、load 位宽和饱和运算结果位宽一致。对 `load_iand` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 的 `iand dst, tmp, and_rhs, width` 或 `iand dst, and_rhs, tmp, width`，并且两条指令之间没有 label target、`tmp` 没有其它读取、load 位宽和 and 位宽一致。对 `load_ior` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 的 `ior dst, tmp, or_rhs, width` 或 `ior dst, or_rhs, tmp, width`，并且两条指令之间没有 label target、`tmp` 没有其它读取、load 位宽和 or 位宽一致。对 `load_isub` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 作为左操作数的 `isub dst, tmp, subtrahend, width`，并且两条指令之间没有 label target、`tmp` 没有其它读取、load 位宽和 sub 位宽一致；`isub dst, lhs, tmp, width` 不允许融合。对 `load_ixor` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 的 `ixor dst, tmp, xor_rhs, width` 或 `ixor dst, xor_rhs, tmp, width`，并且两条指令之间没有 label target、`tmp` 没有其它读取、load 位宽和 xor 位宽一致。任何条件不满足时保留普通指令，不生成错误的超级指令。`call_native` 不需要单独的 ret-slot-copy 超级指令：它的 bytecode record 已携带 `ret0..ret7` 目标槽和宽度，translator 会把 native thunk 返回 tuple 直接写入最终 VM 返回寄存器，从而避免紧跟在调用后的额外 `mov`。

`lowering.vm` 的 fusion 声明使用中文注释、显式 target、源指令序列和保守条件，loader 会解析并由 verifier 校验：

```text
fusion super.gep_load { # 声明 gep 后紧跟 load 的超级内存读取融合模板
  target gep_load # 融合后发射 isa.vm 中的 gep_load 指令
  sequence gep, load # 只允许把连续的 gep 与 load 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # gep 产生的临时指针寄存器只能被紧邻的 load 使用
} # 结束 gep_load 融合模板

fusion super.load_iadd { # 声明 load 后紧跟 iadd 的超级内存算术融合模板
  target load_iadd # 融合后发射 isa.vm 中的 load_iadd 指令
  sequence load, iadd # 只允许把连续的 load 与 iadd 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 iadd 使用
  require same_width # load 位宽和 iadd 结果位宽必须一致
} # 结束 load_iadd 融合模板

fusion super.load_imul { # 声明 load 后紧跟 imul 的超级内存乘法融合模板
  target load_imul # 融合后发射 isa.vm 中的 load_imul 指令
  sequence load, imul # 只允许把连续的 load 与 imul 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 imul 使用
  require same_width # load 位宽和 imul 结果位宽必须一致
} # 结束 load_imul 融合模板

fusion super.load_iudiv { # 声明 load 后紧跟 iudiv 的超级内存无符号除法融合模板
  target load_iudiv # 融合后发射 isa.vm 中的 load_iudiv 指令
  sequence load, iudiv # 只允许把连续的 load 与 iudiv 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 iudiv 使用
  require same_width # load 位宽和 iudiv 结果位宽必须一致
} # 结束 load_iudiv 融合模板

fusion super.load_isdiv { # 声明 load 后紧跟 isdiv 的超级内存有符号除法融合模板
  target load_isdiv # 融合后发射 isa.vm 中的 load_isdiv 指令
  sequence load, isdiv # 只允许把连续的 load 与 isdiv 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 isdiv 使用
  require same_width # load 位宽和 isdiv 结果位宽必须一致
} # 结束 load_isdiv 融合模板

fusion super.load_iurem { # 声明 load 后紧跟 iurem 的超级内存无符号取余融合模板
  target load_iurem # 融合后发射 isa.vm 中的 load_iurem 指令
  sequence load, iurem # 只允许把连续的 load 与 iurem 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 iurem 使用
  require same_width # load 位宽和 iurem 结果位宽必须一致
} # 结束 load_iurem 融合模板

fusion super.load_isrem { # 声明 load 后紧跟 isrem 的超级内存有符号取余融合模板
  target load_isrem # 融合后发射 isa.vm 中的 load_isrem 指令
  sequence load, isrem # 只允许把连续的 load 与 isrem 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 isrem 使用
  require same_width # load 位宽和 isrem 结果位宽必须一致
} # 结束 load_isrem 融合模板

fusion super.load_ishl { # 声明 load 后紧跟 ishl 的超级内存左移融合模板
  target load_ishl # 融合后发射 isa.vm 中的 load_ishl 指令
  sequence load, ishl # 只允许把连续的 load 与 ishl 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 ishl 使用
  require same_width # load 位宽和 ishl 结果位宽必须一致
} # 结束 load_ishl 融合模板

fusion super.load_ilshr { # 声明 load 后紧跟 ilshr 的超级内存逻辑右移融合模板
  target load_ilshr # 融合后发射 isa.vm 中的 load_ilshr 指令
  sequence load, ilshr # 只允许把连续的 load 与 ilshr 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 ilshr 使用
  require same_width # load 位宽和 ilshr 结果位宽必须一致
} # 结束 load_ilshr 融合模板

fusion super.load_iashr { # 声明 load 后紧跟 iashr 的超级内存算术右移融合模板
  target load_iashr # 融合后发射 isa.vm 中的 load_iashr 指令
  sequence load, iashr # 只允许把连续的 load 与 iashr 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 iashr 使用
  require same_width # load 位宽和 iashr 结果位宽必须一致
} # 结束 load_iashr 融合模板

fusion super.load_ismax { # 声明 load 后紧跟 ismax 的超级内存有符号最大值融合模板
  target load_ismax # 融合后发射 isa.vm 中的 load_ismax 指令
  sequence load, ismax # 只允许把连续的 load 与 ismax 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 ismax 使用
  require same_width # load 位宽和 ismax 结果位宽必须一致
} # 结束 load_ismax 融合模板

fusion super.load_ismin { # 声明 load 后紧跟 ismin 的超级内存有符号最小值融合模板
  target load_ismin # 融合后发射 isa.vm 中的 load_ismin 指令
  sequence load, ismin # 只允许把连续的 load 与 ismin 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 ismin 使用
  require same_width # load 位宽和 ismin 结果位宽必须一致
} # 结束 load_ismin 融合模板

fusion super.load_iumax { # 声明 load 后紧跟 iumax 的超级内存无符号最大值融合模板
  target load_iumax # 融合后发射 isa.vm 中的 load_iumax 指令
  sequence load, iumax # 只允许把连续的 load 与 iumax 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 iumax 使用
  require same_width # load 位宽和 iumax 结果位宽必须一致
} # 结束 load_iumax 融合模板

fusion super.load_iumin { # 声明 load 后紧跟 iumin 的超级内存无符号最小值融合模板
  target load_iumin # 融合后发射 isa.vm 中的 load_iumin 指令
  sequence load, iumin # 只允许把连续的 load 与 iumin 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 iumin 使用
  require same_width # load 位宽和 iumin 结果位宽必须一致
} # 结束 load_iumin 融合模板

fusion super.load_iuadd_sat { # 声明 load 后紧跟 iuadd_sat 的超级内存无符号饱和加法融合模板
  target load_iuadd_sat # 融合后发射 isa.vm 中的 load_iuadd_sat 指令
  sequence load, iuadd_sat # 只允许把连续的 load 与 iuadd_sat 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 iuadd_sat 使用
  require same_width # load 位宽和 iuadd_sat 结果位宽必须一致
} # 结束 load_iuadd_sat 融合模板

fusion super.load_iusub_sat { # 声明 load 后紧跟 iusub_sat 的超级内存无符号饱和减法融合模板
  target load_iusub_sat # 融合后发射 isa.vm 中的 load_iusub_sat 指令
  sequence load, iusub_sat # 只允许把连续的 load 与 iusub_sat 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 iusub_sat 使用
  require same_width # load 位宽和 iusub_sat 结果位宽必须一致
} # 结束 load_iusub_sat 融合模板

fusion super.load_isadd_sat { # 声明 load 后紧跟 isadd_sat 的超级内存有符号饱和加法融合模板
  target load_isadd_sat # 融合后发射 isa.vm 中的 load_isadd_sat 指令
  sequence load, isadd_sat # 只允许把连续的 load 与 isadd_sat 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 isadd_sat 使用
  require same_width # load 位宽和 isadd_sat 结果位宽必须一致
} # 结束 load_isadd_sat 融合模板

fusion super.load_issub_sat { # 声明 load 后紧跟 issub_sat 的超级内存有符号饱和减法融合模板
  target load_issub_sat # 融合后发射 isa.vm 中的 load_issub_sat 指令
  sequence load, issub_sat # 只允许把连续的 load 与 issub_sat 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 issub_sat 使用
  require same_width # load 位宽和 issub_sat 结果位宽必须一致
} # 结束 load_issub_sat 融合模板

fusion super.load_iushl_sat { # 声明 load 后紧跟 iushl_sat 的超级内存无符号饱和左移融合模板
  target load_iushl_sat # 融合后发射 isa.vm 中的 load_iushl_sat 指令
  sequence load, iushl_sat # 只允许把连续的 load 与 iushl_sat 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 iushl_sat 使用
  require same_width # load 位宽和 iushl_sat 结果位宽必须一致
} # 结束 load_iushl_sat 融合模板

fusion super.load_isshl_sat { # 声明 load 后紧跟 isshl_sat 的超级内存有符号饱和左移融合模板
  target load_isshl_sat # 融合后发射 isa.vm 中的 load_isshl_sat 指令
  sequence load, isshl_sat # 只允许把连续的 load 与 isshl_sat 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 isshl_sat 使用
  require same_width # load 位宽和 isshl_sat 结果位宽必须一致
} # 结束 load_isshl_sat 融合模板

fusion super.load_iand { # 声明 load 后紧跟 iand 的超级内存按位与融合模板
  target load_iand # 融合后发射 isa.vm 中的 load_iand 指令
  sequence load, iand # 只允许把连续的 load 与 iand 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 iand 使用
  require same_width # load 位宽和 iand 结果位宽必须一致
} # 结束 load_iand 融合模板

fusion super.load_ior { # 声明 load 后紧跟 ior 的超级内存按位或融合模板
  target load_ior # 融合后发射 isa.vm 中的 load_ior 指令
  sequence load, ior # 只允许把连续的 load 与 ior 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 ior 使用
  require same_width # load 位宽和 ior 结果位宽必须一致
} # 结束 load_ior 融合模板

fusion super.load_isub { # 声明 load 后紧跟 isub 的超级内存减法融合模板
  target load_isub # 融合后发射 isa.vm 中的 load_isub 指令
  sequence load, isub # 只允许把连续的 load 与 isub 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 isub 使用
  require same_width # load 位宽和 isub 结果位宽必须一致
} # 结束 load_isub 融合模板

fusion super.load_ixor { # 声明 load 后紧跟 ixor 的超级内存异或融合模板
  target load_ixor # 融合后发射 isa.vm 中的 load_ixor 指令
  sequence load, ixor # 只允许把连续的 load 与 ixor 两条 VM 指令融合
  require adjacent # 两条源指令必须在线性 VM IR 中相邻
  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标
  require temp_single_use # load 产生的临时寄存器只能被紧邻的 ixor 使用
  require same_width # load 位宽和 ixor 结果位宽必须一致
} # 结束 load_ixor 融合模板
```

## ABI 设计

VM ABI 单独放在 `abi.vm`，不能散落在 lowering 规则里。

```text
abi host_to_vm {                  # 默认 host 函数到 VM state 的 ABI 映射
  arg0 -> x0 as i64               # 第 0 个整数/指针参数写入 x0
  arg1 -> x1 as i64               # 第 1 个整数/指针参数写入 x1
  vec0 -> q0 as v128              # 第 0 个宽向量参数写入 q0
  ret0 <- x0 as i64               # 第 0 个整数/指针返回值从 x0 读出
  vret0 <- q0 as v128             # 第 0 个宽向量返回值从 q0 读出
  max_returns = 2                 # 当前 ABI 最多返回 2 个 VM value
}                                 # 结束 host_to_vm ABI
# 下一个 ABI 展示 VM 内部 call/ret 对 lr 的使用
abi vm_call {                     # VM 字节码内部函数调用约定
  call_args = [x0..x7, q0..q7]     # VM 内部调用优先使用 x0-x7 和 q0-q7 传参
  call_link = lr                   # call 指令把下一条 bytecode PC 写入 lr
  ret_pc <- lr                     # ret 指令从 lr 读取返回 bytecode PC
  ret_values = [x0, x1, q0]        # VM 内部调用返回值从 x0、x1、q0 读取
  max_returns = 3                 # 当前 VM 内部调用最多返回 3 个 VM value
}                                 # 结束 vm_call ABI
# native_call 描述 VM 调用外部 LLVM 函数时如何映射参数和返回值
native_call default {             # 默认 native call 策略
  args = [x0..x7, q0..q7]          # 从 VM register 读取 native call 参数
  returns = [x0, x1, q0]           # native call 返回值写回 VM register
  clobbers = [x0..x15, q0..q15]    # native call 允许破坏的 caller-saved VM 寄存器
  policy = direct                  # 直接生成 LLVM call，不额外虚拟化被调函数
}                                  # 结束 native_call 策略
```

需要明确几类返回：

- LLVM scalar return：从 `ret0` 映射回 LLVM return value。
- LLVM aggregate return：从 `ret0..retN` 组装 aggregate。
- `sret`：wrapper 把返回值写到 host ABI 提供的返回指针；direct native call 和 indirect native call 的 `sret` / `byval` 等 call-site 参数属性必须复制到 native thunk、indirect adapter 和 adapter 内部 call，不能只保留 function type。
- VM 内部 call：通过 `abi.vm` 指定 `call_link`、`ret_pc`、参数寄存器、返回寄存器和 clobber 集合，必须支持多返回值映射。
- native call：必须按目标 LLVM function type 和 target ABI 重新生成 call。

VM 支持多返回值，但 wrapper 必须负责把 VM 多返回值映射回 LLVM 的单返回模型或 `sret` 模型。

`abi.vm` 的映射语句必须精确声明类型：`argN -> xM as i64`、`retN <- xM as i64`、`vecN -> qM as v128` 和 `vretN <- qM as v128`。未知的 `->` / `<-` 左值、缺少 `as`、错误类型或尾部多余 token 都必须由 profile parser 拒绝，不能静默落回默认 ABI。`ret_pc <- lr` 是 VM 内部 call/ret 的控制 PC 映射，不携带 `as` 类型；`call_args`、`ret_values`、`args`、`returns` 和 `clobbers` 只能使用 `xN` / `qN` 或同组连续范围。

## 字节码格式

`bytecode.vm` 描述 bytecode segment、record layout 和 relocation。

```text
bytecode {                                      # 定义当前 profile 的字节码容器
  segment header fixed                          # header 段使用固定布局，保存版本、key、段偏移等元数据
  segment const_pool fixed                      # const_pool 段使用固定布局，保存常量池
  segment code compressed                       # code 段允许压缩或加密，保存 VM 指令流
  segment reloc fixed                           # reloc 段使用固定布局，保存 label 和外部引用重定位
}                                               # 结束 bytecode 容器定义
# instr record 描述单条 VM 指令在 code 段中的编码形态
record instr {                                  # 定义 VM 指令 record
  opcode: varint encrypted                      # opcode 使用 varint 编码，并经过 decoder pipeline 保护
  operands: bitpack schema=operand_stream       # operands 根据 operand_stream schema 做 bitpack
  decoded_width: one_of=[4,8,16,32,48,64] default=32 # decoded record 宽度只能从六个固定值中选择
}                                               # 结束 instr record
# label_pc reloc 描述 label 到 bytecode PC 的重定位
reloc label_pc {                                # 定义 label_pc 重定位类型
  width = varint                                # 重定位值使用 varint 宽度
  base = code_start                             # 重定位基址为 code 段起点，值是 decoded 字节偏移
}                                               # 结束 label_pc 重定位定义
```

`isa.vm` 的每条 `instr` 可以用 `decoded_width = 4|8|16|32|48|64` 覆盖默认宽度；未声明时使用 `bytecode.vm` 的 `default`。`decoded_width` 表示 runtime 按 `decoder.vm` 撤销字节级变换后，一条 VM instruction record 在 code stream 中占用的 decoded 字节数，不是 opcode 位宽，也不是 operand 数量。Opcode 仍按 varint 读取，因此 opcode 可以超过 1 byte；operand 按 ISA operand schema 顺序读出。`imm<u7>` 只允许 0..127 的短枚举值，适合 bit width、predicate、ordering、argc 和 ret_count 这类有界字段；`imm<u8>` 表示完整 8-bit 值，verifier 必须按 0..255 的最坏 bitpack 长度计算 record 容量。Encoder 先生成 `[opcode, bitpacked operands...]` 的 varint 字节，若长度小于 `decoded_width` 则用 0 padding 补齐，若超过则拒绝该 profile 或该函数。Runtime 以 `pc` 作为 code segment 内 decoded 字节偏移，取 opcode 后按 opcode 找到 profile 指令描述，再读取该指令声明的 operand 数量；`pc = next` 会加上当前指令的 `decoded_width`，`br`/`br_if`/`vm_call`/`vm_ret` 使用的 label PC 也都是 decoded 字节偏移。

字节码禁止固定成 `i32[]`。它必须支持：

- `u8` stream。
- varint。
- bitpack。
- const pool。
- label relocation。
- per-function key。
- compressed code segment。
- debug dump，用于测试和反查。
- 混合 4/8/16/32/48/64 字节 decoded record，并且 fake/dead bytecode 与 label relocation 必须按字节偏移共存。

`bytecode.vm` 的核心语法必须按示例精确匹配：`segment <name> fixed|compressed` 不能携带尾部字段；`operands:` 当前只接受 `bitpack schema=operand_stream`；`decoded_width:` 只接受 `one_of=[...]` 和 `default=N` 两个字段且不能重复；`reloc label_pc { ... }` 内当前只允许 `width = varint` 和 `base = code_start`；`fake_instruction` / `dead_bytecode` 只允许 `enabled|disabled` 加可选 `count=N`。parser 必须拒绝未知字段、重复字段和未知 relocation 语句，避免 profile 写错后仍用默认行为生成字节码。

## 解码器流水线

`decoder.vm` 描述 runtime 如何把 bytecode stream 解码为 VM instruction。

```text
decoder code {                       # 定义 code 段的 runtime 解码器
  input segment code                  # 解码器输入来自 bytecode 的 code segment
  step xor_stream key=function_key    # 第一步使用当前函数 key 对字节流做 xor 解密
  step add_stream key=function_key    # 第二步撤销编译期 add_stream 逆变换
  step ror amount=3                   # 第三步对解密后的字节流做右旋恢复
  step rol amount=1                   # 第四步对字节流做左旋恢复
  step varint_decode                  # 第五步把变长整数流解码成 opcode/operand token
  step bit_unpack schema=instr        # 第六步按 instr schema 还原结构化 VM 指令
}                                     # 结束 code 段解码器定义
```

`input segment` 当前只允许 `code`。支持的 step 语法只有 `xor_stream key=function_key`、`add_stream key=function_key`、`rol amount=N`、`ror amount=N`、`varint_decode` 和 `bit_unpack schema=instr`。`varint_decode` 和 `bit_unpack` 是结构化边界步骤，必须各出现且只出现一次；所有字节级可逆变换必须位于 `varint_decode` 之前，`bit_unpack` 必须位于 `varint_decode` 之后。Profile parser/verifier 必须拒绝缺失、重复、顺序错误、未知参数、未知输入段或旋转位数不在 `1..=7` 的 decoder profile，避免 profile 通过校验后才在 encoder 逆向流水线中失败。

编译期 encoder 必须执行 decoder 的逆过程：

```text
runtime decoder: xor_stream -> add_stream -> ror -> rol -> varint_decode -> bit_unpack   # runtime 按 profile 声明顺序解码
compiler encoder: bit_pack -> per-record varint_encode/pad -> ror -> rol -> add_stream -> xor_stream # 编译期 encoder 必须执行完全相反的可逆流程
```

因此 decoder step 需要声明是否可逆：

| Step             | 合法性  | 说明                                                |
|------------------|------|---------------------------------------------------|
| `xor_stream`     | 必须支持 | 可逆，适合作为基础加密                                       |
| `add_stream`     | 必须支持 | 可逆，注意溢出语义                                         |
| `rol` / `ror`    | 必须支持 | 可逆                                                |
| `varint`         | 必须支持 | encoder/decoder 成对实现                              |
| `bitpack`        | 必须支持 | 需要 schema                                         |

## Runtime Emitter

runtime 不从 AMICE 写死模板复制，而是由 profile 生成 LLVM IR。

Emitter 输入：

- 固定 VM register model：`x0..x31`、`q0..q64`、寄存器别名和控制状态。
- VM state layout。
- ISA handler semantic。
- bytecode decoder pipeline。
- dispatch 策略。
- ABI marshal/unmarshal 规则。

合法 dispatch 策略：

```text
runtime {              # 定义 runtime 生成策略
  runtime.entry = call # 默认入口形态：wrapper 调用 private dispatcher
  dispatch = switch    # 使用 LLVM switch 生成 dispatcher
}                      # 结束 runtime 生成策略
```

`runtime.entry` 只接受：

- `call`：默认值。wrapper marshal 参数后调用 `dispatch(i64 fn_token, ptr ret_slots, ptr arg_slots) -> i64`，便于调试并保持既有测试工作流。
- `inline`：wrapper marshal 参数后不生成、不引用三参数 dispatcher；runtime emitter 把 descriptor decode、guard 校验、`loop.check`、`execute.decode` 和 handler blocks 直接追加到 wrapper 函数体内。inline 模式不能先生成 dispatcher call 再依赖 LLVM `alwaysinline` 或优化器内联；它必须复用同一套 dispatch CFG emitter，把 VM 完成时的 i64 返回值写入临时 slot，然后 branch 到 wrapper 的 `after_vm` 返回 marshal block。

runtime profile 允许声明以下增强开关：

- threaded dispatch。
- indirect branch dispatch。
- handler splitting。
- handler order shuffle。
- opcode alias。
- per-function handler clone。

runtime profile 还允许声明 `runtime.emit_markers = true|false`。该选项只用于测试和调试，开启后会生成 `AMICEVMP` bytecode magic、`AMICE_VMP_RUNTIME_BYTECODE`、`.amice.vm.meta.*` 以及稳定的 `.amice.vm.bytecode.*` 等可识别符号；生产默认必须保持 `false`。

当前 runtime emitter 的默认数据形态是 descriptor table 模式。无论 `runtime.entry` 是 `call` 还是 `inline`，wrapper 都不把 `code_ptr`、`code_len`、`const_pool_ptr`、`const_pool_len`、bytecode key、native table pointer 或 native call count 作为入口参数；`call` 模式 dispatcher ABI 固定为：

```text
dispatch(i64 fn_token, ptr ret_slots, ptr arg_slots) -> i64
```

dispatch CFG 入口先反解 `fn_token` 得到 `fn_index`，检查 `fn_index < descriptor_count`，再读取并解密 descriptor 的 guard 字段。索引越界或 guard 不匹配时必须走 trap/default safe path，不能对 descriptor table 做越界 GEP。guard 校验通过后，dispatch CFG 从 descriptor 解出执行所需字段，再继续走现有 bytecode decoder、const_pool reader、native-call handler、`ret_slots` 和 `arg_slots` 行为。`call` 模式把这段 CFG 放在 private dispatcher function 中；`inline` 模式把同一段 CFG 放在 wrapper 中。

每个 descriptor 至少包含：

- `code_offset` 和 `code_len`：指向模块级紧凑 bytecode blob 中当前函数的 code segment。
- `const_pool_offset` 和 `const_pool_len`：指向同一 blob 中当前函数的 const_pool segment。
- `bytecode key`：decoder pipeline 与 const_pool 解密使用的 per-function key。
- `native_base` 和 `native_count`：指向模块级 native table 中当前函数可用 thunk 的连续区间。
- `guard`：由 table seed 和 `fn_index` 派生的校验值。

descriptor table 必须是 private global。生产默认不能使用 `.amice.vm.*` 可读符号名；只有 `runtime.emit_markers=true` 或 `AMICE_VM_EMIT_MARKERS=true` 时，才允许生成 `.amice.vm.descriptor_table.*` 这类测试/调试名。descriptor 字段不能明文存储：编译期用固定、可复现的 splitmix64/mix64 派生 field key，并对每个字段做 rotate、xor 和 add 组合；dispatcher 内部使用同一套可复现算法按 `fn_index` 和 field id 解密。token 也不能是裸 `fn_index`：wrapper 必须由 `fn_index` 派生 opaque token，并用拆分 immediate 的计算序列构造；dispatcher 再执行反向 affine/rotate 变换得到 `fn_index`。

`runtime.vm` 也必须使用精确语法：`bank x range x0..x31 type u64`、`bank q range q0..q64 type v128`、`alias name = xN|qN`、`pc: label`、`runtime.entry = call|inline`、`runtime.emit_markers = true|false` 和增强开关 `enhance name = enabled|disabled|func`。alias 缺少 `=`、bank 行存在尾部 token、control-state slot 的类型含多余 token、`runtime.entry` 不是 `call` 或 `inline`，或 enhancement 名称不在白名单内，都必须在 profile parser/verifier 阶段失败。

Verifier 必须拒绝不在本节枚举内的 dispatch 策略或 runtime 增强开关。

## Pass 配置

环境变量：

| 环境变量                     | 说明                                                |
|--------------------------|---------------------------------------------------|
| `AMICE_VM_VIRTUALIZE`    | 是否启用新 VMP 虚拟化 pass                                |
| `AMICE_VM_PROFILE_PATH`  | profile package 路径                                |
| `AMICE_VM_RUNTIME_SCOPE` | 覆盖 profile 中的 runtime scope，仅允许 `func` 或 `module` |
| `AMICE_VM_EMIT_MARKERS`  | 显式生成 VMP 测试/调试 marker；生产默认 `false`             |
| `AMICE_VM_DUMP_BYTECODE` | 调试输出 bytecode                                     |
| `AMICE_VM_DUMP_LOWERING` | 调试输出 LLVM IR 到 VM IR 的 lowering 结果                |

函数注解：

```c
__attribute__((annotate("+vm_virtualize")))
int foo(int x) {
    return x + 1;
}

__attribute__((annotate("+vm_virtualize,vm_profile=profile_a")))
int bar(int x) {
    return x * 3;
}
```

与当前 `+vm_flatten` 的关系：

- `vm_flatten` 保持现有含义：控制流虚拟化/扁平化。
- `vm_virtualize` 表示新 VMP pass：LLVM IR 指令级虚拟化。
- 两者不能默认同时作用在同一个函数。若用户强制组合，必须通过 pass order 明确顺序。

## 校验器

Profile verifier 是这个设计能否长期维护的关键。

必须检查：

- manifest 引用文件存在，版本兼容。
- VM state 字段类型合法。
- register bank 必须符合固定模型：`x0..x31` 和 `q0..q64`。
- register alias 必须指向已存在的 `x` / `q` 寄存器。
- ABI 引用 `lr`、`sp` 等别名时，相关别名必须已在 `runtime.vm` 中定义。
- ISA operand 类型和 semantic 使用一致。
- 每条 handler 对 `pc` 的处理明确：`next`、`branch`、`return` 三者之一。
- lowering rule 只 emit profile 中存在的指令。
- lowering rule 的 `emit` operand 名称必须存在于对应 ISA 指令声明中。
- result 形式的 lowering rule 必须显式 `bind` 其 LLVM result；`llvm.memory.scalar` 的 load 路径也必须声明 `bind %r`。
- lowering rule 的 `bind` 必须引用本 rule 中已 materialize 或 vreg 分配出的 VM value；LLVM result 类型由 translator 的具体 instruction lowering 路径保守检查。
- decoder pipeline 对 encoder 可逆。
- bytecode record layout 必须承载所有 operand。
- ABI `max_returns` 足够覆盖目标函数返回映射。
- native call policy 必须表达目标 call。

如果 profile 不能覆盖某条 LLVM instruction，pass 必须保守跳过该函数，而不是生成部分虚拟化的错误代码。

## 实现要求

- 提供 profile loader 和 verifier。
- 提供 VM IR。
- 提供 LLVM IR normalizer/translator，负责处理 `phi`、`switch`、`sret` 和 ABI 相关 lowering 前置变换。
- 提供 LLVM IR 到 VM IR translator。
- 提供 bytecode encoder，按 `bytecode.vm` 和 `decoder.vm` 执行 decoder inverse。
- 提供 runtime emitter，按 `runtime.vm`、`isa.vm`、`abi.vm` 生成 LLVM IR runtime。
- 支持 `func` 和 `module` 两种 scope。
- 支持整数运算、标量 `half`/`float`/`double` 普通运算、标量 `half`/`float`/`double` 浮点 intrinsic、标量浮点到整数取整 intrinsic、比较、类型转换、内存访问、控制流、direct native call、direct varargs native call、indirect native call、aggregate parameter、aggregate return 和 `sret`。
- 支持 per-function opcode permutation、opcode alias、handler clone、handler order shuffle、const pool 加密、fake instruction 和 dead bytecode。
- 支持 profile 声明的受限 `iadd_xor`、`icmp_br_if`、`gep_load`、`load_iadd`、`load_imul`、`load_iudiv`、`load_isdiv`、`load_iurem`、`load_isrem`、`load_ishl`、`load_ilshr`、`load_iashr`、`load_ismax`、`load_ismin`、`load_iumax`、`load_iumin`、`load_iuadd_sat`、`load_iusub_sat`、`load_isadd_sat`、`load_issub_sat`、`load_iushl_sat`、`load_isshl_sat`、`load_iand`、`load_ior`、`load_isub` 和 `load_ixor` 超级指令；不满足 fuse 条件时必须回退普通 VM 指令序列。

## 测试策略

测试至少分四层：

- Profile parser tests：解析 manifest 和 DSL。
- Verifier tests：故意构造非法 profile，确认拒绝。
- Encoder/decoder round-trip tests：随机 VM instruction 序列编码后能被 runtime decoder 还原。
- Differential tests：同一个 C/Rust fixture 编译 baseline 和 VM virtualized 版本，比较输出。

集成测试需要覆盖：

- scalar 参数和返回。
- 多参数。
- aggregate parameter / aggregate return。
- `sret`。
- branch。
- loop。
- switch。
- load/store/gep。
- direct call。

## 实现边界

AMICE 的职责是根据 profile 生成 VM runtime、翻译 LLVM IR、编码 bytecode。Profile 定义 VM 的 ISA 名称、opcode alias、operand、ABI、bytecode、decoder 和 runtime 形态；AMICE 内置一组可验证的 handler semantic 模板，profile 的 `semantic {}` 必须解析并匹配这些模板。AMICE 不接受不可验证的 profile 扩展，也不会把未知 semantic 当作可执行宿主代码。


固定 `alloca` 允许 byte size 为 0；常量 0 count 和零大小元素类型会通过 profile 的 `llvm.alloca.stack` rule 发射 `alloca bytes=0`，runtime 生成零字节 VM 栈槽并按 64 位 pointer bit 保存结果。动态 `alloca` 的运行时 count 为 0 时仍走 `alloca_dyn`，由 runtime 计算 `reg[count] * elem_size`。

`getelementptr` 允许中间元素类型的 ABI store size 为 0；常量下标对字节偏移贡献 0，动态 GEP 会被拆成 `base + sum(index_i * element_size_i) + constant_offset`。每个动态下标 term 都必须通过 profile 的 `llvm.gep.dynamic` rule 先按 LLVM GEP 的有符号索引语义 `sext` 到 64 位，再发射 `imul` / `iadd` 累加到当前地址，其中元素大小常量可以为 0；只有存在非零常量尾偏移时才追加 `gep`。这样 struct/array 混合 GEP、多动态下标、`[0 x T]`、零大小 struct 和 Rust ZST 风格的指针算术不会因为 scale 为 0 被提前跳过，`i32 -1` 这类窄整数动态下标也不会被误当成零扩展后的巨大正偏移。

Profile verifier 必须在加载阶段强制检查 translator 依赖的 lowering shape：`llvm.gep.dynamic` 必须包含 `sext dst=%vx, src=%vi, from_width=type_width(%index), to_width=64`、`imul dst=%vs, lhs=%vx, rhs=element_size(%base), width=64`、`iadd dst=%vr, lhs=%vb, rhs=%vs, width=64` 和最终 `gep dst=%vr, base=%vr, offset=constant_gep_offset(%r)`；`llvm.cast.pointer`、`llvm.vector.cast.pointer`、`llvm.vp.vector.cast.pointer`、`llvm.constexpr.ptrtoint` 和 `llvm.constexpr.inttoptr` 必须分别保留 `zext`、`trunc`、`bitcast` 三种宽度路径。缺少这些 profile 动作时，package 必须被 verifier 拒绝，不能等到函数 lowering 时再退化为不完整支持。

补充：当前可虚拟化 intrinsic 白名单也包括 `llvm.pseudoprobe`。它不写 VM 寄存器，只通过 profile 的 `pseudoprobe` 指令和 `pseudo_probe(...)` semantic 保留 LLVM 伪探针副作用。

补充：`llvm.prefetch` 不是 metadata no-op，也不能退化为 `fake_nop`。它必须通过 profile 的 `prefetch` 指令和 `prefetch(...)` semantic 保留显式硬件预取提示；`rw/locality/cache` 三个 hint 仍按 LLVM `immarg` 规则在 translator 阶段验证为常量。

`llvm.clear_cache` 通过 profile 中的 `llvm.clear_cache` lowering rule 发射 `clear_cache`，再由 `clear_cache(...)` semantic 进入 runtime。runtime 生成 LLVM IR 时声明并调用 `llvm.clear_cache(ptr, ptr)`，两个地址参数按 64 位 pointer bit 从 x 寄存器还原；translator 只接受两个 pointer 参数且 void 返回的声明，其它签名安全跳过该函数。

volatile atomic load/store 通过 profile 中的 `volatile_atomic_load_width` / `volatile_atomic_store_width` semantic 进入 runtime，运行时生成同宽整数原子 load/store，并同时设置 LLVM atomic ordering、syncscope 和 volatile 标记；它只接受默认 system 或 `syncscope("singlethread")`、自然对齐和 8/16/32/64 位整数/指针/`half`/`float`/`double` 标量内存宽度。

volatile atomicrmw 通过 profile 中的 `volatile_atomic_rmw(op, ...)` semantic 进入 runtime，运行时生成同名 LLVM `atomicrmw` 并设置 volatile 标记；整数、浮点类型、指针 `xchg`、自然对齐、syncscope 和 ordering 限制与普通 `atomicrmw` 相同。

自然对齐指针标量 `atomicrmw` 当前只声明支持 `xchg`，VM x 寄存器按 64 位 pointer bit 保存旧值和新值；其它 pointer RMW 操作不属于 AMICE 当前支持子集。

volatile `cmpxchg` 通过 profile 中的 `volatile_cmpxchg` semantic 进入 runtime，生成带 volatile 标记的 LLVM `cmpxchg`，其它类型、自然对齐、syncscope 和 success/failure ordering 限制与普通 `cmpxchg` 相同。

`fence` 通过 profile 中的 `fence(ordering, sync_scope)` semantic 进入 runtime，运行时只为 `system` 和 `syncscope("singlethread")` 两种有限 syncscope 生成 acquire/release/acq_rel/seq_cst LLVM fence；atomic load/store、atomicrmw 和 cmpxchg 使用同一组有限 syncscope。其它自定义 syncscope 必须在 translator 阶段安全跳过目标函数。

volatile `llvm.memcpy` / `llvm.memmove` / `llvm.memset` 通过 profile 中的 `volatile_memcpy_dynamic` / `volatile_memmove_dynamic` / `volatile_memset_dynamic` semantic 进入 runtime；translator 会把常量长度和动态长度都归一化到逐字节 dynamic handler，以保留 volatile load/store 的访问粒度和标记。

非 volatile `llvm.memcpy.inline` 和 `llvm.memset.inline` 分别通过同一套 `llvm.memory.copy.fixed` 和 `llvm.memory.set.fixed` profile lowering 处理。它们要求 LLVM IR 中的长度 operand 是 `immarg` 常量；长度不超过内联阈值时按固定 `load` / `store` / `gep` 序列展开，超过阈值时分别走 `memcpy_dyn` 或 `memset_dyn` handler。当前不声明独立 inline memory ISA semantic，避免让同一内存语义在 profile 中分裂成两套行为。

取整和数学一元浮点 intrinsic 通过 profile 中的 `float_unary(ffloor/fceil/ftrunc/frint/fnearbyint/fround/froundeven/fsin/fcos/fexp/fexp2/flog/flog10/flog2, ...)` semantic 进入 runtime，并调用 LLVM `llvm.floor.f32/f64`、`llvm.ceil.f32/f64`、`llvm.trunc.f32/f64`、`llvm.rint.f32/f64`、`llvm.nearbyint.f32/f64`、`llvm.round.f32/f64`、`llvm.roundeven.f32/f64`、`llvm.sin.f32/f64`、`llvm.cos.f32/f64`、`llvm.exp.f32/f64`、`llvm.exp2.f32/f64`、`llvm.log.f32/f64`、`llvm.log10.f32/f64` 和 `llvm.log2.f32/f64` intrinsic。

饱和浮点转整数 intrinsic 通过 profile 中的 `float_cast(fptosi_sat/fptoui_sat, ...)` semantic 进入 runtime。translator 接受一个 `half`、`float` 或 `double` 标量参数和 i1/i8/i16/i32/i64 标量整数返回，也接受 fixed vector 源和结果 lane 数相同、源 lane 是 `half` / `float` / `double`、结果 lane 是 i1/i8/i16/i32/i64 的向量形式；runtime 按源浮点宽度和目标整数宽度调用 LLVM `llvm.fptosi.sat.*` / `llvm.fptoui.sat.*` intrinsic；其中 half 源值会先精确扩展为 f32，再复用 f32 饱和转换 intrinsic，不把 NaN 或越界输入退回到普通 `fptosi` / `fptoui` 的未定义边界。

`llvm.lrint`、`llvm.llrint`、`llvm.lround` 和 `llvm.llround` 通过 profile 中的 `float_round_to_int(lrint|llrint|lround|llround, ...)` semantic 进入 runtime。translator 只接受一个 `half`、`float` 或 `double` 参数，以及 i32/i64 标量整数返回；runtime 按源浮点宽度和目标整数宽度声明并调用 LLVM `llvm.lrint.*`、`llvm.llrint.*`、`llvm.lround.*` 或 `llvm.llround.*` intrinsic。`lrint` / `llrint` 保留当前舍入模式语义，`lround` / `llround` 保留远离零取整语义，不能退化为普通 `fptosi` 截断。

固定向量 `llvm.lrint` / `llvm.llrint` / `llvm.lround` / `llvm.llround` 通过 profile 中的 `llvm.vector.lrint.float` / `llvm.vector.llrint.float` / `llvm.vector.lround.float` / `llvm.vector.llround.float` rule 逐 lane 发射 `flrint` / `fllrint` / `flround` / `fllround` handler；translator 只接受 fixed vector 源和结果 lane 数相同、源 lane 是 `half` / `float` / `double`、结果 lane 是 i32/i64 的情况。

`llvm.convert.to.fp16` / `llvm.convert.from.fp16` 不需要独立 VM handler。前者的 i16 返回值是 half bit pattern，translator 必须通过 `llvm.convert.to.fp16` lowering rule 发射 `fptrunc from_width=f32/f64, to_width=16` 并绑定为整数 x 寄存器；后者的 i16 参数是 half bit pattern，translator 必须通过 `llvm.convert.from.fp16` lowering rule 发射 `fpext from_width=16, to_width=f32/f64` 并绑定为浮点 bit。profile 可以重命名这两个 lowering rule 发射的具体 ISA 指令，但 semantic 必须仍来自 `float_cast(fptrunc/fpext, ...)`，不能退化为固定 helper 调用。

`llvm.ssa.copy` 通过 profile 中的 `llvm.ssa.copy.scalar` lowering rule 发射 `mov`，支持整数、指针、`half`、`float` 和 `double` 标量；固定向量 `llvm.ssa.copy` 通过 profile 中的 `llvm.vector.ssa.copy` rule 逐 lane 发射 `mov`，支持整数、指针、`half`、`float` 和 `double` lane。`llvm.arithmetic.fence.*` 通过 profile 中的 `llvm.arithmetic.fence.scalar` lowering rule 发射 `mov`，支持 `half`、`float` 和 `double` 标量；固定向量 `llvm.arithmetic.fence.*` 通过 profile 中的 `llvm.vector.arithmetic.fence` rule 逐 lane 发射 `mov`，只支持 `half`、`float` 和 `double` lane，runtime 只复制 x 寄存器中的浮点原始 bit，不生成专用 native call handler；LLVM 标量 `bitcast` 通过 profile 中的 `llvm.cast.bitcast.scalar` lowering rule 发射 `bitcast`，只支持 x 寄存器可承载的同宽整数/`half`/`float`/`double` bit pattern reinterpret；固定向量 `bitcast` 通过 profile 中的 `llvm.vector.bitcast.element` rule 逐 lane 发射 `bitcast`，只支持 fixed vector 源和结果 lane 数相同且每个 lane 位宽相同的情况；固定整数向量 `zext/sext/trunc` 通过 profile 中的 `llvm.vector.cast.integer` rule 逐 lane 发射 `zext` / `sext` / `trunc`，只支持 fixed vector 源和结果 lane 数相同且每个 lane 都是整数的情况；固定指针向量 `ptrtoint/inttoptr/addrspacecast` 通过 profile 中的 `llvm.vector.cast.pointer` rule 逐 lane 发射 `zext` / `trunc` / `bitcast`，只支持 fixed vector 源和结果 lane 数相同，且每个 lane 都是整数 lane 与 64 位 pointer lane 之间的转换；固定向量 `sitofp/uitofp/fptosi/fptoui` 通过 profile 中的 `llvm.vector.sitofp.float` / `llvm.vector.uitofp.float` / `llvm.vector.fptosi.float` / `llvm.vector.fptoui.float` rule 逐 lane 发射对应 float-cast handler，只支持 fixed vector 源和结果 lane 数相同且浮点端是 `half` / `float` / `double` 的情况；固定浮点向量 `fptrunc/fpext` 通过 profile 中的 `llvm.vector.fptrunc.float` / `llvm.vector.fpext.float` rule 逐 lane 发射 `fptrunc` / `fpext`，只支持 fixed vector 源和结果 lane 数相同且每个 lane 都是 `half` / `float` / `double` 的情况；fixed vector constrained `sitofp/uitofp/fptosi/fptoui/fptrunc/fpext` 通过 profile 中的 `llvm.constrained.vector.sitofp.float`、`llvm.constrained.vector.uitofp.float`、`llvm.constrained.vector.fptosi.float`、`llvm.constrained.vector.fptoui.float`、`llvm.constrained.vector.fptrunc.float` 和 `llvm.constrained.vector.fpext.float` rule 逐 lane 复用对应 float-cast handler，并额外要求 constrained metadata 落在本文定义的保守子集；固定浮点向量 `llvm.fabs/sqrt/canonicalize/floor/ceil/trunc/rint/nearbyint/round/roundeven` intrinsic 通过 profile 中的 `llvm.vector.{fabs,sqrt,canonicalize,floor,ceil,trunc,rint,nearbyint,round,roundeven}.float` rule 逐 lane 发射对应 float-unary handler，只支持 fixed vector 源和结果 lane 数相同且每个 lane 都是 `half` / `float` / `double` 的情况；跨 lane 重新打包的向量 bitcast、未列入固定向量白名单的其它向量浮点 intrinsic，以及 `bfloat`、`fp128` 等非 `half`/`float`/`double` 浮点形态仍安全跳过。
固定向量 `llvm.fptosi.sat/fptoui.sat` 通过 profile 中的 `llvm.vector.fptosi.sat.float` / `llvm.vector.fptoui.sat.float` rule 逐 lane 发射 `fptosi_sat` / `fptoui_sat` handler，只支持 fixed vector 源和结果 lane 数相同、源 lane 是 `half` / `float` / `double`、结果 lane 是 i1/i8/i16/i32/i64 的情况；runtime 仍按 lane 的 `from_width` 和 `to_width` 调用 LLVM 饱和转换 intrinsic。
固定浮点向量 `llvm.copysign/minnum/maxnum/minimum/maximum` intrinsic 通过 profile 中的 `llvm.vector.{copysign,minnum,maxnum,minimum,maximum}.float` rule 逐 lane 发射对应 float-binary handler，只支持 fixed vector 两个源和结果 lane 数相同且每个 lane 都是 `half` / `float` / `double` 的情况。
固定浮点向量 `llvm.pow` intrinsic 通过 profile 中的 `llvm.vector.pow.float` rule 逐 lane 发射对应 float-binary handler，只支持 fixed vector 两个源和结果 lane 数相同且每个 lane 都是 `half` / `float` / `double` 的情况；fixed vector constrained `pow` 通过 `llvm.constrained.vector.pow.float` rule 复用同一组 handler，并额外要求 constrained metadata 是 `round.tonearest` 与 `fpexcept.ignore`。固定浮点向量 `llvm.powi` intrinsic 通过 profile 中的 `llvm.vector.powi.float` rule 逐 lane 发射 `fpowi` handler，只支持 fixed vector 底数和结果 lane 数相同、每个 lane 都是 `half` / `float` / `double`，且指数是共享标量 i32 的情况；fixed vector constrained `powi` 通过 `llvm.constrained.vector.powi.float` rule 复用同一 handler，并额外要求 constrained metadata 是 `round.tonearest` 与 `fpexcept.ignore`。
固定浮点向量 `llvm.is.fpclass` intrinsic 通过 profile 中的 `llvm.vector.is.fpclass.float` rule 逐 lane 发射 `fpclass` handler，只支持 fixed vector 源和结果 lane 数相同、源 lane 是 `half` / `float` / `double`、结果 lane 是 `i1`，且 mask 没有超出 LLVM `FPClassTest` 当前 `0x03ff` 范围的情况。
固定浮点向量 `llvm.vp.is.fpclass` intrinsic 通过 profile 中的 `llvm.vp.vector.is.fpclass.float` rule 逐激活 lane 发射 `fpclass` handler，只支持 fixed vector、常量 `<N x i1>` VP mask、常量 i32 EVL、源和结果 lane 数相同、源 lane 是 `half` / `float` / `double`、结果 lane 是 `i1`，且 class mask 没有超出 LLVM `FPClassTest` 当前 `0x03ff` 范围的情况；inactive lane 不生成 VM 寄存器绑定。
固定浮点向量 `llvm.fma/fmuladd` intrinsic 通过 profile 中的 `llvm.vector.fma.float` / `llvm.vector.fmuladd.float` rule 逐 lane 发射对应 float-ternary handler，只支持 fixed vector 三个源和结果 lane 数相同且每个 lane 都是 `half` / `float` / `double` 的情况；fixed vector constrained `fma/fmuladd` 通过 `llvm.constrained.vector.fma.float` / `llvm.constrained.vector.fmuladd.float` rule 复用同一组 handler，并额外要求 constrained metadata 是 `round.tonearest` 与 `fpexcept.ignore`。
固定浮点向量 `llvm.sin/cos/exp/exp2/log/log10/log2` intrinsic 通过 profile 中的 `llvm.vector.{sin,cos,exp,exp2,log,log10,log2}.float` rule 逐 lane 发射对应 float-unary handler，只支持 fixed vector 源和结果 lane 数相同且每个 lane 都是 `half` / `float` / `double` 的情况。
固定向量 round-to-int intrinsic `llvm.lrint/llrint/lround/llround` 通过 profile 中的 `llvm.vector.{lrint,llrint,lround,llround}.float` rule 逐 lane 发射对应 float-round-to-int handler，只支持 fixed vector 源和结果 lane 数相同、源 lane 是 `half` / `float` / `double`、结果 lane 是 i32/i64 的情况。
固定向量值保持 intrinsic `llvm.ssa.copy` 通过 profile 中的 `llvm.vector.ssa.copy` rule 逐 lane 发射 `mov`，支持整数、指针、`half`、`float` 和 `double` lane；固定向量 `llvm.arithmetic.fence.*` 通过 profile 中的 `llvm.vector.arithmetic.fence` rule 逐 lane 发射 `mov`，只支持 `half`、`float` 和 `double` lane。两者都不生成专用 runtime handler，runtime 只复制 x 寄存器中的原始 bit。

固定向量当前只开放内部临时值的常量 lane 读写、`freeze`、`phi`、scalar i1 条件 `select`、常量 mask `shufflevector`、`llvm.vector.reverse` / `llvm.vector.splice` / `llvm.experimental.vp.splice` / `llvm.vector.insert` / `llvm.vector.extract` / `llvm.experimental.vector.compress`、同 lane 数且同 lane 位宽的固定向量 `bitcast`、固定整数向量 `zext/sext/trunc`、固定指针向量 `ptrtoint/inttoptr/addrspacecast`、固定整数向量二元运算、固定整数向量 `icmp`、固定浮点向量 `fcmp`、固定浮点向量 unary intrinsic `llvm.fabs/sqrt/canonicalize/floor/ceil/trunc/rint/nearbyint/round/roundeven`、固定向量整数/浮点互转 `sitofp/uitofp/fptosi/fptoui`、固定浮点向量 `fptrunc/fpext`、固定浮点向量 `fneg` 和固定浮点向量二元运算：`insertelement` 通过 profile 中的 `llvm.vector.insert.element` rule 把单个 lane 标量复制到 VM x 寄存器字段槽，`extractelement` 通过 `llvm.vector.extract.element` rule 从已构造的字段槽复制出标量结果，`freeze` 通过 `llvm.vector.freeze` rule 对每个 lane 做稳定复制，固定向量 `phi` 通过 `llvm.vector.phi.edge_move` rule 在 predecessor edge 上逐 lane 复制到预分配结果槽，scalar i1 条件 `select` 通过 `llvm.select.vector` rule 先发射 `br_if`，再在 then/else 片段逐 lane `mov`，`shufflevector` 通过 `llvm.vector.shuffle.element` rule 按 LLVM 常量 mask 逐 lane 复制，`llvm.vector.reverse` 通过 `llvm.vector.reverse.element` rule 按反向 lane mask 逐 lane复制，`llvm.vector.splice` 通过 `llvm.vector.splice.element` rule 按 LLVM signed i32 immarg 生成 concat lane mask 后逐 lane 复制，`llvm.experimental.vp.splice` 通过 `llvm.experimental.vp.splice.element` rule 按 LLVM signed i32 immarg、常量 `<N x i1>` mask、常量 EVL1/EVL2 生成窗口 lane mask 后逐激活 lane 复制，mask 禁用、`lane >= EVL2` 或窗口落在无效拼接区间的 lane 保持 poison/未绑定，`llvm.vector.insert` 通过 `llvm.vector.insert.subvector.element` rule 按 i64 非负常量 offset 把 subvector lane 覆盖到 base vector 对应范围，offset 必须是 subvector lane 数的倍数，`llvm.vector.extract` 通过 `llvm.vector.extract.subvector.element` rule 按 i64 非负常量 offset 从 source vector 连续复制 result lane，offset 必须是 result lane 数的倍数，`llvm.experimental.vector.compress` 通过 `llvm.experimental.vector.compress.element` rule 按常量 mask 先连续复制 active value lane，剩余结果 lane 复制对应 passthru lane，固定向量 `bitcast` 通过 `llvm.vector.bitcast.element` rule 逐 lane 发射 `bitcast` 并重新组成结果向量。固定整数向量 `zext/sext/trunc` 通过 `llvm.vector.cast.integer` rule 逐 lane 发射对应 cast handler，结果重新组成目标整数向量；固定指针向量 `ptrtoint/inttoptr/addrspacecast` 通过 `llvm.vector.cast.pointer` rule 逐 lane 发射 `zext` / `trunc` / `bitcast` handler，结果重新组成目标指针或整数向量；固定整数向量 `add/sub/mul/udiv/sdiv/urem/srem` 分别通过 `llvm.vector.{add,sub,mul,udiv,sdiv,urem,srem}.integer` rule 逐 lane 发射对应整数 ALU handler，`xor/and/or` 通过 `llvm.vector.bitops.integer` rule 由 translator 按语义筛选 `ixor`/`iand`/`ior`，`shl/lshr/ashr` 通过 `llvm.vector.shift.integer` rule 筛选 `ishl`/`ilshr`/`iashr`；固定整数向量 `icmp` 通过 `llvm.vector.icmp.integer` rule 逐 lane 发射 `icmp` handler，结果重新组成 `<N x i1>` 绑定；固定浮点向量 `fcmp` 通过 `llvm.vector.fcmp.float` rule 逐 lane 发射 `fcmp` handler，结果重新组成 `<N x i1>` 绑定；固定浮点向量 `llvm.fabs/sqrt/canonicalize/floor/ceil/trunc/rint/nearbyint/round/roundeven` intrinsic 通过 `llvm.vector.{fabs,sqrt,canonicalize,floor,ceil,trunc,rint,nearbyint,round,roundeven}.float` rule 逐 lane 发射对应 float-unary handler；固定向量 `sitofp/uitofp/fptosi/fptoui` 通过 `llvm.vector.sitofp.float` / `llvm.vector.uitofp.float` / `llvm.vector.fptosi.float` / `llvm.vector.fptoui.float` rule 逐 lane 发射对应 float-cast handler；固定浮点向量 `fptrunc/fpext` 通过 `llvm.vector.fptrunc.float` / `llvm.vector.fpext.float` rule 逐 lane 发射 `fptrunc` / `fpext` handler；固定浮点向量 `fneg` 通过 `llvm.vector.fneg.float` rule 逐 lane 发射 `fneg` handler；固定浮点向量 `fadd/fsub/fmul/fdiv/frem` 分别通过 `llvm.vector.{fadd,fsub,fmul,fdiv,frem}.float` rule 逐 lane 发射 `fadd`/`fsub`/`fmul`/`fdiv`/`frem` handler。向量基值可以是 `undef` / `poison`；未写入 lane 在 VM 中选择 0 bit pattern 作为 `freeze` 的稳定代表值，`shufflevector` 的 undef mask lane 或来自未绑定源 lane 的结果 lane 会保持未绑定，但非 freeze 的 `extractelement`、固定向量 `bitcast`、固定整数向量 `zext/sext/trunc`、固定指针向量 `ptrtoint/inttoptr/addrspacecast`、固定整数向量二元运算、固定整数向量 `icmp`、固定浮点向量 `fcmp`、固定浮点向量 unary intrinsic、固定向量整数/浮点互转、固定向量 `llvm.vector.insert` / `llvm.vector.extract` / `llvm.experimental.vector.compress`、固定浮点向量 `fptrunc/fpext`、固定浮点向量 `fneg` 和固定浮点向量二元运算仍只允许读取已由受支持 `insertelement` / `phi` / `select` / `shufflevector` / `llvm.vector.reverse` / `llvm.vector.splice` / `llvm.experimental.vp.splice` / `llvm.vector.insert` / `llvm.vector.extract` / `llvm.experimental.vector.compress` / 固定向量 `bitcast` / 固定整数向量 `zext/sext/trunc` / 固定指针向量 `ptrtoint/inttoptr/addrspacecast` / 固定向量二元运算 / 固定整数向量 `icmp` / 固定浮点向量 `fcmp` / 固定浮点向量 unary intrinsic / 固定向量整数/浮点互转 / 固定浮点向量 `fptrunc/fpext` / 固定向量 `fneg` 写入的 lane。动态 lane 下标、vector condition select、跨 lane 重打包的向量 bitcast、未列入固定向量白名单的其它向量浮点 intrinsic、向量 load/store、向量参数/返回、scalable vector 和 q 寄存器 ABI 仍安全跳过。
fixed vector constrained `sitofp/uitofp/fptosi/fptoui/fptrunc/fpext` 属于上面的固定向量白名单：translator 先按 constrained metadata 做保守校验，再分别通过 `llvm.constrained.vector.sitofp.float`、`llvm.constrained.vector.uitofp.float`、`llvm.constrained.vector.fptosi.float`、`llvm.constrained.vector.fptoui.float`、`llvm.constrained.vector.fptrunc.float` 和 `llvm.constrained.vector.fpext.float` rule 逐 lane 发射普通 float-cast handler；它们读取未绑定 lane 的规则与对应普通 fixed-vector cast 完全一致。
固定整数向量 `llvm.stepvector` 通过 profile 中的 `llvm.vector.step` rule 为每个 lane 直接发射 `mov_imm`，并把所有 lane 重新组成 fixed vector 聚合绑定；它不读取其它向量基值，因此不受未写入 lane 规则影响。
固定整数向量 VP 二元 intrinsic 通过 profile 中的 `llvm.vp.vector.add.integer`、`llvm.vp.vector.sub.integer`、`llvm.vp.vector.mul.integer`、`llvm.vp.vector.udiv.integer`、`llvm.vp.vector.sdiv.integer`、`llvm.vp.vector.urem.integer`、`llvm.vp.vector.srem.integer`、`llvm.vp.vector.bitops.integer` 和 `llvm.vp.vector.shift.integer` rule 逐激活 lane 发射对应整数 ALU handler。VP inactive lane 按 LLVM poison lane 处理，不会进入 VM 寄存器绑定；如果后续 `extractelement` 直接读取 inactive lane，仍按未冻结 poison 的既有规则安全跳过。
固定整数向量 VP min/max 和饱和加减 intrinsic 通过 profile 中的 `llvm.vp.vector.smax.integer`、`llvm.vp.vector.smin.integer`、`llvm.vp.vector.umax.integer`、`llvm.vp.vector.umin.integer`、`llvm.vp.vector.uadd.sat.integer`、`llvm.vp.vector.usub.sat.integer`、`llvm.vp.vector.sadd.sat.integer` 和 `llvm.vp.vector.ssub.sat.integer` rule 逐激活 lane 发射对应 `ismax` / `ismin` / `iumax` / `iumin` / `iuadd_sat` / `iusub_sat` / `isadd_sat` / `issub_sat` handler。LLVM 21 没有 `llvm.vp.ushl.sat` / `llvm.vp.sshl.sat` intrinsic，因此 VP 饱和移位不声明为受支持子集。
固定整数向量 VP 一元 intrinsic 通过 profile 中的 `llvm.vp.vector.ctpop.integer`、`llvm.vp.vector.ctlz.integer`、`llvm.vp.vector.cttz.integer`、`llvm.vp.vector.abs.integer`、`llvm.vp.vector.bswap.integer` 和 `llvm.vp.vector.bitreverse.integer` rule 逐激活 lane 发射对应整数一元 handler。带 flag 的 `ctlz` / `cttz` / `abs` 只接受编译期 i1 常量 flag；该路径和 VP 二元一样保留 inactive lane 的 poison 语义，不生成 VM 寄存器绑定。
固定向量 `llvm.vp.select` 通过 profile 中的 `llvm.vp.select.vector_condition` rule 逐激活 lane 发射 `br_if` / `mov` / `br` 条件复制序列。该 rule 覆盖 fixed `<N x i1>` condition，translator 逐 lane 读取条件；`lane >= EVL` 的 inactive lane 不生成 VM 寄存器绑定。固定向量 `llvm.vp.merge` 通过 profile 中的 `llvm.vp.merge.vector_condition` rule 对所有 lane 发射 `mov_imm` / `icmp ult` / 双 `br_if` / `mov` 序列；translator 把 lane 编号和运行时 pivot 比较，只有 `cond lane == true` 且 `lane < pivot` 复制 then lane，否则复制 else lane，因此 merge 的结果 lane 全部有定义。
固定向量 `llvm.experimental.vp.reverse` 通过 profile 中的 `llvm.experimental.vp.reverse.element` rule 逐激活 lane 发射 `mov`，按反向 lane 顺序复制 source lane；固定向量 `llvm.experimental.vp.splat` 通过 `llvm.experimental.vp.splat.element` rule 逐激活 lane 发射 `mov`，把同一个标量复制到每个 active result lane。两者的 mask 和 EVL 必须是编译期常量，inactive lane 不生成 VM 寄存器绑定。
固定整数向量 `llvm.vp.zext` / `llvm.vp.sext` / `llvm.vp.trunc` 通过 profile 中的 `llvm.vp.vector.cast.integer` rule 逐激活 lane 发射 `zext` / `sext` / `trunc` handler。translator 只接受 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL、源和结果 lane 数相同、源和结果 lane 都是 integer，且宽度方向符合对应 cast；inactive lane 和其它 VP 指令一样保持 poison/未绑定。
固定向量 `llvm.vp.ptrtoint` / `llvm.vp.inttoptr` 通过 profile 中的 `llvm.vp.vector.cast.pointer` rule 逐激活 lane 发射 `zext` / `trunc` / `bitcast` handler。translator 只接受 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL、源和结果 lane 数相同、且每个激活 lane 都是 64 位 pointer bit 与整数 bit 之间的合法转换；`mask[lane] == false` 或 `lane >= EVL` 的 inactive lane 保持 poison/未绑定，不会生成 VM 寄存器绑定。
固定向量 `llvm.vp.icmp` 通过 profile 中的 `llvm.vp.vector.icmp.integer` / `llvm.vp.vector.icmp.pointer` rule 逐激活 lane 发射 `icmp` handler。translator 只接受 fixed vector、metadata 字符串谓词、常量 `<N x i1>` mask、常量 i32 EVL、结果 lane 为 `i1`、两个源 lane 数相同、且源 lane 同为 integer 或同为 pointer；inactive lane 和其它 VP 指令一样保持 poison/未绑定。
固定向量 `llvm.vp.sitofp/uitofp/fptosi/fptoui` 通过 profile 中的 `llvm.vp.vector.sitofp.float`、`llvm.vp.vector.uitofp.float`、`llvm.vp.vector.fptosi.float` 和 `llvm.vp.vector.fptoui.float` rule 逐激活 lane 发射对应 float-cast handler。固定浮点向量 `llvm.vp.fptrunc/fpext` 通过 `llvm.vp.vector.fptrunc.float` / `llvm.vp.vector.fpext.float` rule 逐激活 lane 发射 `fptrunc` / `fpext` handler。translator 只接受 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL、源和结果 lane 数相同，并要求整数/浮点 lane 类型和 `half` / `float` / `double` 窄化/扩宽方向满足对应 cast；inactive lane 和其它 VP 指令一样保持 poison/未绑定。
固定向量 `llvm.vp.fadd/fsub/fmul/fdiv/frem/minnum/maxnum/minimum/maximum/copysign` 通过 profile 中的 `llvm.vp.vector.fadd.float`、`llvm.vp.vector.fsub.float`、`llvm.vp.vector.fmul.float`、`llvm.vp.vector.fdiv.float`、`llvm.vp.vector.frem.float`、`llvm.vp.vector.minnum.float`、`llvm.vp.vector.maxnum.float`、`llvm.vp.vector.minimum.float`、`llvm.vp.vector.maximum.float` 和 `llvm.vp.vector.copysign.float` rule 逐激活 lane 发射对应浮点 ALU handler。translator 只接受 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL、源和结果 lane 数相同、且每个 lane 都是 `half` / `float` / `double`；inactive lane 和其它 VP 指令一样保持 poison/未绑定。
固定向量 `llvm.vp.fneg/fabs/sqrt/canonicalize/floor/ceil/roundtozero/rint/nearbyint/round/roundeven/sin/cos/exp/exp2/log/log10/log2` 通过 profile 中的 `llvm.vp.vector.{fneg,fabs,sqrt,canonicalize,floor,ceil,roundtozero,rint,nearbyint,round,roundeven,sin,cos,exp,exp2,log,log10,log2}.float` rule 逐激活 lane 发射对应浮点一元 handler，其中 `roundtozero` 发射 `ftrunc`。translator 只接受 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL、源和结果 lane 数相同、且每个 lane 都是 `half` / `float` / `double`；inactive lane 和其它 VP 指令一样保持 poison/未绑定。`llvm.vp.trunc` 在 LLVM 21 是整数 cast intrinsic，仍由 `llvm.vp.vector.cast.integer` 边界处理。
固定向量 `llvm.vp.lrint/llrint` 通过 profile 中的 `llvm.vp.vector.lrint.float` / `llvm.vp.vector.llrint.float` rule 逐激活 lane 发射 `flrint` / `fllrint` handler。translator 只接受 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL、source lane 数与 result lane 数一致、source lane 是 `half` / `float` / `double`、result lane 是 i32/i64；inactive lane 和其它 VP 指令一样保持 poison/未绑定。LLVM 21 没有 `llvm.vp.lround` / `llvm.vp.llround`。
固定向量 `llvm.vp.fma/fmuladd` 通过 profile 中的 `llvm.vp.vector.fma.float` 和 `llvm.vp.vector.fmuladd.float` rule 逐激活 lane 发射对应浮点三元 handler。translator 只接受 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL、三个源和结果 lane 数相同、且每个 lane 都是 `half` / `float` / `double`；inactive lane 和其它 VP 指令一样保持 poison/未绑定。
固定向量 `llvm.vp.fcmp` 通过 profile 中的 `llvm.vp.vector.fcmp.float` rule 逐激活 lane 发射 `fcmp` handler。translator 只接受 fixed vector、metadata 字符串谓词、常量 `<N x i1>` mask、常量 i32 EVL、结果 lane 为 `i1`、两个源 lane 数相同、且每个源 lane 都是 `half` / `float` / `double`；inactive lane 和其它 VP 指令一样保持 poison/未绑定。
固定向量 `llvm.vp.is.fpclass` 通过 profile 中的 `llvm.vp.vector.is.fpclass.float` rule 逐激活 lane 发射 `fpclass` handler。translator 只接受 fixed vector、常量 `<N x i1>` mask、常量 i32 EVL、结果 lane 为 `i1`、source lane 是 `half` / `float` / `double`、class mask 在 LLVM `FPClassTest` 当前 `0x03ff` 范围内；inactive lane 和其它 VP 指令一样保持 poison/未绑定。
补充：上段中的向量 load/store 安全跳过只指 packed、scalable、q ABI 或 data layout 不能证明为逐 lane 字节连续的情况；普通 fixed vector `load` / `store` 如果能由 data layout 证明为逐 lane 字节连续，会通过 `llvm.memory.vector.load` / `llvm.memory.vector.store` rule 展开为标量 `load` / `store` / `gep` / `mov`；volatile fixed vector `load` / `store` 使用 `llvm.memory.volatile.vector.load` / `llvm.memory.volatile.vector.store` rule 展开为 `volatile_load` / `volatile_store` / `gep` / `mov`。
补充：上段中的 vector condition select 安全跳过只指 condition 不是 fixed `<N x i1>`、condition 与结果 lane 数不一致、then/else lane 未由受支持 lowering 写入、scalable vector 或 q ABI 的情况；普通 fixed vector condition select 会通过 `llvm.select.vector_condition` rule 对每个 lane 展开为 `br_if` / `mov` / `br`。
补充：上段中的动态 lane 下标安全跳过只指下标不是 x 寄存器可承载整数、动态 `insertelement` 的基础向量存在未定义 lane、或动态 `extractelement` 的源向量存在未定义 lane 的情况；否则固定向量动态 `insertelement` / `extractelement` 会分别通过 `llvm.vector.insert.dynamic_element` / `llvm.vector.extract.dynamic_element` rule 展开为 `mov_imm` / `icmp` / `br_if` / `mov` 链。
补充：固定向量 `llvm.vector.interleave2..8` 通过 profile 中的 `llvm.vector.interleave.element` rule 逐 lane 发射 `mov`，按 LLVM 语义把第 0..factor-1 个输入向量交织成结果向量；translator 只接受 fixed vector 返回、factor 为 2..8、结果 lane 数可被 factor 整除、每个输入 operand lane 数等于 `result_lanes / factor`、每个 lane 类型与对应结果 lane 完全相同，且输入 lane 已由受支持 lowering 写入的情况。固定向量 `llvm.vector.deinterleave2..8` 通过 profile 中的 `llvm.vector.deinterleave.element` rule 逐 lane 发射 `mov`，按 LLVM 语义把单个输入向量拆成 factor 个结果向量；translator 只接受返回值为 factor 个 fixed vector 字段组成的 struct、factor 为 2..8、输入 lane 数可被 factor 整除、每个结果 vector lane 数等于 `input_lanes / factor`、每个 lane 类型与对应输入 lane 完全相同，且输入 lane 已由受支持 lowering 写入的情况。scalable vector、q ABI 宽向量、factor 超出 2..8、lane 数不匹配、lane 类型不匹配或读取未绑定 lane 时仍安全跳过。
补充：上段中的指针向量比较安全跳过只指 operand 不是 fixed `<N x ptr>`、结果不是 fixed `<N x i1>`、左右 lane 数不一致、pointer lane 未由受支持 lowering 写入、scalable vector 或 q ABI 的情况；普通 fixed pointer vector `icmp` 会通过 `llvm.vector.icmp.pointer` rule 逐 lane 发射 `icmp` handler，谓词沿用 LLVM `icmp` predicate 映射。
固定 mask 向量 `llvm.vp.cttz.elts` 通过 profile 中的 `llvm.vp.cttz.elts` rule 展开为标量计数控制流；translator 先把结果初始化为 `min(EVL, lane_count)`，再按高 lane 到低 lane 的顺序只对 VP mask 和 EVL 同时启用的 lane 检查计数 mask，命中 true lane 时用 `mov` 覆盖为当前 lane 编号。该路径不读取 VP inactive lane，因此 inactive lane 可以保持 poison/未绑定；激活 lane 必须是可物化的 i1。
非 scalable `llvm.experimental.get.vector.length` 通过 profile 中的 `llvm.experimental.get.vector.length.integer` rule 展开为标量 `min(AVL, VF)` 控制流；translator 对 i1/i8/i16 AVL 先通过 profile `zext` action 零扩展到 i32 比较宽度，i32/i64 AVL 保持原宽度，再按比较宽度物化 VF，用 `icmp ult` 判断 AVL 是否小于 VF，最后把两个候选值截断到 i32 并通过 `br_if` / `mov` 选择结果。该路径只覆盖 `scalable=false` 的固定 VF 查询，`scalable=true` 仍必须等待显式 vscale 语义建模。
固定整数向量 unary intrinsic `llvm.ctpop/ctlz/cttz/abs/bswap/bitreverse` 通过 `llvm.vector.{ctpop,ctlz,cttz,abs,bswap,bitreverse}.integer` rule 逐 lane 发射对应 integer-unary handler，结果重新组成固定整数向量；后续 `extractelement`、固定整数向量二元运算、固定整数向量 `icmp` 和其它受支持固定向量操作可以读取这些 lane。
固定整数向量 binary intrinsic `llvm.smax/smin/umax/umin/uadd.sat/usub.sat/sadd.sat/ssub.sat/ushl.sat/sshl.sat` 通过 `llvm.vector.{smax,smin,umax,umin,uadd.sat,usub.sat,sadd.sat,ssub.sat,ushl.sat,sshl.sat}.integer` rule 逐 lane 发射对应 integer-binary handler，结果重新组成固定整数向量；后续 `extractelement`、固定整数向量二元运算、固定整数向量 `icmp` 和其它受支持固定向量操作可以读取这些 lane。
固定整数向量 ternary intrinsic `llvm.fshl/fshr` 通过 `llvm.vector.{fshl,fshr}.integer` rule 逐 lane 发射对应 integer-ternary handler，结果重新组成固定整数向量；后续 `extractelement`、固定整数向量二元运算、固定整数向量 `icmp` 和其它受支持固定向量操作可以读取这些 lane。
固定向量 VP 整数 ternary intrinsic `llvm.vp.fshl/fshr` 通过 `llvm.vp.vector.{fshl,fshr}.integer` rule 只对常量 mask 和常量 EVL 共同启用的 lane 发射对应 integer-ternary handler，结果重新组成只含激活 lane 绑定的固定向量；后续只能读取这些激活 lane，读取未激活 lane 仍按 poison/未绑定处理并安全跳过。
固定整数向量值保持 intrinsic `llvm.expect/llvm.expect.with.probability` 通过 `llvm.vector.expect.integer` / `llvm.vector.expect.with_probability.integer` rule 逐 lane 发射 `mov`，结果重新组成固定整数向量；`expected` lane 和 `probability` 仅是优化提示，未绑定或 poison/undef 的 value lane 会继续保持未绑定，只有后续固定向量 `freeze` 可以稳定它。
固定整数向量 reduction intrinsic `llvm.vector.reduce.add/mul/and/or/xor/smax/smin/umax/umin` 通过 `llvm.vector.reduce.{add,mul,and,or,xor,smax,smin,umax,umin}.integer` rule 按 lane 顺序折叠为一个标量 x 寄存器结果；每一步仍由 profile 发射已有整数 ALU handler，因此 bytecode、opcode alias、decoded_width 和 runtime handler 都继续由 ISA instruction identity 驱动。
固定浮点向量 reduction intrinsic `llvm.vector.reduce.fadd/fmul/fmin/fmax/fminimum/fmaximum` 通过 `llvm.vector.reduce.{fadd,fmul,fmin,fmax,fminimum,fmaximum}.float` rule 按 lane 顺序折叠为一个标量 x 寄存器结果；`fadd/fmul` 从 LLVM intrinsic 的标量 accumulator 开始，`fmin/fmax/fminimum/fmaximum` 从第一个 lane 开始；每一步仍由 profile 发射已有浮点 ALU handler，因此 bytecode、opcode alias、decoded_width 和 runtime handler 都继续由 ISA instruction identity 驱动。
固定向量 VP reduction intrinsic 通过 `llvm.vp.reduce.*` rule 从标量 start value 开始，仅折叠常量 mask 与常量 EVL 共同启用的 lane；该路径不新增专用 runtime handler，而是复用 profile 已声明的整数/浮点 ALU handler，因此 bytecode 中仍保留真实执行的 handler 序列，all-inactive 归约只保留 start value 绑定。
固定向量 `llvm.experimental.vector.extract.last.active` 通过 profile 中的 `llvm.experimental.vector.extract.last.active` rule 发射一次 `mov`，结果是最高 active lane；mask 全 false 时结果来自 passthru。该路径不读取 inactive lane，也不为 inactive lane 生成 VM 寄存器绑定。
补充：固定向量饱和浮点转整数 `llvm.fptosi.sat/fptoui.sat` 和固定向量 round-to-int `llvm.lrint/llrint/lround/llround` 也属于上述固定向量白名单；它们写回的整数 lane 后续可被 `extractelement`、整数比较、整数二元运算和其它受支持向量操作读取。
固定浮点向量 binary intrinsic `llvm.copysign/minnum/maxnum/minimum/maximum` 的结果 lane 也会写回固定向量聚合绑定，后续 `extractelement`、`fcmp`、浮点向量二元运算、浮点 cast 和其它受支持向量操作可以读取这些 lane；未列入白名单的 vector floating intrinsic 仍不被视为可读来源。
固定浮点向量 ternary intrinsic `llvm.fma/fmuladd` 的结果 lane 同样会写回固定向量聚合绑定，后续受支持的固定向量操作可以读取这些 lane。
固定浮点向量 `llvm.pow` / `llvm.powi` 的结果 lane 同样会写回固定向量聚合绑定，后续受支持的固定向量操作可以读取这些 lane。
固定浮点向量 `llvm.is.fpclass` 的结果 lane 同样会写回固定向量聚合绑定，后续受支持的固定向量操作可以读取这些 i1 lane。
固定浮点向量数学 unary intrinsic `llvm.sin/cos/exp/exp2/log/log10/log2` 的结果 lane 同样会写回固定向量聚合绑定，后续受支持的固定向量操作可以读取这些 lane。
固定向量 `llvm.fptosi.sat` / `llvm.fptoui.sat` 的结果 lane 同样会写回固定向量聚合绑定，后续 `extractelement`、整数比较、整数二元运算和其它受支持向量操作可以读取这些整数 lane。
固定向量 `llvm.lrint` / `llvm.llrint` / `llvm.lround` / `llvm.llround` 的结果 lane 同样会写回固定向量聚合绑定，后续 `extractelement`、整数比较、整数二元运算和其它受支持向量操作可以读取这些 i32/i64 lane。

`llvm.set.loop.iterations.*`、`llvm.start.loop.iterations.*`、`llvm.test.set.loop.iterations.*` 和 `llvm.test.start.loop.iterations.*` 通过 profile 中的对应 `llvm.*.integer` lowering rule 处理，只接受 i1/i8/i16/i32/i64 标量整数计数器。VM runtime 不维护额外硬件循环状态：`set.loop.iterations` 只发射 `fake_nop` 保留字节码扰动位置；`start.loop.iterations` 通过 `mov` 返回原始计数；`test.set.loop.iterations` 通过 `mov_imm 0` 和 `icmp ne` 返回 `counter != 0`；`test.start.loop.iterations` 通过 `mov` 和 `icmp ne` 形成 `{ counter, counter != 0 }` 聚合绑定。`llvm.loop.decrement.*` 通过 profile 中的 `llvm.loop.decrement.integer` lowering rule 发射 `mov_imm`、`isub`、`icmp`，只支持一个 i1/i8/i16/i32/i64 标量整数计数器参数和 i1 返回，语义为先计算 `counter - 1`，再返回剩余计数是否非零。`llvm.loop.decrement.reg.*` 通过 profile 中的 `llvm.loop.decrement.reg.integer` lowering rule 发射 `isub`，只支持两个同宽 i1/i8/i16/i32/i64 整数参数和同宽整数返回。

`llvm.readcyclecounter` 和 `llvm.readsteadycounter` 通过 profile 中的 `read_counter(cycle|steady)` semantic 进入 runtime；runtime 生成 LLVM IR 时声明并调用对应 LLVM intrinsic，结果以 i64 bit pattern 写入 x 寄存器。translator 只接受无参数且返回 i64 的声明，参数数量不匹配或返回宽度不是 i64 时安全跳过该函数。

`llvm.vscale.*` 通过 profile 中的 `read_vscale()` semantic 进入 runtime；runtime 生成 LLVM IR 时声明并调用 `llvm.vscale.i64`，再按 bytecode operand 中记录的返回位宽截断到原 LLVM 标量整数类型。translator 只接受无参数且返回 i1/i8/i16/i32/i64 整数标量的声明，参数数量不匹配或返回宽度超出 x 寄存器整数子集时安全跳过该函数。

`llvm.get.rounding` 通过 profile 中的 `read_rounding()` semantic 进入 runtime；runtime 生成 LLVM IR 时声明并调用 LLVM `llvm.get.rounding` intrinsic，返回的 i32 rounding mode 先零扩展到 i64，再按 bytecode operand 中记录的返回位宽写入 x 寄存器。translator 只接受无参数且返回 i32 的声明，其它签名安全跳过该函数。

`llvm.flt.rounds` 在 LLVM 21 中会被 IR parser 规范化到 `llvm.get.rounding` intrinsic 名称，因此通常通过同一条 `read_rounding()` semantic 进入 runtime，baseline 和虚拟化后都观察同一个当前舍入状态。若某个前端保留 `llvm.flt.rounds` callee 名称，translator 也只接受无参数且返回 i32 的声明，并可通过 profile 中的 `read_flt_rounds()` semantic 生成独立 handler；其它签名安全跳过该函数。

`llvm.set.rounding` 通过 profile 中的 `write_rounding(...)` semantic 进入 runtime；runtime 生成 LLVM IR 时声明并调用 LLVM `llvm.set.rounding` intrinsic。translator 只接受单个 i32 参数和 void 返回，其它签名安全跳过该函数。

`llvm.get.fpenv.*` / `llvm.set.fpenv.*` 和 `llvm.get.fpmode.*` / `llvm.set.fpmode.*` 通过 profile 中的 `read_fpenv()` / `write_fpenv(...)` / `read_fpmode()` / `write_fpmode(...)` semantic 进入 runtime；runtime 生成 LLVM IR 时声明并调用 i32/i64 两种状态宽度的 LLVM intrinsic，并按 bytecode operand 中记录的状态宽度选择调用目标。`llvm.reset.fpenv` 和 `llvm.reset.fpmode` 通过 `reset_fpenv()` / `reset_fpmode()` semantic 进入 runtime。translator 只接受 get 无参数且返回 i32/i64、set 单 i32/i64 参数且 void 返回、reset 无参数且 void 返回的声明，其它签名安全跳过该函数。

`llvm.thread.pointer` 通过 profile 中的 `read_thread_pointer()` semantic 进入 runtime；runtime 生成 LLVM IR 时声明并调用 LLVM `llvm.thread.pointer.p0` intrinsic，返回 pointer 再转成 i64 bit pattern 写入 x 寄存器。translator 只接受无参数且返回 pointer 的声明；有参数、非 pointer 返回或非 64 位 pointer 模型会安全跳过该函数。

`llvm.stacksave` 和 `llvm.stackrestore` 通过 profile 中的 `stack_save()` / `stack_restore(...)` semantic 进入 runtime；runtime 生成 LLVM IR 时声明并调用对应 LLVM intrinsic，`stacksave` 返回的 pointer 按 64 位地址 bit 写入 x 寄存器，`stackrestore` 再从 x 寄存器还原 pointer。translator 只接受无参数 pointer 返回的 `stacksave` 和单 pointer 参数 void 返回的 `stackrestore`，其它签名安全跳过该函数。

`llvm.clear_cache` 通过 profile 中的 `clear_cache(...)` semantic 进入 runtime；runtime 生成 LLVM IR 时声明并调用 LLVM `llvm.clear_cache` intrinsic。translator 只接受两个 pointer 参数和 void 返回，其它签名安全跳过该函数。

`llvm.pseudoprobe` 通过 profile 中的 `pseudo_probe(...)` semantic 进入 runtime；runtime 生成 LLVM IR 时声明并调用 LLVM `llvm.pseudoprobe(i64, i64, i32, i64)` intrinsic。translator 只接受四个编译期整数参数和 void 返回，`probe_type` 必须能放入 i32；动态参数、非整数参数或其它签名安全跳过该函数。

`llvm.prefetch` 通过 profile 中的 `prefetch(...)` semantic 进入 runtime；runtime 生成 LLVM IR 时声明并调用 LLVM `llvm.prefetch.p0(ptr, i32, i32, i32)` intrinsic。translator 只接受一个 pointer 参数、三个编译期整数 immarg 参数和 void 返回，`rw` 必须为 0 或 1，`locality` 必须在 0..=3，`cache` 必须为 0 或 1；runtime handler 会按这 16 种合法 hint 组合生成常量 intrinsic 调用，避免把动态 VM operand 传给 LLVM `immarg`。

`llvm.objectsize` 通过 profile 中的 `llvm.objectsize.integer` lowering rule 发射 `mov_imm` 或 profile 选择的等价常量物化路径。translator 能静态证明对象大小时优先折叠：直接 fixed alloca、常量元素个数 alloca、直接 `GlobalVariable`、可剥离 pointer `bitcast` / `addrspacecast` 指令或常量表达式之后仍回到上述静态对象的基址，以及只含常量下标且最终回到上述对象的 GEP；结果是从当前指针偏移到对象末尾的剩余字节数。0 字节 alloca、零大小数组 alloca、0 字节 global 和 0 偏移零大小 GEP 会折叠为 size 0。基址是函数参数、native call 返回指针或其它普通未知指针时，translator 按 LLVM unknown object size 规则折叠：`min=true` 返回 0，`min=false` 返回结果位宽全 1，这条规则不受 `dynamic` 标志影响；直接动态 alloca 或只经过 pointer `bitcast` / `addrspacecast` 回到动态 alloca 的形态在 `dynamic=true` 时通过 `llvm.objectsize.dynamic_alloca` lowering rule 发射 `imul` 运行时计算 `count * elem_size`，动态 alloca 在 `dynamic=false` 时仍使用同一 unknown fallback；未落入下段“可记录 offset”子集的动态 GEP 在 `dynamic=false` 时使用同一 unknown fallback，在 `dynamic=true` 时安全跳过。`nullunknown=false` 的 null 指针返回 0，`nullunknown=true` 的 null 指针使用同一 unknown 规则。`llvm.objectsize` 如果请求 `dynamic=true` 且对象大小依赖未记录 offset 的动态 GEP、非可剥离 cast 的动态 alloca 派生指针、偏移越界、immarg 非 i1 常量或结果宽度装不下大小，仍安全跳过该函数；这不影响普通动态 alloca 和普通动态 GEP lowering。

`llvm.objectsize` 对动态 GEP 的当前已实现子集是：GEP 链必须从静态对象或动态 alloca 起始，或从它们的可剥离 pointer `bitcast` / `addrspacecast` 起始；每一段 GEP instruction 必须带 `inbounds`，translator 必须能在 lowering 每段 GEP 时记录运行时 byte offset。静态对象路径用 profile `llvm.objectsize.dynamic_gep_offset` / `llvm.objectsize.dynamic_gep_accumulate` 记录或累加 offset，再由 `llvm.objectsize.static_gep` rule 发射 `isub` 计算 `total_size - offset`。动态 alloca 路径第一段通过 `llvm.objectsize.dynamic_gep_offset` rule 发射 `isub` 保存 `gep_ptr - base_ptr`；后续链式 GEP 先用同一 rule 计算当前段 delta，再通过 `llvm.objectsize.dynamic_gep_accumulate` rule 发射 `iadd` 累加 `previous_offset + delta`；最终 `llvm.objectsize.dynamic_alloca_gep` rule 发射 `imul` + `isub` 计算动态剩余字节数。非 inbounds、基址不是静态对象或动态 alloca 派生链、或任一段 offset 无法记录的动态 GEP 仍属于 safe-skip 边界。

补充：上述浮点 intrinsic 白名单包括 `half`/`float`/`double` 标量 `llvm.fabs`、`llvm.sqrt`、`llvm.canonicalize`、`llvm.floor`、`llvm.ceil`、`llvm.trunc`、`llvm.rint`、`llvm.nearbyint`、`llvm.round`、`llvm.roundeven`、`llvm.lrint`、`llvm.llrint`、`llvm.lround`、`llvm.llround`、`llvm.sin`、`llvm.cos`、`llvm.exp`、`llvm.exp2`、`llvm.log`、`llvm.log10`、`llvm.log2`、`llvm.pow`、`llvm.powi`、`llvm.fma`、`llvm.fmuladd`、`llvm.minnum`、`llvm.maxnum`、`llvm.minimum`、`llvm.maximum` 和 `llvm.copysign`。`llvm.fabs` 必须通过 profile 中的 `llvm.fabs.float` lowering rule 发射 `fabs`，再由 `float_unary(fabs, ...)` semantic 驱动 runtime；`llvm.sqrt` 必须通过 profile 中的 `llvm.sqrt.float` lowering rule 发射 `fsqrt`，再由 `float_unary(fsqrt, ...)` semantic 驱动 runtime；`llvm.canonicalize` 必须通过 profile 中的 `llvm.canonicalize.float` lowering rule 发射 `fcanonicalize`，再由 `float_unary(fcanonicalize, ...)` semantic 驱动 runtime；`llvm.floor`、`llvm.ceil`、`llvm.trunc`、`llvm.rint`、`llvm.nearbyint`、`llvm.round`、`llvm.roundeven`、`llvm.sin`、`llvm.cos`、`llvm.exp`、`llvm.exp2`、`llvm.log`、`llvm.log10` 和 `llvm.log2` 必须分别通过 profile 中的同名 `llvm.*.float` lowering rule 发射 `ffloor`、`fceil`、`ftrunc`、`frint`、`fnearbyint`、`fround`、`froundeven`、`fsin`、`fcos`、`fexp`、`fexp2`、`flog`、`flog10` 和 `flog2`，再由对应 `float_unary(...)` semantic 驱动 runtime；`llvm.lrint`、`llvm.llrint`、`llvm.lround` 和 `llvm.llround` 必须分别通过 profile 中的 `llvm.lrint.float`、`llvm.llrint.float`、`llvm.lround.float` 和 `llvm.llround.float` lowering rule 发射 `flrint`、`fllrint`、`flround` 和 `fllround`，再由 `float_round_to_int(...)` semantic 驱动 runtime；`llvm.pow` 必须通过 profile 中的 `llvm.pow.float` lowering rule 发射 `fpow`，再由 `float_bin(fpow, ...)` semantic 驱动 runtime；`llvm.powi` 必须通过 profile 中的 `llvm.powi.float` lowering rule 发射 `fpowi`，再由 `float_int_bin(fpowi, ...)` semantic 驱动 runtime，且指数 operand 必须是 i32；`llvm.fma` 必须通过 profile 中的 `llvm.fma.float` lowering rule 发射 `ffma`，再由 `float_ternary(fma, ...)` semantic 驱动 runtime；`llvm.fmuladd` 必须通过 profile 中的 `llvm.fmuladd.float` lowering rule 发射 `ffmuladd`，再由 `float_ternary(fmuladd, ...)` semantic 驱动 runtime；`llvm.minnum` 和 `llvm.maxnum` 必须通过 profile 中的 `llvm.minnum.float` / `llvm.maxnum.float` lowering rule 发射 `fminnum` / `fmaxnum`，再由 `float_bin(fminnum/fmaxnum, ...)` semantic 驱动 runtime；`llvm.minimum` 和 `llvm.maximum` 必须通过 profile 中的 `llvm.minimum.float` / `llvm.maximum.float` lowering rule 发射 `fminimum` / `fmaximum`，再由 `float_bin(fminimum/fmaximum, ...)` semantic 驱动 runtime；`llvm.copysign` 必须通过 profile 中的 `llvm.copysign.float` lowering rule 发射 `fcopysign`，再由 `float_bin(fcopysign, ...)` semantic 驱动 runtime。runtime 的 f16 路径先把 half 精确扩展为 f32、调用对应 LLVM `*.f32` intrinsic 或 f32 算术，再截回 f16 bit；round-to-int f16 路径直接调用 LLVM `*.f16` intrinsic。浮点 intrinsic 不能退化为固定外部 `sqrt/sqrtf/pow/powf/fma/fmaf/fmin/fminf/fmax/fmaxf` 符号；选择 `fmuladd` 是为了让包含 `ffma` 或 `ffmuladd` handler 的完整 profile 在未实际执行乘加字节码时也不会给所有虚拟化样本引入 libm 链接要求。

其中 `llvm.threadlocal.address` 的 `tls_addr` 是 VM SSA 绑定标记：TLS `GlobalValue` operand 留在 AMICE 生成的私有 native thunk 中，由 `call_native` 取回运行时地址，不能写入 const_pool 或作为普通整数立即数固化。

普通 LLVM `GlobalValue` 指针 operand，例如 `load i32, ptr @g`、`store ..., ptr @g`、可剥离 pointer `bitcast` / `addrspacecast` 后指向 `GlobalValue` 的常量表达式，以及全部下标为常量的 GEP，通过 AMICE 生成的私有 `global_addr` native thunk 取回重定位后的运行时基址，再由 profile 中的 `global_addr` 指令绑定到 VM x 寄存器。常量 GEP 的 base 可以是 `GlobalValue`、可剥离 pointer cast，或另一层可递归规约的常量 GEP，每层常量 GEP 偏移继续走 profile 的 `llvm.gep.constant` 规则。

常量表达式形式的 `ptrtoint` 会先按上述规则物化运行时指针，再通过 profile 的 `llvm.constexpr.ptrtoint` 规则发射 `zext` / `trunc` / `bitcast`；常量表达式形式的 `inttoptr` 会先物化整数 operand，再通过 `llvm.constexpr.inttoptr` 发射同一组 cast handler。整数常量表达式形式的 `add`、`sub`、`mul`、`udiv`、`sdiv`、`urem`、`srem`、`xor`、`and`、`or`、`shl`、`lshr` 和 `ashr` 会递归物化左右 operand，再通过 `llvm.constexpr.integer.binop` 规则发射 profile ISA 中对应的整数 ALU handler；如果输入模块中仍存在整数常量表达式形式的 `zext`、`sext`、`trunc` 或等宽 `bitcast`，translator 会通过 `llvm.constexpr.integer.cast` 规则发射 profile ISA 中对应的 cast handler。LLVM 21 文本 IR parser 通常已经拒绝 `zext` / `sext` / `trunc` / `shl` / `or` 这类历史 ConstantExpr 写法，因此集成测试主要覆盖仍能由 LLVM 21 接收的 pointer cast、GEP，以及 `add` / `sub` / `xor` 形式的整数二元常量表达式。vector/aggregate 常量表达式、非等宽 integer `bitcast`、不能规约为可重定位 `GlobalValue` 基址加若干层常量字节偏移的非空指针常量表达式，仍安全跳过，避免在 VM const_pool 中固化地址或丢失目标重定位语义。

`insertvalue`、`extractvalue`、aggregate `select`、aggregate parameter、aggregate return、普通/volatile aggregate load/store、native call aggregate parameter 和 native call aggregate return 支持直接 struct、直接固定数组、嵌套 struct、固定数组组成的小聚合，前提是最终叶子字段都是整数、指针、`half`、`float` 或 `double` 标量。direct aggregate parameter 在 wrapper 入口按叶子字段展平成 host-to-VM x 参数槽，并在 translator 初始化时恢复成 VM 聚合绑定；direct/indirect native call aggregate parameter 在调用点按同一 leaf 顺序展平成 native arg 槽，thunk 入口再重建为 callee 的真实 struct/固定数组参数；空聚合参数允许出现在 direct 函数签名以及 direct/indirect native call 签名中，它只建立空 aggregate binding，不消耗 host-to-VM 参数槽或 native arg 槽。direct、native 和 indirect call 的固定数组返回都按同一规则展平到 VM 返回槽；空聚合返回允许出现在 direct 函数签名以及 direct/indirect native call 签名中，它只发射 VM `ret` 或 `call_native ret_count=0`，不消耗 ABI 返回槽。translator 按 LLVM 聚合类型声明顺序把叶子字段展平成 VM 聚合绑定槽；例如 `{ i8, { i32, i64 }, [2 x i16] }` 会映射为 `i8/i32/i64/i16/i16` 五个槽。普通和 volatile aggregate load/store 额外使用 module data layout 计算每个叶子字段的 ABI 字节偏移，偏移为 0 的字段直接走 `load`/`store` 或 `volatile_load`/`volatile_store`，非零偏移字段先走 profile `gep` 再走相同内存 handler；store 源必须来自已支持的聚合 lowering，缺失 undef 字段或未经过 `freeze` 的 undef/poison 聚合会安全跳过。非 volatile 空聚合 load/store、空聚合 `freeze` 和空聚合 `select` 不产生 VM 状态变化，translator 会写入空 aggregate binding 或直接 no-op，不发固定解释器指令；volatile 空聚合 load/store 必须分别通过 profile `llvm.memory.volatile.empty_aggregate.load` / `llvm.memory.volatile.empty_aggregate.store` rule 物化地址 operand，并发射 `sideeffect` handler 保留 volatile 内存访问的可见顺序约束，其中 volatile 空聚合 load 仍写入空 aggregate binding。aggregate `select` 的 then/else 两侧可以是已支持聚合 lowering 产生的绑定，也可以是无 `undef` / `poison` 字段的 LLVM 常量 struct / 固定数组；translator 只发射一次 `br_if`，再在 then/else 片段按叶子字段执行 `llvm.select.aggregate` rule 中的 `mov`，最后把字段寄存器组成新的 aggregate binding。`insertvalue` 插入整个子 struct 或固定数组时，插入值可以来自已支持聚合 lowering，也可以是无 `undef` / `poison` 字段的 LLVM 常量 struct / 固定数组；`extractvalue` 读取整个子 struct 或固定数组时，translator 会按 profile 中的 `llvm.aggregate.insert.subaggregate` / `llvm.aggregate.extract.subaggregate` rule 逐叶子字段发射 `mov`，并把结果重新组成 VM 聚合绑定；缺失的 undef 字段保持未绑定，只有后续读取该字段时才安全跳过。vector 叶子、非 `half`/`float`/`double` 浮点叶子、atomic aggregate load/store、超过 8 个 host-to-VM 或 native_call 参数槽的聚合参数和超过 ABI 返回槽数量的聚合仍必须安全跳过。

aggregate `store` 的源可以来自已支持聚合 lowering 产生的绑定，也可以是无 `undef` / `poison` 字段的 LLVM 常量 struct / 固定数组；常量聚合字段包含 `undef` / `poison`、vector 叶子或非 `half` / `float` / `double` 浮点叶子时仍安全跳过，除非先经过受支持的 `freeze`。

aggregate `ret` 的返回值可以来自已支持聚合 lowering 产生的绑定，也可以是无 `undef` / `poison` 字段的 LLVM 常量 struct / 固定数组；返回字段仍按 ABI return register 顺序复制到 profile 声明的返回槽。

direct/indirect native call 的聚合实参可以来自已支持聚合 lowering，也可以是无 `undef` / `poison` 字段的 LLVM 常量 struct / 固定数组；translator 按 native callee 签名的 leaf 顺序把常量字段物化到 native arg 槽，字段宽度或 leaf 数量不匹配时安全跳过。

`extractvalue` 的源聚合可以来自已支持聚合 lowering，也可以是无 `undef` / `poison` 字段的 LLVM 常量 struct / 固定数组；标量字段仍通过 `llvm.aggregate.extract` rule 发射 `mov`，整个子 struct / 固定数组仍通过 `llvm.aggregate.extract.subaggregate` 逐 leaf `mov` 后组成新的 aggregate binding。

`insertvalue` 的 seed 聚合可以是 `undef` / `poison`、已支持聚合 lowering，或字段中允许包含 `undef` / `poison` 的 LLVM 常量 struct / 固定数组；translator 必须保留常量 seed 中已经定义的 leaf 字段，只把未定义 leaf 记录为空槽。后续如果读取空槽，仍按未冻结 undef/poison 边界安全跳过。

translator 计算 aggregate / fixed vector memory offset 时必须使用 module `target datalayout`。如果输入是手写 LLVM IR 且 datalayout 为空，当前实现按本仓库测试和默认 clang codegen 使用的 x86_64 SysV datalayout 作为保守 fallback，避免 i64、指针和聚合 padding 在 VM bytecode 中按空 layout 错位。

`sret` 不是单独的 VM 聚合返回槽：LLVM 前端已经把它表示为带 `sret(T)` 参数属性的返回缓冲区指针和 `void ret`。translator 把这个隐藏返回指针当作普通 pointer 参数物化，函数体内对返回字段的写入仍通过 profile `store` / `gep` handler 执行；wrapper、native thunk 和 indirect adapter 必须保留 typed `sret(T)` call-site / parameter 属性，保证宿主 ABI 仍由 LLVM 后端处理。`byval(T)` 参数同样按 pointer 值进入 VM，不由 translator 展平或复制结构体；direct native call、indirect native call、adapter 内部 call 和 native thunk 到 adapter 的 call 都必须保留 `byval(T)` / `align` 等 call-site 参数属性，由 LLVM 后端完成真实按值传参。

aggregate `phi` 支持同一组直接 struct / 固定数组小聚合。translator 会在进入 basic block lowering 前为 phi 结果预分配稳定的叶子字段寄存器，并在每条 predecessor edge 上执行 `llvm.aggregate.phi.edge_move` rule，把 incoming 聚合字段复制到这些目标寄存器。固定向量 `phi` 使用同一套 predecessor edge move 模型，但 rule 名称是 `llvm.vector.phi.edge_move`，字段单位是固定向量 lane。incoming 聚合或固定向量可以来自已支持的 lowering，也可以是无 `undef` / `poison` 字段或 lane 的 LLVM 常量 struct / 固定数组 / 固定向量；未冻结 undef/poison、缺失字段/lane、vector 叶子或字段/lane 类型不匹配时仍安全跳过。

fixed vector `load` / `store` 当前不启用 `q` 寄存器文件，而是使用 `llvm.memory.vector.load` / `llvm.memory.vector.store` profile rule 展开为逐 lane 的 `load` / `store` / `gep` / `mov` 标量 VM 指令；volatile fixed vector `load` / `store` 使用 `llvm.memory.volatile.vector.load` / `llvm.memory.volatile.vector.store` rule，并把每个 lane 发射成 `volatile_load` / `volatile_store`。fixed vector `llvm.masked.load` / `llvm.masked.store` 使用 `llvm.memory.masked.vector.load` / `llvm.memory.masked.vector.store` rule：mask 必须是编译期常量 `<N x i1>`，enabled lane 才发射真实内存访问，disabled `masked.load` lane 从 passthru lane 复制，disabled `masked.store` lane 不发射写入。fixed vector `llvm.vp.load` / `llvm.vp.store` 复用同一组 masked vector memory rule：mask 必须是编译期常量 `<N x i1>`，`EVL` 必须是编译期整数常量，只有 `mask[lane] && lane < EVL` 的 lane 才发射真实内存访问；inactive `vp.load` lane 保持未绑定，后续只有 `freeze` 或不读取这些 lane 才能继续虚拟化，inactive `vp.store` lane 不发射写入。fixed vector `llvm.masked.expandload` / `llvm.masked.compressstore` 使用 `llvm.memory.masked.vector.expandload` / `llvm.memory.masked.vector.compressstore` rule：mask 必须是编译期常量 `<N x i1>`，enabled lane 以已启用 lane 数形成 `active_offset(%lane)` 连续访问，disabled `expandload` lane 从 passthru lane 复制，disabled `compressstore` lane 不发射写入。fixed vector `llvm.masked.gather` / `llvm.masked.scatter` 使用 `llvm.memory.masked.vector.gather` / `llvm.memory.masked.vector.scatter` rule：指针 operand 必须是已由受支持 lowering 构造的 fixed pointer vector，mask 必须是编译期常量 `<N x i1>`，enabled lane 逐个使用该 lane 的指针执行 `load` / `store`，disabled `gather` lane 从 passthru lane 复制，disabled `scatter` lane 不发射写入。fixed vector `llvm.vp.gather` / `llvm.vp.scatter` 复用同一组 gather/scatter rule：指针 operand 必须是 fixed pointer vector，mask 和 EVL 必须分别是编译期常量 `<N x i1>` 与 i32，只有 `mask[lane] && lane < EVL` 的 lane 才执行 `load` / `store`；inactive `vp.gather` lane 保持未绑定，inactive `vp.scatter` lane 不发射写入。fixed vector `llvm.experimental.vp.strided.load` / `llvm.experimental.vp.strided.store` 使用 `llvm.memory.vp.strided.vector.load` / `llvm.memory.vp.strided.vector.store` rule：mask 和 EVL 必须是编译期常量，stride 必须是 x 寄存器可承载的整数 byte stride（i1/i8/i16/i32/i64），translator 先通过 profile `sext` action 按有符号步长扩展到 i64，再按 `base + stride * lane` 执行 `load` / `store`；只有 `mask[lane] && lane < EVL` 的 lane 会发射真实访问。inactive strided load lane 保持未绑定，inactive strided store lane 不发射写入。lane 必须能落在 x 寄存器模型中，即整数、指针、`half`、`float` 或 `double`；连续 fixed vector load/store 还必须由 LLVM data layout 证明 lane store size 等于 lane 位宽对应字节数，且 vector store size 等于 lane stride 乘以 lane 数。这样 `<4 x i16>`、`<2 x float>`、volatile `<4 x i16>` load/store、`llvm.masked.load.v4i32`、`llvm.masked.expandload.v4i32`、`llvm.masked.gather.v4i32.v4p0`、常量 EVL 的 `llvm.vp.load.v4i32` / `llvm.vp.store.v4i32` / `llvm.vp.gather.v4i32.v4p0` / `llvm.vp.scatter.v4i32.v4p0` / `llvm.experimental.vp.strided.load.v4i32` / `llvm.experimental.vp.strided.store.v4i32` 这类 fixed vector 内存访问可以虚拟化，packed `<N x i1>`、scalable vector、动态 mask、动态 EVL、非 x 整数 strided stride、未绑定 active pointer/value lane 或目标 data layout 带额外不可拆分 padding 的连续 vector 必须安全跳过。

fixed vector `store` 的源可以来自已支持 fixed vector lowering 产生的绑定，也可以是无 `undef` / `poison` lane 的 LLVM 常量固定向量；含 `undef` / `poison` lane 的常量向量必须先经过受支持的 `freeze`。

`amice-simple-vmp` 是默认兼容性 profile；`ruoke` 是压力测试/示例 profile。两个内置示例 profile 都必须声明 1000 个唯一 opcode alias，并要求 runtime emitter 为这些 alias 生成独立可分发 handler case。当前函数 bytecode 未选中的 alias 也不能生成直接跳 `default` 的空壳；runtime 必须为它们生成目标安全的 dead alias handler，推进 VM `pc` 后回到 dispatch，避免未执行的目标相关 intrinsic 或 native call handler 破坏 codegen。

`va_arg` 当前不进入 profile DSL、VM IR 或 runtime handler。原因是不同目标 ABI 的 `va_list` 布局、寄存器保存区和栈游标更新规则不是 AMICE 当前 x 寄存器 VM 的可验证语义模板；translator 必须在看到 LLVM `va_arg` opcode 时安全跳过整个函数，保留原始 LLVM IR，由宿主后端继续按目标 ABI 生成代码。

普通 `call asm ...` inline assembly 当前也不进入 profile DSL、VM IR 或 runtime handler。原因是 inline asm 的约束字符串、隐式寄存器/内存 clobber、目标汇编语义和 side effect 不能由当前 profile semantic AST 验证；translator 必须在看到 inline asm callee 时安全跳过整个函数，保留原始 LLVM IR。`callbr asm ...` 继续按非结构化控制流边界安全跳过。

`musttail call` 当前也不进入 profile DSL、VM IR 或 runtime handler。原因是 `musttail` 要求 call 后紧邻 return，并保持由 LLVM verifier 约束的尾调用 ABI；当前 VM native bridge 会先进入解释器和 native thunk，无法证明仍满足该尾调用契约。translator 必须在看到 `musttail` call 时安全跳过整个函数，保留原始 LLVM IR。普通 `tail` 只作为优化提示处理，`notail` 与当前非尾调 VM bridge 不冲突。

带 operand bundle 的普通 `call` 当前也不进入 profile DSL、VM IR 或 runtime handler。原因是 `deopt`、`funclet`、GC statepoint、ptrauth 和前端扩展 bundle 会把额外控制流、异常域、栈图或目标相关语义挂在 call site 上；当前 VM native bridge 只保留普通 call-site attributes，不能证明这些 bundle 语义在 native thunk 后仍等价。唯一例外是 `llvm.assume`：它没有运行时语义，operand bundle 只向优化器携带假设，LLVM 要求带 operand bundle 的 `llvm.assume` condition 是常量 `true`；translator 会验证这一点，然后继续通过 profile `llvm.assume` rule 发射 `fake_nop`。除 `llvm.assume` 外，translator 必须在看到任意 operand bundle 时安全跳过整个函数，保留原始 LLVM IR。

带 `naked`、`returns_twice`、`strictfp`、`presplitcoroutine`、`coro_only_destroy_when_complete` 或 `coro_elide_safe` 函数属性的目标函数当前必须安全跳过。原因是 VM wrapper 会引入普通 prologue、dispatcher 调用、native thunk 和返回槽路径，不能证明这些特殊函数级 ABI、浮点环境或 coroutine 契约在重写后仍由 LLVM 后端按原语义处理。

volatile `llvm.memcpy` / `llvm.memmove` / `llvm.memset` 已按上一段说明走 profile 驱动的 dynamic volatile handler。自然对齐整数/指针标量 volatile `cmpxchg`、自然对齐整数/浮点标量 volatile `atomicrmw` 和自然对齐指针标量 volatile `atomicrmw xchg` 已支持，safe-skip 只保留类型、对齐、syncscope 或 ordering 超出当前边界的情况。

`llvm.clear_cache`、`llvm.pseudoprobe` 和 `llvm.prefetch` 已在 intrinsic 白名单内；非双 pointer 参数 void 返回的 `llvm.clear_cache`、非四个编译期整数参数 void 返回的 `llvm.pseudoprobe`、`probe_type` 不能放入 i32 的 `llvm.pseudoprobe`、非 pointer + 三个 immarg + void 形式的 `llvm.prefetch`，或 `llvm.prefetch` 的 `rw/locality/cache` 超出 LLVM 允许范围，仍必须安全跳过目标函数并输出 debug 日志。

以下情况必须安全跳过目标函数并输出 debug 日志。下列“向量”一词只指不满足上文固定向量白名单和后续补充项的向量形态；已经列入白名单的 fixed vector lowering 不属于安全跳过边界：被虚拟化目标函数本身是 varargs，目标函数带 `naked` / `returns_twice` / `strictfp` / coroutine 约束属性，间接 varargs call，固定向量白名单外的向量值，`bfloat`、`fp128`、`x86_fp80`、`ppc_fp128` 等非 `half`/`float`/`double` 浮点值，非 `half`/`float`/`double` 端点的普通浮点转换，固定向量 lane 类型、lane 数、mask、EVL、聚合字段或 undef/poison 状态超出对应白名单，scalable vector，q ABI 宽向量，native call 参数/返回展开后超过 ABI 槽位，direct varargs call 中出现 vector 变参，call-site ABI 属性无法复制到 native thunk / indirect adapter，atomic load/store/atomicrmw/cmpxchg/fence 的非 system/singlethread syncscope，浮点 cmpxchg，非自然对齐或非受支持宽度的 atomic load/store/atomicrmw/cmpxchg，release/acq_rel failure ordering、unordered success/failure ordering 或 failure ordering 强于 success ordering 的 `cmpxchg`，unordered/monotonic/notatomic ordering 的 `fence`，各类 intrinsic 的参数数量、返回类型、immarg、mask、宽度或 metadata 形态超出本文对应白名单，非 integral pointer 或目标相关地址空间语义超出 64 位 bit 保留模型的 `addrspacecast`，普通 inline asm call、带 operand bundle 的 call、`musttail call`、`va_arg`、`invoke`、`callbr`、`indirectbr`、`landingpad`、`resume`、`catchswitch`、`catchpad`、`catchret`、`cleanuppad`、`cleanupret` 等异常或非结构化控制流，除固定布局小聚合普通/volatile load/store 外的非标量内存，动态 struct 字段选择或不可按 data layout 归一化为字节偏移的复杂 GEP，超过 ABI 或 VM 寄存器容量的参数/返回/活跃 SSA 值，profile 未覆盖的 lowering rule，以及 profile verifier 拒绝的 ABI/ISA/bytecode/decoder/runtime 配置。
上述 safe-skip 是函数粒度边界：某个函数因为 `invoke`、`callbr`、`indirectbr`、`resume`、`va_arg` 或其它 unsupported IR 被跳过时，同一 module 内其它满足 profile/verifier/translator 契约的函数仍必须继续虚拟化并保持可执行行为一致。

注意：上段的 LLVM intrinsic 白名单还包括 `llvm.experimental.widenable.condition`、`llvm.allow.runtime.check`、`llvm.allow.ubsan.check` 和 `llvm.fake.use`；`llvm.experimental.widenable.condition` 只支持无参数、返回 i1 的标准签名，`llvm.allow.runtime.check` 只支持单 metadata 参数、返回 i1 的标准签名，`llvm.allow.ubsan.check` 只支持单 i8 immarg 参数、返回 i1 的标准签名，`llvm.fake.use` 只支持 void vararg call 形式并通过 profile `llvm.fake.use` rule 发射 `fake_nop`，任何超出上述签名的形式仍必须安全跳过。
注意：上段的指针身份保持 intrinsic 白名单还包括 `llvm.preserve.array.access.index`、`llvm.preserve.union.access.index`、`llvm.preserve.struct.access.index` 和 `llvm.preserve.static.offset`；preserve access index 只支持 pointer 返回、pointer 源 operand 和标准 i32 immarg 索引，`llvm.preserve.static.offset` 只支持 pointer 返回和单 pointer 源 operand，任何非指针签名或 immarg 不是编译期常量的形式仍必须安全跳过。
注意：上段对 `llvm.objectsize dynamic=true` 动态 GEP 的 safe-skip 不适用于“静态对象或动态 alloca 派生链 + 每段 inbounds GEP + 可记录或累加 byte offset”的子集；该子集已经由 `llvm.objectsize.static_gep`、`llvm.objectsize.dynamic_gep_offset`、`llvm.objectsize.dynamic_gep_accumulate` 和 `llvm.objectsize.dynamic_alloca_gep` rule 覆盖。
注意：上段对向量参数/返回的 safe-skip 不适用于 fixed vector 函数 ABI 和 fixed vector direct/indirect native call；这些路径把每个 lane 展平成 x 寄存器槽并复用 aggregate/native_call 返回槽机制。只有 flattened 参数/返回槽超过 ABI 上限、lane 不是 x 寄存器可承载标量、scalable vector、q ABI 宽向量，或 direct varargs call 中出现 vector 变参时才必须安全跳过。
注意：上段对未列入固定向量白名单的其它向量 intrinsic 的 safe-skip 不适用于 fixed vector `llvm.vector.reverse`、`llvm.vector.splice`、`llvm.experimental.vp.splice`、`llvm.vector.insert` 和 `llvm.vector.extract`；`llvm.vector.reverse` 只支持单 fixed vector operand 和同形状 fixed vector 返回，`llvm.vector.splice` 只支持两个同 lane 数、同 lane 类型的 fixed vector operand、signed i32 immarg，且 immarg 必须落在 `-VL..VL-1` 范围内，`llvm.experimental.vp.splice` 只支持两个同 lane 数、同 lane 类型的 fixed vector operand、signed i32 immarg、常量 `<N x i1>` mask、常量 EVL1/EVL2，要求 `0 <= EVL1 <= VL`、`0 <= EVL2 <= VL` 且 `-EVL1 <= imm < EVL1`，mask 禁用、`lane >= EVL2` 或窗口落在无效拼接区间的 lane 保持 poison/未绑定，`llvm.vector.insert` 只支持 fixed base/result vector、fixed subvector、i64 非负常量 offset，且 `offset + subvector_lanes` 不得超过 base/result lane 数，offset 还必须是 subvector lane 数的倍数，`llvm.vector.extract` 只支持 fixed source/result vector、i64 非负常量 offset，且 `offset + result_lanes` 不得超过 source lane 数，offset 还必须是 result lane 数的倍数；超出这些签名或 scalable vector 形式仍必须安全跳过。
注意：上段对未列入固定向量白名单的其它向量 intrinsic 的 safe-skip 不适用于 fixed vector `llvm.experimental.vector.compress` 的常量 mask 子集；该 intrinsic 已由 `llvm.experimental.vector.compress.element` rule 覆盖，只支持 value / mask / passthru 三个参数、fixed vector value 和 passthru、常量 fixed `<N x i1>` mask、value / passthru / result lane 数一致且 lane 类型一致，active value lane 会按原顺序压到结果前部，剩余 lane 从同编号 passthru lane 复制；动态 mask、scalable vector、lane 类型不一致、未冻结 undef/poison lane 被后续直接读取或 q ABI 场景仍必须安全跳过。
注意：上段对未列入固定向量白名单的其它向量 intrinsic 的 safe-skip 不适用于 fixed integer vector `llvm.stepvector`；该 intrinsic 已由 `llvm.vector.step` rule 覆盖，并只在返回不是 fixed integer vector、lane 宽度不是 i8/i16/i32/i64、参数数量不是 0、scalable vector 或 q ABI 场景下安全跳过。
注意：上段对未列入固定向量白名单的其它向量 intrinsic 的 safe-skip 不适用于 fixed `<N x i1>` 的 `llvm.get.active.lane.mask`；该 intrinsic 已由 `llvm.vector.get.active.lane.mask` rule 覆盖，并只在返回不是 fixed `<N x i1>`、start/end 不是同宽整数、start/end 宽度不是 i1/i8/i16/i32/i64、参数数量不是 2、scalable vector 或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 不适用于 `llvm.experimental.get.vector.length` 的非 scalable 子集；该 intrinsic 已由 `llvm.experimental.get.vector.length.integer` rule 覆盖，并只在参数数量不是 3、VF 不是编译期 i32 immarg、scalable flag 不是编译期 i1 immarg、`isScalable=true`、AVL 不是 i1/i8/i16/i32/i64 整数、返回不是 i32 或 q ABI 场景下安全跳过。
注意：上段对未列入固定向量白名单的其它向量 intrinsic 的 safe-skip 不适用于 fixed `<N x i1>` mask 的 `llvm.experimental.cttz.elts` 且 `zero_is_poison=false/true` 的子集；该 intrinsic 已由 `llvm.experimental.cttz.elts` rule 覆盖，并只在返回不是 i1/i8/i16/i32/i64 标量整数、返回宽度不能表示 lane 总数、mask 不是 fixed `<N x i1>`、mask lane 未绑定或含未冻结 undef/poison、参数数量不是 2、scalable vector 或 q ABI 场景下安全跳过。`zero_is_poison=true` 的全零 mask 结果是 poison，VM 选择 lane 总数作为确定代表值。
注意：上段对未列入固定向量白名单的其它向量 intrinsic 的 safe-skip 也不适用于 fixed `<N x i1>` mask 的 `llvm.vp.cttz.elts` 且 `zero_is_poison=false/true`、常量 VP mask、常量 EVL 的子集；该 intrinsic 已由 `llvm.vp.cttz.elts` rule 覆盖，并只在返回不是 i1/i8/i16/i32/i64 标量整数、返回宽度不能表示 `min(EVL, lane_count)`、计数 mask 不是 fixed `<N x i1>`、VP mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、激活计数 mask lane 未绑定或含未冻结 undef/poison、参数数量不是 4、scalable vector 或 q ABI 场景下安全跳过。`zero_is_poison=true` 的激活 lane 全零结果是 poison，VM 选择激活 lane 数作为确定代表值。
注意：上段对其它 LLVM intrinsic 的 safe-skip 不适用于 fixed integer vector `llvm.vp.add/sub/mul/udiv/sdiv/urem/srem/xor/and/or/shl/lshr/ashr` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.*` rule 覆盖，并只在 mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、返回或 operand 不是 fixed integer vector、lane 宽度不是 i8/i16/i32/i64、参数数量不是 4、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed integer vector `llvm.vp.smax/smin/umax/umin/uadd.sat/usub.sat/sadd.sat/ssub.sat` 的常量 mask/常量 EVL 子集；该子集已由对应 `llvm.vp.vector.*.integer` rule 覆盖，并只在 mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、返回或 operand 不是 fixed integer vector、lane 宽度不是 i8/i16/i32/i64、参数数量不是 4、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed integer vector `llvm.vp.ctpop/ctlz/cttz/abs/bswap/bitreverse` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.ctpop.integer`、`llvm.vp.vector.ctlz.integer`、`llvm.vp.vector.cttz.integer`、`llvm.vp.vector.abs.integer`、`llvm.vp.vector.bswap.integer` 和 `llvm.vp.vector.bitreverse.integer` rule 覆盖，并只在 mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、`ctlz` / `cttz` / `abs` 的 poison flag 不是编译期 i1 常量、返回或 operand 不是 fixed integer vector、lane 宽度不是该 intrinsic 允许的整数宽度、参数数量不匹配、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.select` 的常量 EVL 子集和 fixed vector `llvm.vp.merge` 的 i32 pivot 子集；select 子集已由 `llvm.vp.select.vector_condition` rule 覆盖，并只在 EVL 不是编译期整数常量、condition 不是 matching fixed `<N x i1>`、then/else/result 不是同 lane 数同类型同宽 fixed vector、lane 不是 x 寄存器可承载标量、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。merge 子集已由 `llvm.vp.merge.vector_condition` rule 覆盖，并只在 pivot 不是整数、condition 不是 matching fixed `<N x i1>`、then/else/result 不匹配、lane 不是 x 寄存器可承载标量、scalable vector 或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.experimental.vp.reverse` / `llvm.experimental.vp.splat` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.experimental.vp.reverse.element` / `llvm.experimental.vp.splat.element` rule 覆盖，并只在 mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、source/scalar/result lane 数或 lane 类型不匹配、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取、scalar i1 mask 或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed integer vector `llvm.vp.zext/sext/trunc` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.cast.integer` rule 覆盖，并只在 mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、源或结果不是 fixed integer vector、lane 数不一致、lane 不是 integer、宽度方向不符合对应 cast、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.ptrtoint` / `llvm.vp.inttoptr` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.cast.pointer` rule 覆盖，并只在 mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、源或结果不是 fixed vector、lane 数不一致、lane 不是整数与 64 位 pointer 之间的合法转换、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取、q ABI 或目标地址空间语义超出 64 位 bit 保留模型时安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.icmp` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.icmp.integer` / `llvm.vp.vector.icmp.pointer` rule 覆盖，并只在 predicate 不是 metadata 字符串、predicate 超出 LLVM icmp 谓词集合、mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、源或结果不是 fixed vector、结果 lane 不是 i1、源 lane 数不一致、源 lane 不是同为 integer 或同为 pointer、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.sitofp/uitofp/fptosi/fptoui/fptrunc/fpext` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.sitofp.float`、`llvm.vp.vector.uitofp.float`、`llvm.vp.vector.fptosi.float`、`llvm.vp.vector.fptoui.float`、`llvm.vp.vector.fptrunc.float` 和 `llvm.vp.vector.fpext.float` rule 覆盖，并只在 mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、源或结果不是 fixed vector、lane 数不一致、lane 类型不符合对应整数/浮点转换、浮点 lane 不是 `half` / `float` / `double`、`fptrunc/fpext` 宽度方向非法、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.fadd/fsub/fmul/fdiv/frem/minnum/maxnum/minimum/maximum/copysign` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.fadd.float` / `llvm.vp.vector.fsub.float` / `llvm.vp.vector.fmul.float` / `llvm.vp.vector.fdiv.float` / `llvm.vp.vector.frem.float` / `llvm.vp.vector.minnum.float` / `llvm.vp.vector.maxnum.float` / `llvm.vp.vector.minimum.float` / `llvm.vp.vector.maximum.float` / `llvm.vp.vector.copysign.float` rule 覆盖，并只在 mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、源或结果不是 fixed vector、lane 数不一致、lane 不是 `half` / `float` / `double`、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.fneg/fabs/sqrt/canonicalize/floor/ceil/roundtozero/rint/nearbyint/round/roundeven/sin/cos/exp/exp2/log/log10/log2` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.{fneg,fabs,sqrt,canonicalize,floor,ceil,roundtozero,rint,nearbyint,round,roundeven,sin,cos,exp,exp2,log,log10,log2}.float` rule 覆盖，`roundtozero` 复用 `ftrunc`，并只在 mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、源或结果不是 fixed vector、lane 数不一致、lane 不是 `half` / `float` / `double`、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.lrint/llrint` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.lrint.float` / `llvm.vp.vector.llrint.float` rule 覆盖，并只在 mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、源或结果不是 fixed vector、lane 数不一致、source lane 不是 `half` / `float` / `double`、result lane 不是 i32/i64、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.fma/fmuladd` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.fma.float` / `llvm.vp.vector.fmuladd.float` rule 覆盖，并只在 mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、任一源或结果不是 fixed vector、lane 数不一致、lane 不是 `half` / `float` / `double`、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.fcmp` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.fcmp.float` rule 覆盖，并只在 predicate 不是 metadata 字符串、predicate 超出 LLVM fcmp 谓词集合、mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、源或结果不是 fixed vector、结果 lane 不是 i1、源 lane 数不一致、源 lane 不是 `half` / `float` / `double`、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取或 q ABI 场景下安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.is.fpclass` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.is.fpclass.float` rule 覆盖，并只在 class mask 超出 LLVM `FPClassTest` 当前 `0x03ff` 范围、mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、源或结果不是 fixed vector、结果 lane 不是 i1、source lane 数不一致、source lane 不是 `half` / `float` / `double`、激活源 lane 未由受支持 lowering 写入、scalable vector、未冻结 inactive lane 被读取、scalar i1 mask 或 q ABI 场景下安全跳过。
注意：上段对未列入固定向量白名单的其它向量 intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vector.interleave2..8` / `llvm.vector.deinterleave2..8`；这两条路径已分别由 `llvm.vector.interleave.element` 和 `llvm.vector.deinterleave.element` rule 覆盖。`interleave` 只在 factor 不是 2..8、返回不是 fixed vector、结果 lane 数不能被 factor 整除、输入 operand 数或 lane 数不匹配、lane 类型不匹配、输入 lane 未绑定、scalable vector 或 q ABI 场景下安全跳过；`deinterleave` 只在 factor 不是 2..8、返回不是 factor 个 fixed vector 字段组成的 struct、输入 lane 数不能被 factor 整除、结果 vector lane 数不匹配、lane 类型不匹配、输入 lane 未绑定、scalable vector 或 q ABI 场景下安全跳过。
注意：上段对 aggregate `select` then/else 不是已支持聚合 lowering 绑定的 safe-skip 不适用于无 `undef` / `poison` 字段的 LLVM 常量 struct / 固定数组；这些常量聚合会按 leaf 字段物化后进入 `llvm.select.aggregate` rule。常量聚合字段包含 `undef` / `poison`、vector 叶子或非 `half` / `float` / `double` 浮点叶子时仍安全跳过，除非先经过受支持的 `freeze`。上段对 aggregate / fixed vector `phi` incoming 不是已支持 lowering 绑定的 safe-skip 不适用于无 `undef` / `poison` 字段或 lane 的 LLVM 常量 struct / 固定数组 / 固定向量；这些常量会按 predecessor edge 物化后进入 `llvm.aggregate.phi.edge_move` 或 `llvm.vector.phi.edge_move` rule。
注意：上段对带 operand bundle 的 call 的 safe-skip 不适用于 condition 为常量 `true` 的 `llvm.assume`；`llvm.assume` 的 operand bundle 没有运行时语义，已由 `llvm.assume` rule 发射 `fake_nop` 覆盖，并只在 condition 不是常量 `true` 时安全跳过。
注意：上段对向量 load/store 的安全跳过不适用于普通或 volatile 且逐 lane 字节连续的 fixed vector `load` / `store`；这些内存访问已由 `llvm.memory.vector.load` / `llvm.memory.vector.store` / `llvm.memory.volatile.vector.load` / `llvm.memory.volatile.vector.store` rule 覆盖，并只在 packed、scalable、q ABI、lane 非 x 寄存器可承载标量，或 data layout 不能证明 vector store size 等于 lane stride 乘以 lane 数时安全跳过。上段对 masked/VP vector memory 的安全跳过也不适用于常量 `<N x i1>` mask 的 fixed vector `llvm.masked.load` / `llvm.masked.store` / `llvm.masked.expandload` / `llvm.masked.compressstore` / `llvm.masked.gather` / `llvm.masked.scatter`，以及常量 EVL 的 fixed vector `llvm.vp.load` / `llvm.vp.store` / `llvm.vp.gather` / `llvm.vp.scatter` / `llvm.experimental.vp.strided.load` / `llvm.experimental.vp.strided.store`；它们已由 `llvm.memory.masked.vector.load` / `llvm.memory.masked.vector.store` / `llvm.memory.masked.vector.expandload` / `llvm.memory.masked.vector.compressstore` / `llvm.memory.masked.vector.gather` / `llvm.memory.masked.vector.scatter` / `llvm.memory.vp.strided.vector.load` / `llvm.memory.vp.strided.vector.store` rule 覆盖，并只在动态 mask、mask lane 不是 i1、VP EVL 非常量、strided stride 不是 x 寄存器可承载整数 byte stride、active passthru/source/pointer lane 未由受支持 lowering 写入、packed、scalable、q ABI、lane 非 x 寄存器可承载标量，或连续 vector load/store 的 data layout 不能证明逐 lane 字节连续时安全跳过。
注意：上段对动态 lane 下标的安全跳过不适用于固定向量动态 `insertelement` / `extractelement` 的普通路径；这两类指令已由 `llvm.vector.insert.dynamic_element` / `llvm.vector.extract.dynamic_element` rule 覆盖，并只在 index 不是整数、index 位宽超出 x 寄存器模型、源向量存在未定义 lane、vector condition select、scalable vector 或 q ABI 场景下安全跳过。
注意：上段对指针向量比较的安全跳过不适用于 fixed `<N x ptr>` 的普通 `icmp`；该路径已由 `llvm.vector.icmp.pointer` rule 覆盖，并只在 operand/result 不是固定向量、lane 数不一致、左右 operand 不是 pointer lane、lane 未绑定、scalable vector 或 q ABI 时安全跳过。
注意：上段对向量值和指针转换的安全跳过不适用于 fixed vector `ptrtoint` / `inttoptr` / `addrspacecast`；该路径已由 `llvm.vector.cast.pointer` rule 覆盖，并只在源/结果不是固定向量、lane 数不一致、lane 不是整数与 64 位 pointer 之间的合法转换、源 lane 未由受支持 lowering 写入、scalable vector、q ABI 或目标地址空间语义超出 64 位 bit 保留模型时安全跳过。
注意：上段对向量浮点 intrinsic 的安全跳过不适用于 fixed vector `llvm.copysign/minnum/maxnum/minimum/maximum`、`llvm.pow`、`llvm.powi`、`llvm.is.fpclass`、`llvm.fma/fmuladd`、`llvm.sin/cos/exp/exp2/log/log10/log2` 和 `llvm.fptosi.sat/fptoui.sat`，这些 binary / float-int-binary / classification / ternary / unary / 饱和 float-cast intrinsic 已由 `llvm.vector.{copysign,minnum,maxnum,minimum,maximum}.float`、`llvm.vector.pow.float`、`llvm.vector.powi.float`、`llvm.vector.is.fpclass.float`、`llvm.vector.fma.float`、`llvm.vector.fmuladd.float`、`llvm.vector.{sin,cos,exp,exp2,log,log10,log2}.float`、`llvm.vector.fptosi.sat.float` 和 `llvm.vector.fptoui.sat.float` rule 覆盖；它们只在 lane 数、operand lane 类型或结果 lane 类型超出各自边界时安全跳过，其中 `llvm.powi` 还要求指数 operand 是所有 lane 共享的标量 i32，`llvm.is.fpclass` 还要求结果 lane 是 `i1` 且 mask 在 LLVM `FPClassTest` 当前 `0x03ff` 范围内，`llvm.fptosi.sat/fptoui.sat` 还要求源 lane 是 `half` / `float` / `double` 且结果 lane 是 i1/i8/i16/i32/i64。
注意：上段对 round-to-int intrinsic 的安全跳过也不适用于 fixed vector `llvm.lrint/llrint/lround/llround`；这些 intrinsic 已由 `llvm.vector.lrint.float`、`llvm.vector.llrint.float`、`llvm.vector.lround.float` 和 `llvm.vector.llround.float` rule 覆盖，并只在 lane 数不匹配、源 lane 不是 `half` / `float` / `double` 或结果 lane 不是 i32/i64 时安全跳过。
注意：上段对 constrained floating intrinsic 的安全跳过不适用于标量和 fixed vector `llvm.experimental.constrained.fadd/fsub/fmul/fdiv/frem` 的 `round.tonearest` + `fpexcept.ignore` 子集，也不适用于标量和 fixed vector `llvm.experimental.constrained.fabs/sqrt/canonicalize/floor/ceil/trunc/rint/nearbyint/round/roundeven/sin/cos/exp/exp2/log/log10/log2` 的保守 constrained unary 子集，也不适用于标量和 fixed vector `llvm.experimental.constrained.copysign/pow/powi/fma/fmuladd/minnum/maxnum/minimum/maximum` 的保守 constrained math 子集，也不适用于标量 `llvm.experimental.constrained.lrint/llrint/lround/llround` 的保守 constrained round-to-int 子集，也不适用于标量和 fixed vector `llvm.experimental.constrained.fcmp/fcmps` 的合法 fcmp predicate metadata + `fpexcept.ignore` 子集，还不适用于标量和 fixed vector `llvm.experimental.constrained.sitofp/uitofp/fptosi/fptoui/fptrunc/fpext` 的保守 constrained cast 子集。
这些 intrinsic 已分别由 `llvm.constrained.{fadd,fsub,fmul,fdiv,frem}.float` / `llvm.constrained.vector.{fadd,fsub,fmul,fdiv,frem}.float`、`llvm.constrained.{fabs,sqrt,canonicalize,floor,ceil,trunc,rint,nearbyint,round,roundeven,sin,cos,exp,exp2,log,log10,log2}.float` / `llvm.constrained.vector.{fabs,sqrt,canonicalize,floor,ceil,trunc,rint,nearbyint,round,roundeven,sin,cos,exp,exp2,log,log10,log2}.float`、`llvm.constrained.{copysign,pow,minnum,maxnum,minimum,maximum}.float` / `llvm.constrained.vector.{copysign,pow,minnum,maxnum,minimum,maximum}.float`、`llvm.constrained.{powi,fma,fmuladd}.float` / `llvm.constrained.vector.{powi,fma,fmuladd}.float`、`llvm.constrained.{lrint,llrint,lround,llround}.float`、`llvm.constrained.{fcmp,fcmps}.float` / `llvm.constrained.vector.{fcmp,fcmps}.float` 和 `llvm.constrained.{sitofp,uitofp,fptosi,fptoui,fptrunc,fpext}.float` / `llvm.constrained.vector.{sitofp,uitofp,fptosi,fptoui,fptrunc,fpext}.float` rule 覆盖。
二元算术、constrained unary、constrained binary math、constrained `powi/fma/fmuladd`、比较和 constrained cast 只在 rounding metadata 不是对应子集要求的 `round.tonearest`、exception metadata 不是 `fpexcept.ignore`、参数数量不匹配、operand/result 不是同宽 `half` / `float` / `double` 标量或同 lane 数同 lane 宽 fixed vector、scalable vector 或 q ABI 场景下安全跳过；其中 `copysign/minnum/maxnum/minimum/maximum` 没有 rounding metadata，`pow/powi/fma/fmuladd` 需要 rounding metadata，fixed vector `powi` 还要求指数 operand 是所有 lane 共享的标量 i32。constrained round-to-int 当前仍只支持标量，遇到 fixed/scalable vector、类型不符合 round-to-int 子集、metadata 不匹配、参数数量不匹配或 q ABI 场景时安全跳过。
注意：上段对整数 intrinsic 的安全跳过不适用于 fixed vector `llvm.ctpop/ctlz/cttz/abs/bswap/bitreverse`；这些 unary intrinsic 已由 `llvm.vector.{ctpop,ctlz,cttz,abs,bswap,bitreverse}.integer` rule 覆盖，并只在 lane 数不匹配、operand/result 不是整数 lane、宽度超出 i1/i8/i16/i32/i64，或 `bswap` lane 不是 i16/i32/i64 时安全跳过。
注意：上段对整数 intrinsic 的安全跳过也不适用于 fixed vector `llvm.smax/smin/umax/umin/uadd.sat/usub.sat/sadd.sat/ssub.sat/ushl.sat/sshl.sat`；这些 binary intrinsic 已由 `llvm.vector.{smax,smin,umax,umin,uadd.sat,usub.sat,sadd.sat,ssub.sat,ushl.sat,sshl.sat}.integer` rule 覆盖，并只在 lane 数不匹配、operand/result 不是整数 lane 或宽度超出 i1/i8/i16/i32/i64 时安全跳过。
注意：上段对整数 overflow intrinsic 的安全跳过也不适用于 fixed vector `llvm.uadd.with.overflow/sadd.with.overflow/usub.with.overflow/ssub.with.overflow/umul.with.overflow/smul.with.overflow`；这些 intrinsic 已由 `llvm.vector.{uadd,sadd,usub,ssub,umul,smul}.with.overflow.integer` rule 覆盖，并只在返回类型不是 `{ <N x iW>, <N x i1> }`、输入和结果值 lane 数不匹配、输入或结果值不是整数 lane、溢出标志不是同 lane 数 `i1`、宽度超出 i1/i8/i16/i32/i64、scalable vector 或 q ABI 时安全跳过。
注意：上段对整数 intrinsic 的安全跳过也不适用于 fixed vector `llvm.fshl/fshr`；这些 ternary intrinsic 已由 `llvm.vector.{fshl,fshr}.integer` rule 覆盖，并只在 lane 数不匹配、operand/result 不是整数 lane 或宽度超出 i1/i8/i16/i32/i64 时安全跳过。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.fshl/fshr` 的常量 mask/常量 EVL 子集；该子集已由 `llvm.vp.vector.{fshl,fshr}.integer` rule 覆盖，并只在参数数量不是 5、mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、源或结果不是 fixed integer vector、lane 数不匹配、lane 宽度超出 i1/i8/i16/i32/i64、激活源 lane 未由受支持 lowering 写入、scalable vector、q ABI 场景下安全跳过。
注意：上段对其它硬件 loop intrinsic 的安全跳过不适用于标量 `llvm.set.loop.iterations`、`llvm.start.loop.iterations`、`llvm.test.set.loop.iterations`、`llvm.test.start.loop.iterations`、`llvm.loop.decrement` 与 `llvm.loop.decrement.reg`；这些 intrinsic 已由对应 profile lowering rule 覆盖，并只在参数不是 i1/i8/i16/i32/i64 标量整数、返回类型与 LLVM 21 `Intrinsics.td` 定义不一致，或 `loop.decrement.reg` 两个参数不是同宽整数时安全跳过。
注意：上段对 `llvm.expect` / `llvm.expect.with.probability` 的安全跳过也不适用于 fixed vector 整数形态；这些值保持 intrinsic 已由 `llvm.vector.expect.integer` / `llvm.vector.expect.with_probability.integer` rule 覆盖，并只在 lane 数不匹配、value/expected/result 不是整数 lane 或宽度超出 i1/i8/i16/i32/i64 时安全跳过。
注意：上段对整数 intrinsic 的安全跳过也不适用于 fixed vector `llvm.vector.reduce.add/mul/and/or/xor/smax/smin/umax/umin`；这些 reduction intrinsic 已由 `llvm.vector.reduce.{add,mul,and,or,xor,smax,smin,umax,umin}.integer` rule 覆盖，并只在源不是 fixed integer vector、lane 未由受支持 lowering 写入、标量返回宽度与 lane 宽度不一致、宽度超出 i1/i8/i16/i32/i64、scalable vector 或 q ABI 时安全跳过。
注意：上段对浮点 intrinsic 的安全跳过也不适用于 fixed vector `llvm.vector.reduce.fadd/fmul/fmin/fmax/fminimum/fmaximum`；这些 reduction intrinsic 已由 `llvm.vector.reduce.{fadd,fmul,fmin,fmax,fminimum,fmaximum}.float` rule 覆盖。`fadd/fmul` 只在 accumulator 不是同宽 `half` / `float` / `double` 标量、源不是 fixed floating vector、lane 未由受支持 lowering 写入、标量返回宽度与 lane 宽度不一致、scalable vector 或 q ABI 时安全跳过；`fmin/fmax/fminimum/fmaximum` 没有 accumulator，但同样要求源是已绑定 fixed floating vector、lane 和标量返回同宽且类型为 `half` / `float` / `double`。
注意：上段对其它 LLVM intrinsic 的 safe-skip 也不适用于 fixed vector `llvm.vp.reduce.add/mul/and/or/xor/smax/smin/umax/umin` 和 `llvm.vp.reduce.fadd/fmul/fmin/fmax/fminimum/fmaximum` 的常量 mask/常量 EVL 子集；该子集已由对应 `llvm.vp.reduce.*` rule 覆盖，并只在参数数量不是 4、start 不是同宽标量、mask 不是常量 `<N x i1>`、EVL 不是编译期整数常量、源不是 fixed vector、lane 类型或宽度不匹配、激活源 lane 未由受支持 lowering 写入、scalable vector、q ABI 场景下安全跳过。
注意：上段对其它 experimental vector intrinsic 的 safe-skip 不适用于 fixed vector `llvm.experimental.vector.extract.last.active` 的常量 mask 子集；该子集已由 `llvm.experimental.vector.extract.last.active` rule 覆盖，并只在参数数量不是 3、mask 不是常量 `<N x i1>`、source 不是 fixed vector、result/lane/passthru 类型或宽度不匹配、选中 lane 未由受支持 lowering 写入、passthru 未冻结、scalable vector、q ABI 场景下安全跳过。
注意：上段对 `llvm.ssa.copy` 和 `llvm.arithmetic.fence` 的安全跳过也不适用于 fixed vector 白名单形态；`llvm.ssa.copy` 已由 `llvm.vector.ssa.copy` rule 覆盖，并只在 lane 数不匹配、value/result lane 类型或宽度不一致，或 lane 类型不是整数、指针、`half`、`float`、`double` 时安全跳过；`llvm.arithmetic.fence` 已由 `llvm.vector.arithmetic.fence` rule 覆盖，并只在 lane 数不匹配、value/result lane 类型或宽度不一致，或 lane 类型不是 `half`、`float`、`double` 时安全跳过。
