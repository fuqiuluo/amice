//! 从 `isa.vm` 解析出的 ISA 与 semantic AST。
//!
//! # 契约
//! 指令名、opcode alias、operand 顺序和 handler 语义都来自 profile。AMICE 会把解析后的
//! `semantic {}` block 映射到这个有限 AST，使 verifier 和 runtime emitter 可以在不执行
//! profile 提供代码的前提下推导副作用。
//!
//! # 不变量
//! - `InstructionDesc::name` 是保留到 VM IR 和 bytecode 的 identity。
//! - `opcode_aliases` 不能为空，并且在整个 profile 内必须唯一。
//! - `effect` 来自 semantic AST，并会和选中的后端 handler template 交叉检查。
//! - q-register 引用以文本形式跟踪，使 `q.lowering = disabled` 能在 lowering 开始前拒绝它们。

use serde::{Deserialize, Serialize};

use crate::lowering::NATIVE_CALL_MAX_RETURNS;

/// profile 中声明的 opcode 值。
///
/// bytecode 仍用 varint 编码；这里用 `u16` 是为了让示例 profile 能覆盖超过 8 bit
/// 的 opcode 空间，同时避免把寄存器编号等其它小整数类型一起放宽。
pub type Opcode = u16;

/// 一条 decoded VM instruction record 允许占用的字节数。
pub const SUPPORTED_DECODED_WIDTHS: &[u8] = &[4, 8, 16, 32, 48, 64];

/// 内置 VM profile 支持的整数 ALU 操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinOp {
    /// wrapping 整数加法。
    Add,
    /// wrapping 整数减法。
    Sub,
    /// wrapping 整数乘法。
    Mul,
    /// 无符号整数除法；除数为 0 时沿用 LLVM 未定义行为。
    UDiv,
    /// 有符号整数除法；除数为 0 或最小值除以 -1 时沿用 LLVM 未定义行为。
    SDiv,
    /// 无符号整数取余；除数为 0 时沿用 LLVM 未定义行为。
    URem,
    /// 有符号整数取余；除数为 0 或最小值对 -1 取余时沿用 LLVM 未定义行为。
    SRem,
    /// 按位异或。
    Xor,
    /// 按位与。
    And,
    /// 按位或。
    Or,
    /// 逻辑左移。
    Shl,
    /// 逻辑右移。
    LShr,
    /// 算术右移。
    AShr,
    /// 有符号最大值，等价于 LLVM `smax` intrinsic。
    SMax,
    /// 有符号最小值，等价于 LLVM `smin` intrinsic。
    SMin,
    /// 无符号最大值，等价于 LLVM `umax` intrinsic。
    UMax,
    /// 无符号最小值，等价于 LLVM `umin` intrinsic。
    UMin,
    /// 无符号饱和加法，等价于 LLVM `uadd.sat` intrinsic。
    UAddSat,
    /// 无符号饱和减法，等价于 LLVM `usub.sat` intrinsic。
    USubSat,
    /// 有符号饱和加法，等价于 LLVM `sadd.sat` intrinsic。
    SAddSat,
    /// 有符号饱和减法，等价于 LLVM `ssub.sat` intrinsic。
    SSubSat,
    /// 无符号饱和左移，等价于 LLVM `ushl.sat` intrinsic。
    UShlSat,
    /// 有符号饱和左移，等价于 LLVM `sshl.sat` intrinsic。
    SShlSat,
}

/// 内置 VM profile 支持的整数溢出标志计算。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntOverflowOp {
    /// 无符号加法溢出，等价于 LLVM `uadd.with.overflow` intrinsic 的 flag。
    UAdd,
    /// 有符号加法溢出，等价于 LLVM `sadd.with.overflow` intrinsic 的 flag。
    SAdd,
    /// 无符号减法溢出，等价于 LLVM `usub.with.overflow` intrinsic 的 flag。
    USub,
    /// 有符号减法溢出，等价于 LLVM `ssub.with.overflow` intrinsic 的 flag。
    SSub,
    /// 无符号乘法溢出，等价于 LLVM `umul.with.overflow` intrinsic 的 flag。
    UMul,
    /// 有符号乘法溢出，等价于 LLVM `smul.with.overflow` intrinsic 的 flag。
    SMul,
}

/// 内置 VM profile 支持的标量整数一元操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntUnaryOp {
    /// 统计置位 bit 数，等价于 LLVM `ctpop` intrinsic。
    CtPop,
    /// 统计前导零 bit 数，等价于 `is_zero_undef=false` 的 LLVM `ctlz` intrinsic。
    CtLz,
    /// 统计尾随零 bit 数，等价于 `is_zero_undef=false` 的 LLVM `cttz` intrinsic。
    CtTz,
    /// 计算有符号绝对值，等价于 `is_int_min_poison=false` 的 LLVM `abs` intrinsic。
    Abs,
    /// 按整数位宽做字节序反转，等价于 LLVM `bswap` intrinsic。
    BSwap,
    /// 按整数位宽做 bit 顺序反转，等价于 LLVM `bitreverse` intrinsic。
    BitReverse,
}

/// 内置 VM profile 支持的标量整数三元操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntTernaryOp {
    /// 左 funnel shift，等价于 LLVM `fshl` intrinsic。
    FShl,
    /// 右 funnel shift，等价于 LLVM `fshr` intrinsic。
    FShr,
}

/// 内置 profile 当前支持的受限超级指令模板。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SuperOp {
    /// 先做 wrapping add，再把结果与第三个 operand 做 xor。
    AddXor,
    /// 先做整数比较，再直接按比较结果选择两个 bytecode 目标。
    IcmpBrIf,
    /// 先做常量字节偏移指针运算，再从计算出的地址加载标量。
    GepLoad,
    /// 先从指针读取标量，再与寄存器加数做整数加法。
    LoadAdd,
}

/// 内置 VM profile 支持的硬件/系统计数器读取。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CounterKind {
    /// LLVM `llvm.readcyclecounter`，读取目标相关 cycle counter。
    Cycle,
    /// LLVM `llvm.readsteadycounter`，读取单调 steady counter。
    Steady,
}

/// 内置 VM profile 支持的标量浮点 ALU 操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FloatBinOp {
    /// IEEE 浮点加法。
    Add,
    /// IEEE 浮点减法。
    Sub,
    /// IEEE 浮点乘法。
    Mul,
    /// IEEE 浮点除法。
    Div,
    /// IEEE 浮点余数。
    Rem,
    /// IEEE minnum，等价于 LLVM `llvm.minnum` intrinsic。
    MinNum,
    /// IEEE maxnum，等价于 LLVM `llvm.maxnum` intrinsic。
    MaxNum,
    /// IEEE minimum，等价于 LLVM `llvm.minimum` intrinsic。
    Minimum,
    /// IEEE maximum，等价于 LLVM `llvm.maximum` intrinsic。
    Maximum,
    /// 复制第二个 operand 的符号位到第一个 operand，等价于 LLVM `llvm.copysign`。
    CopySign,
}

/// 内置 VM profile 支持的标量浮点一元操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FloatUnaryOp {
    /// IEEE 浮点取反，等价于 LLVM `fneg`。
    Neg,
    /// IEEE 浮点绝对值，等价于 LLVM `llvm.fabs`。
    Abs,
    /// IEEE 浮点平方根，等价于 LLVM `llvm.sqrt`。
    Sqrt,
    /// IEEE 浮点 canonicalize，等价于 LLVM `llvm.canonicalize`。
    Canonicalize,
    /// 向负无穷方向取整，等价于 LLVM `llvm.floor`。
    Floor,
    /// 向正无穷方向取整，等价于 LLVM `llvm.ceil`。
    Ceil,
    /// 向零方向取整，等价于 LLVM 浮点 `llvm.trunc` intrinsic。
    Trunc,
    /// 按当前舍入模式取整，等价于 LLVM `llvm.rint`。
    Rint,
    /// 按当前舍入模式取整但不触发不精确异常，等价于 LLVM `llvm.nearbyint`。
    NearbyInt,
    /// 四舍五入到远离零的整数，等价于 LLVM `llvm.round`。
    Round,
    /// 四舍五入到最近偶数，等价于 LLVM `llvm.roundeven`。
    RoundEven,
}

