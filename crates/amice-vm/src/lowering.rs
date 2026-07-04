//! LLVM-to-VMP translator 在 bytecode 编码前生成的 VM IR。
//!
//! # 契约
//! `VmInstruction` 记录 runtime handler 将执行的语义 operand。
//! `VmFunction::profile_instructions` 记录 `lowering.vm` 选中的精确 ISA 指令名；
//! encoder 必须用这个 identity 选择 opcode alias 和 operand 顺序。
//!
//! # 坑点
//! `push` 只服务测试和内置默认值。profile 驱动 lowering 必须使用 `push_profile`，
//! 这样同语义的两条指令仍能编码成不同 opcode 和 layout。

use crate::isa::{
    AtomicRmwOp, BinOp, CastOp, CmpPredicate, CounterKind, FloatBinOp, FloatCastOp, FloatIntBinOp, FloatPredicate,
    FloatRoundToIntOp, FloatTernaryOp, FloatUnaryOp, FpStateKind, HandlerSemantic, IntOverflowOp, IntTernaryOp,
    IntUnaryOp, IsaProfile, MemoryOrdering, SuperOp,
};
use crate::profile::LoweringProfile;
use std::collections::HashMap;
use std::collections::HashSet;

/// `native_call` thunk 使用固定参数向量，使每个调用点无论 callee LLVM 类型如何，
/// 都能使用一条可被 profile 序列化的 bytecode record。
pub const NATIVE_CALL_MAX_ARGS: usize = 8;

/// runtime 返回固定宽度 tuple，并且只存储前 `ret_count` 个元素。
/// 固定上限能让间接 thunk 调用拥有稳定 LLVM 函数类型，同时仍允许 profile 选择更少返回槽。
pub const NATIVE_CALL_MAX_RETURNS: usize = 8;

/// VM 函数内部的 label 标识符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabelId(pub u32);

/// profile 声明的 `native_call` 返回目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeReturn {
    /// 此 native-call 返回槽写入的目标 `x` 寄存器。
    pub dst: u8,
    /// 截断 native 返回值时使用的整数位宽。
    pub width: u8,
}

