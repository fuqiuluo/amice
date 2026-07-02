# VMP 虚拟化实现规范

本文定义 AMICE VMP 的实现与架构。该 pass 的职责是把目标函数的 LLVM IR 翻译成 profile 指定的 VM bytecode，并在当前 LLVM `Module` 中生成能够解释执行这些 bytecode 的 VM runtime。

## 目标

- VM 的 ISA、ABI、字节码格式、解码器流水线和 lowering 规则由外部 profile 描述。
- VM 执行模型固定为寄存器虚拟机：LLVM SSA value lowering 后进入虚拟寄存器，handler 通过 register file 读写状态。
- AMICE 插件只内置 profile 解析、校验、LLVM IR 翻译框架、bytecode encoder 和 runtime emitter。
- runtime 不能是旧 `vm_flatten` 那类写死解释器；它必须从 profile 解析出的 ISA semantic、ABI、decoder pipeline 和 bytecode layout 生成 LLVM IR。AMICE 允许的 handler semantic 是受限 typed DSL 模板，超出模板的 profile 必须在 verifier 阶段被拒绝。
- 默认使用模块级 runtime、函数级 bytecode：同一个 LLVM `Module` 内共享一套 runtime，每个被保护函数拥有自己的 bytecode、key、重定位和 wrapper。
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
bytecode.scope = func        # 每个被保护函数生成自己的 bytecode
polymorph.scope = func       # 每个函数允许独立 opcode/key/layout 多态化
```

也就是：

- 每个 LLVM `Module` 生成一套 VM runtime。
- 每个被保护函数生成一份 bytecode global。
- 每个被保护函数拥有自己的 function key、opcode permutation、bytecode layout salt。
- 原始函数 body 被替换成 wrapper：负责 marshal 参数、调用 VM dispatch、marshal 返回值。

生成后的形态类似：

```text
.amice.vm.state.type
.amice.vm.dispatch
.amice.vm.decode
.amice.vm.handler.add_i32
.amice.vm.handler.br_if