/// 内置 VM profile 支持的标量浮点三元操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FloatTernaryOp {
    /// IEEE fused multiply-add，等价于 LLVM `llvm.fma`。
    Fma,
    /// 可融合乘加，等价于 LLVM `llvm.fmuladd` intrinsic。
    MulAdd,
}

/// 内置 VM profile 支持的标量整数/浮点转换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FloatCastOp {
    /// 有符号整数转 IEEE 浮点，等价于 LLVM `sitofp`。
    SignedIntToFloat,
    /// 无符号整数转 IEEE 浮点，等价于 LLVM `uitofp`。
    UnsignedIntToFloat,
    /// IEEE 浮点转有符号整数，等价于 LLVM `fptosi`。
    FloatToSignedInt,
    /// IEEE 浮点转无符号整数，等价于 LLVM `fptoui`。
    FloatToUnsignedInt,
    /// IEEE 浮点截断，等价于 LLVM `fptrunc`。
    FloatTrunc,
    /// IEEE 浮点扩展，等价于 LLVM `fpext`。
    FloatExt,
}

/// VM `icmp` 使用的整数比较谓词。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum CmpPredicate {
    /// 相等比较。
    Eq,
    /// 不相等比较。
    Ne,
    /// 无符号大于比较。
    Ugt,
    /// 无符号大于等于比较。
    Uge,
    /// 无符号小于比较。
    Ult,
    /// 无符号小于等于比较。
    Ule,
    /// 有符号大于比较。
    Sgt,
    /// 有符号大于等于比较。
    Sge,
    /// 有符号小于比较。
    Slt,
    /// 有符号小于等于比较。
    Sle,
}

impl CmpPredicate {
    /// 当谓词按有符号整数解释 operand 时返回 true。
    pub fn is_signed(self) -> bool {
        matches!(self, Self::Sgt | Self::Sge | Self::Slt | Self::Sle)
    }
}

/// VM `fcmp` 使用的浮点比较谓词。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum FloatPredicate {
    /// 恒为 false。
    False,
    /// ordered equal。
    Oeq,
    /// ordered greater-than。
    Ogt,
    /// ordered greater-or-equal。
    Oge,
    /// ordered less-than。
    Olt,
    /// ordered less-or-equal。
    Ole,
    /// ordered not-equal。
    One,
    /// ordered，两个 operand 都不是 NaN。
    Ord,
    /// unordered，任一 operand 是 NaN。
    Uno,
    /// unordered equal。
    Ueq,
    /// unordered greater-than。
    Ugt,
    /// unordered greater-or-equal。
    Uge,
    /// unordered less-than。
    Ult,
    /// unordered less-or-equal。
    Ule,
    /// unordered not-equal。
    Une,
    /// 恒为 true。
    True,
}

/// VM bytecode 中编码的原子内存顺序。
///
/// 数值需要稳定，因为 encoder 会把它写进 bytecode，runtime 再按同一编号恢复 LLVM
/// atomic ordering。`NotAtomic` 不在此枚举中；非原子 load/store 使用独立 handler。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum MemoryOrdering {
    /// LLVM unordered ordering。
    Unordered,
    /// LLVM monotonic ordering。
    Monotonic,
    /// LLVM acquire ordering，仅对 atomic load 有效。
    Acquire,
    /// LLVM release ordering，仅对 atomic store 有效。
    Release,
    /// LLVM acquire-release ordering，当前只为 future atomicrmw/cmpxchg 预留。
    AcquireRelease,
    /// LLVM sequentially-consistent ordering。
    SequentiallyConsistent,
}

/// VM bytecode 中编码的 atomic read-modify-write 操作。
///
/// 整数操作直接恢复成同名 LLVM `atomicrmw`；浮点操作只允许 `float` / `double`
/// 标量，x 寄存器里保存参与 RMW 的 IEEE 原始 bit。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum AtomicRmwOp {
    /// LLVM `atomicrmw xchg`。
    Xchg,
    /// LLVM `atomicrmw add`。
    Add,
    /// LLVM `atomicrmw sub`。
    Sub,
    /// LLVM `atomicrmw and`。
    And,
    /// LLVM `atomicrmw or`。
    Or,
    /// LLVM `atomicrmw xor`。
    Xor,
    /// LLVM `atomicrmw nand`。
    Nand,
    /// LLVM `atomicrmw max`。
    Max,
    /// LLVM `atomicrmw min`。
    Min,
    /// LLVM `atomicrmw umax`。
    UMax,
    /// LLVM `atomicrmw umin`。
    UMin,
    /// LLVM `atomicrmw uinc_wrap`。
    UIncWrap,
    /// LLVM `atomicrmw udec_wrap`。
    UDecWrap,
    /// LLVM `atomicrmw usub_cond`。
    USubCond,
    /// LLVM `atomicrmw usub_sat`。
    USubSat,
    /// LLVM `atomicrmw fadd`。
    FAdd,
    /// LLVM `atomicrmw fsub`。
    FSub,
    /// LLVM `atomicrmw fmax`。
    FMax,
    /// LLVM `atomicrmw fmin`。
    FMin,
    /// LLVM `atomicrmw fmaximum`。
    FMaximum,
    /// LLVM `atomicrmw fminimum`。
    FMinimum,
}

impl AtomicRmwOp {
    /// 当前操作是否要求 LLVM 浮点标量 operand。
    pub fn is_floating_point(self) -> bool {
        matches!(
            self,
            Self::FAdd | Self::FSub | Self::FMax | Self::FMin | Self::FMaximum | Self::FMinimum
        )
    }
}

/// 当前 lowering 子集支持的整数 cast 操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CastOp {
    /// 对整数值做零扩展。
    ZExt,
    /// 对整数值做符号扩展。
    SExt,
    /// 截断整数值。
    Trunc,
    /// 在等宽整数/指针 cast 中保留原始 bit。
    Bitcast,
}

/// 从解析后的 `semantic {}` program 选择出的 runtime template。
///
/// profile 拥有指令名和 opcode。这个 enum 不是从名称硬编码推导的；
/// 它是 semantic DSL 被解析并校验后，AMICE 能够 lowering 的有限后端操作集合。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandlerSemantic {
    /// 物化 inline immediate。
    MovImm,
    /// 从 bytecode const pool 加载值。
    ConstLoad,
    /// profile 声明的受限超级指令。
    Super(SuperOp),
    /// 读取 LLVM 计数器 intrinsic，结果写入 x 寄存器。
    ReadCounter(CounterKind),
    /// 复制一个 VM 寄存器。
    Mov,
    /// 整数二元运算。
    Bin(BinOp),
    /// 标量整数一元运算。
    IntUnary(IntUnaryOp),
    /// 标量整数三元运算。
    IntTernary(IntTernaryOp),
    /// 标量整数 with.overflow intrinsic，一条 handler 写入结果和溢出标志两个 x 寄存器。
    IntOverflow(IntOverflowOp),
    /// 标量浮点二元运算，x 寄存器保存 f32/f64 原始 bit。
    FloatBin(FloatBinOp),
    /// 标量浮点一元运算，x 寄存器保存 f32/f64 原始 bit。
    FloatUnary(FloatUnaryOp),
    /// 标量浮点三元运算，x 寄存器保存 f32/f64 原始 bit。
    FloatTernary(FloatTernaryOp),
    /// 标量整数/浮点转换，x 寄存器保存整数值或 f32/f64 原始 bit。
    FloatCast(FloatCastOp),
    /// 标量浮点分类，按 LLVM `is.fpclass` mask 生成 i1。
    FloatClass,
    /// 整数比较。
    Icmp,
    /// 标量浮点比较。
    Fcmp,
    /// 整数或指针 cast。
    Cast(CastOp),
    /// 固定大小栈分配。
    Alloca,
    /// 运行时元素个数栈分配。
    DynamicAlloca,
    /// 标量内存读取。
    Load,
    /// 标量内存写入。
    Store,
    /// 标量 volatile 内存读取。
    VolatileLoad,
    /// 标量 volatile 内存写入。
    VolatileStore,
    /// 运行时长度 memcpy。
    MemcpyDynamic,
    /// 运行时长度 memmove。
    MemmoveDynamic,
    /// 运行时长度 memset。
    MemsetDynamic,
    /// 运行时长度 volatile memcpy。
    VolatileMemcpyDynamic,
    /// 运行时长度 volatile memmove。
    VolatileMemmoveDynamic,
    /// 运行时长度 volatile memset。
    VolatileMemsetDynamic,
    /// 标量 atomic load。
    AtomicLoad,
    /// 标量 atomic store。
    AtomicStore,
    /// 标量 volatile atomic load。
    VolatileAtomicLoad,
    /// 标量 volatile atomic store。
    VolatileAtomicStore,
    /// 标量整数 atomic read-modify-write，结果为内存中的旧值。
    AtomicRmw(AtomicRmwOp),
    /// 标量 volatile atomic read-modify-write，结果为内存中的旧值。
    VolatileAtomicRmw(AtomicRmwOp),
    /// 标量整数/指针 compare-exchange，结果为旧值和成功标志两个 x 寄存器。
    CmpXchg,
    /// 标量 volatile compare-exchange，结果为旧值和成功标志两个 x 寄存器。
    VolatileCmpXchg,
    /// LLVM atomic fence，同步副作用由 ordering operand 决定。
    Fence,
    /// 按字节偏移做指针运算。
    Gep,
    /// 直接 native LLVM call bridge。
    CallNative,
    /// LLVM `sideeffect` intrinsic，必须作为可见副作用保留。
    SideEffect,
    /// 保持状态不变的 fake 或 no-op 指令。
    Nop,
    /// 无条件分支。
    Br,
    /// 条件分支。
    BrCond,
    /// VM 内部调用。
    VmCall,
    /// VM 内部返回。
    VmRet,
    /// LLVM `unreachable` 终结指令，执行到该路径时保持未定义行为。
    Unreachable,
    /// LLVM `trap` intrinsic，执行到该路径时触发运行时陷阱并终止控制流。
    Trap,
    /// 受保护函数返回。
    Ret,
}