/// bytecode 编码前的 VM 指令流。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmInstruction {
    /// 将 inline immediate 物化到 `x` 寄存器。
    MovImm {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// profile 位宽截断前的 inline immediate 值。
        imm: u64,
        /// 结果位宽。
        width: u8,
    },
    /// 从 bytecode const pool 加载值到 `x` 寄存器。
    ConstLoad {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 逻辑常量值；encoder 会分配 const-pool index。
        value: u64,
        /// 结果位宽。
        width: u8,
    },
    /// 读取 LLVM 计数器 intrinsic 并写入 `x` 寄存器。
    ReadCounter {
        /// 计数器类别。
        kind: CounterKind,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 结果位宽，LLVM intrinsic 当前固定为 64。
        width: u8,
    },
    /// 读取 LLVM `vscale` 目标运行时常量并写入 `x` 寄存器。
    ReadVScale {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 结果位宽，当前 translator 接受 i1/i8/i16/i32/i64。
        width: u8,
    },
    /// 读取 LLVM 当前浮点 rounding mode 并写入 `x` 寄存器。
    ReadRounding {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 结果位宽，LLVM intrinsic 固定返回 i32。
        width: u8,
    },
    /// 读取 C/LLVM `FLT_ROUNDS` 当前舍入方向并写入 `x` 寄存器。
    ReadFltRounds {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 结果位宽，LLVM intrinsic 固定返回 i32。
        width: u8,
    },
    /// 设置 LLVM 当前浮点 rounding mode。
    WriteRounding {
        /// 源 `x` 寄存器。
        src: u8,
        /// 源值位宽，当前固定为 32。
        width: u8,
    },
    /// 读取 LLVM 浮点环境状态并写入 `x` 寄存器。
    ReadFpState {
        /// 状态类别。
        kind: FpStateKind,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 结果位宽。
        width: u8,
    },
    /// 写回 LLVM 浮点环境状态。
    WriteFpState {
        /// 状态类别。
        kind: FpStateKind,
        /// 源 `x` 寄存器。
        src: u8,
        /// 源值位宽。
        width: u8,
    },
    /// 重置 LLVM 浮点环境状态。
    ResetFpState {
        /// 状态类别。
        kind: FpStateKind,
    },
    /// 读取 LLVM thread pointer 并写入 `x` 寄存器。
    ReadThreadPointer {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 结果位宽，当前固定为 64 位指针 bit。
        width: u8,
    },
    /// 保存当前 LLVM 栈状态，返回值作为指针地址写入 `x` 寄存器。
    StackSave {
        /// 目标 `x` 寄存器。
        dst: u8,
    },
    /// 恢复到此前保存的 LLVM 栈状态。
    StackRestore {
        /// 保存点指针所在的 `x` 寄存器。
        ptr: u8,
    },
    /// 刷新一段 instruction cache 地址范围。
    ClearCache {
        /// 起始地址所在的 `x` 寄存器。
        start: u8,
        /// 结束地址所在的 `x` 寄存器。
        end: u8,
    },
    /// 保留 LLVM `pseudoprobe` intrinsic 的可见采样/PGO 伪探针副作用。
    PseudoProbe {
        /// 函数或调用点 GUID。
        guid: u64,
        /// 探针索引。
        index: u64,
        /// LLVM pseudoprobe 类型。
        probe_type: u32,
        /// LLVM pseudoprobe 属性 bitmask。
        attributes: u64,
    },
    /// 执行 LLVM `prefetch` intrinsic，保留源 IR 的显式预取提示。
    Prefetch {
        /// 要预取的指针所在的 `x` 寄存器。
        ptr: u8,
        /// 读写方向 immarg，0 表示 read，1 表示 write。
        rw: u8,
        /// locality immarg，取值范围 0..3。
        locality: u8,
        /// cache type immarg，0 表示 instruction，1 表示 data。
        cache: u8,
    },
    /// 超级指令：先执行整数加法，再与第三个操作数做 xor。
    SuperAddXor {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 加法左操作数 `x` 寄存器。
        lhs: u8,
        /// 加法右操作数 `x` 寄存器。
        rhs: u8,
        /// xor 右操作数 `x` 寄存器。
        xor_rhs: u8,
        /// 结果位宽。
        width: u8,
    },
    /// 超级指令：整数比较后直接选择两个 bytecode 分支目标。
    SuperIcmpBrIf {
        /// 归一化为 VM 形式的 LLVM 比较谓词。
        pred: CmpPredicate,
        /// 比较左操作数 `x` 寄存器。
        lhs: u8,
        /// 比较右操作数 `x` 寄存器。
        rhs: u8,
        /// 比较操作数位宽。
        width: u8,
        /// 比较为 true 时的目标 label。
        then_label: LabelId,
        /// 比较为 false 时的目标 label。
        else_label: LabelId,
    },
    /// 超级指令：常量字节偏移 GEP 后立即读取标量。
    SuperGepLoad {
        /// 加载目标 `x` 寄存器。
        dst: u8,
        /// 基址指针寄存器。
        base: u8,
        /// 加到基址上的字节偏移。
        offset: u64,
        /// 加载位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再与寄存器加数做整数加法。
    SuperLoadAdd {
        /// 加法结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 加到已加载值上的寄存器。
        addend: u8,
        /// 加载和加法结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再与寄存器因子做整数乘法。
    SuperLoadMul {
        /// 乘法结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 与已加载值相乘的寄存器。
        factor: u8,
        /// 加载和乘法结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再除以寄存器无符号除数。
    SuperLoadUDiv {
        /// 无符号除法结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 用于除已加载值的无符号除数寄存器。
        divisor: u8,
        /// 加载和除法结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再除以寄存器有符号除数。
    SuperLoadSDiv {
        /// 有符号除法结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 用于除已加载值的有符号除数寄存器。
        divisor: u8,
        /// 加载和除法结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再对寄存器无符号除数取余。
    SuperLoadURem {
        /// 无符号取余结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 用于对已加载值取余的无符号除数寄存器。
        divisor: u8,
        /// 加载和取余结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再对寄存器有符号除数取余。
    SuperLoadSRem {
        /// 有符号取余结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 用于对已加载值取余的有符号除数寄存器。
        divisor: u8,
        /// 加载和取余结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再按寄存器移位量左移。
    SuperLoadShl {
        /// 左移结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 移位量寄存器。
        shift: u8,
        /// 加载和左移结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再按寄存器移位量逻辑右移。
    SuperLoadLShr {
        /// 逻辑右移结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 移位量寄存器。
        shift: u8,
        /// 加载和逻辑右移结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再按寄存器移位量算术右移。
    SuperLoadAShr {
        /// 算术右移结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 移位量寄存器。
        shift: u8,
        /// 加载和算术右移结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再与寄存器操作数做有符号最大值。
    SuperLoadSMax {
        /// 有符号最大值结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 与已加载值比较的寄存器。
        rhs: u8,
        /// 加载和比较结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再与寄存器操作数做有符号最小值。
    SuperLoadSMin {
        /// 有符号最小值结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 与已加载值比较的寄存器。
        rhs: u8,
        /// 加载和比较结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再与寄存器操作数做无符号最大值。
    SuperLoadUMax {
        /// 无符号最大值结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 与已加载值比较的寄存器。
        rhs: u8,
        /// 加载和比较结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再与寄存器操作数做无符号最小值。
    SuperLoadUMin {
        /// 无符号最小值结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 与已加载值比较的寄存器。
        rhs: u8,
        /// 加载和比较结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再做无符号饱和加法。
    SuperLoadUAddSat {
        /// 饱和加法结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 与已加载值相加的寄存器。
        rhs: u8,
        /// 加载和饱和结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再做无符号饱和减法。
    SuperLoadUSubSat {
        /// 饱和减法结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 从已加载值中减去的寄存器。
        rhs: u8,
        /// 加载和饱和结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再做有符号饱和加法。
    SuperLoadSAddSat {
        /// 饱和加法结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 与已加载值相加的寄存器。
        rhs: u8,
        /// 加载和饱和结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再做有符号饱和减法。
    SuperLoadSSubSat {
        /// 饱和减法结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 从已加载值中减去的寄存器。
        rhs: u8,
        /// 加载和饱和结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再做无符号饱和左移。
    SuperLoadUShlSat {
        /// 饱和左移结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 左移位数寄存器。
        rhs: u8,
        /// 加载和饱和结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再做有符号饱和左移。
    SuperLoadSShlSat {
        /// 饱和左移结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 左移位数寄存器。
        rhs: u8,
        /// 加载和饱和结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再与寄存器操作数做整数与。
    SuperLoadAnd {
        /// 与运算结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 与已加载值做 and 的寄存器。
        and_rhs: u8,
        /// 加载和 and 结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再与寄存器操作数做整数或。
    SuperLoadOr {
        /// 或运算结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 与已加载值做 or 的寄存器。
        or_rhs: u8,
        /// 加载和 or 结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再减去寄存器减数。
    SuperLoadSub {
        /// 减法结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 从已加载值中减去的寄存器。
        subtrahend: u8,
        /// 加载和减法结果位宽。
        width: u8,
    },
    /// 超级指令：先从内存读取标量，再与寄存器操作数做整数异或。
    SuperLoadXor {
        /// 异或结果目标 `x` 寄存器。
        dst: u8,
        /// 被读取的指针寄存器。
        ptr: u8,
        /// 与已加载值异或的寄存器。
        xor_rhs: u8,
        /// 加载和异或结果位宽。
        width: u8,
    },
    /// 在两个 VM 寄存器之间复制值。
    Mov {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 源 `x` 寄存器。
        src: u8,
        /// 结果位宽。
        width: u8,
    },
    /// 整数二元运算。
    Bin {
        /// lowering 选中的后端语义运算。
        op: BinOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 左操作数 `x` 寄存器。
        lhs: u8,
        /// 右操作数 `x` 寄存器。
        rhs: u8,
        /// 结果位宽。
        width: u8,
    },
    /// 整数一元运算。
    IntUnary {
        /// lowering 选中的后端语义运算。
        op: IntUnaryOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 源 `x` 寄存器。
        src: u8,
        /// 操作数位宽。
        width: u8,
    },
    /// 整数三元运算。
    IntTernary {
        /// lowering 选中的后端语义运算。
        op: IntTernaryOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 左操作数 `x` 寄存器。
        lhs: u8,
        /// 右操作数 `x` 寄存器。
        rhs: u8,
        /// 第三个操作数 `x` 寄存器。
        third: u8,
        /// 操作数位宽。
        width: u8,
    },
    /// 整数 with.overflow intrinsic：同时产生 wrapping 结果和 `i1` 溢出标志。
    IntOverflow {
        /// lowering 选中的溢出检测类别。
        op: IntOverflowOp,
        /// wrapping 结果目标 `x` 寄存器。
        dst: u8,
        /// 溢出标志目标 `x` 寄存器。
        overflow: u8,
        /// 左操作数 `x` 寄存器。
        lhs: u8,
        /// 右操作数 `x` 寄存器。
        rhs: u8,
        /// 操作数位宽。
        width: u8,
    },
    /// 整数比较，在 `x` 寄存器中生成 `i1` 值。
    Icmp {
        /// 归一化为 VM 形式的 LLVM 比较谓词。
        pred: CmpPredicate,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 左操作数 `x` 寄存器。
        lhs: u8,
        /// 右操作数 `x` 寄存器。
        rhs: u8,
        /// 操作数位宽。
        width: u8,
    },
    /// 标量浮点二元运算，输入和输出都以原始 bit 存在 `x` 寄存器中。
    FloatBin {
        /// lowering 选中的后端浮点运算。
        op: FloatBinOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 左操作数 `x` 寄存器。
        lhs: u8,
        /// 右操作数 `x` 寄存器。
        rhs: u8,
        /// 浮点位宽，仅支持 32 或 64。
        width: u8,
    },
    /// 标量浮点/整数混合二元运算，左操作数是浮点 bit，右操作数是整数值。
    FloatIntBin {
        /// lowering 选中的后端混合运算。
        op: FloatIntBinOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 浮点左操作数 `x` 寄存器。
        lhs: u8,
        /// 整数右操作数 `x` 寄存器。
        rhs: u8,
        /// 浮点位宽，仅支持 32 或 64。
        width: u8,
    },
    /// 标量浮点一元运算，输入和输出都以原始 bit 存在 `x` 寄存器中。
    FloatUnary {
        /// lowering 选中的后端浮点一元运算。
        op: FloatUnaryOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 源 `x` 寄存器。
        src: u8,
        /// 浮点位宽，仅支持 32 或 64。
        width: u8,
    },
    /// 标量浮点三元运算，输入和输出都以原始 bit 存在 `x` 寄存器中。
    FloatTernary {
        /// lowering 选中的后端浮点三元运算。
        op: FloatTernaryOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 左操作数 `x` 寄存器。
        lhs: u8,
        /// 右操作数 `x` 寄存器。
        rhs: u8,
        /// 第三个操作数 `x` 寄存器。
        third: u8,
        /// 浮点位宽，仅支持 32 或 64。
        width: u8,
    },
    /// 标量整数/浮点转换，整数值和浮点 bit 都存放在 `x` 寄存器中。
    FloatCast {
        /// lowering 选中的后端转换操作。
        op: FloatCastOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 源 `x` 寄存器。
        src: u8,
        /// 源位宽。
        from_width: u8,
        /// 目标位宽。
        to_width: u8,
    },
    /// 标量浮点到整数取整 intrinsic，输入为浮点 bit，输出为整数值。
    FloatRoundToInt {
        /// lowering 选中的后端 round-to-int intrinsic。
        op: FloatRoundToIntOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 源 `x` 寄存器。
        src: u8,
        /// 源浮点位宽。
        from_width: u8,
        /// 目标整数位宽。
        to_width: u8,
    },
    /// 标量浮点比较，在 `x` 寄存器中生成 `i1` 值。
    Fcmp {
        /// 归一化为 VM 形式的 LLVM 浮点比较谓词。
        pred: FloatPredicate,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 左操作数 `x` 寄存器。
        lhs: u8,
        /// 右操作数 `x` 寄存器。
        rhs: u8,
        /// 操作数位宽，仅支持 32 或 64。
        width: u8,
    },
    /// 标量浮点分类，在 `x` 寄存器中生成 `i1` 值。
    FloatClass {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 源浮点 bit 所在 `x` 寄存器。
        src: u8,
        /// LLVM `FPClassTest` mask。
        mask: u16,
        /// 操作数位宽，仅支持 32 或 64。
        width: u8,
    },
    /// 整数或指针位宽转换。
    Cast {
        /// lowering 选中的 cast 操作。
        op: CastOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 源 `x` 寄存器。
        src: u8,
        /// 源位宽。
        from_width: u8,
        /// 目标位宽。
        to_width: u8,
    },
    /// 在 VM runtime frame 内进行固定大小栈分配。
    Alloca {
        /// 目标指针寄存器。
        dst: u8,
        /// 分配大小，单位为字节。
        bytes: u64,
        /// 所需对齐，单位为字节。
        align: u8,
    },
    /// 在 VM runtime frame 内按运行时元素个数进行栈分配。
    DynamicAlloca {
        /// 目标指针寄存器。
        dst: u8,
        /// 保存元素个数的 `x` 寄存器。
        count: u8,
        /// 单元素大小，单位为字节。
        elem_size: u64,
        /// 所需对齐，单位为字节。
        align: u8,
    },
    /// 从 `x` 寄存器保存的地址加载标量。
    Load {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 加载位宽。
        width: u8,
    },
    /// 向 `x` 寄存器保存的地址存储标量。
    Store {
        /// 源值寄存器。
        src: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 存储位宽。
        width: u8,
    },
    /// 从 `x` 寄存器保存的地址执行 volatile 标量加载。
    VolatileLoad {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 加载位宽。
        width: u8,
    },
    /// 向 `x` 寄存器保存的地址执行 volatile 标量存储。
    VolatileStore {
        /// 源值寄存器。
        src: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 存储位宽。
        width: u8,
    },
    /// 按运行时长度从源地址向目标地址复制连续内存，语义等同 LLVM memcpy。
    MemcpyDynamic {
        /// 目标指针寄存器。
        dst: u8,
        /// 源指针寄存器。
        src: u8,
        /// 保存复制字节数的 `x` 寄存器。
        len: u8,
    },
    /// 按运行时长度复制可重叠内存，语义等同 LLVM memmove。
    MemmoveDynamic {
        /// 目标指针寄存器。
        dst: u8,
        /// 源指针寄存器。
        src: u8,
        /// 保存复制字节数的 `x` 寄存器。
        len: u8,
    },
    /// 按运行时长度把同一个 i8 值写入连续内存。
    MemsetDynamic {
        /// 目标指针寄存器。
        dst: u8,
        /// 保存 i8 写入值的 `x` 寄存器。
        value: u8,
        /// 保存写入字节数的 `x` 寄存器。
        len: u8,
    },
    /// 按运行时长度执行 volatile memcpy。
    VolatileMemcpyDynamic {
        /// 目标指针寄存器。
        dst: u8,
        /// 源指针寄存器。
        src: u8,
        /// 保存复制字节数的 `x` 寄存器。
        len: u8,
    },
    /// 按运行时长度执行 volatile memmove。
    VolatileMemmoveDynamic {
        /// 目标指针寄存器。
        dst: u8,
        /// 源指针寄存器。
        src: u8,
        /// 保存复制字节数的 `x` 寄存器。
        len: u8,
    },
    /// 按运行时长度执行 volatile memset。
    VolatileMemsetDynamic {
        /// 目标指针寄存器。
        dst: u8,
        /// 保存 i8 写入值的 `x` 寄存器。
        value: u8,
        /// 保存写入字节数的 `x` 寄存器。
        len: u8,
    },
    /// 从 `x` 寄存器保存的地址执行标量 atomic load。
    AtomicLoad {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 加载位宽。
        width: u8,
        /// LLVM atomic ordering 的 VM 编码。
        ordering: MemoryOrdering,
        /// LLVM atomic syncscope ID，只允许 system 与 singlethread。
        sync_scope: u8,
    },
    /// 向 `x` 寄存器保存的地址执行标量 atomic store。
    AtomicStore {
        /// 源值寄存器。
        src: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 存储位宽。
        width: u8,
        /// LLVM atomic ordering 的 VM 编码。
        ordering: MemoryOrdering,
        /// LLVM atomic syncscope ID，只允许 system 与 singlethread。
        sync_scope: u8,
    },
    /// 从 `x` 寄存器保存的地址执行标量 volatile atomic load。
    VolatileAtomicLoad {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 加载位宽。
        width: u8,
        /// LLVM atomic ordering 的 VM 编码。
        ordering: MemoryOrdering,
        /// LLVM atomic syncscope ID，只允许 system 与 singlethread。
        sync_scope: u8,
    },
    /// 向 `x` 寄存器保存的地址执行标量 volatile atomic store。
    VolatileAtomicStore {
        /// 源值寄存器。
        src: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 存储位宽。
        width: u8,
        /// LLVM atomic ordering 的 VM 编码。
        ordering: MemoryOrdering,
        /// LLVM atomic syncscope ID，只允许 system 与 singlethread。
        sync_scope: u8,
    },
    /// 对 `x` 寄存器保存的地址执行标量 atomic read-modify-write，结果是旧值。
    AtomicRmw {
        /// lowering 选中的 RMW 操作。
        op: AtomicRmwOp,
        /// 目标 `x` 寄存器，保存内存旧值。
        dst: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 源值寄存器。
        src: u8,
        /// 操作位宽。
        width: u8,
        /// LLVM atomic ordering 的 VM 编码。
        ordering: MemoryOrdering,
        /// LLVM atomic syncscope ID，只允许 system 与 singlethread。
        sync_scope: u8,
    },
    /// 对 `x` 寄存器保存的地址执行标量 volatile atomic read-modify-write，结果是旧值。
    VolatileAtomicRmw {
        /// lowering 选中的 RMW 操作。
        op: AtomicRmwOp,
        /// 目标 `x` 寄存器，保存内存旧值。
        dst: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 源值寄存器。
        src: u8,
        /// 操作位宽。
        width: u8,
        /// LLVM atomic ordering 的 VM 编码。
        ordering: MemoryOrdering,
        /// LLVM atomic syncscope ID，只允许 system 与 singlethread。
        sync_scope: u8,
    },
    /// 对 `x` 寄存器保存的地址执行 scalar compare-exchange。
    CmpXchg {
        /// 保存内存旧值的目标 `x` 寄存器。
        old: u8,
        /// 保存成功标志的目标 `x` 寄存器。
        success: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 期望旧值寄存器。
        cmp: u8,
        /// 成功时写入的新值寄存器。
        new: u8,
        /// 操作位宽。
        width: u8,
        /// LLVM cmpxchg success ordering 的 VM 编码。
        success_ordering: MemoryOrdering,
        /// LLVM cmpxchg failure ordering 的 VM 编码。
        failure_ordering: MemoryOrdering,
        /// LLVM atomic syncscope ID，只允许 system 与 singlethread。
        sync_scope: u8,
    },
    /// 对 `x` 寄存器保存的地址执行 volatile scalar compare-exchange。
    VolatileCmpXchg {
        /// 保存内存旧值的目标 `x` 寄存器。
        old: u8,
        /// 保存成功标志的目标 `x` 寄存器。
        success: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 期望旧值寄存器。
        cmp: u8,
        /// 成功时写入的新值寄存器。
        new: u8,
        /// 操作位宽。
        width: u8,
        /// LLVM cmpxchg success ordering 的 VM 编码。
        success_ordering: MemoryOrdering,
        /// LLVM cmpxchg failure ordering 的 VM 编码。
        failure_ordering: MemoryOrdering,
        /// LLVM atomic syncscope ID，只允许 system 与 singlethread。
        sync_scope: u8,
    },
    /// 执行 LLVM atomic fence。
    Fence {
        /// LLVM fence ordering 的 VM 编码。
        ordering: MemoryOrdering,
        /// LLVM fence syncscope ID，只允许 system 与 singlethread。
        sync_scope: u8,
    },
    /// 常量字节偏移指针运算。
    Gep {
        /// 目标指针寄存器。
        dst: u8,
        /// 基址指针寄存器。
        base: u8,
        /// 加到基址上的字节偏移。
        offset: u64,
    },
    /// 通过生成的 runtime call table 执行直接 native LLVM 调用。
    CallNative {
        /// runtime call table 槽位。
        call_id: u16,
        /// 参数寄存器；encoder 会填充到 profile record 形状。
        args: Vec<u8>,
        /// wrapper 使用的返回寄存器与位宽。
        returns: Vec<NativeReturn>,
    },
    /// 保留 LLVM `sideeffect` intrinsic 的可见副作用。
    SideEffect,
    /// 不改变 VM 状态的显式 no-op。用于承载 LLVM metadata intrinsic 等无运行时语义的 IR。
    Nop,
    /// 无条件 bytecode 分支。
    Br {
        /// 目标 bytecode label。
        target: LabelId,
    },
    /// 条件 bytecode 分支。
    BrCond {
        /// 保存 `0` 或 `1` 的条件寄存器。
        cond: u8,
        /// `cond != 0` 时的目标 label。
        then_label: LabelId,
        /// `cond == 0` 时的目标 label。
        else_label: LabelId,
    },
    /// VM 内部调用，会把 return PC 存入 profile 的 `lr` 别名。
    VmCall {
        /// 目标 bytecode label。
        target: LabelId,
    },
    /// 使用 profile `lr` 别名的 VM 内部返回。
    VmRet,
    /// LLVM `unreachable` 终结路径；runtime 执行到这里会直接发出 LLVM `unreachable`。
    Unreachable,
    /// LLVM `trap` intrinsic；runtime 执行到这里会调用 LLVM trap intrinsic 并终止。
    Trap,
    /// 从受保护函数返回一个标量返回槽。
    Ret {
        /// 复制到 ABI 返回槽的源寄存器。
        src: u8,
    },
    /// 从 `void` 受保护函数返回。
    RetVoid,
}

impl VmInstruction {
    /// 返回内置 simple VMP profile 使用的规范指令名。
    /// profile 驱动 lowering 应调用 `push_profile`，以便重命名或同语义指令保持精确 identity。
    pub fn default_profile_instruction(&self) -> &'static str {
        match self {
            Self::MovImm { .. } => "mov_imm",
            Self::ConstLoad { .. } => "const_load",
            Self::ReadCounter {
                kind: CounterKind::Cycle,
                ..
            } => "read_cycle",
            Self::ReadCounter {
                kind: CounterKind::Steady,
                ..
            } => "read_steady",
            Self::ReadVScale { .. } => "read_vscale",
            Self::ReadRounding { .. } => "read_rounding",
            Self::ReadFltRounds { .. } => "read_flt_rounds",
            Self::WriteRounding { .. } => "write_rounding",
            Self::ReadFpState {
                kind: FpStateKind::Env, ..
            } => "read_fpenv",
            Self::ReadFpState {
                kind: FpStateKind::Mode,
                ..
            } => "read_fpmode",
            Self::WriteFpState {
                kind: FpStateKind::Env, ..
            } => "write_fpenv",
            Self::WriteFpState {
                kind: FpStateKind::Mode,
                ..
            } => "write_fpmode",
            Self::ResetFpState { kind: FpStateKind::Env } => "reset_fpenv",
            Self::ResetFpState {
                kind: FpStateKind::Mode,
            } => "reset_fpmode",
            Self::ReadThreadPointer { .. } => "read_thread_pointer",
            Self::StackSave { .. } => "stacksave",
            Self::StackRestore { .. } => "stackrestore",
            Self::ClearCache { .. } => "clear_cache",
            Self::PseudoProbe { .. } => "pseudoprobe",
            Self::Prefetch { .. } => "prefetch",
            Self::SuperAddXor { .. } => "iadd_xor",
            Self::SuperIcmpBrIf { .. } => "icmp_br_if",
            Self::SuperGepLoad { .. } => "gep_load",
            Self::SuperLoadAdd { .. } => "load_iadd",
            Self::SuperLoadMul { .. } => "load_imul",
            Self::SuperLoadUDiv { .. } => "load_iudiv",
            Self::SuperLoadSDiv { .. } => "load_isdiv",
            Self::SuperLoadURem { .. } => "load_iurem",
            Self::SuperLoadSRem { .. } => "load_isrem",
            Self::SuperLoadShl { .. } => "load_ishl",
            Self::SuperLoadLShr { .. } => "load_ilshr",
            Self::SuperLoadAShr { .. } => "load_iashr",
            Self::SuperLoadSMax { .. } => "load_ismax",
            Self::SuperLoadSMin { .. } => "load_ismin",
            Self::SuperLoadUMax { .. } => "load_iumax",
            Self::SuperLoadUMin { .. } => "load_iumin",
            Self::SuperLoadUAddSat { .. } => "load_iuadd_sat",
            Self::SuperLoadUSubSat { .. } => "load_iusub_sat",
            Self::SuperLoadSAddSat { .. } => "load_isadd_sat",
            Self::SuperLoadSSubSat { .. } => "load_issub_sat",
            Self::SuperLoadUShlSat { .. } => "load_iushl_sat",
            Self::SuperLoadSShlSat { .. } => "load_isshl_sat",
            Self::SuperLoadAnd { .. } => "load_iand",
            Self::SuperLoadOr { .. } => "load_ior",
            Self::SuperLoadSub { .. } => "load_isub",
            Self::SuperLoadXor { .. } => "load_ixor",
            Self::Mov { .. } => "mov",
            Self::Bin { op, .. } => match op {
                BinOp::Add => "iadd",
                BinOp::Sub => "isub",
                BinOp::Mul => "imul",
                BinOp::UDiv => "iudiv",
                BinOp::SDiv => "isdiv",
                BinOp::URem => "iurem",
                BinOp::SRem => "isrem",
                BinOp::Xor => "ixor",
                BinOp::And => "iand",
                BinOp::Or => "ior",
                BinOp::Shl => "ishl",
                BinOp::LShr => "ilshr",
                BinOp::AShr => "iashr",
                BinOp::SMax => "ismax",
                BinOp::SMin => "ismin",
                BinOp::UMax => "iumax",
                BinOp::UMin => "iumin",
                BinOp::UAddSat => "iuadd_sat",
                BinOp::USubSat => "iusub_sat",
                BinOp::SAddSat => "isadd_sat",
                BinOp::SSubSat => "issub_sat",
                BinOp::UShlSat => "iushl_sat",
                BinOp::SShlSat => "isshl_sat",
            },
            Self::IntUnary { op, .. } => match op {
                IntUnaryOp::CtPop => "ctpop",
                IntUnaryOp::CtLz => "ctlz",
                IntUnaryOp::CtTz => "cttz",
                IntUnaryOp::Abs => "iabs",
                IntUnaryOp::BSwap => "bswap",
                IntUnaryOp::BitReverse => "bitreverse",
            },
            Self::IntTernary { op, .. } => match op {
                IntTernaryOp::FShl => "fshl",
                IntTernaryOp::FShr => "fshr",
            },
            Self::IntOverflow { op, .. } => match op {
                IntOverflowOp::UAdd => "iuadd_overflow",
                IntOverflowOp::SAdd => "isadd_overflow",
                IntOverflowOp::USub => "iusub_overflow",
                IntOverflowOp::SSub => "issub_overflow",
                IntOverflowOp::UMul => "iumul_overflow",
                IntOverflowOp::SMul => "ismul_overflow",
            },
            Self::Icmp { .. } => "icmp",
            Self::FloatBin { op, .. } => match op {
                FloatBinOp::Add => "fadd",
                FloatBinOp::Sub => "fsub",
                FloatBinOp::Mul => "fmul",
                FloatBinOp::Div => "fdiv",
                FloatBinOp::Rem => "frem",
                FloatBinOp::MinNum => "fminnum",
                FloatBinOp::MaxNum => "fmaxnum",
                FloatBinOp::Minimum => "fminimum",
                FloatBinOp::Maximum => "fmaximum",
                FloatBinOp::CopySign => "fcopysign",
                FloatBinOp::Pow => "fpow",
            },
            Self::FloatIntBin { op, .. } => match op {
                FloatIntBinOp::PowI => "fpowi",
            },
            Self::FloatUnary { op, .. } => match op {
                FloatUnaryOp::Neg => "fneg",
                FloatUnaryOp::Abs => "fabs",
                FloatUnaryOp::Sqrt => "fsqrt",
                FloatUnaryOp::Canonicalize => "fcanonicalize",
                FloatUnaryOp::Floor => "ffloor",
                FloatUnaryOp::Ceil => "fceil",
                FloatUnaryOp::Trunc => "ftrunc",
                FloatUnaryOp::Rint => "frint",
                FloatUnaryOp::NearbyInt => "fnearbyint",
                FloatUnaryOp::Round => "fround",
                FloatUnaryOp::RoundEven => "froundeven",
                FloatUnaryOp::Sin => "fsin",
                FloatUnaryOp::Cos => "fcos",
                FloatUnaryOp::Exp => "fexp",
                FloatUnaryOp::Exp2 => "fexp2",
                FloatUnaryOp::Log => "flog",
                FloatUnaryOp::Log10 => "flog10",
                FloatUnaryOp::Log2 => "flog2",
            },
            Self::FloatTernary { op, .. } => match op {
                FloatTernaryOp::Fma => "ffma",
                FloatTernaryOp::MulAdd => "ffmuladd",
            },
            Self::FloatCast { op, .. } => match op {
                FloatCastOp::SignedIntToFloat => "sitofp",
                FloatCastOp::UnsignedIntToFloat => "uitofp",
                FloatCastOp::FloatToSignedInt => "fptosi",
                FloatCastOp::FloatToUnsignedInt => "fptoui",
                FloatCastOp::FloatToSignedIntSat => "fptosi_sat",
                FloatCastOp::FloatToUnsignedIntSat => "fptoui_sat",
                FloatCastOp::FloatTrunc => "fptrunc",
                FloatCastOp::FloatExt => "fpext",
            },
            Self::FloatRoundToInt { op, .. } => match op {
                FloatRoundToIntOp::LRint => "flrint",
                FloatRoundToIntOp::LLRint => "fllrint",
                FloatRoundToIntOp::LRound => "flround",
                FloatRoundToIntOp::LLRound => "fllround",
            },
            Self::Fcmp { .. } => "fcmp",
            Self::FloatClass { .. } => "fpclass",
            Self::Cast { op, .. } => match op {
                CastOp::ZExt => "zext",
                CastOp::SExt => "sext",
                CastOp::Trunc => "trunc",
                CastOp::Bitcast => "bitcast",
            },
            Self::Alloca { .. } => "alloca",
            Self::DynamicAlloca { .. } => "alloca_dyn",
            Self::Load { .. } => "load",
            Self::Store { .. } => "store",
            Self::VolatileLoad { .. } => "volatile_load",
            Self::VolatileStore { .. } => "volatile_store",
            Self::MemcpyDynamic { .. } => "memcpy_dyn",
            Self::MemmoveDynamic { .. } => "memmove_dyn",
            Self::MemsetDynamic { .. } => "memset_dyn",
            Self::VolatileMemcpyDynamic { .. } => "volatile_memcpy_dyn",
            Self::VolatileMemmoveDynamic { .. } => "volatile_memmove_dyn",
            Self::VolatileMemsetDynamic { .. } => "volatile_memset_dyn",
            Self::AtomicLoad { .. } => "atomic_load",
            Self::AtomicStore { .. } => "atomic_store",
            Self::VolatileAtomicLoad { .. } => "volatile_atomic_load",
            Self::VolatileAtomicStore { .. } => "volatile_atomic_store",
            Self::AtomicRmw { op, .. } => match op {
                AtomicRmwOp::Xchg => "atomic_rmw_xchg",
                AtomicRmwOp::Add => "atomic_rmw_add",
                AtomicRmwOp::Sub => "atomic_rmw_sub",
                AtomicRmwOp::And => "atomic_rmw_and",
                AtomicRmwOp::Or => "atomic_rmw_or",
                AtomicRmwOp::Xor => "atomic_rmw_xor",
                AtomicRmwOp::Nand => "atomic_rmw_nand",
                AtomicRmwOp::Max => "atomic_rmw_max",
                AtomicRmwOp::Min => "atomic_rmw_min",
                AtomicRmwOp::UMax => "atomic_rmw_umax",
                AtomicRmwOp::UMin => "atomic_rmw_umin",
                AtomicRmwOp::UIncWrap => "atomic_rmw_uinc_wrap",
                AtomicRmwOp::UDecWrap => "atomic_rmw_udec_wrap",
                AtomicRmwOp::USubCond => "atomic_rmw_usub_cond",
                AtomicRmwOp::USubSat => "atomic_rmw_usub_sat",
                AtomicRmwOp::FAdd => "atomic_rmw_fadd",
                AtomicRmwOp::FSub => "atomic_rmw_fsub",
                AtomicRmwOp::FMax => "atomic_rmw_fmax",
                AtomicRmwOp::FMin => "atomic_rmw_fmin",
                AtomicRmwOp::FMaximum => "atomic_rmw_fmaximum",
                AtomicRmwOp::FMinimum => "atomic_rmw_fminimum",
            },
            Self::VolatileAtomicRmw { op, .. } => match op {
                AtomicRmwOp::Xchg => "volatile_atomic_rmw_xchg",
                AtomicRmwOp::Add => "volatile_atomic_rmw_add",
                AtomicRmwOp::Sub => "volatile_atomic_rmw_sub",
                AtomicRmwOp::And => "volatile_atomic_rmw_and",
                AtomicRmwOp::Or => "volatile_atomic_rmw_or",
                AtomicRmwOp::Xor => "volatile_atomic_rmw_xor",
                AtomicRmwOp::Nand => "volatile_atomic_rmw_nand",
                AtomicRmwOp::Max => "volatile_atomic_rmw_max",
                AtomicRmwOp::Min => "volatile_atomic_rmw_min",
                AtomicRmwOp::UMax => "volatile_atomic_rmw_umax",
                AtomicRmwOp::UMin => "volatile_atomic_rmw_umin",
                AtomicRmwOp::UIncWrap => "volatile_atomic_rmw_uinc_wrap",
                AtomicRmwOp::UDecWrap => "volatile_atomic_rmw_udec_wrap",
                AtomicRmwOp::USubCond => "volatile_atomic_rmw_usub_cond",
                AtomicRmwOp::USubSat => "volatile_atomic_rmw_usub_sat",
                AtomicRmwOp::FAdd => "volatile_atomic_rmw_fadd",
                AtomicRmwOp::FSub => "volatile_atomic_rmw_fsub",
                AtomicRmwOp::FMax => "volatile_atomic_rmw_fmax",
                AtomicRmwOp::FMin => "volatile_atomic_rmw_fmin",
                AtomicRmwOp::FMaximum => "volatile_atomic_rmw_fmaximum",
                AtomicRmwOp::FMinimum => "volatile_atomic_rmw_fminimum",
            },
            Self::CmpXchg { .. } => "cmpxchg",
            Self::VolatileCmpXchg { .. } => "volatile_cmpxchg",
            Self::Fence { .. } => "fence",
            Self::Gep { .. } => "gep",
            Self::CallNative { .. } => "call_native",
            Self::SideEffect => "sideeffect",
            Self::Nop => "fake_nop",
            Self::Br { .. } => "br",
            Self::BrCond { .. } => "br_if",
            Self::VmCall { .. } => "vm_call",
            Self::VmRet => "vm_ret",
            Self::Unreachable => "unreachable",
            Self::Trap => "trap",
            Self::Ret { .. } | Self::RetVoid => "ret",
        }
    }
}

/// 已完成、可交给 bytecode encoder 的 VM 函数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmFunction {
    /// 用于诊断和多态 key 派生的源函数名。
    pub name: String,
    /// 已分配的 `x` 寄存器数量，不能超过 32。
    pub vreg_count: u8,
    /// 宿主标量返回位宽；`void` 返回时为 0。
    pub return_width: u8,
    /// 按执行顺序排列的 VM 指令流。
    pub instructions: Vec<VmInstruction>,
    /// 每条 VM 指令对应的 profile ISA 指令名。
    pub profile_instructions: Vec<String>,
    /// 每个 label 绑定到的 bytecode PC。
    pub label_pcs: HashMap<LabelId, usize>,
}

/// 根据 profile 声明的超级指令，对已经生成的 VM IR 做保守融合。
///
/// 当前支持：
/// - `Super(AddXor)`：`iadd tmp, lhs, rhs` 紧跟 `ixor dst, tmp, xor_rhs`。
/// - `Super(IcmpBrIf)`：`icmp tmp, lhs, rhs` 紧跟使用该 tmp 的 `br_if`。
/// - `Super(GepLoad)`：`gep tmp, base, offset` 紧跟使用该 tmp 的 `load`。
/// - `Super(LoadAdd)`：`load tmp, ptr` 紧跟使用该 tmp 的 `iadd`。
/// - `Super(LoadMul)`：`load tmp, ptr` 紧跟使用该 tmp 的 `imul`。
/// - `Super(LoadUDiv)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `iudiv`。
/// - `Super(LoadSDiv)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `isdiv`。
/// - `Super(LoadURem)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `iurem`。
/// - `Super(LoadSRem)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `isrem`。
/// - `Super(LoadShl)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `ishl`。
/// - `Super(LoadLShr)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `ilshr`。
/// - `Super(LoadAShr)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `iashr`。
/// - `Super(LoadSMax)`：`load tmp, ptr` 紧跟使用该 tmp 的 `ismax`。
/// - `Super(LoadSMin)`：`load tmp, ptr` 紧跟使用该 tmp 的 `ismin`。
/// - `Super(LoadUMax)`：`load tmp, ptr` 紧跟使用该 tmp 的 `iumax`。
/// - `Super(LoadUMin)`：`load tmp, ptr` 紧跟使用该 tmp 的 `iumin`。
/// - `Super(LoadUAddSat)`：`load tmp, ptr` 紧跟使用该 tmp 的 `iuadd_sat`。
/// - `Super(LoadUSubSat)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `iusub_sat`。
/// - `Super(LoadSAddSat)`：`load tmp, ptr` 紧跟使用该 tmp 的 `isadd_sat`。
/// - `Super(LoadSSubSat)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `issub_sat`。
/// - `Super(LoadUShlSat)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `iushl_sat`。
/// - `Super(LoadSShlSat)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `isshl_sat`。
/// - `Super(LoadAnd)`：`load tmp, ptr` 紧跟使用该 tmp 的 `iand`。
/// - `Super(LoadOr)`：`load tmp, ptr` 紧跟使用该 tmp 的 `ior`。
/// - `Super(LoadSub)`：`load tmp, ptr` 紧跟使用该 tmp 作为左操作数的 `isub`。
/// - `Super(LoadXor)`：`load tmp, ptr` 紧跟使用该 tmp 的 `ixor`。
///
/// 如果中间位置是 label target，或临时值还有其它 use，就保持普通指令不变。
pub fn fuse_superinstructions(function: VmFunction, isa: &IsaProfile, lowering: &LoweringProfile) -> VmFunction {
    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::AddXor) {
        fuse_add_xor(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::IcmpBrIf) {
        fuse_icmp_br_if(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::GepLoad) {
        fuse_gep_load(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadAdd) {
        fuse_load_add(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadMul) {
        fuse_load_mul(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadUDiv) {
        fuse_load_udiv(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadSDiv) {
        fuse_load_sdiv(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadURem) {
        fuse_load_urem(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadSRem) {
        fuse_load_srem(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadShl) {
        fuse_load_shl(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadLShr) {
        fuse_load_lshr(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadAShr) {
        fuse_load_ashr(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadSMax) {
        fuse_load_smax(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadSMin) {
        fuse_load_smin(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadUMax) {
        fuse_load_umax(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadUMin) {
        fuse_load_umin(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadUAddSat) {
        fuse_load_uadd_sat(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadUSubSat) {
        fuse_load_usub_sat(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadSAddSat) {
        fuse_load_sadd_sat(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadSSubSat) {
        fuse_load_ssub_sat(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadUShlSat) {
        fuse_load_ushl_sat(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadSShlSat) {
        fuse_load_sshl_sat(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadAnd) {
        fuse_load_and(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadOr) {
        fuse_load_or(function, name)
    } else {
        function
    };

    let function = if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadSub) {
        fuse_load_sub(function, name)
    } else {
        function
    };

    if let Some(name) = enabled_super_instruction(isa, lowering, SuperOp::LoadXor) {
        fuse_load_xor(function, name)
    } else {
        function
    }
}

fn enabled_super_instruction<'a>(isa: &'a IsaProfile, lowering: &LoweringProfile, op: SuperOp) -> Option<&'a str> {
    let desc = isa.by_semantic(&HandlerSemantic::Super(op))?;
    lowering.fusion_for_target(&desc.name)?;
    Some(desc.name.as_str())
}

fn fuse_add_xor(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_add_xor(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_add_xor(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Bin {
        op: BinOp::Add,
        dst: add_dst,
        lhs,
        rhs,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::Xor,
        dst,
        lhs: xor_lhs,
        rhs: xor_rhs,
        width: xor_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != xor_width
        || add_dst != xor_lhs
        || add_dst == xor_rhs
        || temporary_read_count_before_next_write(instructions, index + 1, *add_dst) != 1
    {
        return None;
    }

    Some(VmInstruction::SuperAddXor {
        dst: *dst,
        lhs: *lhs,
        rhs: *rhs,
        xor_rhs: *xor_rhs,
        width: *width,
    })
}

fn fuse_icmp_br_if(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_icmp_br_if(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_icmp_br_if(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Icmp {
        pred,
        dst: cmp_dst,
        lhs,
        rhs,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::BrCond {
        cond,
        then_label,
        else_label,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if cmp_dst != cond || temporary_read_count_before_next_write(instructions, index + 1, *cmp_dst) != 1 {
        return None;
    }

    Some(VmInstruction::SuperIcmpBrIf {
        pred: *pred,
        lhs: *lhs,
        rhs: *rhs,
        width: *width,
        then_label: *then_label,
        else_label: *else_label,
    })
}

fn fuse_gep_load(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_gep_load(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_gep_load(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Gep {
        dst: gep_dst,
        base,
        offset,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Load { dst, ptr, width } = instructions.get(index + 1)? else {
        return None;
    };
    if gep_dst != ptr || temporary_read_count_before_next_write(instructions, index + 1, *gep_dst) != 1 {
        return None;
    }

    Some(VmInstruction::SuperGepLoad {
        dst: *dst,
        base: *base,
        offset: *offset,
        width: *width,
    })
}

fn fuse_load_add(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_add(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_add(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::Add,
        dst,
        lhs,
        rhs,
        width: add_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != add_width || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1 {
        return None;
    }
    let addend = if load_dst == lhs {
        *rhs
    } else if load_dst == rhs {
        *lhs
    } else {
        return None;
    };
    if addend == *load_dst {
        return None;
    }

    Some(VmInstruction::SuperLoadAdd {
        dst: *dst,
        ptr: *ptr,
        addend,
        width: *width,
    })
}

fn fuse_load_mul(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_mul(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_mul(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::Mul,
        dst,
        lhs,
        rhs,
        width: mul_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != mul_width || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1 {
        return None;
    }
    let factor = if load_dst == lhs {
        *rhs
    } else if load_dst == rhs {
        *lhs
    } else {
        return None;
    };
    if factor == *load_dst {
        return None;
    }

    Some(VmInstruction::SuperLoadMul {
        dst: *dst,
        ptr: *ptr,
        factor,
        width: *width,
    })
}

fn fuse_load_udiv(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_udiv(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_udiv(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::UDiv,
        dst,
        lhs,
        rhs,
        width: div_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != div_width
        || load_dst != lhs
        || load_dst == rhs
        || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1
    {
        return None;
    }

    Some(VmInstruction::SuperLoadUDiv {
        dst: *dst,
        ptr: *ptr,
        divisor: *rhs,
        width: *width,
    })
}

fn fuse_load_sdiv(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_sdiv(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_sdiv(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::SDiv,
        dst,
        lhs,
        rhs,
        width: div_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != div_width
        || load_dst != lhs
        || load_dst == rhs
        || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1
    {
        return None;
    }

    Some(VmInstruction::SuperLoadSDiv {
        dst: *dst,
        ptr: *ptr,
        divisor: *rhs,
        width: *width,
    })
}

fn fuse_load_urem(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_urem(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_urem(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::URem,
        dst,
        lhs,
        rhs,
        width: rem_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != rem_width
        || load_dst != lhs
        || load_dst == rhs
        || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1
    {
        return None;
    }

    Some(VmInstruction::SuperLoadURem {
        dst: *dst,
        ptr: *ptr,
        divisor: *rhs,
        width: *width,
    })
}

fn fuse_load_srem(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_srem(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_srem(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::SRem,
        dst,
        lhs,
        rhs,
        width: rem_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != rem_width
        || load_dst != lhs
        || load_dst == rhs
        || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1
    {
        return None;
    }

    Some(VmInstruction::SuperLoadSRem {
        dst: *dst,
        ptr: *ptr,
        divisor: *rhs,
        width: *width,
    })
}

fn fuse_load_shl(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_shl(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_shl(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::Shl,
        dst,
        lhs,
        rhs,
        width: shl_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != shl_width
        || load_dst != lhs
        || load_dst == rhs
        || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1
    {
        return None;
    }

    Some(VmInstruction::SuperLoadShl {
        dst: *dst,
        ptr: *ptr,
        shift: *rhs,
        width: *width,
    })
}

fn fuse_load_lshr(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_lshr(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_lshr(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::LShr,
        dst,
        lhs,
        rhs,
        width: lshr_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != lshr_width
        || load_dst != lhs
        || load_dst == rhs
        || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1
    {
        return None;
    }

    Some(VmInstruction::SuperLoadLShr {
        dst: *dst,
        ptr: *ptr,
        shift: *rhs,
        width: *width,
    })
}

fn fuse_load_ashr(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_ashr(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_ashr(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::AShr,
        dst,
        lhs,
        rhs,
        width: ashr_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != ashr_width
        || load_dst != lhs
        || load_dst == rhs
        || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1
    {
        return None;
    }

    Some(VmInstruction::SuperLoadAShr {
        dst: *dst,
        ptr: *ptr,
        shift: *rhs,
        width: *width,
    })
}

fn fuse_load_smax(function: VmFunction, profile_instruction: &str) -> VmFunction {
    fuse_load_minmax(function, profile_instruction, BinOp::SMax)
}

fn fuse_load_smin(function: VmFunction, profile_instruction: &str) -> VmFunction {
    fuse_load_minmax(function, profile_instruction, BinOp::SMin)
}

fn fuse_load_umax(function: VmFunction, profile_instruction: &str) -> VmFunction {
    fuse_load_minmax(function, profile_instruction, BinOp::UMax)
}

fn fuse_load_umin(function: VmFunction, profile_instruction: &str) -> VmFunction {
    fuse_load_minmax(function, profile_instruction, BinOp::UMin)
}

fn fuse_load_uadd_sat(function: VmFunction, profile_instruction: &str) -> VmFunction {
    fuse_load_saturating(function, profile_instruction, BinOp::UAddSat, true)
}

fn fuse_load_usub_sat(function: VmFunction, profile_instruction: &str) -> VmFunction {
    fuse_load_saturating(function, profile_instruction, BinOp::USubSat, false)
}

fn fuse_load_sadd_sat(function: VmFunction, profile_instruction: &str) -> VmFunction {
    fuse_load_saturating(function, profile_instruction, BinOp::SAddSat, true)
}

fn fuse_load_ssub_sat(function: VmFunction, profile_instruction: &str) -> VmFunction {
    fuse_load_saturating(function, profile_instruction, BinOp::SSubSat, false)
}

fn fuse_load_ushl_sat(function: VmFunction, profile_instruction: &str) -> VmFunction {
    fuse_load_saturating(function, profile_instruction, BinOp::UShlSat, false)
}

fn fuse_load_sshl_sat(function: VmFunction, profile_instruction: &str) -> VmFunction {
    fuse_load_saturating(function, profile_instruction, BinOp::SShlSat, false)
}

fn fuse_load_minmax(function: VmFunction, profile_instruction: &str, op: BinOp) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_minmax(&function.instructions, &label_targets, index, op) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_minmax(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
    expected_op: BinOp,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op,
        dst,
        lhs,
        rhs,
        width: bin_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if *op != expected_op
        || width != bin_width
        || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1
    {
        return None;
    }
    let rhs = if load_dst == lhs {
        *rhs
    } else if load_dst == rhs {
        *lhs
    } else {
        return None;
    };
    if rhs == *load_dst {
        return None;
    }

    match expected_op {
        BinOp::SMax => Some(VmInstruction::SuperLoadSMax {
            dst: *dst,
            ptr: *ptr,
            rhs,
            width: *width,
        }),
        BinOp::SMin => Some(VmInstruction::SuperLoadSMin {
            dst: *dst,
            ptr: *ptr,
            rhs,
            width: *width,
        }),
        BinOp::UMax => Some(VmInstruction::SuperLoadUMax {
            dst: *dst,
            ptr: *ptr,
            rhs,
            width: *width,
        }),
        BinOp::UMin => Some(VmInstruction::SuperLoadUMin {
            dst: *dst,
            ptr: *ptr,
            rhs,
            width: *width,
        }),
        BinOp::Add
        | BinOp::Sub
        | BinOp::Mul
        | BinOp::UDiv
        | BinOp::SDiv
        | BinOp::URem
        | BinOp::SRem
        | BinOp::Xor
        | BinOp::And
        | BinOp::Or
        | BinOp::Shl
        | BinOp::LShr
        | BinOp::AShr
        | BinOp::UAddSat
        | BinOp::USubSat
        | BinOp::SAddSat
        | BinOp::SSubSat
        | BinOp::UShlSat
        | BinOp::SShlSat => None,
    }
}

fn fuse_load_saturating(
    function: VmFunction,
    profile_instruction: &str,
    op: BinOp,
    allow_commuted: bool,
) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_saturating(&function.instructions, &label_targets, index, op, allow_commuted)
        {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| (label, old_to_new[pc]))
        .collect();

    VmFunction {
        instructions,
        profile_instructions,
        label_pcs,
        ..function
    }
}

fn try_fuse_load_saturating(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
    expected_op: BinOp,
    allow_commuted: bool,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op,
        dst,
        lhs,
        rhs,
        width: bin_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if *op != expected_op
        || width != bin_width
        || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1
    {
        return None;
    }
    let rhs = if load_dst == lhs {
        *rhs
    } else if allow_commuted && load_dst == rhs {
        *lhs
    } else {
        return None;
    };
    if rhs == *load_dst {
        return None;
    }

    match expected_op {
        BinOp::UAddSat => Some(VmInstruction::SuperLoadUAddSat {
            dst: *dst,
            ptr: *ptr,
            rhs,
            width: *width,
        }),
        BinOp::USubSat => Some(VmInstruction::SuperLoadUSubSat {
            dst: *dst,
            ptr: *ptr,
            rhs,
            width: *width,
        }),
        BinOp::SAddSat => Some(VmInstruction::SuperLoadSAddSat {
            dst: *dst,
            ptr: *ptr,
            rhs,
            width: *width,
        }),
        BinOp::SSubSat => Some(VmInstruction::SuperLoadSSubSat {
            dst: *dst,
            ptr: *ptr,
            rhs,
            width: *width,
        }),
        BinOp::UShlSat => Some(VmInstruction::SuperLoadUShlSat {
            dst: *dst,
            ptr: *ptr,
            rhs,
            width: *width,
        }),
        BinOp::SShlSat => Some(VmInstruction::SuperLoadSShlSat {
            dst: *dst,
            ptr: *ptr,
            rhs,
            width: *width,
        }),
        BinOp::Add
        | BinOp::Sub
        | BinOp::Mul
        | BinOp::UDiv
        | BinOp::SDiv
        | BinOp::URem
        | BinOp::SRem
        | BinOp::Xor
        | BinOp::And
        | BinOp::Or
        | BinOp::Shl
        | BinOp::LShr
        | BinOp::AShr
        | BinOp::SMax
        | BinOp::SMin
        | BinOp::UMax
        | BinOp::UMin => None,
    }
}

fn fuse_load_and(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_and(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_and(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::And,
        dst,
        lhs,
        rhs,
        width: and_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != and_width || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1 {
        return None;
    }
    let and_rhs = if load_dst == lhs {
        *rhs
    } else if load_dst == rhs {
        *lhs
    } else {
        return None;
    };
    if and_rhs == *load_dst {
        return None;
    }

    Some(VmInstruction::SuperLoadAnd {
        dst: *dst,
        ptr: *ptr,
        and_rhs,
        width: *width,
    })
}

fn fuse_load_or(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_or(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_or(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::Or,
        dst,
        lhs,
        rhs,
        width: or_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != or_width || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1 {
        return None;
    }
    let or_rhs = if load_dst == lhs {
        *rhs
    } else if load_dst == rhs {
        *lhs
    } else {
        return None;
    };
    if or_rhs == *load_dst {
        return None;
    }

    Some(VmInstruction::SuperLoadOr {
        dst: *dst,
        ptr: *ptr,
        or_rhs,
        width: *width,
    })
}

fn fuse_load_sub(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_sub(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_sub(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::Sub,
        dst,
        lhs,
        rhs,
        width: sub_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != sub_width
        || load_dst != lhs
        || load_dst == rhs
        || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1
    {
        return None;
    }

    Some(VmInstruction::SuperLoadSub {
        dst: *dst,
        ptr: *ptr,
        subtrahend: *rhs,
        width: *width,
    })
}

fn fuse_load_xor(function: VmFunction, profile_instruction: &str) -> VmFunction {
    let label_targets = function.label_pcs.values().copied().collect::<HashSet<_>>();
    let mut old_to_new = vec![0usize; function.instructions.len() + 1];
    let mut instructions = Vec::with_capacity(function.instructions.len());
    let mut profile_instructions = Vec::with_capacity(function.profile_instructions.len());
    let mut index = 0;

    while index < function.instructions.len() {
        old_to_new[index] = instructions.len();
        if let Some(fused) = try_fuse_load_xor(&function.instructions, &label_targets, index) {
            instructions.push(fused);
            profile_instructions.push(profile_instruction.to_owned());
            index += 2;
        } else {
            instructions.push(function.instructions[index].clone());
            profile_instructions.push(function.profile_instructions[index].clone());
            index += 1;
        }
    }
    old_to_new[function.instructions.len()] = instructions.len();

    let label_pcs = function
        .label_pcs
        .into_iter()
        .map(|(label, pc)| {
            let new_pc = old_to_new.get(pc).copied().unwrap_or(instructions.len());
            (label, new_pc)
        })
        .collect();

    VmFunction {
        name: function.name,
        vreg_count: function.vreg_count,
        return_width: function.return_width,
        instructions,
        profile_instructions,
        label_pcs,
    }
}

fn try_fuse_load_xor(
    instructions: &[VmInstruction],
    label_targets: &HashSet<usize>,
    index: usize,
) -> Option<VmInstruction> {
    if label_targets.contains(&(index + 1)) {
        return None;
    }
    let VmInstruction::Load {
        dst: load_dst,
        ptr,
        width,
    } = instructions.get(index)?
    else {
        return None;
    };
    let VmInstruction::Bin {
        op: BinOp::Xor,
        dst,
        lhs,
        rhs,
        width: xor_width,
    } = instructions.get(index + 1)?
    else {
        return None;
    };
    if width != xor_width || temporary_read_count_before_next_write(instructions, index + 1, *load_dst) != 1 {
        return None;
    }
    let xor_rhs = if load_dst == lhs {
        *rhs
    } else if load_dst == rhs {
        *lhs
    } else {
        return None;
    };
    if xor_rhs == *load_dst {
        return None;
    }

    Some(VmInstruction::SuperLoadXor {
        dst: *dst,
        ptr: *ptr,
        xor_rhs,
        width: *width,
    })
}

fn temporary_read_count_before_next_write(instructions: &[VmInstruction], start_index: usize, reg: u8) -> usize {
    let mut reads = 0;
    for instruction in instructions.iter().skip(start_index) {
        reads += instruction_register_reads(instruction)
            .into_iter()
            .filter(|read| *read == reg)
            .count();
        if instruction_register_writes(instruction).contains(&reg) {
            break;
        }
    }
    reads
}

fn instruction_register_reads(instruction: &VmInstruction) -> Vec<u8> {
    match instruction {
        VmInstruction::Mov { src, .. } => vec![*src],
        VmInstruction::Bin { lhs, rhs, .. } => vec![*lhs, *rhs],
        VmInstruction::SuperAddXor { lhs, rhs, xor_rhs, .. } => vec![*lhs, *rhs, *xor_rhs],
        VmInstruction::SuperIcmpBrIf { lhs, rhs, .. } => vec![*lhs, *rhs],
        VmInstruction::SuperGepLoad { base, .. } => vec![*base],
        VmInstruction::SuperLoadAdd { ptr, addend, .. } => vec![*ptr, *addend],
        VmInstruction::SuperLoadMul { ptr, factor, .. } => vec![*ptr, *factor],
        VmInstruction::SuperLoadUDiv { ptr, divisor, .. } => vec![*ptr, *divisor],
        VmInstruction::SuperLoadSDiv { ptr, divisor, .. } => vec![*ptr, *divisor],
        VmInstruction::SuperLoadURem { ptr, divisor, .. } => vec![*ptr, *divisor],
        VmInstruction::SuperLoadSRem { ptr, divisor, .. } => vec![*ptr, *divisor],
        VmInstruction::SuperLoadShl { ptr, shift, .. } => vec![*ptr, *shift],
        VmInstruction::SuperLoadLShr { ptr, shift, .. } => vec![*ptr, *shift],
        VmInstruction::SuperLoadAShr { ptr, shift, .. } => vec![*ptr, *shift],
        VmInstruction::SuperLoadSMax { ptr, rhs, .. }
        | VmInstruction::SuperLoadSMin { ptr, rhs, .. }
        | VmInstruction::SuperLoadUMax { ptr, rhs, .. }
        | VmInstruction::SuperLoadUMin { ptr, rhs, .. }
        | VmInstruction::SuperLoadUAddSat { ptr, rhs, .. }
        | VmInstruction::SuperLoadUSubSat { ptr, rhs, .. }
        | VmInstruction::SuperLoadSAddSat { ptr, rhs, .. }
        | VmInstruction::SuperLoadSSubSat { ptr, rhs, .. }
        | VmInstruction::SuperLoadUShlSat { ptr, rhs, .. }
        | VmInstruction::SuperLoadSShlSat { ptr, rhs, .. } => vec![*ptr, *rhs],
        VmInstruction::SuperLoadAnd { ptr, and_rhs, .. } => vec![*ptr, *and_rhs],
        VmInstruction::SuperLoadOr { ptr, or_rhs, .. } => vec![*ptr, *or_rhs],
        VmInstruction::SuperLoadSub { ptr, subtrahend, .. } => vec![*ptr, *subtrahend],
        VmInstruction::SuperLoadXor { ptr, xor_rhs, .. } => vec![*ptr, *xor_rhs],
        VmInstruction::IntUnary { src, .. } => vec![*src],
        VmInstruction::IntTernary { lhs, rhs, third, .. } => vec![*lhs, *rhs, *third],
        VmInstruction::IntOverflow { lhs, rhs, .. } => vec![*lhs, *rhs],
        VmInstruction::Icmp { lhs, rhs, .. } | VmInstruction::Fcmp { lhs, rhs, .. } => vec![*lhs, *rhs],
        VmInstruction::FloatBin { lhs, rhs, .. } | VmInstruction::FloatIntBin { lhs, rhs, .. } => vec![*lhs, *rhs],
        VmInstruction::FloatTernary { lhs, rhs, third, .. } => vec![*lhs, *rhs, *third],
        VmInstruction::FloatUnary { src, .. }
        | VmInstruction::FloatCast { src, .. }
        | VmInstruction::FloatRoundToInt { src, .. }
        | VmInstruction::FloatClass { src, .. } => vec![*src],
        VmInstruction::Cast { src, .. } => vec![*src],
        VmInstruction::DynamicAlloca { count, .. } => vec![*count],
        VmInstruction::StackRestore { ptr } => vec![*ptr],
        VmInstruction::ClearCache { start, end } => vec![*start, *end],
        VmInstruction::Prefetch { ptr, .. } => vec![*ptr],
        VmInstruction::Load { ptr, .. } => vec![*ptr],
        VmInstruction::Store { src, ptr, .. } => vec![*src, *ptr],
        VmInstruction::VolatileLoad { ptr, .. } => vec![*ptr],
        VmInstruction::VolatileStore { src, ptr, .. } => vec![*src, *ptr],
        VmInstruction::MemcpyDynamic { dst, src, len } | VmInstruction::MemmoveDynamic { dst, src, len } => {
            vec![*dst, *src, *len]
        },
        VmInstruction::MemsetDynamic { dst, value, len } => vec![*dst, *value, *len],
        VmInstruction::VolatileMemcpyDynamic { dst, src, len }
        | VmInstruction::VolatileMemmoveDynamic { dst, src, len } => vec![*dst, *src, *len],
        VmInstruction::VolatileMemsetDynamic { dst, value, len } => vec![*dst, *value, *len],
        VmInstruction::AtomicLoad { ptr, .. } => vec![*ptr],
        VmInstruction::AtomicStore { src, ptr, .. } => vec![*src, *ptr],
        VmInstruction::VolatileAtomicLoad { ptr, .. } => vec![*ptr],
        VmInstruction::VolatileAtomicStore { src, ptr, .. } => vec![*src, *ptr],
        VmInstruction::AtomicRmw { ptr, src, .. } | VmInstruction::VolatileAtomicRmw { ptr, src, .. } => {
            vec![*ptr, *src]
        },
        VmInstruction::CmpXchg { ptr, cmp, new, .. } | VmInstruction::VolatileCmpXchg { ptr, cmp, new, .. } => {
            vec![*ptr, *cmp, *new]
        },
        VmInstruction::Gep { base, .. } => vec![*base],
        VmInstruction::CallNative { args, .. } => args.clone(),
        VmInstruction::BrCond { cond, .. } => vec![*cond],
        VmInstruction::WriteRounding { src, .. } | VmInstruction::WriteFpState { src, .. } => vec![*src],
        VmInstruction::Ret { src } => vec![*src],
        VmInstruction::MovImm { .. }
        | VmInstruction::ConstLoad { .. }
        | VmInstruction::ReadCounter { .. }
        | VmInstruction::ReadVScale { .. }
        | VmInstruction::ReadRounding { .. }
        | VmInstruction::ReadFltRounds { .. }
        | VmInstruction::ReadFpState { .. }
        | VmInstruction::ResetFpState { .. }
        | VmInstruction::ReadThreadPointer { .. }
        | VmInstruction::StackSave { .. }
        | VmInstruction::Alloca { .. }
        | VmInstruction::PseudoProbe { .. }
        | VmInstruction::Fence { .. }
        | VmInstruction::SideEffect
        | VmInstruction::Br { .. }
        | VmInstruction::Nop
        | VmInstruction::VmCall { .. }
        | VmInstruction::VmRet
        | VmInstruction::Unreachable
        | VmInstruction::Trap
        | VmInstruction::RetVoid => Vec::new(),
    }
}

fn instruction_register_writes(instruction: &VmInstruction) -> Vec<u8> {
    match instruction {
        VmInstruction::MovImm { dst, .. }
        | VmInstruction::ConstLoad { dst, .. }
        | VmInstruction::ReadCounter { dst, .. }
        | VmInstruction::ReadVScale { dst, .. }
        | VmInstruction::ReadRounding { dst, .. }
        | VmInstruction::ReadFltRounds { dst, .. }
        | VmInstruction::ReadFpState { dst, .. }
        | VmInstruction::ReadThreadPointer { dst, .. }
        | VmInstruction::StackSave { dst }
        | VmInstruction::SuperAddXor { dst, .. }
        | VmInstruction::SuperGepLoad { dst, .. }
        | VmInstruction::SuperLoadAdd { dst, .. }
        | VmInstruction::SuperLoadMul { dst, .. }
        | VmInstruction::SuperLoadUDiv { dst, .. }
        | VmInstruction::SuperLoadSDiv { dst, .. }
        | VmInstruction::SuperLoadURem { dst, .. }
        | VmInstruction::SuperLoadSRem { dst, .. }
        | VmInstruction::SuperLoadShl { dst, .. }
        | VmInstruction::SuperLoadLShr { dst, .. }
        | VmInstruction::SuperLoadAShr { dst, .. }
        | VmInstruction::SuperLoadSMax { dst, .. }
        | VmInstruction::SuperLoadSMin { dst, .. }
        | VmInstruction::SuperLoadUMax { dst, .. }
        | VmInstruction::SuperLoadUMin { dst, .. }
        | VmInstruction::SuperLoadUAddSat { dst, .. }
        | VmInstruction::SuperLoadUSubSat { dst, .. }
        | VmInstruction::SuperLoadSAddSat { dst, .. }
        | VmInstruction::SuperLoadSSubSat { dst, .. }
        | VmInstruction::SuperLoadUShlSat { dst, .. }
        | VmInstruction::SuperLoadSShlSat { dst, .. }
        | VmInstruction::SuperLoadAnd { dst, .. }
        | VmInstruction::SuperLoadOr { dst, .. }
        | VmInstruction::SuperLoadSub { dst, .. }
        | VmInstruction::SuperLoadXor { dst, .. }
        | VmInstruction::Mov { dst, .. }
        | VmInstruction::Bin { dst, .. }
        | VmInstruction::IntUnary { dst, .. }
        | VmInstruction::IntTernary { dst, .. }
        | VmInstruction::Icmp { dst, .. }
        | VmInstruction::FloatBin { dst, .. }
        | VmInstruction::FloatIntBin { dst, .. }
        | VmInstruction::FloatUnary { dst, .. }
        | VmInstruction::FloatTernary { dst, .. }
        | VmInstruction::FloatCast { dst, .. }
        | VmInstruction::FloatRoundToInt { dst, .. }
        | VmInstruction::Fcmp { dst, .. }
        | VmInstruction::FloatClass { dst, .. }
        | VmInstruction::Cast { dst, .. }
        | VmInstruction::Alloca { dst, .. }
        | VmInstruction::DynamicAlloca { dst, .. }
        | VmInstruction::Load { dst, .. }
        | VmInstruction::VolatileLoad { dst, .. }
        | VmInstruction::AtomicLoad { dst, .. }
        | VmInstruction::VolatileAtomicLoad { dst, .. }
        | VmInstruction::AtomicRmw { dst, .. }
        | VmInstruction::VolatileAtomicRmw { dst, .. }
        | VmInstruction::Gep { dst, .. } => vec![*dst],
        VmInstruction::IntOverflow { dst, overflow, .. } => vec![*dst, *overflow],
        VmInstruction::CmpXchg { old, success, .. } | VmInstruction::VolatileCmpXchg { old, success, .. } => {
            vec![*old, *success]
        },
        VmInstruction::CallNative { returns, .. } => returns.iter().map(|ret| ret.dst).collect(),
        VmInstruction::StackRestore { .. }
        | VmInstruction::WriteRounding { .. }
        | VmInstruction::WriteFpState { .. }
        | VmInstruction::ResetFpState { .. }
        | VmInstruction::ClearCache { .. }
        | VmInstruction::PseudoProbe { .. }
        | VmInstruction::Prefetch { .. }
        | VmInstruction::SuperIcmpBrIf { .. }
        | VmInstruction::Store { .. }
        | VmInstruction::VolatileStore { .. }
        | VmInstruction::MemcpyDynamic { .. }
        | VmInstruction::MemmoveDynamic { .. }
        | VmInstruction::MemsetDynamic { .. }
        | VmInstruction::VolatileMemcpyDynamic { .. }
        | VmInstruction::VolatileMemmoveDynamic { .. }
        | VmInstruction::VolatileMemsetDynamic { .. }
        | VmInstruction::AtomicStore { .. }
        | VmInstruction::VolatileAtomicStore { .. }
        | VmInstruction::Fence { .. }
        | VmInstruction::SideEffect
        | VmInstruction::Nop
        | VmInstruction::Br { .. }
        | VmInstruction::BrCond { .. }
        | VmInstruction::VmCall { .. }
        | VmInstruction::VmRet
        | VmInstruction::Unreachable
        | VmInstruction::Trap
        | VmInstruction::Ret { .. }
        | VmInstruction::RetVoid => Vec::new(),
    }
}

/// 拥有 label 分配并校验所有 label 都已绑定的 builder。
#[derive(Debug)]
pub struct VmFunctionBuilder {
    name: String,
    vreg_count: u8,
    free_vregs: Vec<u8>,
    return_width: u8,
    instructions: Vec<VmInstruction>,
    profile_instructions: Vec<String>,
    label_pcs: HashMap<LabelId, usize>,
    next_label: u32,
}

impl VmFunctionBuilder {
    /// 为源函数创建 VM function builder。
    pub fn new(name: impl Into<String>, initial_vregs: u8, return_width: u8) -> Self {
        Self {
            name: name.into(),
            vreg_count: initial_vregs,
            free_vregs: Vec::new(),
            return_width,
            instructions: Vec::new(),
            profile_instructions: Vec::new(),
            label_pcs: HashMap::new(),
            next_label: 0,
        }
    }

    /// 分配新的 VM label。
    pub fn new_label(&mut self) -> LabelId {
        let label = LabelId(self.next_label);
        self.next_label += 1;
        label
    }

    /// 把 label 绑定到下一条指令 PC。
    pub fn bind_label(&mut self, label: LabelId) {
        self.label_pcs.insert(label, self.instructions.len());
    }

    /// 分配新的、由 `x` 寄存器承载的 VM 虚拟寄存器。
    pub fn alloc_vreg(&mut self) -> anyhow::Result<u8> {
        if let Some(reg) = self.free_vregs.pop() {
            return Ok(reg);
        }

        if self.vreg_count >= 32 {
            anyhow::bail!("VM x-register budget exceeded: x0..x31 are available");
        }
        let reg = self.vreg_count;
        self.vreg_count += 1;
        Ok(reg)
    }

    /// 分配新的 VM `x` 寄存器，并保证它不会被 `native_call` 等 ABI 操作触碰。
    pub fn alloc_vreg_excluding(&mut self, excluded: &HashSet<u8>) -> anyhow::Result<u8> {
        if let Some(index) = self.free_vregs.iter().rposition(|reg| !excluded.contains(reg)) {
            return Ok(self.free_vregs.swap_remove(index));
        }

        while self.vreg_count < 32 {
            let reg = self.vreg_count;
            self.vreg_count += 1;
            if !excluded.contains(&reg) {
                return Ok(reg);
            }
        }

        anyhow::bail!("VM x-register budget exceeded: no register outside native_call clobbers is available");
    }

    /// 记录预分配寄存器已经被使用。
    pub fn reserve_vregs(&mut self, count: u8) -> anyhow::Result<()> {
        if count > 32 {
            anyhow::bail!("VM x-register budget exceeded: requested {count}");
        }
        self.vreg_count = self.vreg_count.max(count);
        self.free_vregs.retain(|reg| *reg < count);
        Ok(())
    }

    /// 在最后一次 SSA 使用已生成后，把 VM 寄存器标记为可复用。
    pub fn release_vreg(&mut self, reg: u8) {
        if reg < self.vreg_count && !self.free_vregs.contains(&reg) {
            self.free_vregs.push(reg);
        }
    }

    /// 追加 VM 指令。
    pub fn push(&mut self, instruction: VmInstruction) {
        let profile_instruction = instruction.default_profile_instruction().to_owned();
        self.push_profile(instruction, profile_instruction);
    }

    /// 追加 VM 指令以及 lowering 选中的精确 profile 指令名。
    pub fn push_profile(&mut self, instruction: VmInstruction, profile_instruction: impl Into<String>) {
        self.instructions.push(instruction);
        self.profile_instructions.push(profile_instruction.into());
    }

    /// 校验 label 一致性后完成 VM 函数构建。
    pub fn finish(self) -> anyhow::Result<VmFunction> {
        if self.instructions.len() != self.profile_instructions.len() {
            anyhow::bail!(
                "VM instruction stream has {} records but {} profile instruction names",
                self.instructions.len(),
                self.profile_instructions.len()
            );
        }
        for label in self.referenced_labels() {
            if !self.label_pcs.contains_key(&label) {
                anyhow::bail!("unbound VM label {:?}", label);
            }
        }

        Ok(VmFunction {
            name: self.name,
            vreg_count: self.vreg_count,
            return_width: self.return_width,
            instructions: self.instructions,
            profile_instructions: self.profile_instructions,
            label_pcs: self.label_pcs,
        })
    }

    fn referenced_labels(&self) -> Vec<LabelId> {
        self.instructions
            .iter()
            .flat_map(|instruction| match instruction {
                VmInstruction::Br { target } => vec![*target],
                VmInstruction::VmCall { target } => vec![*target],
                VmInstruction::BrCond {
                    then_label, else_label, ..
                } => vec![*then_label, *else_label],
                VmInstruction::SuperIcmpBrIf {
                    then_label, else_label, ..
                } => vec![*then_label, *else_label],
                VmInstruction::MovImm { .. }
                | VmInstruction::ConstLoad { .. }
                | VmInstruction::ReadCounter { .. }
                | VmInstruction::ReadVScale { .. }
                | VmInstruction::ReadRounding { .. }
                | VmInstruction::ReadFltRounds { .. }
                | VmInstruction::WriteRounding { .. }
                | VmInstruction::ReadFpState { .. }
                | VmInstruction::WriteFpState { .. }
                | VmInstruction::ResetFpState { .. }
                | VmInstruction::ReadThreadPointer { .. }
                | VmInstruction::StackSave { .. }
                | VmInstruction::StackRestore { .. }
                | VmInstruction::ClearCache { .. }
                | VmInstruction::PseudoProbe { .. }
                | VmInstruction::Prefetch { .. }
                | VmInstruction::SuperAddXor { .. }
                | VmInstruction::SuperGepLoad { .. }
                | VmInstruction::SuperLoadAdd { .. }
                | VmInstruction::SuperLoadMul { .. }
                | VmInstruction::SuperLoadUDiv { .. }
                | VmInstruction::SuperLoadSDiv { .. }
                | VmInstruction::SuperLoadURem { .. }
                | VmInstruction::SuperLoadSRem { .. }
                | VmInstruction::SuperLoadShl { .. }
                | VmInstruction::SuperLoadLShr { .. }
                | VmInstruction::SuperLoadAShr { .. }
                | VmInstruction::SuperLoadSMax { .. }
                | VmInstruction::SuperLoadSMin { .. }
                | VmInstruction::SuperLoadUMax { .. }
                | VmInstruction::SuperLoadUMin { .. }
                | VmInstruction::SuperLoadUAddSat { .. }
                | VmInstruction::SuperLoadUSubSat { .. }
                | VmInstruction::SuperLoadSAddSat { .. }
                | VmInstruction::SuperLoadSSubSat { .. }
                | VmInstruction::SuperLoadUShlSat { .. }
                | VmInstruction::SuperLoadSShlSat { .. }
                | VmInstruction::SuperLoadAnd { .. }
                | VmInstruction::SuperLoadOr { .. }
                | VmInstruction::SuperLoadSub { .. }
                | VmInstruction::SuperLoadXor { .. }
                | VmInstruction::Mov { .. }
                | VmInstruction::Bin { .. }
                | VmInstruction::IntUnary { .. }
                | VmInstruction::IntTernary { .. }
                | VmInstruction::IntOverflow { .. }
                | VmInstruction::Icmp { .. }
                | VmInstruction::FloatBin { .. }
                | VmInstruction::FloatIntBin { .. }
                | VmInstruction::FloatUnary { .. }
                | VmInstruction::FloatTernary { .. }
                | VmInstruction::FloatCast { .. }
                | VmInstruction::FloatRoundToInt { .. }
                | VmInstruction::Fcmp { .. }
                | VmInstruction::FloatClass { .. }
                | VmInstruction::Cast { .. }
                | VmInstruction::Alloca { .. }
                | VmInstruction::DynamicAlloca { .. }
                | VmInstruction::Load { .. }
                | VmInstruction::Store { .. }
                | VmInstruction::VolatileLoad { .. }
                | VmInstruction::VolatileStore { .. }
                | VmInstruction::MemcpyDynamic { .. }
                | VmInstruction::MemmoveDynamic { .. }
                | VmInstruction::MemsetDynamic { .. }
                | VmInstruction::VolatileMemcpyDynamic { .. }
                | VmInstruction::VolatileMemmoveDynamic { .. }
                | VmInstruction::VolatileMemsetDynamic { .. }
                | VmInstruction::AtomicLoad { .. }
                | VmInstruction::AtomicStore { .. }
                | VmInstruction::VolatileAtomicLoad { .. }
                | VmInstruction::VolatileAtomicStore { .. }
                | VmInstruction::AtomicRmw { .. }
                | VmInstruction::VolatileAtomicRmw { .. }
                | VmInstruction::CmpXchg { .. }
                | VmInstruction::VolatileCmpXchg { .. }
                | VmInstruction::Fence { .. }
                | VmInstruction::Gep { .. }
                | VmInstruction::CallNative { .. }
                | VmInstruction::SideEffect
                | VmInstruction::Nop
                | VmInstruction::VmRet
                | VmInstruction::Unreachable
                | VmInstruction::Trap
                | VmInstruction::Ret { .. }
                | VmInstruction::RetVoid => Vec::new(),
            })
            .collect()
    }
}