.amice.vm.bytecode.foo
.amice.vm.meta.foo
foo(...) {
  state = init_state(...)
  marshal_args(state, ...)
  call .amice.vm.dispatch(&bytecode_foo, &state)
  return marshal_ret(state)
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

`isa.vm` 描述 VM 指令、operand 和 handler 语义。语义块不是任意脚本；当前 AMICE 支持赋值、`pc` 赋值、`store_width`、`volatile_store_width`、`atomic_store_width`、`volatile_atomic_store_width`、`state = unchanged`、寄存器引用、常量池引用、整数二元运算、整数溢出标志、整数比较、标量浮点二元运算、标量浮点一元运算、标量浮点比较、标量浮点分类、宽度转换、`stack_alloc`、`load_width`、`volatile_load_width`、`atomic_load_width`、`volatile_atomic_load_width`、`atomic_rmw`、`volatile_atomic_rmw`、`memcpy_dynamic`、`memmove_dynamic`、`memset_dynamic`、`volatile_memcpy_dynamic`、`volatile_memmove_dynamic`、`volatile_memset_dynamic`、`cmpxchg`、`fence` 和 `call_table` 返回槽读取。Verifier 会把这些语句解析成 typed AST，并匹配到 AMICE 已实现的有限 handler 模板；不能匹配的 semantic 会被拒绝。

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

- 整数运算：`add`、`sub`、`mul`、`udiv`、`sdiv`、`urem`、`srem`、`xor`、`and`、`or`、`shl`、`lshr`、`ashr`，以及 i8/i16/i32/i64 的 `llvm.ctpop`、`llvm.ctlz`、`llvm.cttz`、`llvm.abs`、`llvm.smax`、`llvm.smin`、`llvm.umax`、`llvm.umin`、`llvm.uadd.sat`、`llvm.usub.sat`、`llvm.sadd.sat`、`llvm.ssub.sat`、`llvm.ushl.sat`、`llvm.sshl.sat`、`llvm.uadd.with.overflow`、`llvm.sadd.with.overflow`、`llvm.usub.with.overflow`、`llvm.ssub.with.overflow`、`llvm.umul.with.overflow`、`llvm.smul.with.overflow`、`llvm.bswap`、`llvm.bitreverse`、`llvm.fshl`、`llvm.fshr` intrinsic，以及 i1/i8/i16/i32/i64 的 `llvm.expect` / `llvm.expect.with.probability` 值保持 intrinsic；`llvm.ctlz` / `llvm.cttz` 接受 `is_zero_undef=true|false`，`llvm.abs` 接受 `is_int_min_poison=true|false`，true flag 触发的 poison 输入沿用 LLVM 未定义边界。
- 标量浮点运算和分类：`fadd`、`fsub`、`fmul`、`fdiv`、`frem`、`fneg`，以及 `float` / `double` 标量 `llvm.fabs`、`llvm.sqrt`、`llvm.canonicalize`、`llvm.floor`、`llvm.ceil`、`llvm.trunc`、`llvm.rint`、`llvm.nearbyint`、`llvm.round`、`llvm.roundeven`、`llvm.fma`、`llvm.fmuladd`、`llvm.minnum`、`llvm.maxnum`、`llvm.minimum`、`llvm.maximum`、`llvm.copysign` 和 `llvm.is.fpclass`，当前仅限 LLVM `float` 和 `double`，VM x 寄存器保存 IEEE 原始 bit；`llvm.fabs` 清除符号位并保留其它 bit，`llvm.sqrt` 通过 profile 中的 `float_unary(fsqrt, ...)` semantic 进入 runtime 并调用 LLVM `sqrt` intrinsic，`llvm.fma` 通过 profile 中的 `float_ternary(fma, ...)` semantic 进入 runtime 并调用 LLVM `fmuladd` intrinsic，`llvm.fmuladd` 通过 profile 中的 `float_ternary(fmuladd, ...)` semantic 进入 runtime 并调用 LLVM `fmuladd` intrinsic，`llvm.minnum` / `llvm.maxnum` 通过 profile 中的 `float_bin(fminnum/fmaxnum, ...)` semantic 进入 runtime 并调用 LLVM `minnum` / `maxnum` intrinsic，`llvm.minimum` / `llvm.maximum` 通过 profile 中的 `float_bin(fminimum/fmaximum, ...)` semantic 进入 runtime 并调用 LLVM `minimum` / `maximum` intrinsic，`llvm.copysign` 保留第一个 operand 的数值位并复制第二个 operand 的符号位，`llvm.is.fpclass` 的 mask 按 LLVM `FPClassTest` 位定义解释。
- 标量浮点取整 intrinsic：`llvm.floor`、`llvm.ceil`、`llvm.trunc`、`llvm.rint`、`llvm.nearbyint`、`llvm.round` 和 `llvm.roundeven` 必须由 profile 中的 `llvm.*.float` lowering rule 发射到 `ffloor`、`fceil`、`ftrunc`、`frint`、`fnearbyint`、`fround` 和 `froundeven`，再由对应 `float_unary(...)` semantic 驱动 runtime 调用 LLVM intrinsic。
- 标量整数/浮点转换：`sitofp`、`uitofp`、`fptosi`、`fptoui`、`fptrunc`、`fpext`，浮点端当前仅限 LLVM `float` 和 `double`。
- 比较：整数/指针标量 `icmp`、`float`/`double` 标量 `fcmp`。
- 选择：`select`，当前支持整数、指针、`float` 和 `double` 标量，以及直接 struct / 固定数组小聚合；标量按 x 寄存器 bit 复制，聚合按叶子字段展开为 profile `llvm.select.aggregate` 中的 `br_if` / `mov` / `br` 序列。
- 类型转换：`zext`、`sext`、`trunc`、同宽整数/`float`/`double` 标量 `bitcast`、`ptrtoint`、`inttoptr`、`addrspacecast`；`addrspacecast` 当前只按 64 位指针 bit 模式保留。
- 值稳定化：`freeze`，当前支持整数、指针、`float` 或 `double` 标量，以及直接 struct / 固定数组小聚合。
- 指针值保持、指针 mask、编译期查询和注释类 intrinsic：`llvm.launder.invariant.group`、`llvm.strip.invariant.group`、`llvm.invariant.start`、`llvm.threadlocal.address`、`llvm.ptrmask`、`llvm.is.constant`、`llvm.objectsize`、`llvm.annotation`、`llvm.ptr.annotation`，当前只保留整数或 64 位指针 bit，`llvm.is.constant` 在 translator 中按原始 operand 是否为 LLVM 常量折叠成 i1 并通过 profile 常量物化进入 VM，`llvm.objectsize` 当前只接受静态可确定的 alloca、GlobalVariable 和常量偏移 GEP，按剩余对象字节数通过 `llvm.objectsize.integer` lowering rule 物化为 VM 常量，invariant-group、invariant 描述符长度和 annotation metadata 不进入 VM 状态；`llvm.threadlocal.address` 的 TLS GlobalValue operand 必须留在私有 native thunk 内，VM bytecode 通过 `call_native` + `tls_addr` 记录该边界。
- SSA 值保持 intrinsic：`llvm.ssa.copy` 当前支持整数、指针、`float` 和 `double` 标量，必须由 profile 中的 `llvm.ssa.copy.scalar` lowering rule 发射到 `mov`，只复制 x 寄存器 bit，不生成专用 runtime handler。
- 计数器 intrinsic：`llvm.readcyclecounter` 和 `llvm.readsteadycounter` 当前支持无参数 `i64` 返回形式，必须由 profile 中的 `llvm.readcyclecounter.integer` / `llvm.readsteadycounter.integer` lowering rule 发射到 `read_cycle` / `read_steady`，再由 `read_counter(cycle|steady)` semantic 驱动 runtime 调用对应 LLVM intrinsic。
- 内存、陷阱和无运行时语义 intrinsic：固定大小和运行时元素个数 `alloca`、`load`、`store`、自然对齐整数/指针/`float`/`double` 标量 atomic `load`/`store` 和 volatile atomic `load`/`store`、自然对齐整数标量 `atomicrmw xchg/add/sub/and/or/xor/nand/min/max/umin/umax/uinc_wrap/udec_wrap/usub_cond/usub_sat` 及其 volatile 形式、自然对齐 `float`/`double` 标量 `atomicrmw fadd/fsub/fmax/fmin/fmaximum/fminimum` 及其 volatile 形式、自然对齐整数/指针标量 `cmpxchg`（weak 按 strong 形式发射）、默认 syncscope 的 acquire/release/acq_rel/seq_cst `fence`、`getelementptr`、固定小长度内联展开的非 volatile `llvm.memcpy` / `llvm.memmove` / `llvm.memset` intrinsic、固定大长度或动态长度的非 volatile `llvm.memcpy` / `llvm.memmove` / `llvm.memset` intrinsic、常量长度和动态长度的 volatile `llvm.memcpy` / `llvm.memmove` / `llvm.memset` intrinsic、`llvm.trap` / `llvm.debugtrap` / `llvm.ubsantrap`，以及 `llvm.invariant.end`、`llvm.prefetch`、`llvm.experimental.noalias.scope.decl`、`llvm.donothing`、`llvm.lifetime.start` / `llvm.lifetime.end`、`llvm.assume`、`llvm.dbg.*`、`llvm.var.annotation`、`llvm.codeview.annotation` 这类不改变运行时状态的 intrinsic。
- 控制流：`br`、条件 `br`、`switch`、`ret`。
- 调用：direct call 通过 `native_call` 规则重新生成 LLVM call；indirect call 通过 AMICE 生成的 adapter thunk 把运行时 callee 指针作为第 0 个 native 参数传入，再由同一套 `call_native` VM handler 调用；被调函数是否虚拟化由函数选择器单独决定，call lowering 不隐式递归虚拟化被调函数。

`phi` 不得作为普通指令进入 VM lowering。translator 在 predecessor edge 上使用 `llvm.phi.edge_move` 的 profile `emit mov` 形态生成标量 VM move，并把 result 绑定到 phi 的目标 VM 寄存器；直接 struct / 固定数组小聚合 phi 使用 `llvm.aggregate.phi.edge_move` 按叶子字段在前驱边逐个 `mov` 到预分配的 aggregate result 字段寄存器。

`select`、`switch`、动态 GEP、aggregate parameter、aggregate return、`sret`、direct/indirect native call、direct varargs native call 和 multi-block phi 需要 host context 才能计算 label、field、native call id、indirect adapter、call-site 变参类型或 ABI 参数/返回槽；这些路径的控制结构由 Rust translator 保守生成，但每条实际 VM instruction 仍从对应 lowering rule 中按 operand shape 选取具名 `emit`。同一 handler semantic 有多条 profile 指令时，普通 lowering 以 `emit` 指令名为准；host-context helper 只有在该 semantic 唯一时才允许按 semantic 选择，否则必须由 lowering rule 的具名 `emit` 消解。

### 超级指令

`isa.vm` 可以声明受限超级指令，`lowering.vm` 必须同时声明对应 `fusion` 模板。当前已实现的超级指令模板是 `iadd_xor`、`icmp_br_if`、`gep_load` 和 `load_iadd`。超级指令仍然使用普通 `instr` 语法，profile 通过 semantic AST 声明组合语义；translator 只有在 profile 存在对应 `Super(...)` semantic 且 `lowering.vm` 声明了同名 target fusion 时才启用融合，否则保留普通 VM 指令序列。

`iadd_xor` 语义为先计算 `lhs + rhs`，再与 `xor_rhs` 做 `xor`，最后按 `width` 截断：

```text
instr iadd_xor(dst: vreg<i64>, lhs: vreg<i64>, rhs: vreg<i64>, xor_rhs: vreg<i64>, width: imm<u8>) { # 超级指令：整数加法后立即异或
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
instr icmp_br_if(pred: imm<u8>, lhs: vreg<i64>, rhs: vreg<i64>, width: imm<u8>, then_pc: label, else_pc: label) { # 超级指令：整数比较后直接条件跳转
  opcode alias [0x10d, 0x10e]               # 超级分支同样支持 opcode alias
  decoded_width = 32                        # 两个 label PC 最坏各占 64 位 bitpacked operand
  semantic {                                # 语义由 verifier 解析成受限 AST
    pc = select(compare(pred, reg[lhs], reg[rhs], width), then_pc, else_pc) # 比较结果直接驱动 PC
  }                                         # 结束语义块
}                                           # 结束 icmp_br_if
```

`gep_load` 语义为先执行常量字节偏移 GEP，再直接从计算出的地址读取标量，不写中间指针寄存器：

```text
instr gep_load(dst: vreg<i64>, base: vreg<ptr>, offset: imm<u64>, width: imm<u8>) { # 超级指令：常量字节偏移 GEP 后立即读取标量
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
instr load_iadd(dst: vreg<i64>, ptr: vreg<ptr>, addend: vreg<i64>, width: imm<u8>) { # 超级指令：标量 load 后立即执行整数加法
  opcode alias [0x111, 0x112]              # 超级内存算术指令同样支持 varint opcode
  decoded_width = 32                       # 三个寄存器操作数和位宽操作数使用 32 字节 record
  semantic {                               # 语义由 verifier 解析成受限 AST
    reg[dst] = trunc_width(load_width(reg[ptr], width) + reg[addend], width) # 读内存后立即做整数加法
    pc = next                              # 执行后按当前 decoded_width 前进
  }                                        # 结束语义块
}                                          # 结束 load_iadd
```

VM IR fuse pass 对 `iadd_xor` 只融合线性相邻的 `iadd tmp, lhs, rhs` 与 `ixor dst, tmp, xor_rhs`，并且必须满足：两条指令之间没有 label target，`tmp` 没有除紧邻 `ixor` 之外的其它读取，两个操作位宽相同，没有 memory/native side effect。对 `icmp_br_if` 只融合线性相邻的 `icmp tmp, lhs, rhs` 与使用该 `tmp` 的 `br_if`，并且两条指令之间没有 label target、`tmp` 没有其它读取。对 `gep_load` 只融合线性相邻的 `gep tmp, base, offset` 与使用该 `tmp` 的 `load dst, tmp, width`，并且两条指令之间没有 label target、`tmp` 没有其它读取。对 `load_iadd` 只融合线性相邻的 `load tmp, ptr, width` 与使用该 `tmp` 的 `iadd dst, tmp, addend, width` 或 `iadd dst, addend, tmp, width`，并且两条指令之间没有 label target、`tmp` 没有其它读取、load 位宽和 add 位宽一致。任何条件不满足时保留普通指令，不生成错误的超级指令。`call_native` 不需要单独的 ret-slot-copy 超级指令：它的 bytecode record 已携带 `ret0..ret7` 目标槽和宽度，translator 会把 native thunk 返回 tuple 直接写入最终 VM 返回寄存器，从而避免紧跟在调用后的额外 `mov`。

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
- `sret`：wrapper 把返回值写到 host ABI 提供的返回指针。
- VM 内部 call：通过 `abi.vm` 指定 `call_link`、`ret_pc`、参数寄存器、返回寄存器和 clobber 集合，必须支持多返回值映射。
- native call：必须按目标 LLVM function type 和 target ABI 重新生成 call。

VM 支持多返回值，但 wrapper 必须负责把 VM 多返回值映射回 LLVM 的单返回模型或 `sret` 模型。

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

`isa.vm` 的每条 `instr` 可以用 `decoded_width = 4|8|16|32|48|64` 覆盖默认宽度；未声明时使用 `bytecode.vm` 的 `default`。`decoded_width` 表示 runtime 按 `decoder.vm` 撤销字节级变换后，一条 VM instruction record 在 code stream 中占用的 decoded 字节数，不是 opcode 位宽，也不是 operand 数量。Opcode 仍按 varint 读取，因此 opcode 可以超过 1 byte；operand 按 ISA operand schema 顺序读出。Encoder 先生成 `[opcode, bitpacked operands...]` 的 varint 字节，若长度小于 `decoded_width` 则用 0 padding 补齐，若超过则拒绝该 profile 或该函数。Runtime 以 `pc` 作为 code segment 内 decoded 字节偏移，取 opcode 后按 opcode 找到 profile 指令描述，再读取该指令声明的 operand 数量；`pc = next` 会加上当前指令的 `decoded_width`，`br`/`br_if`/`vm_call`/`vm_ret` 使用的 label PC 也都是 decoded 字节偏移。

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
  dispatch = switch    # 使用 LLVM switch 生成 dispatcher
}                      # 结束 runtime 生成策略
```

runtime profile 允许声明以下增强开关：

- threaded dispatch。
- indirect branch dispatch。
- handler splitting。
- handler order shuffle。
- opcode alias。
- per-function handler clone。

Verifier 必须拒绝不在本节枚举内的 dispatch 策略或 runtime 增强开关。

## Pass 配置

环境变量：

| 环境变量                     | 说明                                                |
|--------------------------|---------------------------------------------------|
| `AMICE_VM_VIRTUALIZE`    | 是否启用新 VMP 虚拟化 pass                                |
| `AMICE_VM_PROFILE_PATH`  | profile package 路径                                |
| `AMICE_VM_RUNTIME_SCOPE` | 覆盖 profile 中的 runtime scope，仅允许 `func` 或 `module` |
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
- 支持整数运算、标量 `float`/`double` 运算、比较、类型转换、内存访问、控制流、direct native call、direct varargs native call、indirect native call、aggregate parameter、aggregate return 和 `sret`。
- 支持 per-function opcode permutation、opcode alias、handler clone、handler order shuffle、const pool 加密、fake instruction 和 dead bytecode。
- 支持 profile 声明的受限 `iadd_xor`、`icmp_br_if`、`gep_load` 和 `load_iadd` 超级指令；不满足 fuse 条件时必须回退普通 VM 指令序列。

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

当前可虚拟化 LLVM IR 子集是 64 位小端目标上的整数/指针/标量浮点路径：整数参数、指针参数、`float`/`double` 参数、直接 struct/固定数组小聚合参数，void/scalar/小聚合/sret 返回，小聚合字段允许整数、指针、`float` 和 `double` 标量，整数算术、整数除法/取余和位运算，i8/i16/i32/i64 的 `llvm.ctpop` / `llvm.ctlz` / `llvm.cttz` / `llvm.abs` / `llvm.smax` / `llvm.smin` / `llvm.umax` / `llvm.umin` / `llvm.uadd.sat` / `llvm.usub.sat` / `llvm.sadd.sat` / `llvm.ssub.sat` / `llvm.ushl.sat` / `llvm.sshl.sat` / `llvm.uadd.with.overflow` / `llvm.sadd.with.overflow` / `llvm.usub.with.overflow` / `llvm.ssub.with.overflow` / `llvm.umul.with.overflow` / `llvm.smul.with.overflow` / `llvm.bswap` / `llvm.bitreverse` / `llvm.fshl` / `llvm.fshr` 整数 intrinsic，i1/i8/i16/i32/i64 的 `llvm.expect` / `llvm.expect.with.probability` 值保持 intrinsic，标量 `llvm.is.constant` 编译期查询 intrinsic，静态可确定对象的 `llvm.objectsize` 编译期查询 intrinsic，指针的 `llvm.launder.invariant.group` / `llvm.strip.invariant.group` / `llvm.invariant.start` / `llvm.threadlocal.address` 值保持 intrinsic，指针的 `llvm.ptrmask` intrinsic，整数 `llvm.annotation` 和指针 `llvm.ptr.annotation` 值保持 intrinsic，`float`/`double` 的 `fadd`/`fsub`/`fmul`/`fdiv`/`frem`/`fneg`、`llvm.fabs`、`llvm.sqrt`、`llvm.canonicalize`、`llvm.floor`、`llvm.ceil`、`llvm.trunc`、`llvm.rint`、`llvm.nearbyint`、`llvm.round`、`llvm.roundeven`、`llvm.fma`、`llvm.fmuladd`、`llvm.minnum`、`llvm.maxnum`、`llvm.minimum`、`llvm.maximum`、`llvm.copysign` 和 `llvm.is.fpclass`，`sitofp`/`uitofp`/`fptosi`/`fptoui`，`double` 到 `float` 的 `fptrunc`，`float` 到 `double` 的 `fpext`，整数/指针标量 `icmp` 和 `float`/`double` 标量 `fcmp`，整数/指针/`float`/`double` 标量 `select`、直接 struct/固定数组小聚合 `select`，`zext`/`sext`/`trunc`/`bitcast`/`ptrtoint`/`inttoptr`/`addrspacecast`，整数、指针、`float` 或 `double` 标量 `freeze`，小聚合 `freeze`，固定 alloca、运行时元素个数 alloca，标量 load/store、标量 volatile load/store、固定布局小聚合普通/volatile load/store，自然对齐整数/指针/`float`/`double` 标量 atomic load/store，自然对齐整数标量 `atomicrmw xchg/add/sub/and/or/xor/nand/min/max/umin/umax/uinc_wrap/udec_wrap/usub_cond/usub_sat` 及其 volatile 形式，自然对齐 `float`/`double` 标量 `atomicrmw fadd/fsub/fmax/fmin/fmaximum/fminimum` 及其 volatile 形式，自然对齐整数/指针标量 `cmpxchg`（weak 按 strong 形式发射），默认 syncscope 的 acquire/release/acq_rel/seq_cst `fence`，常量 GEP、多动态下标 GEP，以及常量 struct 字段选择和 array/pointer 动态下标混合的 GEP，固定长度且非 volatile 的 `llvm.memcpy` / `llvm.memmove` / `llvm.memset`（不超过 64 字节内联展开，超过 64 字节走动态 handler），以及动态长度且非 volatile 的 `llvm.memcpy` / `llvm.memmove` / `llvm.memset`，`llvm.trap` / `llvm.debugtrap` / `llvm.ubsantrap`，`llvm.invariant.end`，`llvm.prefetch`，`llvm.experimental.noalias.scope.decl`，`llvm.donothing`，`llvm.lifetime.start` / `llvm.lifetime.end`，`llvm.assume`，`llvm.dbg.*`，`llvm.var.annotation`，`llvm.codeview.annotation`，`br`、条件 `br`、`switch`、`unreachable`、loop/phi edge move，direct native call、direct varargs native call 和 indirect native call。标量浮点值在 VM x 寄存器中按原始 bit 保存，`llvm.fabs` 通过 profile 中的 `float_unary(fabs, ...)` semantic 进入 runtime 并清除 IEEE 符号位，`llvm.canonicalize` 通过 profile 中的 `float_unary(fcanonicalize, ...)` semantic 进入 runtime 并调用 LLVM `llvm.canonicalize.f32/f64` intrinsic，`llvm.minnum` 和 `llvm.maxnum` 通过 profile 中的 `float_bin(fminnum/fmaxnum, ...)` semantic 进入 runtime 并调用 LLVM `llvm.minnum.f32/f64` / `llvm.maxnum.f32/f64` intrinsic，`llvm.minimum` 和 `llvm.maximum` 通过 profile 中的 `float_bin(fminimum/fmaximum, ...)` semantic 进入 runtime 并调用 LLVM `llvm.minimum.f32/f64` / `llvm.maximum.f32/f64` intrinsic，`llvm.copysign` 通过 profile 中的 `float_bin(fcopysign, ...)` semantic 进入 runtime 并用整数 mask 复制 IEEE 符号位，`llvm.is.fpclass` 通过 profile 中的 `float_class` semantic 进入 runtime，按 LLVM `FPClassTest` mask 区分 signaling NaN、quiet NaN、正负无穷、normal、subnormal 和 zero；指针和 `addrspacecast` 按 64 位原始 bit 保留，普通 load/store 复用同一套标量内存读写；标量 volatile load/store 通过 profile 中的 `volatile_load_width` / `volatile_store_width` semantic 生成带 volatile 标记的逐字节 LLVM load/store；普通固定布局小聚合 load/store 通过 profile 中的 `llvm.memory.aggregate.load` / `llvm.memory.aggregate.store` rule 按 data layout 展开为逐叶子字段 `gep`、`load`、`store` 和 `mov`，volatile 固定布局小聚合 load/store 通过 `llvm.memory.volatile.aggregate.load` / `llvm.memory.volatile.aggregate.store` rule 展开为逐叶子字段 `gep`、`volatile_load`、`volatile_store` 和 `mov`，不引入聚合专用固定解释器；atomic load/store、atomicrmw、cmpxchg 和 fence 使用独立 profile semantic 和 runtime handler，按 bytecode 中的 ordering operand 生成真正的 LLVM atomic load/store/atomicrmw/cmpxchg/fence；volatile atomicrmw 通过 profile 中的 `volatile_atomic_rmw(...)` semantic 进入同一套 runtime handler，并额外设置 LLVM atomicrmw 的 volatile 标记；浮点 atomic load/store 通过同宽整数 bit pattern 原子搬运，不改变 VM x 寄存器表示；整数 atomicrmw 的扩展变体通过 profile 中的 `atomic_rmw(uinc_wrap/udec_wrap/usub_cond/usub_sat, ...)` semantic 进入 runtime，并生成 LLVM 原生无符号回绕、条件减法和饱和减法 `atomicrmw`；浮点 atomicrmw 通过 profile 中的 `atomic_rmw(fadd/fsub/fmax/fmin/fmaximum/fminimum, ...)` semantic 进入 runtime，运行时按 `width` 把 x 寄存器 bit 还原成 `float` 或 `double`，再生成 LLVM 原生浮点 `atomicrmw`。LLVM weak `cmpxchg` 允许但不要求伪失败，AMICE runtime 按 strong `cmpxchg` 发射，不额外记录 weak bit。`cmpxchg` lowering 会把 `{old, success}` 结果记录成 aggregate binding，后续 `extractvalue` 再读取两个 VM 寄存器。`unreachable` 通过 profile 中的 `unreachable` semantic 进入 runtime；执行到该 handler 时 runtime 生成 LLVM `unreachable`，不写返回槽，也不把 UB 路径变成正常 `ret`。`llvm.trap`、`llvm.debugtrap` 和 `llvm.ubsantrap` 通过 profile 中的 `trap` semantic 进入 runtime；执行到该 handler 时 runtime 先调用 LLVM `llvm.trap` intrinsic，再生成 LLVM `unreachable` 终止该路径，因此不会把陷阱副作用退化成普通 UB；`llvm.ubsantrap` 的 immarg 只作为诊断类别检查为编译期常量，不写入 VM 状态。标量 `select` 和聚合 `select` 都由 profile 中的 `br_if`/`mov`/`br` 序列生成，不依赖硬编码 handler；聚合 `select` 使用 `llvm.select.aggregate` rule 按叶子字段复制并在 join label 组成新的 aggregate binding；`iadd_xor`、`icmp_br_if`、`gep_load` 和 `load_iadd` 超级指令由 profile semantic 控制，只有相邻且临时寄存器无额外 use 的普通 VM 指令序列才会被融合；direct native call、direct varargs native call 和 indirect native call 的参数和返回值同样通过 x 寄存器的 i64 bit pattern 跨 thunk 边界传递，支持整数、指针、`float`、`double`、小聚合参数和小聚合返回，direct varargs native call 的 thunk 参数类型来自实际 call-site 变参 operand，聚合参数在 call lowering 时按叶子字段写入 native arg 槽，native thunk 再按原 callee ABI 或 call-site ABI 重建 struct/固定数组，返回 tuple 会直接写入 bytecode 中的最终返回槽，不再依赖后续返回槽 `mov`；`ctpop`、`ctlz`、`cttz`、`abs`、`bswap` 和 `bitreverse` 通过 profile 中的 `int_unary` semantic 进入 runtime，并由纯 LLVM IR 位运算生成；`smax`、`smin`、`umax`、`umin` 和标量饱和加减/左移通过 profile 中的 `int_bin` semantic 进入 runtime，并按 width 选择有符号、无符号或饱和钳制语义；`uadd.with.overflow`、`sadd.with.overflow`、`usub.with.overflow`、`ssub.with.overflow`、`umul.with.overflow` 和 `smul.with.overflow` 通过 profile 中的 `int_overflow` semantic 进入 runtime，结果 `{value, overflow}` 会记录成 aggregate binding 并由后续 `extractvalue` 读取，乘法溢出在 runtime 中临时扩展到 i128 判定；`fshl` 和 `fshr` 通过 profile 中的 `int_ternary` semantic 进入 runtime，并按 LLVM funnel shift 语义执行；`expect`/`expect.with.probability`、`launder/strip.invariant.group`、`invariant.start` 和 `annotation` 值保持 intrinsic 通过 profile 中的 `mov` semantic 返回运行时值 operand，不物化 expected、probability、invariant-group、invariant 描述符长度或 annotation metadata 提示；`llvm.is.constant` 在 translator 中按 operand 是否为 LLVM 常量折叠成 i1，并通过 profile 的 `mov_imm` 或 `const_load` 常量物化路径进入 VM，不生成专用 runtime handler；`llvm.objectsize` 在 translator 中对 fixed alloca、GlobalVariable 和常量偏移 GEP 折叠成剩余对象字节数，并通过 profile 的 `llvm.objectsize.integer` rule 物化为 VM 常量；`llvm.threadlocal.address` 通过 profile 中的 `tls_addr` 指令保留运行时指针 bit；`llvm.ptrmask` 通过 profile 中的 `ptrmask` 指令按 64 位地址 bit 执行 and mask，仅支持 i64 mask；`fneg`、`llvm.fabs`、`llvm.copysign` 和标量整数/浮点转换直接在 runtime 内按 `width` 或 `from_width`/`to_width` 还原为 LLVM IR；`frem` runtime 使用 `a - trunc(a / b) * b` 的纯 IR 形式，覆盖普通有限输入，避免生成 `fmod/fmodf` 外部依赖。`udiv`、`sdiv`、`urem`、`srem`、`fptosi`、`fptoui`、`atomicrmw` 和 `cmpxchg` 对除零、溢出、越界、NaN 或并发竞争等 LLVM 未定义/目标定义输入沿用 LLVM 原语义，不在 VM runtime 中重新定义；动态 alloca 的字节数由 runtime 中的 `reg[count] * elem_size` 计算，零大小、过大或溢出输入仍沿用 LLVM/目标栈分配边界；动态和过大固定长度 `llvm.memcpy` 使用前向字节复制，沿用 LLVM 对重叠输入的未定义行为边界；动态和过大固定长度 `llvm.memmove` 在 runtime 中按地址方向选择前向或后向字节复制，保留源目标重叠语义；动态和过大固定长度 `llvm.memset` 在 runtime 中循环写入填充值低 8 位。`llvm.ctlz` / `llvm.cttz` 在 `is_zero_undef=false` 时定义零输入结果，在 `is_zero_undef=true` 时对零输入沿用 LLVM poison/UB 边界；`llvm.abs` 在 `is_int_min_poison=false` 时定义有符号最小值输入结果，在 `is_int_min_poison=true` 时对有符号最小值输入沿用 LLVM poison/UB 边界；`freeze` 对非 poison 标量输入通过 `mov` 保留原值，对 poison/undef 标量选择稳定的 0/null；小聚合 `freeze` 会按 profile 中的 `llvm.aggregate.freeze` lowering rule 逐叶子字段发射 `mov`，已有字段保留原 bit pattern，缺失或 undef/poison 字段选择稳定的 0/null bit pattern；`llvm.invariant.end`、`llvm.prefetch`、`llvm.experimental.noalias.scope.decl`、`llvm.donothing`、`llvm.lifetime.start/end`、`llvm.assume`、`llvm.dbg.*`、`llvm.var.annotation` 和 `llvm.codeview.annotation` 通过 profile 中的 Nop semantic 发射 `fake_nop`，只保留字节码位置扰动，不改变 VM 状态，其中 `llvm.assume` 的条件不会在 VM 中重新检查，只沿用 LLVM 对违反假设输入的未定义行为边界。`llvm.memmove` 固定长度 lowering 先读取所有源分块再写目标分块，用于保留小固定长度重叠复制语义。GEP lowering 使用 module data layout 计算 struct 字段 ABI 字节偏移，动态 index 则由 VM 内的 `imul`/`iadd`/`gep` profile emit 组合完成。`q0..q64` 固定存在，但内置 profile 通过 `q.lowering = disabled` 禁用宽值 lowering；任何依赖 q 寄存器的 ABI、lowering 或 semantic 都必须被 verifier 拒绝。

volatile atomic load/store 通过 profile 中的 `volatile_atomic_load_width` / `volatile_atomic_store_width` semantic 进入 runtime，运行时生成同宽整数原子 load/store，并同时设置 LLVM atomic ordering 和 volatile 标记；它仍要求默认 syncscope、自然对齐和 8/16/32/64 位整数/指针/`float`/`double` 标量内存宽度。

volatile atomicrmw 通过 profile 中的 `volatile_atomic_rmw(op, ...)` semantic 进入 runtime，运行时生成同名 LLVM `atomicrmw` 并设置 volatile 标记；整数、指针、浮点类型、自然对齐、syncscope 和 ordering 限制与普通 `atomicrmw` 相同。

volatile `cmpxchg` 通过 profile 中的 `volatile_cmpxchg` semantic 进入 runtime，生成带 volatile 标记的 LLVM `cmpxchg`，其它类型、自然对齐、syncscope 和 success/failure ordering 限制与普通 `cmpxchg` 相同。

volatile `llvm.memcpy` / `llvm.memmove` / `llvm.memset` 通过 profile 中的 `volatile_memcpy_dynamic` / `volatile_memmove_dynamic` / `volatile_memset_dynamic` semantic 进入 runtime；translator 会把常量长度和动态长度都归一化到逐字节 dynamic handler，以保留 volatile load/store 的访问粒度和标记。

非 volatile `llvm.memcpy.inline` 和 `llvm.memset.inline` 分别通过同一套 `llvm.memory.copy.fixed` 和 `llvm.memory.set.fixed` profile lowering 处理。它们要求 LLVM IR 中的长度 operand 是 `immarg` 常量；长度不超过内联阈值时按固定 `load` / `store` / `gep` 序列展开，超过阈值时分别走 `memcpy_dyn` 或 `memset_dyn` handler。当前不声明独立 inline memory ISA semantic，避免让同一内存语义在 profile 中分裂成两套行为。

取整类浮点 intrinsic 通过 profile 中的 `float_unary(ffloor/fceil/ftrunc/frint/fnearbyint/fround/froundeven, ...)` semantic 进入 runtime，并调用 LLVM `llvm.floor.f32/f64`、`llvm.ceil.f32/f64`、`llvm.trunc.f32/f64`、`llvm.rint.f32/f64`、`llvm.nearbyint.f32/f64`、`llvm.round.f32/f64` 和 `llvm.roundeven.f32/f64` intrinsic。

`llvm.ssa.copy` 通过 profile 中的 `llvm.ssa.copy.scalar` lowering rule 发射 `mov`，支持整数、指针、`float` 和 `double` 标量；LLVM 标量 `bitcast` 通过 profile 中的 `llvm.cast.bitcast.scalar` lowering rule 发射 `bitcast`，只支持 x 寄存器可承载的同宽整数/`float`/`double` bit pattern reinterpret；向量和 `half`、`bfloat`、`fp128` 等非 `float`/`double` 浮点形态仍安全跳过。

`llvm.readcyclecounter` 和 `llvm.readsteadycounter` 通过 profile 中的 `read_counter(cycle|steady)` semantic 进入 runtime；runtime 生成 LLVM IR 时声明并调用对应 LLVM intrinsic，结果以 i64 bit pattern 写入 x 寄存器。translator 只接受无参数且返回 i64 的声明，参数数量不匹配或返回宽度不是 i64 时安全跳过该函数。

`llvm.objectsize` 通过 profile 中的 `llvm.objectsize.integer` lowering rule 发射 `mov_imm` 或 profile 选择的等价常量物化路径。translator 只在能静态证明对象大小时折叠：直接 fixed alloca、直接 `GlobalVariable`、以及只含常量下标且最终回到上述对象的 GEP；结果是从当前指针偏移到对象末尾的剩余字节数。动态 alloca、函数参数指针、动态 GEP、非 GlobalVariable、偏移越界、immarg 非 i1 常量或结果宽度装不下大小时安全跳过该函数。

补充：上述浮点 intrinsic 白名单包括 `float`/`double` 标量 `llvm.sqrt`、`llvm.canonicalize`、`llvm.floor`、`llvm.ceil`、`llvm.trunc`、`llvm.rint`、`llvm.nearbyint`、`llvm.round`、`llvm.roundeven`、`llvm.fma`、`llvm.fmuladd`、`llvm.minnum`、`llvm.maxnum`、`llvm.minimum` 和 `llvm.maximum`。`llvm.sqrt` 必须通过 profile 中的 `llvm.sqrt.float` lowering rule 发射 `fsqrt`，再由 `float_unary(fsqrt, ...)` semantic 驱动 runtime；`llvm.canonicalize` 必须通过 profile 中的 `llvm.canonicalize.float` lowering rule 发射 `fcanonicalize`，再由 `float_unary(fcanonicalize, ...)` semantic 驱动 runtime；`llvm.floor`、`llvm.ceil`、`llvm.trunc`、`llvm.rint`、`llvm.nearbyint`、`llvm.round` 和 `llvm.roundeven` 必须分别通过 profile 中的同名 `llvm.*.float` lowering rule 发射 `ffloor`、`fceil`、`ftrunc`、`frint`、`fnearbyint`、`fround` 和 `froundeven`，再由对应 `float_unary(...)` semantic 驱动 runtime；`llvm.fma` 必须通过 profile 中的 `llvm.fma.float` lowering rule 发射 `ffma`，再由 `float_ternary(fma, ...)` semantic 驱动 runtime；`llvm.fmuladd` 必须通过 profile 中的 `llvm.fmuladd.float` lowering rule 发射 `ffmuladd`，再由 `float_ternary(fmuladd, ...)` semantic 驱动 runtime；`llvm.minnum` 和 `llvm.maxnum` 必须通过 profile 中的 `llvm.minnum.float` / `llvm.maxnum.float` lowering rule 发射 `fminnum` / `fmaxnum`，再由 `float_bin(fminnum/fmaxnum, ...)` semantic 驱动 runtime；`llvm.minimum` 和 `llvm.maximum` 必须通过 profile 中的 `llvm.minimum.float` / `llvm.maximum.float` lowering rule 发射 `fminimum` / `fmaximum`，再由 `float_bin(fminimum/fmaximum, ...)` semantic 驱动 runtime。runtime 只能声明和调用 LLVM `llvm.sqrt.f32/f64`、`llvm.canonicalize.f32/f64`、`llvm.floor.f32/f64`、`llvm.ceil.f32/f64`、`llvm.trunc.f32/f64`、`llvm.rint.f32/f64`、`llvm.nearbyint.f32/f64`、`llvm.round.f32/f64`、`llvm.roundeven.f32/f64`、`llvm.fmuladd.f32/f64`、`llvm.minnum.f32/f64`、`llvm.maxnum.f32/f64`、`llvm.minimum.f32/f64` 与 `llvm.maximum.f32/f64` intrinsic，不能退化为固定外部 `sqrt/sqrtf/fma/fmaf/fmin/fminf/fmax/fmaxf` 符号；选择 `fmuladd` 是为了让包含 `ffma` 或 `ffmuladd` handler 的完整 profile 在未实际执行乘加字节码时也不会给所有虚拟化样本引入 libm 链接要求。

其中 `llvm.threadlocal.address` 的 `tls_addr` 是 VM SSA 绑定标记：TLS `GlobalValue` operand 留在 AMICE 生成的私有 native thunk 中，由 `call_native` 取回运行时地址，不能写入 const_pool 或作为普通整数立即数固化。

普通 LLVM `GlobalValue` 指针 operand，例如 `load i32, ptr @g`、`store ..., ptr @g`、可剥离 pointer `bitcast` / `addrspacecast` 后指向 `GlobalValue` 的常量表达式，以及全部下标为常量的 GEP，通过 AMICE 生成的私有 `global_addr` native thunk 取回重定位后的运行时基址，再由 profile 中的 `global_addr` 指令绑定到 VM x 寄存器。常量 GEP 的 base 可以是 `GlobalValue`、可剥离 pointer cast，或另一层可递归规约的常量 GEP，每层常量 GEP 偏移继续走 profile 的 `llvm.gep.constant` 规则。

常量表达式形式的 `ptrtoint` 会先按上述规则物化运行时指针，再通过 profile 的 `llvm.constexpr.ptrtoint` 规则发射 `zext` / `trunc` / `bitcast`；常量表达式形式的 `inttoptr` 会先物化整数 operand，再通过 `llvm.constexpr.inttoptr` 发射同一组 cast handler。整数常量表达式形式的 `add`、`sub`、`mul`、`udiv`、`sdiv`、`urem`、`srem`、`xor`、`and`、`or`、`shl`、`lshr` 和 `ashr` 会递归物化左右 operand，再通过 `llvm.constexpr.integer.binop` 规则发射 profile ISA 中对应的整数 ALU handler；如果输入模块中仍存在整数常量表达式形式的 `zext`、`sext`、`trunc` 或等宽 `bitcast`，translator 会通过 `llvm.constexpr.integer.cast` 规则发射 profile ISA 中对应的 cast handler。LLVM 21 文本 IR parser 通常已经拒绝 `zext` / `sext` / `trunc` / `shl` / `or` 这类历史 ConstantExpr 写法，因此集成测试主要覆盖仍能由 LLVM 21 接收的 pointer cast、GEP，以及 `add` / `sub` / `xor` 形式的整数二元常量表达式。vector/aggregate 常量表达式、非等宽 integer `bitcast`、不能规约为可重定位 `GlobalValue` 基址加若干层常量字节偏移的非空指针常量表达式，仍安全跳过，避免在 VM const_pool 中固化地址或丢失目标重定位语义。

`insertvalue`、`extractvalue`、aggregate `select`、aggregate parameter、aggregate return、普通/volatile aggregate load/store、native call aggregate parameter 和 native call aggregate return 支持直接 struct、直接固定数组、嵌套 struct、固定数组组成的小聚合，前提是最终叶子字段都是整数、指针、`float` 或 `double` 标量。direct aggregate parameter 在 wrapper 入口按叶子字段展平成 host-to-VM x 参数槽，并在 translator 初始化时恢复成 VM 聚合绑定；direct/indirect native call aggregate parameter 在调用点按同一 leaf 顺序展平成 native arg 槽，thunk 入口再重建为 callee 的真实 struct/固定数组参数；direct、native 和 indirect call 的固定数组返回都按同一规则展平到 VM 返回槽。translator 按 LLVM 聚合类型声明顺序把叶子字段展平成 VM 聚合绑定槽；例如 `{ i8, { i32, i64 }, [2 x i16] }` 会映射为 `i8/i32/i64/i16/i16` 五个槽。普通和 volatile aggregate load/store 额外使用 module data layout 计算每个叶子字段的 ABI 字节偏移，偏移为 0 的字段直接走 `load`/`store` 或 `volatile_load`/`volatile_store`，非零偏移字段先走 profile `gep` 再走相同内存 handler；store 源必须来自已支持的聚合 lowering，缺失 undef 字段或未经过 `freeze` 的 undef/poison 聚合会安全跳过。aggregate `select` 的 then/else 两侧必须都是已支持聚合 lowering 产生的绑定；translator 只发射一次 `br_if`，再在 then/else 片段按叶子字段执行 `llvm.select.aggregate` rule 中的 `mov`，最后把字段寄存器组成新的 aggregate binding。`insertvalue` 插入整个子 struct 或固定数组、`extractvalue` 读取整个子 struct 或固定数组时，translator 会按 profile 中的 `llvm.aggregate.insert.subaggregate` / `llvm.aggregate.extract.subaggregate` rule 逐叶子字段发射 `mov`，并把结果重新组成 VM 聚合绑定；缺失的 undef 字段保持未绑定，只有后续读取该字段时才安全跳过。vector 叶子、非 `float`/`double` 浮点叶子、空聚合参数/返回、空聚合 load/store、atomic aggregate load/store、超过 8 个 host-to-VM 或 native_call 参数槽的聚合参数和超过 ABI 返回槽数量的聚合仍必须安全跳过。

aggregate `phi` 支持同一组直接 struct / 固定数组小聚合。translator 会在进入 basic block lowering 前为 phi 结果预分配稳定的叶子字段寄存器，并在每条 predecessor edge 上执行 `llvm.aggregate.phi.edge_move` rule，把 incoming 聚合字段复制到这些目标寄存器。incoming 聚合必须来自已支持的 aggregate lowering；未冻结 undef/poison、缺失字段或字段类型不匹配时仍安全跳过。

`amice-simple-vmp` 是默认兼容性 profile；`ruoke` 是压力测试/示例 profile，声明 1000 个唯一 opcode alias，并要求 runtime emitter 为这些 alias 生成独立可分发 handler case。

volatile `llvm.memcpy` / `llvm.memmove` / `llvm.memset` 已按上一段说明走 profile 驱动的 dynamic volatile handler。自然对齐整数/指针标量 volatile `cmpxchg` 和自然对齐标量 volatile `atomicrmw` 已支持，safe-skip 只保留类型、对齐、syncscope 或 ordering 超出当前边界的情况。

以下情况必须安全跳过目标函数并输出 debug 日志：被虚拟化目标函数本身是 varargs，间接 varargs call，向量值，`half`、`bfloat`、`fp128`、`x86_fp80`、`ppc_fp128` 等非 `float`/`double` 浮点值，非 `float`/`double` 端点的浮点转换，向量 `freeze`，向量或非 `float`/`double` 的 `llvm.fabs` / `llvm.sqrt` / `llvm.canonicalize` / `llvm.floor` / `llvm.ceil` / `llvm.trunc` / `llvm.rint` / `llvm.nearbyint` / `llvm.round` / `llvm.roundeven` / `llvm.fma` / `llvm.fmuladd` / `llvm.minnum` / `llvm.maxnum` / `llvm.minimum` / `llvm.maximum` / `llvm.copysign` / `llvm.is.fpclass`，`llvm.is.fpclass` mask 超出 LLVM `FPClassTest` 当前 `0x03ff` 有效位范围，未经过 `freeze` 的 undef/poison 值，聚合 `select` 的 then/else 不是已支持聚合 lowering 产生的完整绑定，非默认 atomic syncscope，浮点 cmpxchg，非自然对齐或非 8/16/32/64 位整数/指针/`float`/`double` atomic load/store，非自然对齐或非 8/16/32/64 位整数/指针 atomicrmw/cmpxchg，非自然对齐或非 32/64 位 `float`/`double` atomicrmw，release/acq_rel failure ordering、unordered success/failure ordering 或 failure ordering 强于 success ordering 的 `cmpxchg`，unordered/monotonic/notatomic ordering 的 `fence`，非 i8/i16/i32/i64 宽度的整数 intrinsic，非 i1/i8/i16/i32/i64 宽度的 `llvm.expect` / `llvm.expect.with.probability`，非标量或 undef/poison operand 的 `llvm.is.constant`，动态 alloca、函数参数指针、动态 GEP、非 GlobalVariable、偏移越界、immarg 非 i1 常量或结果宽度装不下大小的 `llvm.objectsize`，非整数签名的 `llvm.annotation`，非指针签名的 `llvm.ptr.annotation` / `llvm.launder.invariant.group` / `llvm.strip.invariant.group` / `llvm.invariant.start` / `llvm.threadlocal.address` / `llvm.ptrmask`，非 i64 mask 的 `llvm.ptrmask`，`llvm.invariant.start/end` 或 `llvm.prefetch` 的 immarg 不是编译期常量，`llvm.ubsantrap` 的 trap kind 不是编译期常量，除 `ctpop`/`ctlz`/`cttz`/`abs`/`smax`/`smin`/`umax`/`umin`/`uadd.sat`/`usub.sat`/`sadd.sat`/`ssub.sat`/`ushl.sat`/`sshl.sat`/`uadd.with.overflow`/`sadd.with.overflow`/`usub.with.overflow`/`ssub.with.overflow`/`umul.with.overflow`/`smul.with.overflow`/`bswap`/`bitreverse`/`fshl`/`fshr`、`expect`/`expect.with.probability`、`is.constant`、`objectsize`、`fabs`、`sqrt`、`canonicalize`、`floor`、`ceil`、`trunc`、`rint`、`nearbyint`、`round`、`roundeven`、`fma`、`fmuladd`、`minnum`、`maxnum`、`minimum`、`maximum`、`copysign`、`is.fpclass`、`annotation`/`ptr.annotation`、`threadlocal.address`、`ptrmask`、`trap`/`debugtrap`/`ubsantrap`、`launder.invariant.group`/`strip.invariant.group`、`invariant.start`/`invariant.end`、`prefetch`、`experimental.noalias.scope.decl`、`donothing`、`lifetime.start`/`lifetime.end`、`assume`、`dbg.*`、`var.annotation`、`codeview.annotation` 、固定长度 memory intrinsic 和动态长度 `llvm.memcpy`/`llvm.memmove`/`llvm.memset` 外的其它 LLVM intrinsic，非 integral pointer 或目标相关地址空间语义超出 64 位 bit 保留模型的 `addrspacecast`，`va_arg`、`invoke`、`callbr`、`indirectbr`、`landingpad`、`resume`、`catchswitch`、`catchpad`、`catchret`、`cleanuppad`、`cleanupret` 等异常或非结构化控制流、除固定布局小聚合普通/volatile load/store 外的非标量内存、动态 struct 字段选择或不可按 data layout 归一化为字节偏移的复杂 GEP、超过 ABI 或 VM 寄存器容量的参数/返回/活跃 SSA 值、profile 未覆盖的 lowering rule、profile verifier 拒绝的 ABI/ISA/bytecode/decoder/runtime 配置。