/// 从 `isa.vm` 解析出的 handler semantic program。
///
/// DSL 被刻意限制为小集合：赋值、寄存器引用、基础算术、内存 helper、native-call 结果读取和显式
/// PC 副作用。保持类型化可以让 verifier 在不执行宿主代码、也不接受字符串模式语义的前提下推导副作用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticProgram {
    /// 按源码顺序解析出的语句。
    pub statements: Vec<SemanticStmt>,
    /// 从 `statements` 派生的静态副作用摘要。
    pub effect: HandlerEffect,
    /// 此 program 引用到的 `q` 组寄存器名。
    pub q_register_references: Vec<String>,
}

impl SemanticProgram {
    /// 为测试和旧默认 profile 构建只包含契约的 program。
    pub fn from_template(template: &HandlerSemantic) -> Self {
        Self {
            statements: Vec::new(),
            effect: template.expected_effect(),
            q_register_references: Vec::new(),
        }
    }
}

/// 支持的 handler semantic DSL 中的一条语句。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticStmt {
    /// 把 semantic expression 赋给 VM register operand 或别名。
    AssignReg {
        /// 目标 register operand 或别名名称。
        dst: String,
        /// 写入目标的值。
        value: SemanticExpr,
    },
    /// 赋值下一条 VM program counter。
    AssignPc {
        /// 新 PC 表达式。
        value: PcExpr,
    },
    /// 向内存写入标量值。
    StoreWidth {
        /// 指针表达式。
        ptr: SemanticExpr,
        /// 值表达式。
        value: SemanticExpr,
        /// 位宽表达式。
        width: SemanticExpr,
    },
    /// 向内存执行 volatile 标量写入。
    VolatileStoreWidth {
        /// 指针表达式。
        ptr: SemanticExpr,
        /// 值表达式。
        value: SemanticExpr,
        /// 位宽表达式。
        width: SemanticExpr,
    },
    /// 按指定 ordering 向内存原子写入标量值。
    AtomicStoreWidth {
        /// 指针表达式。
        ptr: SemanticExpr,
        /// 值表达式。
        value: SemanticExpr,
        /// 位宽表达式。
        width: SemanticExpr,
        /// memory ordering 表达式。
        ordering: SemanticExpr,
    },
    /// 按指定 ordering 向内存执行 volatile atomic 标量写入。
    VolatileAtomicStoreWidth {
        /// 指针表达式。
        ptr: SemanticExpr,
        /// 值表达式。
        value: SemanticExpr,
        /// 位宽表达式。
        width: SemanticExpr,
        /// memory ordering 表达式。
        ordering: SemanticExpr,
    },
    /// 按运行时长度从源地址逐字节复制到目标地址。
    MemcpyDynamic {
        /// 目标指针表达式。
        dst: SemanticExpr,
        /// 源指针表达式。
        src: SemanticExpr,
        /// 复制长度表达式，单位为字节。
        len: SemanticExpr,
    },
    /// 按运行时长度执行允许重叠的逐字节复制。
    MemmoveDynamic {
        /// 目标指针表达式。
        dst: SemanticExpr,
        /// 源指针表达式。
        src: SemanticExpr,
        /// 复制长度表达式，单位为字节。
        len: SemanticExpr,
    },
    /// 按运行时长度逐字节写入同一个 i8 值。
    MemsetDynamic {
        /// 目标指针表达式。
        dst: SemanticExpr,
        /// 写入的 i8 值表达式。
        value: SemanticExpr,
        /// 写入长度表达式，单位为字节。
        len: SemanticExpr,
    },
    /// 按运行时长度执行 volatile 逐字节复制。
    VolatileMemcpyDynamic {
        /// 目标指针表达式。
        dst: SemanticExpr,
        /// 源指针表达式。
        src: SemanticExpr,
        /// 复制长度表达式，单位为字节。
        len: SemanticExpr,
    },
    /// 按运行时长度执行允许重叠的 volatile 逐字节复制。
    VolatileMemmoveDynamic {
        /// 目标指针表达式。
        dst: SemanticExpr,
        /// 源指针表达式。
        src: SemanticExpr,
        /// 复制长度表达式，单位为字节。
        len: SemanticExpr,
    },
    /// 按运行时长度 volatile 逐字节写入同一个 i8 值。
    VolatileMemsetDynamic {
        /// 目标指针表达式。
        dst: SemanticExpr,
        /// 写入的 i8 值表达式。
        value: SemanticExpr,
        /// 写入长度表达式，单位为字节。
        len: SemanticExpr,
    },
    /// 对内存执行 compare-exchange，并把旧值和成功标志写入两个寄存器。
    CmpXchg {
        /// 保存内存旧值的目标 register operand 名称。
        old: String,
        /// 保存比较是否成功的目标 register operand 名称。
        success: String,
        /// 指针表达式。
        ptr: SemanticExpr,
        /// 期望旧值表达式。
        compare: SemanticExpr,
        /// 成功时写入的新值表达式。
        new: SemanticExpr,
        /// 操作位宽表达式。
        width: SemanticExpr,
        /// 成功 ordering 表达式。
        success_ordering: SemanticExpr,
        /// 失败 ordering 表达式。
        failure_ordering: SemanticExpr,
    },
    /// 对内存执行 volatile compare-exchange，并把旧值和成功标志写入两个寄存器。
    VolatileCmpXchg {
        /// 保存内存旧值的目标 register operand 名称。
        old: String,
        /// 保存比较是否成功的目标 register operand 名称。
        success: String,
        /// 指针表达式。
        ptr: SemanticExpr,
        /// 期望旧值表达式。
        compare: SemanticExpr,
        /// 成功时写入的新值表达式。
        new: SemanticExpr,
        /// 操作位宽表达式。
        width: SemanticExpr,
        /// 成功 ordering 表达式。
        success_ordering: SemanticExpr,
        /// 失败 ordering 表达式。
        failure_ordering: SemanticExpr,
    },
    /// 执行 LLVM atomic fence。
    Fence {
        /// fence ordering 表达式。
        ordering: SemanticExpr,
    },
    /// 直接终止当前 LLVM 控制流路径，对应 LLVM `unreachable`。
    Unreachable,
    /// 触发 LLVM `trap` intrinsic 并终止当前控制流路径。
    Trap,
    /// 执行 LLVM `sideeffect` intrinsic，保留优化屏障副作用。
    SideEffect,
    /// 声明 handler 不改变 VM 数据状态。
    StateUnchanged,
}

/// handler semantic statement 内接受的值表达式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticExpr {
    /// 引用 profile 声明的 instruction operand。
    Operand(String),
    /// 引用 VM register operand 或别名。
    Register(String),
    /// 按 operand 名称从 bytecode const pool 读取。
    ConstPool(String),
    /// 表示下一条 bytecode PC 的伪值。
    NextPc,
    /// 将值截断或 mask 到目标位宽。
    TruncWidth {
        /// 需要截断的值。
        value: Box<SemanticExpr>,
        /// 位宽表达式。
        width: Box<SemanticExpr>,
    },
    /// 对值做零扩展。
    ZeroExtend {
        /// 源值。
        value: Box<SemanticExpr>,
        /// 源位宽。
        from_width: Box<SemanticExpr>,
        /// 目标位宽。
        to_width: Box<SemanticExpr>,
    },
    /// 对值做符号扩展。
    SignExtend {
        /// 源值。
        value: Box<SemanticExpr>,
        /// 源位宽。
        from_width: Box<SemanticExpr>,
        /// 可选目标位宽；缺省表示保持 handler 结果位宽。
        to_width: Option<Box<SemanticExpr>>,
    },
    /// 在整数/指针位宽之间重新解释 bit。
    BitcastWidth {
        /// 源值。
        value: Box<SemanticExpr>,
        /// 源位宽。
        from_width: Box<SemanticExpr>,
        /// 目标位宽。
        to_width: Box<SemanticExpr>,
    },
    /// 整数二元表达式。
    Binary {
        /// 要应用的操作。
        op: SemanticBinOp,
        /// 左操作数。
        lhs: Box<SemanticExpr>,
        /// 右操作数。
        rhs: Box<SemanticExpr>,
    },
    /// 标量整数一元表达式。
    IntUnary {
        /// 要应用的整数一元操作。
        op: SemanticIntUnaryOp,
        /// 源操作数。
        value: Box<SemanticExpr>,
        /// 操作数位宽。
        width: Box<SemanticExpr>,
    },
    /// 标量整数三元表达式。
    IntTernary {
        /// 要应用的整数三元操作。
        op: SemanticIntTernaryOp,
        /// 左操作数。
        lhs: Box<SemanticExpr>,
        /// 右操作数。
        rhs: Box<SemanticExpr>,
        /// 第三个操作数。
        third: Box<SemanticExpr>,
        /// 操作数位宽。
        width: Box<SemanticExpr>,
    },
    /// 整数加减运算的溢出标志表达式。
    IntOverflow {
        /// 要检测的加减溢出类别。
        op: SemanticIntOverflowOp,
        /// 左操作数。
        lhs: Box<SemanticExpr>,
        /// 右操作数。
        rhs: Box<SemanticExpr>,
        /// 操作数位宽。
        width: Box<SemanticExpr>,
    },
    /// 整数比较表达式。
    Compare {
        /// 按 LLVM `icmp` 方式编码的谓词表达式。
        pred: Box<SemanticExpr>,
        /// 左操作数。
        lhs: Box<SemanticExpr>,
        /// 右操作数。
        rhs: Box<SemanticExpr>,
        /// 操作数位宽。
        width: Box<SemanticExpr>,
    },
    /// 标量浮点二元表达式。
    FloatBinary {
        /// 要应用的浮点操作。
        op: SemanticFloatBinOp,
        /// 左操作数。
        lhs: Box<SemanticExpr>,
        /// 右操作数。
        rhs: Box<SemanticExpr>,
        /// 操作数位宽，仅支持 32 或 64。
        width: Box<SemanticExpr>,
    },
    /// 标量浮点一元表达式。
    FloatUnary {
        /// 要应用的浮点操作。
        op: SemanticFloatUnaryOp,
        /// 源操作数。
        value: Box<SemanticExpr>,
        /// 操作数位宽，仅支持 32 或 64。
        width: Box<SemanticExpr>,
    },
    /// 标量浮点三元表达式。
    FloatTernary {
        /// 要应用的浮点操作。
        op: SemanticFloatTernaryOp,
        /// 左操作数。
        lhs: Box<SemanticExpr>,
        /// 右操作数。
        rhs: Box<SemanticExpr>,
        /// 第三个操作数。
        third: Box<SemanticExpr>,
        /// 操作数位宽，仅支持 32 或 64。
        width: Box<SemanticExpr>,
    },
    /// 标量整数/浮点转换表达式。
    FloatCast {
        /// 要应用的转换操作。
        op: SemanticFloatCastOp,
        /// 源操作数。
        value: Box<SemanticExpr>,
        /// 源位宽；浮点源仅支持 32 或 64。
        from_width: Box<SemanticExpr>,
        /// 目标位宽；浮点目标仅支持 32 或 64。
        to_width: Box<SemanticExpr>,
    },
    /// 标量浮点比较表达式。
    FloatCompare {
        /// 按 LLVM `fcmp` 方式编码的谓词表达式。
        pred: Box<SemanticExpr>,
        /// 左操作数。
        lhs: Box<SemanticExpr>,
        /// 右操作数。
        rhs: Box<SemanticExpr>,
        /// 操作数位宽，仅支持 32 或 64。
        width: Box<SemanticExpr>,
    },
    /// 标量浮点分类表达式，按 LLVM `FPClassTest` mask 生成 i1。
    FloatClass {
        /// 源浮点 bit。
        value: Box<SemanticExpr>,
        /// LLVM `FPClassTest` mask。
        mask: Box<SemanticExpr>,
        /// 操作数位宽，仅支持 32 或 64。
        width: Box<SemanticExpr>,
    },
    /// 无分支 select 表达式。
    Select {
        /// 条件表达式；零表示 false。
        cond: Box<SemanticExpr>,
        /// 条件非零时选择的值。
        then_value: Box<SemanticExpr>,
        /// 条件为零时选择的值。
        else_value: Box<SemanticExpr>,
    },
    /// 读取 LLVM 计数器 intrinsic。
    ReadCounter {
        /// 计数器类别。
        kind: CounterKind,
    },
    /// 分配 VM stack slot。
    StackAlloc {
        /// 分配大小，单位为字节。
        bytes: Box<SemanticExpr>,
        /// 所需对齐，单位为字节。
        align: Box<SemanticExpr>,
    },
    /// 按运行时元素个数分配 VM stack slot。
    StackAllocDynamic {
        /// 元素个数表达式。
        count: Box<SemanticExpr>,
        /// 单元素大小，单位为字节。
        elem_size: Box<SemanticExpr>,
        /// 所需对齐，单位为字节。
        align: Box<SemanticExpr>,
    },
    /// 从内存加载标量值。
    LoadWidth {
        /// 指针表达式。
        ptr: Box<SemanticExpr>,
        /// 位宽表达式。
        width: Box<SemanticExpr>,
    },
    /// 从内存执行 volatile 标量加载。
    VolatileLoadWidth {
        /// 指针表达式。
        ptr: Box<SemanticExpr>,
        /// 位宽表达式。
        width: Box<SemanticExpr>,
    },
    /// 从内存原子加载标量值。
    AtomicLoadWidth {
        /// 指针表达式。
        ptr: Box<SemanticExpr>,
        /// 位宽表达式。
        width: Box<SemanticExpr>,
        /// memory ordering 表达式。
        ordering: Box<SemanticExpr>,
    },
    /// 从内存执行 volatile atomic 标量加载。
    VolatileAtomicLoadWidth {
        /// 指针表达式。
        ptr: Box<SemanticExpr>,
        /// 位宽表达式。
        width: Box<SemanticExpr>,
        /// memory ordering 表达式。
        ordering: Box<SemanticExpr>,
    },
    /// 对内存执行 atomic read-modify-write，并返回旧值。
    AtomicRmw {
        /// 要应用的 atomicrmw 操作。
        op: SemanticAtomicRmwOp,
        /// 指针表达式。
        ptr: Box<SemanticExpr>,
        /// 参与 RMW 的新值表达式。
        value: Box<SemanticExpr>,
        /// 位宽表达式。
        width: Box<SemanticExpr>,
        /// memory ordering 表达式。
        ordering: Box<SemanticExpr>,
    },
    /// 对内存执行 volatile atomic read-modify-write，并返回旧值。
    VolatileAtomicRmw {
        /// 要应用的 atomicrmw 操作。
        op: SemanticAtomicRmwOp,
        /// 指针表达式。
        ptr: Box<SemanticExpr>,
        /// 参与 RMW 的新值表达式。
        value: Box<SemanticExpr>,
        /// 位宽表达式。
        width: Box<SemanticExpr>,
        /// memory ordering 表达式。
        ordering: Box<SemanticExpr>,
    },
    /// 从生成的 native-call bridge table 读取一个返回槽。
    CallTableReturn {
        /// callee operand 名称。
        callee: String,
        /// 返回槽序号。
        index: u8,
        /// 传给 bridge 的参数与元数据。
        args: Vec<SemanticExpr>,
    },
}

/// handler semantic DSL 中的二元运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticBinOp {
    /// wrapping 整数加法。
    Add,
    /// wrapping 整数减法。
    Sub,
    /// wrapping 整数乘法。
    Mul,
    /// 无符号整数除法。
    UDiv,
    /// 有符号整数除法。
    SDiv,
    /// 无符号整数取余。
    URem,
    /// 有符号整数取余。
    SRem,
    /// 按位异或。
    Xor,
    /// 按位与。
    And,
    /// 按位或。
    Or,
    /// 逻辑左移。
    Shl,
    /// 逻辑右移。
    LShr,
    /// 算术右移。
    AShr,
    /// 有符号最大值。
    SMax,
    /// 有符号最小值。
    SMin,
    /// 无符号最大值。
    UMax,
    /// 无符号最小值。
    UMin,
    /// 无符号饱和加法。
    UAddSat,
    /// 无符号饱和减法。
    USubSat,
    /// 有符号饱和加法。
    SAddSat,
    /// 有符号饱和减法。
    SSubSat,
    /// 无符号饱和左移。
    UShlSat,
    /// 有符号饱和左移。
    SShlSat,
}

/// handler semantic DSL 中的整数一元运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticIntUnaryOp {
    /// 统计置位 bit 数。
    CtPop,
    /// 统计前导零 bit 数。
    CtLz,
    /// 统计尾随零 bit 数。
    CtTz,
    /// 计算有符号绝对值。
    Abs,
    /// 按整数位宽做字节序反转。
    BSwap,
    /// 按整数位宽做 bit 顺序反转。
    BitReverse,
}

/// handler semantic DSL 中的整数三元运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticIntTernaryOp {
    /// LLVM `fshl` funnel shift left。
    FShl,
    /// LLVM `fshr` funnel shift right。
    FShr,
}

/// handler semantic DSL 中的整数溢出检测操作符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticIntOverflowOp {
    /// 无符号加法溢出。
    UAdd,
    /// 有符号加法溢出。
    SAdd,
    /// 无符号减法溢出。
    USub,
    /// 有符号减法溢出。
    SSub,
    /// 无符号乘法溢出。
    UMul,
    /// 有符号乘法溢出。
    SMul,
}

/// handler semantic DSL 中的浮点二元运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticFloatBinOp {
    /// 浮点加法。
    Add,
    /// 浮点减法。
    Sub,
    /// 浮点乘法。
    Mul,
    /// 浮点除法。
    Div,
    /// 浮点余数。
    Rem,
    /// 浮点 minnum。
    MinNum,
    /// 浮点 maxnum。
    MaxNum,
    /// 浮点 minimum。
    Minimum,
    /// 浮点 maximum。
    Maximum,
    /// 浮点符号位复制。
    CopySign,
}

/// handler semantic DSL 中的浮点一元运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticFloatUnaryOp {
    /// 浮点取反。
    Neg,
    /// 浮点绝对值。
    Abs,
    /// 浮点平方根。
    Sqrt,
    /// 浮点 canonicalize。
    Canonicalize,
    /// 浮点 floor。
    Floor,
    /// 浮点 ceil。
    Ceil,
    /// 浮点 trunc。
    Trunc,
    /// 浮点 rint。
    Rint,
    /// 浮点 nearbyint。
    NearbyInt,
    /// 浮点 round。
    Round,
    /// 浮点 roundeven。
    RoundEven,
}

/// handler semantic DSL 中的浮点三元运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticFloatTernaryOp {
    /// fused multiply-add。
    Fma,
    /// 可融合乘加。
    MulAdd,
}

/// handler semantic DSL 中的标量整数/浮点转换操作符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticFloatCastOp {
    /// 有符号整数转浮点。
    SignedIntToFloat,
    /// 无符号整数转浮点。
    UnsignedIntToFloat,
    /// 浮点转有符号整数。
    FloatToSignedInt,
    /// 浮点转无符号整数。
    FloatToUnsignedInt,
    /// 浮点截断。
    FloatTrunc,
    /// 浮点扩展。
    FloatExt,
}

/// handler semantic DSL 中的 atomic read-modify-write 操作符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticAtomicRmwOp {
    /// 原子交换。
    Xchg,
    /// 原子加法。
    Add,
    /// 原子减法。
    Sub,
    /// 原子按位与。
    And,
    /// 原子按位或。
    Or,
    /// 原子按位异或。
    Xor,
    /// 原子按位与后取反。
    Nand,
    /// 原子有符号最大值。
    Max,
    /// 原子有符号最小值。
    Min,
    /// 原子无符号最大值。
    UMax,
    /// 原子无符号最小值。
    UMin,
    /// 原子无符号加一并按输入上界回绕。
    UIncWrap,
    /// 原子无符号减一并按输入上界回绕。
    UDecWrap,
    /// 原子无符号条件减法。
    USubCond,
    /// 原子无符号饱和减法。
    USubSat,
    /// 原子浮点加法。
    FAdd,
    /// 原子浮点减法。
    FSub,
    /// 原子浮点 maxnum 风格最大值。
    FMax,
    /// 原子浮点 minnum 风格最小值。
    FMin,
    /// 原子浮点 maximum 风格最大值。
    FMaximum,
    /// 原子浮点 minimum 风格最小值。
    FMinimum,
}

/// `pc = ...` 赋值中接受的 program-counter 表达式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PcExpr {
    /// 继续执行下一条 bytecode 指令。
    Next,
    /// 退出 VM dispatch loop。
    Return,
    /// 跳转到 label operand。
    Label(String),
    /// 跳转到寄存器别名保存的 PC，通常是 `lr`。
    Register(String),
    /// 在两个 label operand 之间选择。
    Select {
        /// 条件表达式；零表示 false。
        cond: Box<SemanticExpr>,
        /// 条件非零时选择的 label。
        then_pc: String,
        /// 条件为零时选择的 label。
        else_pc: String,
    },
}

/// handler 改变 VM program counter 的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PcEffect {
    /// handler fallthrough 到下一条 bytecode 指令。
    Next,
    /// handler 可能跳转到非顺序 bytecode PC。
    Branch,
    /// handler 退出 dispatcher。
    Return,
}

/// 从 ISA `semantic` block 派生的静态副作用摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandlerEffect {
    /// handler 需要产生的 program-counter 副作用。
    pub pc: PcEffect,
    /// handler 读取的 register operand 或别名。
    pub register_reads: Vec<String>,
    /// handler 写入的 register operand 或别名。
    pub register_writes: Vec<String>,
    /// handler 是否读取宿主/VM 内存。
    pub memory_read: bool,
    /// handler 是否写入宿主/VM 内存。
    pub memory_write: bool,
    /// handler 是否调用 native LLVM 代码。
    pub native_call: bool,
}

/// profile DSL 接受的 operand kind。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperandKind {
    /// VM 寄存器操作数。
    VReg,
    /// inline immediate 操作数。
    Imm,
    /// constant-pool index 操作数。
    ConstPoolIndex,
    /// bytecode label 操作数。
    Label,
    /// 仅供旧测试构造器使用的 parser fallback。
    Unknown,
}

/// 从 `isa.vm` 指令 header 解析出的 operand 声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperandDesc {
    /// `isa.vm` 中声明的 operand 名称。
    pub name: String,
    /// operand 编码/语义 kind。
    pub kind: OperandKind,
    /// profile 值类型，例如 `i64`、`ptr` 或 `label`。
    pub value_type: String,
}

impl HandlerSemantic {
    /// 为内置语义类别构建 verifier 已知的副作用契约。
    pub fn expected_effect(&self) -> HandlerEffect {
        match self {
            Self::MovImm => HandlerEffect::new(PcEffect::Next).writes(["dst"]),
            Self::ConstLoad => HandlerEffect::new(PcEffect::Next).writes(["dst"]),
            Self::Super(SuperOp::AddXor) => HandlerEffect::new(PcEffect::Next)
                .reads(["lhs", "rhs", "xor_rhs"])
                .writes(["dst"]),
            Self::Super(SuperOp::IcmpBrIf) => HandlerEffect::new(PcEffect::Branch).reads(["lhs", "rhs"]),
            Self::Super(SuperOp::GepLoad) => HandlerEffect::new(PcEffect::Next)
                .reads(["base"])
                .writes(["dst"])
                .with_memory_read(),
            Self::Super(SuperOp::LoadAdd) => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr", "addend"])
                .writes(["dst"])
                .with_memory_read(),
            Self::ReadCounter(_) => HandlerEffect::new(PcEffect::Next).writes(["dst"]).with_native_call(),
            Self::Mov => HandlerEffect::new(PcEffect::Next).reads(["src"]).writes(["dst"]),
            Self::Bin(_) => HandlerEffect::new(PcEffect::Next).reads(["lhs", "rhs"]).writes(["dst"]),
            Self::IntUnary(_) => HandlerEffect::new(PcEffect::Next).reads(["src"]).writes(["dst"]),
            Self::IntTernary(_) => HandlerEffect::new(PcEffect::Next)
                .reads(["lhs", "rhs", "third"])
                .writes(["dst"]),
            Self::IntOverflow(_) => HandlerEffect::new(PcEffect::Next)
                .reads(["lhs", "rhs"])
                .writes(["dst", "overflow"]),
            Self::FloatBin(_) => HandlerEffect::new(PcEffect::Next).reads(["lhs", "rhs"]).writes(["dst"]),
            Self::FloatUnary(_) => HandlerEffect::new(PcEffect::Next).reads(["src"]).writes(["dst"]),
            Self::FloatTernary(_) => HandlerEffect::new(PcEffect::Next)
                .reads(["lhs", "rhs", "third"])
                .writes(["dst"]),
            Self::FloatCast(_) => HandlerEffect::new(PcEffect::Next).reads(["src"]).writes(["dst"]),
            Self::FloatClass => HandlerEffect::new(PcEffect::Next).reads(["src"]).writes(["dst"]),
            Self::Icmp => HandlerEffect::new(PcEffect::Next).reads(["lhs", "rhs"]).writes(["dst"]),
            Self::Fcmp => HandlerEffect::new(PcEffect::Next).reads(["lhs", "rhs"]).writes(["dst"]),
            Self::Cast(_) => HandlerEffect::new(PcEffect::Next).reads(["src"]).writes(["dst"]),
            Self::Alloca => HandlerEffect::new(PcEffect::Next).writes(["dst"]),
            Self::DynamicAlloca => HandlerEffect::new(PcEffect::Next).reads(["count"]).writes(["dst"]),
            Self::Load => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr"])
                .writes(["dst"])
                .with_memory_read(),
            Self::Store => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr", "src"])
                .with_memory_write(),
            Self::VolatileLoad => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr"])
                .writes(["dst"])
                .with_memory_read(),
            Self::VolatileStore => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr", "src"])
                .with_memory_write(),
            Self::MemcpyDynamic | Self::MemmoveDynamic => HandlerEffect::new(PcEffect::Next)
                .reads(["dst", "src", "len"])
                .with_memory_read()
                .with_memory_write(),
            Self::MemsetDynamic => HandlerEffect::new(PcEffect::Next)
                .reads(["dst", "value", "len"])
                .with_memory_write(),
            Self::VolatileMemcpyDynamic | Self::VolatileMemmoveDynamic => HandlerEffect::new(PcEffect::Next)
                .reads(["dst", "src", "len"])
                .with_memory_read()
                .with_memory_write(),
            Self::VolatileMemsetDynamic => HandlerEffect::new(PcEffect::Next)
                .reads(["dst", "value", "len"])
                .with_memory_write(),
            Self::AtomicLoad => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr"])
                .writes(["dst"])
                .with_memory_read(),
            Self::AtomicStore => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr", "src"])
                .with_memory_write(),
            Self::VolatileAtomicLoad => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr"])
                .writes(["dst"])
                .with_memory_read(),
            Self::VolatileAtomicStore => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr", "src"])
                .with_memory_write(),
            Self::AtomicRmw(_) => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr", "src"])
                .writes(["dst"])
                .with_memory_read()
                .with_memory_write(),
            Self::VolatileAtomicRmw(_) => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr", "src"])
                .writes(["dst"])
                .with_memory_read()
                .with_memory_write(),
            Self::CmpXchg => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr", "cmp", "new"])
                .writes(["old", "success"])
                .with_memory_read()
                .with_memory_write(),
            Self::VolatileCmpXchg => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr", "cmp", "new"])
                .writes(["old", "success"])
                .with_memory_read()
                .with_memory_write(),
            Self::Fence => HandlerEffect::new(PcEffect::Next)
                .with_memory_read()
                .with_memory_write(),
            Self::Gep => HandlerEffect::new(PcEffect::Next).reads(["base"]).writes(["dst"]),
            Self::CallNative => HandlerEffect::new(PcEffect::Next)
                .reads(["arg0", "arg1", "arg2", "arg3", "arg4", "arg5", "arg6", "arg7"])
                .writes((0..NATIVE_CALL_MAX_RETURNS).map(|index| format!("ret{index}")))
                .with_native_call(),
            Self::SideEffect => HandlerEffect::new(PcEffect::Next).with_native_call(),
            Self::Nop => HandlerEffect::new(PcEffect::Next),
            Self::Br => HandlerEffect::new(PcEffect::Branch),
            Self::BrCond => HandlerEffect::new(PcEffect::Branch).reads(["cond"]),
            Self::VmCall => HandlerEffect::new(PcEffect::Branch).writes(["lr"]),
            Self::VmRet => HandlerEffect::new(PcEffect::Branch).reads(["lr"]),
            Self::Unreachable => HandlerEffect::new(PcEffect::Return),
            Self::Trap => HandlerEffect::new(PcEffect::Return).with_native_call(),
            Self::Ret => HandlerEffect::new(PcEffect::Return).reads(["src"]).writes(["ret0"]),
        }
    }
}

impl HandlerEffect {
    /// 创建不包含寄存器和内存副作用的摘要。
    pub fn new(pc: PcEffect) -> Self {
        Self {
            pc,
            register_reads: Vec::new(),
            register_writes: Vec::new(),
            memory_read: false,
            memory_write: false,
            native_call: false,
        }
    }

    /// 向副作用摘要添加寄存器读取。
    pub fn reads<I, S>(mut self, registers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.register_reads.extend(registers.into_iter().map(Into::into));
        self
    }

    /// 向副作用摘要添加寄存器写入。
    pub fn writes<I, S>(mut self, registers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.register_writes.extend(registers.into_iter().map(Into::into));
        self
    }

    /// 标记 handler 会读取内存。
    pub fn with_memory_read(mut self) -> Self {
        self.memory_read = true;
        self
    }

    /// 标记 handler 会写入内存。
    pub fn with_memory_write(mut self) -> Self {
        self.memory_write = true;
        self
    }

    /// 标记 handler 会调用生成的 native-call bridge 代码。
    pub fn with_native_call(mut self) -> Self {
        self.native_call = true;
        self
    }
}

/// profile ISA 声明的 VM 指令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionDesc {
    /// 保留到 VM IR 和 bytecode 的 profile 指令名。
    pub name: String,
    /// 第一个 opcode alias；为兼容旧调用点而保留。
    pub opcode: Opcode,
    /// runtime dispatcher 接受的完整 opcode alias 集合。
    pub opcode_aliases: Vec<Opcode>,
    /// 编码 operand 数量。
    pub operands: u8,
    /// decoder pipeline 还原后，此指令 record 在 code stream 中占用的字节数。
    pub decoded_width: u8,
    /// 来自指令 header 的有序 operand 声明。
    pub operand_descs: Vec<OperandDesc>,
    /// 从 semantic AST 选择的后端 handler template。
    pub semantic: HandlerSemantic,
    /// 从 `isa.vm` 解析出的 semantic program。
    pub semantic_program: SemanticProgram,
    /// 从 `semantic_program` 派生的静态副作用。
    pub effect: HandlerEffect,
}

impl InstructionDesc {
    /// 用一个或多个 profile 声明的 opcode alias 创建指令描述。
    pub fn new(name: impl Into<String>, opcode_aliases: Vec<Opcode>, operands: u8, semantic: HandlerSemantic) -> Self {
        let semantic_program = SemanticProgram::from_template(&semantic);
        let effect = semantic_program.effect.clone();
        let operand_descs = (0..operands)
            .map(|index| OperandDesc {
                name: format!("op{index}"),
                kind: OperandKind::Unknown,
                value_type: "unknown".to_owned(),
            })
            .collect();
        Self::new_with_semantic_program(
            name,
            opcode_aliases,
            operands,
            16,
            operand_descs,
            semantic,
            semantic_program,
            effect,
        )
    }

    /// 用 parser 派生的副作用创建指令描述。
    pub fn new_with_semantic_program(
        name: impl Into<String>,
        opcode_aliases: Vec<Opcode>,
        operands: u8,
        decoded_width: u8,
        operand_descs: Vec<OperandDesc>,
        semantic: HandlerSemantic,
        semantic_program: SemanticProgram,
        effect: HandlerEffect,
    ) -> Self {
        let opcode = opcode_aliases
            .first()
            .copied()
            .expect("instruction descriptors need at least one opcode alias");

        Self {
            name: name.into(),
            opcode,
            opcode_aliases,
            operands,
            decoded_width,
            operand_descs,
            semantic,
            semantic_program,
            effect,
        }
    }

    /// 返回此指令接受的所有 opcode。
    pub fn opcodes(&self) -> &[Opcode] {
        &self.opcode_aliases
    }

    /// 为一个具体编码指令位置选择 opcode alias。
    pub fn opcode_for_site(&self, function_key: u64, pc: usize) -> Opcode {
        let aliases = self.opcodes();
        let mixed = function_key ^ (pc as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let folded = mixed ^ (mixed >> 33) ^ (mixed >> 17);
        aliases[(folded as usize) % aliases.len()]
    }
}

/// lowering、encoding 和 runtime emission 使用的已解析 ISA 表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsaProfile {
    /// 按声明顺序排列的 profile 指令。
    pub instructions: Vec<InstructionDesc>,
}

impl IsaProfile {
    /// 按 profile 声明名查找指令描述。
    pub fn by_name(&self, name: &str) -> Option<&InstructionDesc> {
        self.instructions.iter().find(|desc| desc.name == name)
    }

    /// 按请求的语义类别查找指令描述。
    pub fn by_semantic(&self, semantic: &HandlerSemantic) -> Option<&InstructionDesc> {
        self.instructions.iter().find(|desc| desc.semantic == *semantic)
    }

    /// 查找 opcode 对应的指令描述。
    pub fn by_opcode(&self, opcode: Opcode) -> Option<&InstructionDesc> {
        self.instructions.iter().find(|desc| desc.opcodes().contains(&opcode))
    }

    /// 当此 ISA 内所有 opcode 都唯一时返回 true。
    pub fn has_unique_opcodes(&self) -> bool {
        let alias_count = self.instructions.iter().map(|desc| desc.opcodes().len()).sum();
        let mut seen = std::collections::HashSet::with_capacity(alias_count);
        self.instructions
            .iter()
            .flat_map(InstructionDesc::opcodes)
            .all(|opcode| seen.insert(*opcode))
    }
}

impl Default for IsaProfile {
    fn default() -> Self {
        use BinOp::*;
        use CastOp::*;
        use HandlerSemantic::*;
        use IntOverflowOp::*;
        use IntTernaryOp::*;
        use IntUnaryOp::*;

        let instructions = vec![
            InstructionDesc::new("mov_imm", vec![0x01], 3, MovImm),
            InstructionDesc::new("const_load", vec![0x03], 3, ConstLoad),
            InstructionDesc::new("read_cycle", vec![0x12e], 2, ReadCounter(CounterKind::Cycle)),
            InstructionDesc::new("read_steady", vec![0x12f], 2, ReadCounter(CounterKind::Steady)),
            InstructionDesc::new("mov", vec![0x02], 3, Mov),
            InstructionDesc::new("iadd", vec![0x10], 4, Bin(Add)),
            InstructionDesc::new("isub", vec![0x11], 4, Bin(Sub)),
            InstructionDesc::new("imul", vec![0x12], 4, Bin(Mul)),
            InstructionDesc::new("iudiv", vec![0x19], 4, Bin(UDiv)),
            InstructionDesc::new("isdiv", vec![0x1a], 4, Bin(SDiv)),
            InstructionDesc::new("iurem", vec![0x1b], 4, Bin(URem)),
            InstructionDesc::new("isrem", vec![0x1c], 4, Bin(SRem)),
            InstructionDesc::new("ixor", vec![0x13], 4, Bin(Xor)),
            InstructionDesc::new("iand", vec![0x14], 4, Bin(And)),
            InstructionDesc::new("ior", vec![0x15], 4, Bin(Or)),
            InstructionDesc::new("ishl", vec![0x16], 4, Bin(Shl)),
            InstructionDesc::new("ilshr", vec![0x17], 4, Bin(LShr)),
            InstructionDesc::new("iashr", vec![0x18], 4, Bin(AShr)),
            InstructionDesc::new("ismax", vec![0x54], 4, Bin(SMax)),
            InstructionDesc::new("ismin", vec![0x55], 4, Bin(SMin)),
            InstructionDesc::new("iumax", vec![0x56], 4, Bin(UMax)),
            InstructionDesc::new("iumin", vec![0x57], 4, Bin(UMin)),
            InstructionDesc::new("iuadd_sat", vec![0x58], 4, Bin(UAddSat)),
            InstructionDesc::new("iusub_sat", vec![0x59], 4, Bin(USubSat)),
            InstructionDesc::new("isadd_sat", vec![0x5a], 4, Bin(SAddSat)),
            InstructionDesc::new("issub_sat", vec![0x5b], 4, Bin(SSubSat)),
            InstructionDesc::new("iushl_sat", vec![0x5c], 4, Bin(UShlSat)),
            InstructionDesc::new("isshl_sat", vec![0x5d], 4, Bin(SShlSat)),
            InstructionDesc::new("iuadd_overflow", vec![0x5e], 5, IntOverflow(UAdd)),
            InstructionDesc::new("isadd_overflow", vec![0x5f], 5, IntOverflow(SAdd)),
            InstructionDesc::new("iusub_overflow", vec![0x60], 5, IntOverflow(USub)),
            InstructionDesc::new("issub_overflow", vec![0x61], 5, IntOverflow(SSub)),
            InstructionDesc::new("iumul_overflow", vec![0x62], 5, IntOverflow(UMul)),
            InstructionDesc::new("ismul_overflow", vec![0x63], 5, IntOverflow(SMul)),
            InstructionDesc::new("ctpop", vec![0x1d], 3, IntUnary(CtPop)),
            InstructionDesc::new("ctlz", vec![0x101], 3, IntUnary(CtLz)),
            InstructionDesc::new("cttz", vec![0x102], 3, IntUnary(CtTz)),
            InstructionDesc::new("iabs", vec![0x53], 3, IntUnary(Abs)),
            InstructionDesc::new("bswap", vec![0x1e], 3, IntUnary(BSwap)),
            InstructionDesc::new("bitreverse", vec![0x1f], 3, IntUnary(BitReverse)),
            InstructionDesc::new("fshl", vec![0x22], 5, IntTernary(FShl)),
            InstructionDesc::new("fshr", vec![0x23], 5, IntTernary(FShr)),
            InstructionDesc::new("icmp", vec![0x20], 5, Icmp),
            InstructionDesc::new("zext", vec![0x30], 4, Cast(ZExt)),
            InstructionDesc::new("sext", vec![0x31], 4, Cast(SExt)),
            InstructionDesc::new("trunc", vec![0x32], 4, Cast(Trunc)),
            InstructionDesc::new("bitcast", vec![0x33], 4, Cast(Bitcast)),
            InstructionDesc::new("alloca", vec![0x34], 3, Alloca),
            InstructionDesc::new("alloca_dyn", vec![0x6a], 4, DynamicAlloca),
            InstructionDesc::new("load", vec![0x35], 3, Load),
            InstructionDesc::new("store", vec![0x36], 3, Store),
            InstructionDesc::new("volatile_load", vec![0x113], 3, VolatileLoad),
            InstructionDesc::new("volatile_store", vec![0x114], 3, VolatileStore),
            InstructionDesc::new("memcpy_dyn", vec![0x6c], 3, MemcpyDynamic),
            InstructionDesc::new("memmove_dyn", vec![0x6d], 3, MemmoveDynamic),
            InstructionDesc::new("memset_dyn", vec![0x6b], 3, MemsetDynamic),
            InstructionDesc::new("volatile_memcpy_dyn", vec![0x129], 3, VolatileMemcpyDynamic),
            InstructionDesc::new("volatile_memmove_dyn", vec![0x12a], 3, VolatileMemmoveDynamic),
            InstructionDesc::new("volatile_memset_dyn", vec![0x12b], 3, VolatileMemsetDynamic),
            InstructionDesc::new("atomic_load", vec![0x44], 4, AtomicLoad),
            InstructionDesc::new("atomic_store", vec![0x45], 4, AtomicStore),
            InstructionDesc::new("volatile_atomic_load", vec![0x127], 4, VolatileAtomicLoad),
            InstructionDesc::new("volatile_atomic_store", vec![0x128], 4, VolatileAtomicStore),
            InstructionDesc::new("atomic_rmw_xchg", vec![0x46], 5, AtomicRmw(AtomicRmwOp::Xchg)),
            InstructionDesc::new("atomic_rmw_add", vec![0x47], 5, AtomicRmw(AtomicRmwOp::Add)),
            InstructionDesc::new("atomic_rmw_sub", vec![0x48], 5, AtomicRmw(AtomicRmwOp::Sub)),
            InstructionDesc::new("atomic_rmw_and", vec![0x49], 5, AtomicRmw(AtomicRmwOp::And)),
            InstructionDesc::new("atomic_rmw_or", vec![0x4a], 5, AtomicRmw(AtomicRmwOp::Or)),
            InstructionDesc::new("atomic_rmw_xor", vec![0x4b], 5, AtomicRmw(AtomicRmwOp::Xor)),
            InstructionDesc::new("atomic_rmw_nand", vec![0x4c], 5, AtomicRmw(AtomicRmwOp::Nand)),
            InstructionDesc::new("atomic_rmw_max", vec![0x4d], 5, AtomicRmw(AtomicRmwOp::Max)),
            InstructionDesc::new("atomic_rmw_min", vec![0x4e], 5, AtomicRmw(AtomicRmwOp::Min)),
            InstructionDesc::new("atomic_rmw_umax", vec![0x4f], 5, AtomicRmw(AtomicRmwOp::UMax)),
            InstructionDesc::new("atomic_rmw_umin", vec![0x50], 5, AtomicRmw(AtomicRmwOp::UMin)),
            InstructionDesc::new("atomic_rmw_uinc_wrap", vec![0x11d], 5, AtomicRmw(AtomicRmwOp::UIncWrap)),
            InstructionDesc::new("atomic_rmw_udec_wrap", vec![0x11e], 5, AtomicRmw(AtomicRmwOp::UDecWrap)),
            InstructionDesc::new("atomic_rmw_usub_cond", vec![0x11f], 5, AtomicRmw(AtomicRmwOp::USubCond)),
            InstructionDesc::new("atomic_rmw_usub_sat", vec![0x120], 5, AtomicRmw(AtomicRmwOp::USubSat)),
            InstructionDesc::new("atomic_rmw_fadd", vec![0x117], 5, AtomicRmw(AtomicRmwOp::FAdd)),
            InstructionDesc::new("atomic_rmw_fsub", vec![0x118], 5, AtomicRmw(AtomicRmwOp::FSub)),
            InstructionDesc::new("atomic_rmw_fmax", vec![0x119], 5, AtomicRmw(AtomicRmwOp::FMax)),
            InstructionDesc::new("atomic_rmw_fmin", vec![0x11a], 5, AtomicRmw(AtomicRmwOp::FMin)),
            InstructionDesc::new("atomic_rmw_fmaximum", vec![0x11b], 5, AtomicRmw(AtomicRmwOp::FMaximum)),
            InstructionDesc::new("atomic_rmw_fminimum", vec![0x11c], 5, AtomicRmw(AtomicRmwOp::FMinimum)),
            InstructionDesc::new(
                "volatile_atomic_rmw_add",
                vec![0x12d],
                5,
                VolatileAtomicRmw(AtomicRmwOp::Add),
            ),
            InstructionDesc::new("cmpxchg", vec![0x51], 8, CmpXchg),
            InstructionDesc::new("volatile_cmpxchg", vec![0x12c], 8, VolatileCmpXchg),
            InstructionDesc::new("fence", vec![0x52], 1, Fence),
            InstructionDesc::new("gep", vec![0x37], 3, Gep),
            InstructionDesc::new("call_native", vec![0x38], 27, CallNative),
            InstructionDesc::new("br", vec![0x40], 1, Br),
            InstructionDesc::new("br_if", vec![0x41], 3, BrCond),
            InstructionDesc::new("vm_call", vec![0x42], 1, VmCall),
            InstructionDesc::new("vm_ret", vec![0x43], 0, VmRet),
            InstructionDesc::new("fminnum", vec![0x68], 4, FloatBin(FloatBinOp::MinNum)),
            InstructionDesc::new("fmaxnum", vec![0x69], 4, FloatBin(FloatBinOp::MaxNum)),
            InstructionDesc::new("fminimum", vec![0x6e], 4, FloatBin(FloatBinOp::Minimum)),
            InstructionDesc::new("fmaximum", vec![0x6f], 4, FloatBin(FloatBinOp::Maximum)),
            InstructionDesc::new("fcopysign", vec![0x66], 4, FloatBin(FloatBinOp::CopySign)),
            InstructionDesc::new("fabs", vec![0x64], 3, FloatUnary(FloatUnaryOp::Abs)),
            InstructionDesc::new("fsqrt", vec![0x65], 3, FloatUnary(FloatUnaryOp::Sqrt)),
            InstructionDesc::new("fcanonicalize", vec![0x71], 3, FloatUnary(FloatUnaryOp::Canonicalize)),
            InstructionDesc::new("ffloor", vec![0x72], 3, FloatUnary(FloatUnaryOp::Floor)),
            InstructionDesc::new("fceil", vec![0x73], 3, FloatUnary(FloatUnaryOp::Ceil)),
            InstructionDesc::new("ftrunc", vec![0x74], 3, FloatUnary(FloatUnaryOp::Trunc)),
            InstructionDesc::new("frint", vec![0x75], 3, FloatUnary(FloatUnaryOp::Rint)),
            InstructionDesc::new("fnearbyint", vec![0x76], 3, FloatUnary(FloatUnaryOp::NearbyInt)),
            InstructionDesc::new("fround", vec![0x77], 3, FloatUnary(FloatUnaryOp::Round)),
            InstructionDesc::new("froundeven", vec![0x78], 3, FloatUnary(FloatUnaryOp::RoundEven)),
            InstructionDesc::new("ffma", vec![0x67], 5, FloatTernary(FloatTernaryOp::Fma)),
            InstructionDesc::new("ffmuladd", vec![0x70], 5, FloatTernary(FloatTernaryOp::MulAdd)),
            InstructionDesc::new("unreachable", vec![0x79], 0, Unreachable),
            InstructionDesc::new("trap", vec![0x7a], 0, Trap),
            InstructionDesc::new("ret", vec![0x7f], 1, Ret),
        ];

        Self { instructions }
    }
}
