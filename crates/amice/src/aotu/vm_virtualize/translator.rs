//! LLVM IR 到 AMICE VM IR 的 lowering 层。
//!
//! # 读代码入口
//! `translate_function` 先抽取函数签名，再构造 `FunctionLowerer`。`FunctionLowerer::lower`
//! 按 basic block 顺序绑定 VM label，然后逐条 LLVM instruction 进入 `lower_*` 方法。
//!
//! # profile 驱动点
//! 这里不直接写死 opcode。每个 `lower_*` 方法只选择一个 lowering contract，例如
//! `llvm.add.integer` 或 `llvm.memory.scalar`，再执行 `lowering.vm` 中对应 action。
//! action 里的 `emit` 会保留 profile ISA 指令名，使后续 encoder 能选择 profile 声明的
//! opcode alias 和 operand 顺序。
//!
//! # safe-skip 契约
//! 此文件中遇到暂不支持的 LLVM IR 会返回 `Err`。上层 pass 捕获错误并保留原函数，
//! 因而这里宁可保守跳过，也不能生成不完整 VM IR。

use amice_llvm::inkwell2::{CallInst, FunctionExt, GepInst, InstructionExt, ModuleExt, PhiInst, SwitchInst};
use amice_plugin::inkwell::basic_block::BasicBlock;
use amice_plugin::inkwell::llvm_sys::core::{
    LLVMConstIntGetSExtValue, LLVMCountStructElementTypes, LLVMGetAlignment, LLVMGetAllocatedType,
    LLVMGetAtomicSyncScopeID, LLVMGetCalledFunctionType, LLVMGetCalledValue, LLVMGetCmpXchgFailureOrdering,
    LLVMGetCmpXchgSuccessOrdering, LLVMGetConstOpcode, LLVMGetElementType, LLVMGetGEPSourceElementType,
    LLVMGetNumOperands, LLVMGetOperand, LLVMGetTypeKind, LLVMGlobalGetValueType, LLVMIsAAllocaInst, LLVMIsAConstant,
    LLVMIsAConstantExpr, LLVMIsAConstantInt, LLVMIsAGetElementPtrInst, LLVMIsAGlobalValue, LLVMIsAGlobalVariable,
    LLVMStructGetTypeAtIndex, LLVMTypeOf,
};
use amice_plugin::inkwell::llvm_sys::prelude::{LLVMTypeRef, LLVMValueRef};
use amice_plugin::inkwell::llvm_sys::target::{LLVMOffsetOfElement, LLVMStoreSizeOfType};
use amice_plugin::inkwell::llvm_sys::{LLVMOpcode, LLVMTypeKind};
use amice_plugin::inkwell::module::{Linkage, Module};
use amice_plugin::inkwell::targets::TargetData;
use amice_plugin::inkwell::types::{
    AnyTypeEnum, AsTypeRef, BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType,
};
use amice_plugin::inkwell::values::{
    AnyValue, AsValueRef, BasicMetadataValueEnum, BasicValueEnum, FunctionValue, InstructionOpcode, InstructionValue,
    PointerValue, UnnamedAddress,
};
use amice_plugin::inkwell::{
    AddressSpace, AtomicOrdering, AtomicRMWBinOp, FloatPredicate as LlvmFloatPredicate, IntPredicate,
};
use amice_vm::abi::{AbiProfile, VmRegister};
use amice_vm::isa::{
    AtomicRmwOp, BinOp, CastOp, CmpPredicate, CounterKind, FloatBinOp, FloatCastOp, FloatPredicate as VmFloatPredicate,
    FloatTernaryOp, FloatUnaryOp, HandlerSemantic, InstructionDesc, IntOverflowOp, IntTernaryOp, IntUnaryOp,
    IsaProfile, MemoryOrdering, OperandKind,
};
use amice_vm::profile::{LoweringAction, LoweringProfile, LoweringRule, lowering_match_pattern};
use amice_vm::{
    LabelId, NATIVE_CALL_MAX_ARGS, NATIVE_CALL_MAX_RETURNS, NativeReturn, VmFunction, VmFunctionBuilder, VmInstruction,
    fuse_superinstructions,
};
use anyhow::{Context, bail};
use std::collections::{HashMap, HashSet};

type ValueKey = usize;
type BlockKey = usize;

const MAX_MEMORY_INTRINSIC_INLINE_BYTES: u64 = 64;
const LLVM_SYSTEM_SYNC_SCOPE_ID: u32 = 1;
const FPCLASS_ALL_FLAGS: u64 = 0x03ff;

#[derive(Debug, Clone, Copy)]
struct ValueBinding {
    // VM x 寄存器编号。
    reg: u8,
    // 该寄存器当前承载的 LLVM 标量位宽；runtime 统一用 i64 存储，handler 按 width 截断。
    width: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    // 使用 x 寄存器保存普通整数。
    Integer,
    // 使用 x 寄存器保存指针地址。
    Pointer,
    // 使用 x 寄存器保存 f32/f64 的原始 bit。
    Float,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryIntrinsicKind {
    Memcpy,
    Memmove,
    Memset,
}

#[derive(Debug, Clone, Copy)]
enum TrapIntrinsicKind {
    Trap,
    DebugTrap,
    UbsanTrap,
}

impl TrapIntrinsicKind {
    fn validate(self, instruction: InstructionValue<'_>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        match self {
            Self::Trap | Self::DebugTrap => {
                if actual_args != 0 {
                    bail!("{self:?} expects exactly 0 arguments, got {actual_args}");
                }
            },
            Self::UbsanTrap => {
                if actual_args != 1 {
                    bail!("llvm.ubsantrap expects exactly 1 argument, got {actual_args}");
                }
                let _ = constant_int_operand(instruction, 0, "llvm.ubsantrap trap kind")?;
            },
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum NopIntrinsicKind {
    LifetimeStart,
    LifetimeEnd,
    InvariantEnd,
    Prefetch,
    NoAliasScopeDecl,
    DoNothing,
    Assume,
    Debug,
    VarAnnotation,
    CodeViewAnnotation,
}

impl NopIntrinsicKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::LifetimeStart => "llvm.lifetime.start",
            Self::LifetimeEnd => "llvm.lifetime.end",
            Self::InvariantEnd => "llvm.invariant.end",
            Self::Prefetch => "llvm.prefetch",
            Self::NoAliasScopeDecl => "llvm.experimental.noalias.scope.decl",
            Self::DoNothing => "llvm.donothing",
            Self::Assume => "llvm.assume",
            Self::Debug => "llvm.dbg.nop",
            Self::VarAnnotation => "llvm.var.annotation",
            Self::CodeViewAnnotation => "llvm.codeview.annotation",
        }
    }

    fn checked_arg_count(self) -> Option<u32> {
        match self {
            Self::LifetimeStart | Self::LifetimeEnd => Some(2),
            Self::InvariantEnd => Some(3),
            Self::Prefetch => Some(4),
            Self::DoNothing => Some(0),
            Self::Assume => Some(1),
            Self::Debug | Self::NoAliasScopeDecl | Self::CodeViewAnnotation => None,
            Self::VarAnnotation => Some(5),
        }
    }

    fn constant_operand_indices(self) -> &'static [u32] {
        match self {
            Self::LifetimeStart | Self::LifetimeEnd => &[0],
            Self::InvariantEnd => &[1],
            Self::Prefetch => &[1, 2, 3],
            Self::Assume
            | Self::Debug
            | Self::NoAliasScopeDecl
            | Self::DoNothing
            | Self::VarAnnotation
            | Self::CodeViewAnnotation => &[],
        }
    }

    fn pointer_operand_indices(self) -> &'static [u32] {
        match self {
            Self::Prefetch => &[0],
            Self::LifetimeStart
            | Self::LifetimeEnd
            | Self::InvariantEnd
            | Self::NoAliasScopeDecl
            | Self::DoNothing
            | Self::Assume
            | Self::Debug
            | Self::VarAnnotation
            | Self::CodeViewAnnotation => &[],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityIntrinsicKind {
    Expect,
    ExpectWithProbability,
    SsaCopyScalar,
    LaunderInvariantGroup,
    StripInvariantGroup,
    InvariantStart,
    AnnotationInteger,
    PtrAnnotationPointer,
    ThreadLocalAddress,
}

impl IdentityIntrinsicKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Expect => "llvm.expect.integer",
            Self::ExpectWithProbability => "llvm.expect.with_probability.integer",
            Self::SsaCopyScalar => "llvm.ssa.copy.scalar",
            Self::LaunderInvariantGroup => "llvm.launder.invariant.group.pointer",
            Self::StripInvariantGroup => "llvm.strip.invariant.group.pointer",
            Self::InvariantStart => "llvm.invariant.start.pointer",
            Self::AnnotationInteger => "llvm.annotation.integer",
            Self::PtrAnnotationPointer => "llvm.ptr.annotation.pointer",
            Self::ThreadLocalAddress => "llvm.threadlocal.address.pointer",
        }
    }

    fn arg_count(self) -> u32 {
        match self {
            Self::SsaCopyScalar => 1,
            Self::Expect => 2,
            Self::ExpectWithProbability => 3,
            Self::LaunderInvariantGroup | Self::StripInvariantGroup => 1,
            Self::InvariantStart => 2,
            Self::AnnotationInteger => 4,
            Self::PtrAnnotationPointer => 5,
            Self::ThreadLocalAddress => 1,
        }
    }

    fn value_operand_index(self) -> u32 {
        match self {
            Self::SsaCopyScalar => 0,
            Self::InvariantStart => 1,
            Self::Expect
            | Self::ExpectWithProbability
            | Self::LaunderInvariantGroup
            | Self::StripInvariantGroup
            | Self::AnnotationInteger
            | Self::PtrAnnotationPointer
            | Self::ThreadLocalAddress => 0,
        }
    }

    fn constant_length_operand_index(self) -> Option<u32> {
        match self {
            Self::InvariantStart => Some(0),
            Self::Expect
            | Self::ExpectWithProbability
            | Self::SsaCopyScalar
            | Self::LaunderInvariantGroup
            | Self::StripInvariantGroup
            | Self::AnnotationInteger
            | Self::PtrAnnotationPointer
            | Self::ThreadLocalAddress => None,
        }
    }

    fn is_expect_hint(self) -> bool {
        matches!(self, Self::Expect | Self::ExpectWithProbability)
    }

    fn is_pointer_identity(self) -> bool {
        matches!(
            self,
            Self::LaunderInvariantGroup
                | Self::StripInvariantGroup
                | Self::InvariantStart
                | Self::PtrAnnotationPointer
                | Self::ThreadLocalAddress
        )
    }

    fn is_integer_identity(self) -> bool {
        matches!(self, Self::AnnotationInteger)
    }

    fn is_scalar_copy(self) -> bool {
        matches!(self, Self::SsaCopyScalar)
    }
}

#[derive(Debug, Clone, Copy)]
enum PointerIntrinsicKind {
    PtrMask,
}

impl PointerIntrinsicKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::PtrMask => "llvm.ptrmask.pointer",
        }
    }

    fn arg_count(self) -> u32 {
        match self {
            Self::PtrMask => 2,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CompileTimeIntrinsicKind {
    IsConstant,
    ObjectSize,
}

impl CompileTimeIntrinsicKind {
    fn result(self, value: BasicValueEnum<'_>) -> anyhow::Result<u64> {
        match self {
            Self::IsConstant => {
                if is_undef_or_poison_value(value) {
                    bail!("llvm.is.constant operand cannot be undef or poison");
                }
                let _ = value_width(value).context("llvm.is.constant only supports scalar operands")?;
                // SAFETY: `value` 属于当前 live LLVM module。这里仅查询 Value 的常量分类，
                // 不读取用户内存，也不会把运行时值当作编译期常量折叠。
                Ok(u64::from(!unsafe { LLVMIsAConstant(value.as_value_ref()) }.is_null()))
            },
            Self::ObjectSize => bail!("llvm.objectsize needs target data and is handled by lower_objectsize_intrinsic"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum FloatIntrinsicKind {
    FAbs,
    Sqrt,
    Canonicalize,
    Floor,
    Ceil,
    Trunc,
    Rint,
    NearbyInt,
    Round,
    RoundEven,
    Fma,
    FmulAdd,
    MinNum,
    MaxNum,
    Minimum,
    Maximum,
    CopySign,
    IsFpClass,
}

impl FloatIntrinsicKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::FAbs => "llvm.fabs.float",
            Self::Sqrt => "llvm.sqrt.float",
            Self::Canonicalize => "llvm.canonicalize.float",
            Self::Floor => "llvm.floor.float",
            Self::Ceil => "llvm.ceil.float",
            Self::Trunc => "llvm.trunc.float",
            Self::Rint => "llvm.rint.float",
            Self::NearbyInt => "llvm.nearbyint.float",
            Self::Round => "llvm.round.float",
            Self::RoundEven => "llvm.roundeven.float",
            Self::Fma => "llvm.fma.float",
            Self::FmulAdd => "llvm.fmuladd.float",
            Self::MinNum => "llvm.minnum.float",
            Self::MaxNum => "llvm.maxnum.float",
            Self::Minimum => "llvm.minimum.float",
            Self::Maximum => "llvm.maximum.float",
            Self::CopySign => "llvm.copysign.float",
            Self::IsFpClass => "llvm.is.fpclass.float",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        match self {
            Self::FAbs => HandlerSemantic::FloatUnary(FloatUnaryOp::Abs),
            Self::Sqrt => HandlerSemantic::FloatUnary(FloatUnaryOp::Sqrt),
            Self::Canonicalize => HandlerSemantic::FloatUnary(FloatUnaryOp::Canonicalize),
            Self::Floor => HandlerSemantic::FloatUnary(FloatUnaryOp::Floor),
            Self::Ceil => HandlerSemantic::FloatUnary(FloatUnaryOp::Ceil),
            Self::Trunc => HandlerSemantic::FloatUnary(FloatUnaryOp::Trunc),
            Self::Rint => HandlerSemantic::FloatUnary(FloatUnaryOp::Rint),
            Self::NearbyInt => HandlerSemantic::FloatUnary(FloatUnaryOp::NearbyInt),
            Self::Round => HandlerSemantic::FloatUnary(FloatUnaryOp::Round),
            Self::RoundEven => HandlerSemantic::FloatUnary(FloatUnaryOp::RoundEven),
            Self::Fma => HandlerSemantic::FloatTernary(FloatTernaryOp::Fma),
            Self::FmulAdd => HandlerSemantic::FloatTernary(FloatTernaryOp::MulAdd),
            Self::MinNum => HandlerSemantic::FloatBin(FloatBinOp::MinNum),
            Self::MaxNum => HandlerSemantic::FloatBin(FloatBinOp::MaxNum),
            Self::Minimum => HandlerSemantic::FloatBin(FloatBinOp::Minimum),
            Self::Maximum => HandlerSemantic::FloatBin(FloatBinOp::Maximum),
            Self::CopySign => HandlerSemantic::FloatBin(FloatBinOp::CopySign),
            Self::IsFpClass => HandlerSemantic::FloatClass,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum IntegerIntrinsicKind {
    CtPop,
    CtLz,
    CtTz,
    Abs,
    SMax,
    SMin,
    UMax,
    UMin,
    UAddSat,
    USubSat,
    SAddSat,
    SSubSat,
    UShlSat,
    SShlSat,
    UAddOverflow,
    SAddOverflow,
    USubOverflow,
    SSubOverflow,
    UMulOverflow,
    SMulOverflow,
    BSwap,
    BitReverse,
    FShl,
    FShr,
}

impl IntegerIntrinsicKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::CtPop => "llvm.ctpop.integer",
            Self::CtLz => "llvm.ctlz.integer",
            Self::CtTz => "llvm.cttz.integer",
            Self::Abs => "llvm.abs.integer",
            Self::SMax => "llvm.smax.integer",
            Self::SMin => "llvm.smin.integer",
            Self::UMax => "llvm.umax.integer",
            Self::UMin => "llvm.umin.integer",
            Self::UAddSat => "llvm.uadd.sat.integer",
            Self::USubSat => "llvm.usub.sat.integer",
            Self::SAddSat => "llvm.sadd.sat.integer",
            Self::SSubSat => "llvm.ssub.sat.integer",
            Self::UShlSat => "llvm.ushl.sat.integer",
            Self::SShlSat => "llvm.sshl.sat.integer",
            Self::UAddOverflow => "llvm.uadd.with.overflow.integer",
            Self::SAddOverflow => "llvm.sadd.with.overflow.integer",
            Self::USubOverflow => "llvm.usub.with.overflow.integer",
            Self::SSubOverflow => "llvm.ssub.with.overflow.integer",
            Self::UMulOverflow => "llvm.umul.with.overflow.integer",
            Self::SMulOverflow => "llvm.smul.with.overflow.integer",
            Self::BSwap => "llvm.bswap.integer",
            Self::BitReverse => "llvm.bitreverse.integer",
            Self::FShl => "llvm.fshl.integer",
            Self::FShr => "llvm.fshr.integer",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        match self {
            Self::CtPop => HandlerSemantic::IntUnary(IntUnaryOp::CtPop),
            Self::CtLz => HandlerSemantic::IntUnary(IntUnaryOp::CtLz),
            Self::CtTz => HandlerSemantic::IntUnary(IntUnaryOp::CtTz),
            Self::Abs => HandlerSemantic::IntUnary(IntUnaryOp::Abs),
            Self::SMax => HandlerSemantic::Bin(BinOp::SMax),
            Self::SMin => HandlerSemantic::Bin(BinOp::SMin),
            Self::UMax => HandlerSemantic::Bin(BinOp::UMax),
            Self::UMin => HandlerSemantic::Bin(BinOp::UMin),
            Self::UAddSat => HandlerSemantic::Bin(BinOp::UAddSat),
            Self::USubSat => HandlerSemantic::Bin(BinOp::USubSat),
            Self::SAddSat => HandlerSemantic::Bin(BinOp::SAddSat),
            Self::SSubSat => HandlerSemantic::Bin(BinOp::SSubSat),
            Self::UShlSat => HandlerSemantic::Bin(BinOp::UShlSat),
            Self::SShlSat => HandlerSemantic::Bin(BinOp::SShlSat),
            Self::UAddOverflow => HandlerSemantic::IntOverflow(IntOverflowOp::UAdd),
            Self::SAddOverflow => HandlerSemantic::IntOverflow(IntOverflowOp::SAdd),
            Self::USubOverflow => HandlerSemantic::IntOverflow(IntOverflowOp::USub),
            Self::SSubOverflow => HandlerSemantic::IntOverflow(IntOverflowOp::SSub),
            Self::UMulOverflow => HandlerSemantic::IntOverflow(IntOverflowOp::UMul),
            Self::SMulOverflow => HandlerSemantic::IntOverflow(IntOverflowOp::SMul),
            Self::BSwap => HandlerSemantic::IntUnary(IntUnaryOp::BSwap),
            Self::BitReverse => HandlerSemantic::IntUnary(IntUnaryOp::BitReverse),
            Self::FShl => HandlerSemantic::IntTernary(IntTernaryOp::FShl),
            Self::FShr => HandlerSemantic::IntTernary(IntTernaryOp::FShr),
        }
    }

    fn arity(self) -> u32 {
        match self {
            Self::CtPop | Self::BSwap | Self::BitReverse => 1,
            Self::CtLz
            | Self::CtTz
            | Self::Abs
            | Self::SMax
            | Self::SMin
            | Self::UMax
            | Self::UMin
            | Self::UAddSat
            | Self::USubSat
            | Self::SAddSat
            | Self::SSubSat
            | Self::UShlSat
            | Self::SShlSat
            | Self::UAddOverflow
            | Self::SAddOverflow
            | Self::USubOverflow
            | Self::SSubOverflow
            | Self::UMulOverflow
            | Self::SMulOverflow => 2,
            Self::FShl | Self::FShr => 3,
        }
    }

    fn is_binary_intrinsic(self) -> bool {
        matches!(
            self,
            Self::SMax
                | Self::SMin
                | Self::UMax
                | Self::UMin
                | Self::UAddSat
                | Self::USubSat
                | Self::SAddSat
                | Self::SSubSat
                | Self::UShlSat
                | Self::SShlSat
        )
    }

    fn is_overflow_intrinsic(self) -> bool {
        matches!(
            self,
            Self::UAddOverflow
                | Self::SAddOverflow
                | Self::USubOverflow
                | Self::SSubOverflow
                | Self::UMulOverflow
                | Self::SMulOverflow
        )
    }
}

impl MemoryIntrinsicKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Memcpy => "llvm.memcpy.fixed",
            Self::Memmove => "llvm.memmove.fixed",
            Self::Memset => "llvm.memset.fixed",
        }
    }

    fn dynamic_lowering_rule(self, volatile: bool) -> &'static str {
        match (self, volatile) {
            (Self::Memcpy, false) => "llvm.memcpy.dynamic",
            (Self::Memmove, false) => "llvm.memmove.dynamic",
            (Self::Memset, false) => "llvm.memset.dynamic",
            (Self::Memcpy, true) => "llvm.volatile.memcpy.dynamic",
            (Self::Memmove, true) => "llvm.volatile.memmove.dynamic",
            (Self::Memset, true) => "llvm.volatile.memset.dynamic",
        }
    }

    fn dynamic_semantic(self, volatile: bool) -> HandlerSemantic {
        match (self, volatile) {
            (Self::Memcpy, false) => HandlerSemantic::MemcpyDynamic,
            (Self::Memmove, false) => HandlerSemantic::MemmoveDynamic,
            (Self::Memset, false) => HandlerSemantic::MemsetDynamic,
            (Self::Memcpy, true) => HandlerSemantic::VolatileMemcpyDynamic,
            (Self::Memmove, true) => HandlerSemantic::VolatileMemmoveDynamic,
            (Self::Memset, true) => HandlerSemantic::VolatileMemsetDynamic,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MemoryChunk {
    offset: u64,
    width: u8,
}

#[derive(Debug, Clone, Copy)]
struct LoadedMemoryChunk {
    chunk: MemoryChunk,
    value: ValueBinding,
}

#[derive(Debug, Clone, Copy)]
struct AggregateMemoryField {
    offset: u64,
    info: ReturnField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReturnField {
    // aggregate/native return 字段的标量位宽。
    pub width: u8,
    // 字段在 wrapper/thunk 边界的标量类别。
    pub kind: ScalarKind,
}

#[derive(Debug)]
pub struct FunctionSignature {
    // 标量返回快捷路径使用的位宽；void 时保留为 64，实际不读取。
    pub return_width: u8,
    // 宿主参数按 leaf scalar 展平后映射到 VM ABI 时使用的位宽。
    pub param_widths: Vec<u8>,
    pub returns_void: bool,
    pub return_is_pointer: bool,
    pub return_is_float: bool,
    // 每个原始宿主参数在扁平 VM 参数槽中的位置；direct aggregate 参数会占用多个槽。
    pub params: Vec<FunctionParamSlots>,
    // 非空表示直接 struct/array aggregate return；字段会通过 wrapper ret_slots 重建。
    pub aggregate_return_fields: Vec<ReturnField>,
}

impl FunctionSignature {
    pub fn return_slot_count(&self) -> usize {
        if self.returns_void {
            0
        } else {
            self.aggregate_return_fields.len().max(1)
        }
    }

    pub fn has_aggregate_return(&self) -> bool {
        !self.aggregate_return_fields.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct FunctionParamSlots {
    pub start: usize,
    pub fields: Vec<ReturnField>,
}

#[derive(Debug, Clone)]
pub struct NativeCallTarget<'ctx> {
    // VM 内部 call_native 最终要调用的真实 LLVM 函数。
    pub function: FunctionValue<'ctx>,
    // thunk 重建真实 LLVM call 参数时使用的 call-site 参数类型；varargs callee 需要包含实际变参。
    pub arg_types: Vec<BasicMetadataTypeEnum<'ctx>>,
    // native call 参数按 leaf scalar 展平后映射到 VM ABI 时使用的位宽。
    pub param_widths: Vec<u8>,
    // 每个原始 native 参数在扁平 VM 参数槽中的位置；aggregate 参数会占用多个槽。
    pub params: Vec<FunctionParamSlots>,
    pub returns_void: bool,
    // thunk 统一返回固定 i64 tuple；这里描述哪些 tuple slot 有效以及如何截断。
    pub return_fields: Vec<ReturnField>,
}

#[derive(Debug)]
pub struct VmTranslation<'ctx> {
    // 已完成 lowering 的 VM IR，仍未经过 bytecode encoder。
    pub function: VmFunction,
    // wrapper 改写需要的宿主 ABI 视图。
    pub signature: FunctionSignature,
    // bytecode 中 call_native 指令引用的 native thunk 目标。
    pub native_calls: Vec<NativeCallTarget<'ctx>>,
}

pub fn supported_signature(function: FunctionValue<'_>) -> anyhow::Result<FunctionSignature> {
    let fn_type = function.get_type();
    // signature 和 instruction lowering 里的每个 `bail!` 都是 safe-skip 契约的一部分：
    // module pass 会捕获错误、以 debug 级别记录原因，并保持原函数体不变，而不是生成半虚拟化 IR。
    if fn_type.is_var_arg() {
        bail!("varargs functions are not supported");
    }

    let mut aggregate_return_fields = Vec::new();
    let (returns_void, return_width, return_kind) = match fn_type.get_return_type() {
        None => (true, 64, ScalarKind::Integer),
        Some(BasicTypeEnum::IntType(return_type)) => {
            (false, checked_width(return_type.get_bit_width())?, ScalarKind::Integer)
        },
        Some(BasicTypeEnum::PointerType(_)) => (false, 64, ScalarKind::Pointer),
        Some(BasicTypeEnum::FloatType(return_type)) => {
            (false, float_type_width(return_type.as_type_ref())?, ScalarKind::Float)
        },
        Some(BasicTypeEnum::StructType(return_type)) => {
            aggregate_return_fields =
                return_fields_from_aggregate_type(BasicTypeEnum::StructType(return_type)).context("return fields")?;
            if aggregate_return_fields.is_empty() {
                bail!("empty aggregate returns are not supported");
            }
            (false, aggregate_return_fields[0].width, ScalarKind::Integer)
        },
        Some(BasicTypeEnum::ArrayType(return_type)) => {
            aggregate_return_fields =
                return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(return_type)).context("return fields")?;
            if aggregate_return_fields.is_empty() {
                bail!("empty aggregate returns are not supported");
            }
            (false, aggregate_return_fields[0].width, ScalarKind::Integer)
        },
        Some(_) => {
            bail!("only void, scalar integer, pointer, float, and direct struct/array aggregate returns are supported")
        },
    };

    let param_types = fn_type.get_param_types();
    let mut param_widths = Vec::new();
    let mut params = Vec::with_capacity(param_types.len());
    for (index, ty) in param_types.iter().enumerate() {
        let start = param_widths.len();
        let fields = match ty {
            BasicMetadataTypeEnum::IntType(int_ty) => vec![ReturnField {
                width: checked_width(int_ty.get_bit_width())?,
                kind: ScalarKind::Integer,
            }],
            BasicMetadataTypeEnum::PointerType(_) => vec![ReturnField {
                width: 64,
                kind: ScalarKind::Pointer,
            }],
            BasicMetadataTypeEnum::FloatType(float_ty) => vec![ReturnField {
                width: float_type_width(float_ty.as_type_ref())?,
                kind: ScalarKind::Float,
            }],
            BasicMetadataTypeEnum::StructType(struct_ty) => {
                let fields = return_fields_from_aggregate_type(BasicTypeEnum::StructType(*struct_ty))
                    .with_context(|| format!("aggregate parameter {index} fields"))?;
                if fields.is_empty() {
                    bail!("empty aggregate parameter {index} is not supported");
                }
                fields
            },
            BasicMetadataTypeEnum::ArrayType(array_ty) => {
                let fields = return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(*array_ty))
                    .with_context(|| format!("aggregate parameter {index} fields"))?;
                if fields.is_empty() {
                    bail!("empty aggregate parameter {index} is not supported");
                }
                fields
            },
            _ => {
                bail!("only scalar integer, pointer, float, and direct struct/array aggregate parameters are supported")
            },
        };
        for field in &fields {
            param_widths.push(field.width);
        }
        params.push(FunctionParamSlots { start, fields });
    }

    if param_widths.len() > 8 {
        bail!(
            "only up to 8 flattened scalar integer/pointer/float parameter slots are supported, got {}",
            param_widths.len()
        );
    }

    Ok(FunctionSignature {
        return_width,
        param_widths,
        returns_void,
        return_is_pointer: return_kind == ScalarKind::Pointer,
        return_is_float: return_kind == ScalarKind::Float,
        params,
        aggregate_return_fields,
    })
}

pub fn translate_function<'ctx>(
    module: &mut Module<'ctx>,
    function: FunctionValue<'ctx>,
    abi: &AbiProfile,
    lowering: &LoweringProfile,
    isa: &IsaProfile,
) -> anyhow::Result<VmTranslation<'ctx>> {
    let signature = supported_signature(function)?;
    let name = function.get_name().to_str().unwrap_or("<invalid-name>").to_owned();

    let lowerer = FunctionLowerer::new(module, function, &name, &signature, abi, lowering, isa)?;
    let (function, native_calls) = lowerer.lower()?;
    Ok(VmTranslation {
        function,
        signature,
        native_calls,
    })
}

struct FunctionLowerer<'m, 'ctx, 'profile> {
    module: &'m mut Module<'ctx>,
    function: FunctionValue<'ctx>,
    // lowering.vm 的规则表。translator 通过 contract 名称查找规则，不从 LLVM opcode 直接写死 VM 指令。
    lowering: &'profile LoweringProfile,
    // isa.vm 的指令表。emit 阶段用它验证 profile operand 名称、kind 和 semantic。
    isa: &'profile IsaProfile,
    target_data: TargetData,
    builder: VmFunctionBuilder,
    // LLVM SSA value 到 VM x 寄存器的当前绑定。
    values: HashMap<ValueKey, ValueBinding>,
    // insertvalue/extractvalue 和多返回 native call 使用的临时 aggregate 绑定。
    aggregates: HashMap<ValueKey, AggregateBinding>,
    // aggregate binding 可能在 insertvalue 链中共享字段寄存器；引用计数归零后才能释放。
    aggregate_reg_refs: HashMap<u8, usize>,
    // LLVM basic block 到 VM bytecode label 的映射。
    labels: HashMap<BlockKey, LabelId>,
    native_calls: Vec<NativeCallTarget<'ctx>>,
    return_registers: Vec<u8>,
    native_arg_registers: Vec<u8>,
    native_return_registers: Vec<u8>,
    native_touched_registers: HashSet<u8>,
    aggregate_return_fields: Vec<ReturnField>,
    // 已经真正 materialize 到 VM 寄存器的 SSA value。native_call 保存 clobber 时依赖这个集合，
    // 避免保存尚未定义的虚拟值。
    defined_values: HashSet<ValueKey>,
    // 简单 liveness 复用计划。跨 block 值会 pinned，block-local 临时值在最后一次使用后释放。
    reuse_plan: Option<ReusePlan>,
    // 单条 lowering 过程中申请的 scratch 寄存器，指令结束后统一释放。
    temporary_regs: Vec<u8>,
}

#[derive(Debug, Clone)]
struct AggregateBinding {
    fields: Vec<Option<AggregateField>>,
}

#[derive(Debug, Clone, Copy)]
struct AggregateField {
    binding: ValueBinding,
    owned: bool,
}

impl AggregateField {
    fn owned(binding: ValueBinding) -> Self {
        Self { binding, owned: true }
    }

    fn borrowed(binding: ValueBinding) -> Self {
        Self { binding, owned: false }
    }
}

#[derive(Debug, Clone)]
struct AggregateSelection {
    start: usize,
    fields: Vec<ReturnField>,
    is_aggregate: bool,
}

#[derive(Debug, Clone, Copy)]
struct RegisterMove {
    dst: u8,
    src: ValueBinding,
}

#[derive(Debug, Clone)]
struct SelectLoweringActions {
    br_if: LoweringAction,
    then_mov: LoweringAction,
    br: LoweringAction,
    else_mov: LoweringAction,
}

#[derive(Debug)]
struct ReusePlan {
    pinned_values: HashSet<ValueKey>,
    pinned_aggregate_values: HashSet<ValueKey>,
    block_last_uses: HashMap<BlockKey, HashMap<ValueKey, usize>>,
    block_last_aggregate_uses: HashMap<BlockKey, HashMap<ValueKey, usize>>,
    current_block: Option<BlockKey>,
    instruction_index: usize,
}

#[derive(Debug, Clone, Copy)]
enum LoweringValue {
    Reg(ValueBinding),
    Imm(u64),
    Label(LabelId),
}

#[derive(Debug, Default)]
struct LoweringEnv<'ctx> {
    // lowering.vm action 可引用的 VM 侧值，例如 `%va`、`%vr`、`type_width(%r)`。
    values: HashMap<String, LoweringValue>,
    // bind action 需要把 profile 里的 LLVM placeholder 反查到真实 SSA value。
    llvm_value_keys: HashMap<String, ValueKey>,
    // materialize action 可以从这里把 LLVM operand 拉进 VM 寄存器。
    llvm_sources: HashMap<String, BasicValueEnum<'ctx>>,
}

impl<'ctx> LoweringEnv<'ctx> {
    fn new() -> Self {
        Self::default()
    }

    fn binding(mut self, name: impl Into<String>, binding: ValueBinding) -> Self {
        self.values.insert(name.into(), LoweringValue::Reg(binding));
        self
    }

    fn llvm_value(mut self, name: impl Into<String>, key: ValueKey) -> Self {
        self.llvm_value_keys.insert(name.into(), key);
        self
    }

    fn llvm_source(mut self, name: impl Into<String>, value: BasicValueEnum<'ctx>) -> Self {
        self.llvm_sources.insert(name.into(), value);
        self
    }

    fn reg(mut self, name: impl Into<String>, reg: u8, width: u8) -> Self {
        self.values
            .insert(name.into(), LoweringValue::Reg(ValueBinding { reg, width }));
        self
    }

    fn imm(mut self, name: impl Into<String>, value: u64) -> Self {
        self.values.insert(name.into(), LoweringValue::Imm(value));
        self
    }

    fn label(mut self, name: impl Into<String>, label: LabelId) -> Self {
        self.values.insert(name.into(), LoweringValue::Label(label));
        self
    }

    fn get(&self, expr: &str) -> anyhow::Result<LoweringValue> {
        let expr = expr.trim();
        if let Some(value) = self.values.get(expr).copied() {
            return Ok(value);
        }
        if let Some(index) = expr.strip_prefix('x').and_then(|value| value.parse::<u8>().ok()) {
            return Ok(LoweringValue::Reg(ValueBinding { reg: index, width: 64 }));
        }
        if let Some(value) = parse_u64_literal(expr) {
            return Ok(LoweringValue::Imm(value));
        }
        bail!("lowering expression {expr} is not available in this translator context")
    }

    fn insert(&mut self, name: impl Into<String>, value: LoweringValue) {
        self.values.insert(name.into(), value);
    }

    fn llvm_key(&self, name: &str) -> Option<ValueKey> {
        self.llvm_value_keys.get(name).copied()
    }

    fn llvm_source_value(&self, name: &str) -> Option<BasicValueEnum<'ctx>> {
        self.llvm_sources.get(name).copied()
    }
}

#[derive(Debug, Default)]
struct ProfileInstructionArgs {
    values: HashMap<String, LoweringValue>,
}

impl ProfileInstructionArgs {
    fn from_emit(desc: &InstructionDesc, operands: &[(String, String)], env: &LoweringEnv<'_>) -> anyhow::Result<Self> {
        let mut values = HashMap::with_capacity(desc.operand_descs.len());
        for operand in &desc.operand_descs {
            let value = if let Some((_, expr)) = operands.iter().find(|(name, _)| name == &operand.name) {
                env.get(expr)?
            } else {
                env.get(&operand.name).with_context(|| {
                    format!(
                        "emit {} misses operand {} and translator context has no default",
                        desc.name, operand.name
                    )
                })?
            };
            values.insert(operand.name.clone(), value);
        }
        Ok(Self { values })
    }

    fn from_values(values: impl IntoIterator<Item = (String, LoweringValue)>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }

    fn raw(&self, name: &str) -> anyhow::Result<LoweringValue> {
        self.values
            .get(name)
            .copied()
            .with_context(|| format!("profile instruction operand {name} is missing"))
    }

    fn imm(&self, name: &str) -> anyhow::Result<u64> {
        match self.raw(name)? {
            LoweringValue::Imm(value) => Ok(value),
            LoweringValue::Reg(binding) => Ok(binding.reg as u64),
            LoweringValue::Label(_) => bail!("profile instruction operand {name} expected an immediate"),
        }
    }

    fn label(&self, name: &str) -> anyhow::Result<LabelId> {
        match self.raw(name)? {
            LoweringValue::Label(label) => Ok(label),
            LoweringValue::Reg(_) | LoweringValue::Imm(_) => {
                bail!("profile instruction operand {name} expected a label")
            },
        }
    }
}

fn operands_match_shape(operands: &[(String, String)], required: &[(&str, &str)]) -> bool {
    required.iter().all(|(name, expr)| {
        operands
            .iter()
            .any(|(actual_name, actual_expr)| actual_name == name && actual_expr == expr)
    })
}

impl<'m, 'ctx, 'profile> FunctionLowerer<'m, 'ctx, 'profile> {
    fn new(
        module: &'m mut Module<'ctx>,
        function: FunctionValue<'ctx>,
        name: &str,
        signature: &FunctionSignature,
        abi: &AbiProfile,
        lowering: &'profile LoweringProfile,
        isa: &'profile IsaProfile,
    ) -> anyhow::Result<Self> {
        let native_arg_registers = x_register_list("native_call args", &abi.native_args)?;
        let native_return_registers = x_register_list("native_call returns", &abi.native_returns)?;
        let native_clobber_registers = x_register_list("native_call clobbers", &abi.native_clobbers)?;
        let return_registers = (0..signature.return_slot_count())
            .map(|index| {
                abi.integer_returns
                    .get(index)
                    .copied()
                    .with_context(|| format!("profile ABI does not map return value {index}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let param_regs = signature
            .param_widths
            .iter()
            .enumerate()
            .map(|(index, _)| {
                abi.integer_args
                    .get(index)
                    .copied()
                    .with_context(|| format!("profile ABI does not map argument {index}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let reserved_vregs = param_regs
            .iter()
            .copied()
            .chain(return_registers.iter().copied())
            .chain(native_arg_registers.iter().copied())
            .chain(native_return_registers.iter().copied())
            .max()
            .map(|reg| reg + 1)
            .unwrap_or(0);
        let mut builder = VmFunctionBuilder::new(name, 0, signature.return_width);
        builder.reserve_vregs(reserved_vregs)?;
        let data_layout = module.get_data_layout();
        let layout = data_layout.as_str().to_string_lossy().into_owned();
        drop(data_layout);
        let target_data = TargetData::create(&layout);

        let mut values = HashMap::new();
        let mut aggregates = HashMap::new();
        let function_params = function.get_params();
        if function_params.len() != signature.params.len() {
            bail!(
                "function parameter count mismatch: LLVM has {}, signature has {}",
                function_params.len(),
                signature.params.len()
            );
        }
        for (index, (value, slots)) in function_params.into_iter().zip(signature.params.iter()).enumerate() {
            match value.get_type() {
                BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_) => {
                    let fields = slots
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(relative, field)| {
                            let slot = slots.start + relative;
                            let reg = *param_regs
                                .get(slot)
                                .with_context(|| format!("aggregate parameter {index} field {relative} slot {slot}"))?;
                            Ok(Some(AggregateField::borrowed(ValueBinding {
                                reg,
                                width: field.width,
                            })))
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    aggregates.insert(value_key(value), AggregateBinding { fields });
                },
                _ => {
                    let field = slots
                        .fields
                        .first()
                        .copied()
                        .with_context(|| format!("scalar parameter {index} has no field mapping"))?;
                    if slots.fields.len() != 1 {
                        bail!(
                            "scalar parameter {index} unexpectedly maps to {} fields",
                            slots.fields.len()
                        );
                    }
                    let reg = *param_regs
                        .get(slots.start)
                        .with_context(|| format!("scalar parameter {index} slot {}", slots.start))?;
                    values.insert(
                        value_key(value),
                        ValueBinding {
                            reg,
                            width: field.width,
                        },
                    );
                },
            }
        }
        let defined_values = values.keys().copied().collect();
        let native_touched_registers = native_arg_registers
            .iter()
            .chain(native_return_registers.iter())
            .chain(native_clobber_registers.iter())
            .copied()
            .collect();

        Ok(Self {
            module,
            function,
            lowering,
            isa,
            target_data,
            builder,
            values,
            aggregates,
            aggregate_reg_refs: HashMap::new(),
            labels: HashMap::new(),
            native_calls: Vec::new(),
            return_registers,
            native_arg_registers,
            native_return_registers,
            native_touched_registers,
            aggregate_return_fields: signature.aggregate_return_fields.clone(),
            defined_values,
            reuse_plan: None,
            temporary_regs: Vec::new(),
        })
    }

    fn lower(mut self) -> anyhow::Result<(VmFunction, Vec<NativeCallTarget<'ctx>>)> {
        let basic_blocks = self.function.get_basic_blocks();
        if basic_blocks.is_empty() {
            bail!("function has no basic blocks");
        }

        // LLVM CFG 边先变成 VM label。后续 branch/phi/select lowering 只引用 label，
        // bytecode encoder 再把 label 修正为指令 PC。
        for block in &basic_blocks {
            let label = self.builder.new_label();
            self.labels.insert(block_key(*block), label);
        }

        self.prepare_register_reuse(&basic_blocks)?;

        // phi 的语义通过 predecessor edge move 表达；phi 指令本身在 block 开头不 emit。
        for block in basic_blocks {
            let label = self
                .labels
                .get(&block_key(block))
                .copied()
                .context("missing VM label for basic block")?;
            self.builder.bind_label(label);
            self.lower_block(block)?;
        }

        let function = self.builder.finish()?;
        Ok((
            fuse_superinstructions(function, self.isa, self.lowering),
            self.native_calls,
        ))
    }

    fn prepare_register_reuse(&mut self, basic_blocks: &[BasicBlock<'ctx>]) -> anyhow::Result<()> {
        let plan = self.build_reuse_plan(basic_blocks)?;

        // pinned phi/result 需要在被引用前就拥有稳定寄存器，否则不同控制流路径会看到不同寄存器。
        for block in basic_blocks {
            for instruction in block.get_instructions() {
                let key = instruction_key(instruction);
                if instruction.get_opcode() == InstructionOpcode::Phi
                    && plan.pinned_values.contains(&key)
                    && !self.values.contains_key(&key)
                {
                    let width = instruction_result_width(instruction)?
                        .context("pinned VM value must have a scalar register width")?;
                    let reg = self.builder.alloc_vreg()?;
                    self.values.insert(key, ValueBinding { reg, width });
                }
                if instruction.get_opcode() == InstructionOpcode::Phi
                    && plan.pinned_aggregate_values.contains(&key)
                    && !self.aggregates.contains_key(&key)
                {
                    self.ensure_aggregate_result_binding(instruction)?;
                }
            }
        }
        self.reuse_plan = Some(plan);
        Ok(())
    }

    fn build_reuse_plan(&self, basic_blocks: &[BasicBlock<'ctx>]) -> anyhow::Result<ReusePlan> {
        let mut value_blocks = HashMap::new();
        let mut aggregate_value_blocks = HashMap::new();
        let mut result_values = HashSet::new();
        let mut aggregate_result_values = HashSet::new();
        let mut pinned_values = self.values.keys().copied().collect::<HashSet<_>>();
        let mut pinned_aggregate_values = HashSet::new();

        for block in basic_blocks {
            let block_key = block_key(*block);
            for instruction in block.get_instructions() {
                if instruction_result_width(instruction)?.is_some() {
                    let key = instruction_key(instruction);
                    value_blocks.insert(key, block_key);
                    result_values.insert(key);
                    if instruction.get_opcode() == InstructionOpcode::Phi {
                        pinned_values.insert(key);
                    }
                }
                if instruction_has_aggregate_result(instruction) {
                    let key = instruction_key(instruction);
                    aggregate_value_blocks.insert(key, block_key);
                    aggregate_result_values.insert(key);
                    if instruction.get_opcode() == InstructionOpcode::Phi {
                        pinned_aggregate_values.insert(key);
                    }
                }
            }
        }

        // 跨 basic-block 或 phi edge 的值不能靠简单线性扫描回收：VM bytecode 可能绕过本应重新
        // 分配该寄存器的 block。固定这些值能让 allocator 保守，同时仍允许 block-local SSA 临时值密集复用。
        for block in basic_blocks {
            let user_block = block_key(*block);
            for instruction in block.get_instructions() {
                for operand in instruction_value_operands(instruction) {
                    let key = value_key(operand);
                    let Some(def_block) = value_blocks.get(&key).copied() else {
                        continue;
                    };
                    if instruction.get_opcode() == InstructionOpcode::Phi || def_block != user_block {
                        pinned_values.insert(key);
                    }
                }
                for operand in instruction_value_operands(instruction) {
                    let key = value_key(operand);
                    let Some(def_block) = aggregate_value_blocks.get(&key).copied() else {
                        continue;
                    };
                    if instruction.get_opcode() == InstructionOpcode::Phi || def_block != user_block {
                        pinned_aggregate_values.insert(key);
                    }
                }
            }
        }

        let mut block_last_uses = HashMap::new();
        let mut block_last_aggregate_uses = HashMap::new();
        for block in basic_blocks {
            let user_block = block_key(*block);
            let mut last_uses = HashMap::new();
            let mut last_aggregate_uses = HashMap::new();
            for (index, instruction) in block.get_instructions().enumerate() {
                if instruction.get_opcode() == InstructionOpcode::Phi {
                    continue;
                }
                for operand in instruction_value_operands(instruction) {
                    let key = value_key(operand);
                    if pinned_values.contains(&key) {
                        continue;
                    }
                    if result_values.contains(&key) && value_blocks.get(&key).copied() == Some(user_block) {
                        last_uses.insert(key, index);
                    }
                    if !pinned_aggregate_values.contains(&key)
                        && aggregate_result_values.contains(&key)
                        && aggregate_value_blocks.get(&key).copied() == Some(user_block)
                    {
                        last_aggregate_uses.insert(key, index);
                    }
                }
            }
            block_last_uses.insert(user_block, last_uses);
            block_last_aggregate_uses.insert(user_block, last_aggregate_uses);
        }

        Ok(ReusePlan {
            pinned_values,
            pinned_aggregate_values,
            block_last_uses,
            block_last_aggregate_uses,
            current_block: None,
            instruction_index: 0,
        })
    }

    fn begin_instruction(&mut self, _instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        Ok(())
    }

    fn ensure_result_binding(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<ValueBinding> {
        let key = instruction_key(instruction);
        if let Some(binding) = self.values.get(&key).copied() {
            return Ok(binding);
        }

        let width =
            instruction_result_width(instruction)?.context("instruction has no scalar result register width")?;
        let reg = self.builder.alloc_vreg()?;
        let binding = ValueBinding { reg, width };
        self.values.insert(key, binding);
        Ok(binding)
    }

    fn ensure_aggregate_result_binding(
        &mut self,
        instruction: InstructionValue<'ctx>,
    ) -> anyhow::Result<AggregateBinding> {
        let key = instruction_key(instruction);
        if let Some(binding) = self.aggregates.get(&key).cloned() {
            return Ok(binding);
        }

        let field_infos = return_fields_from_aggregate_type(instruction_aggregate_type(instruction)?)
            .context("aggregate result fields")?;
        if field_infos.is_empty() {
            bail!("aggregate result has no scalar leaf fields");
        }

        let fields = field_infos
            .into_iter()
            .map(|info| {
                Ok(Some(AggregateField::owned(ValueBinding {
                    reg: self.builder.alloc_vreg()?,
                    width: info.width,
                })))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let binding = AggregateBinding { fields };
        self.insert_aggregate_value(key, binding.clone());
        Ok(binding)
    }

    fn finish_instruction(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let result_key = instruction_key(instruction);
        if instruction_result_width(instruction)?.is_some() {
            self.defined_values.insert(result_key);
        }
        self.release_after_instruction(instruction)?;
        self.advance_reuse_plan();
        Ok(())
    }

    fn release_after_instruction(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let Some(plan) = self.reuse_plan.as_ref() else {
            return Ok(());
        };
        let instruction_index = plan.instruction_index;
        let last_uses = plan
            .current_block
            .and_then(|block| plan.block_last_uses.get(&block))
            .cloned()
            .unwrap_or_default();
        let last_aggregate_uses = plan
            .current_block
            .and_then(|block| plan.block_last_aggregate_uses.get(&block))
            .cloned()
            .unwrap_or_default();
        let pinned_values = plan.pinned_values.clone();
        let pinned_aggregate_values = plan.pinned_aggregate_values.clone();

        let mut released = if instruction.get_opcode() == InstructionOpcode::Phi {
            Vec::new()
        } else {
            instruction_value_operands(instruction)
                .into_iter()
                .map(value_key)
                .filter(|key| !pinned_values.contains(key))
                .filter(|key| last_uses.get(key).copied() == Some(instruction_index))
                .collect::<Vec<_>>()
        };

        let result_key = instruction_key(instruction);
        if instruction_result_width(instruction)?.is_some()
            && !pinned_values.contains(&result_key)
            && !last_uses.contains_key(&result_key)
        {
            released.push(result_key);
        }

        let mut released_aggregates = if instruction.get_opcode() == InstructionOpcode::Phi {
            Vec::new()
        } else {
            instruction_value_operands(instruction)
                .into_iter()
                .map(value_key)
                .filter(|key| !pinned_aggregate_values.contains(key))
                .filter(|key| last_aggregate_uses.get(key).copied() == Some(instruction_index))
                .collect::<Vec<_>>()
        };

        if instruction_has_aggregate_result(instruction)
            && !pinned_aggregate_values.contains(&result_key)
            && !last_aggregate_uses.contains_key(&result_key)
        {
            released_aggregates.push(result_key);
        }

        self.release_instruction_temporaries();
        for key in released {
            if let Some(binding) = self.values.get(&key).copied() {
                self.builder.release_vreg(binding.reg);
            }
            self.defined_values.remove(&key);
            self.values.remove(&key);
        }
        for key in released_aggregates {
            self.release_aggregate_value(key);
        }

        Ok(())
    }

    fn advance_reuse_plan(&mut self) {
        if let Some(plan) = self.reuse_plan.as_mut() {
            plan.instruction_index += 1;
        }
    }

    fn alloc_temporary_vreg(&mut self) -> anyhow::Result<u8> {
        let reg = self.builder.alloc_vreg()?;
        if self.reuse_plan.is_some() {
            self.temporary_regs.push(reg);
        }
        Ok(reg)
    }

    fn alloc_temporary_vreg_excluding(&mut self, excluded: &HashSet<u8>) -> anyhow::Result<u8> {
        let reg = self.builder.alloc_vreg_excluding(excluded)?;
        if self.reuse_plan.is_some() {
            self.temporary_regs.push(reg);
        }
        Ok(reg)
    }

    fn release_instruction_temporaries(&mut self) {
        for reg in self.temporary_regs.drain(..) {
            self.builder.release_vreg(reg);
        }
    }

    fn insert_aggregate_value(&mut self, key: ValueKey, binding: AggregateBinding) {
        if let Some(previous) = self.aggregates.remove(&key) {
            self.release_aggregate_binding(previous);
        }
        self.retain_aggregate_binding(&binding);
        self.aggregates.insert(key, binding);
    }

    fn release_aggregate_value(&mut self, key: ValueKey) {
        if let Some(binding) = self.aggregates.remove(&key) {
            self.release_aggregate_binding(binding);
        }
    }

    fn retain_aggregate_binding(&mut self, binding: &AggregateBinding) {
        for field in binding.fields.iter().flatten().filter(|field| field.owned) {
            *self.aggregate_reg_refs.entry(field.binding.reg).or_insert(0) += 1;
        }
    }

    fn release_aggregate_binding(&mut self, binding: AggregateBinding) {
        for field in binding.fields.into_iter().flatten().filter(|field| field.owned) {
            let Some(count) = self.aggregate_reg_refs.get_mut(&field.binding.reg) else {
                continue;
            };
            *count -= 1;
            if *count == 0 {
                self.aggregate_reg_refs.remove(&field.binding.reg);
                self.builder.release_vreg(field.binding.reg);
            }
        }
    }

    fn begin_block(&mut self, block: BasicBlock<'ctx>) {
        if let Some(plan) = self.reuse_plan.as_mut() {
            plan.current_block = Some(block_key(block));
            plan.instruction_index = 0;
        }
    }

    fn end_block(&mut self) {
        if let Some(plan) = self.reuse_plan.as_mut() {
            plan.current_block = None;
            plan.instruction_index = 0;
        }
        self.release_instruction_temporaries();
    }

    fn lower_block(&mut self, block: BasicBlock<'ctx>) -> anyhow::Result<()> {
        self.begin_block(block);
        for instruction in block.get_instructions() {
            self.begin_instruction(instruction)?;
            if let Some(reason) = unsupported_control_flow_reason(instruction.get_opcode()) {
                bail!("{reason}");
            }
            match instruction.get_opcode() {
                InstructionOpcode::Phi => {},
                InstructionOpcode::Add
                | InstructionOpcode::Sub
                | InstructionOpcode::Mul
                | InstructionOpcode::UDiv
                | InstructionOpcode::SDiv
                | InstructionOpcode::URem
                | InstructionOpcode::SRem
                | InstructionOpcode::Xor
                | InstructionOpcode::And
                | InstructionOpcode::Or
                | InstructionOpcode::Shl
                | InstructionOpcode::LShr
                | InstructionOpcode::AShr => self.lower_binop(instruction)?,
                InstructionOpcode::FAdd
                | InstructionOpcode::FSub
                | InstructionOpcode::FMul
                | InstructionOpcode::FDiv
                | InstructionOpcode::FRem => self.lower_float_binop(instruction)?,
                InstructionOpcode::FNeg => self.lower_float_unary(instruction)?,
                InstructionOpcode::SIToFP
                | InstructionOpcode::UIToFP
                | InstructionOpcode::FPToSI
                | InstructionOpcode::FPToUI
                | InstructionOpcode::FPTrunc
                | InstructionOpcode::FPExt => self.lower_float_cast(instruction)?,
                InstructionOpcode::ICmp => self.lower_icmp(instruction)?,
                InstructionOpcode::FCmp => self.lower_fcmp(instruction)?,
                InstructionOpcode::ZExt
                | InstructionOpcode::SExt
                | InstructionOpcode::Trunc
                | InstructionOpcode::BitCast
                | InstructionOpcode::PtrToInt
                | InstructionOpcode::IntToPtr
                | InstructionOpcode::AddrSpaceCast => self.lower_cast(instruction)?,
                InstructionOpcode::Freeze => self.lower_freeze(instruction)?,
                InstructionOpcode::Alloca => self.lower_alloca(instruction)?,
                InstructionOpcode::Load => self.lower_load(instruction)?,
                InstructionOpcode::Store => self.lower_store(instruction)?,
                InstructionOpcode::AtomicRMW => self.lower_atomic_rmw(instruction)?,
                InstructionOpcode::AtomicCmpXchg => self.lower_cmpxchg(instruction)?,
                InstructionOpcode::Fence => self.lower_fence(instruction)?,
                InstructionOpcode::GetElementPtr => self.lower_gep(instruction)?,
                InstructionOpcode::Call => self.lower_call(instruction)?,
                InstructionOpcode::Select => self.lower_select(instruction)?,
                InstructionOpcode::InsertValue => self.lower_insert_value(instruction)?,
                InstructionOpcode::ExtractValue => self.lower_extract_value(instruction)?,
                InstructionOpcode::Br => self.lower_branch(block, instruction)?,
                InstructionOpcode::Switch => self.lower_switch(block, instruction)?,
                InstructionOpcode::Unreachable => self.lower_unreachable()?,
                InstructionOpcode::Return => self.lower_return(instruction)?,
                opcode => bail!("unsupported instruction opcode: {opcode:?}"),
            }
            self.finish_instruction(instruction)?;
        }
        self.end_block();

        Ok(())
    }

    fn lowering_rule(&self, contract: &str) -> anyhow::Result<&LoweringRule> {
        let pattern = lowering_match_pattern(contract)
            .with_context(|| format!("translator requested unknown lowering contract {contract}"))?;
        self.lowering
            .rule_by_match(pattern)
            .ok_or_else(|| anyhow::anyhow!("profile lowering does not declare {contract} match {pattern}"))
    }

    fn instruction_desc(&self, name: &str) -> anyhow::Result<&InstructionDesc> {
        self.isa
            .instructions
            .iter()
            .find(|instruction| instruction.name == name)
            .ok_or_else(|| anyhow::anyhow!("profile ISA does not declare instruction {name}"))
    }

    fn instruction_desc_for_semantic(&self, semantic: &HandlerSemantic) -> anyhow::Result<InstructionDesc> {
        let mut matches = self
            .isa
            .instructions
            .iter()
            .filter(|instruction| instruction.semantic == *semantic);
        let first = matches
            .next()
            .cloned()
            .with_context(|| format!("profile ISA does not declare semantic {semantic:?}"))?;
        if let Some(second) = matches.next() {
            bail!(
                "profile ISA semantic {semantic:?} is ambiguous between {} and {}; use a lowering emit instruction name",
                first.name,
                second.name
            );
        }
        Ok(first)
    }

    fn emit_action_for_shape(
        &self,
        rule: &str,
        semantic: &HandlerSemantic,
        required_operands: &[(&str, &str)],
    ) -> anyhow::Result<LoweringAction> {
        let mut matches = self
            .lowering_rule(rule)?
            .actions
            .iter()
            .filter_map(|action| match action {
                LoweringAction::Emit { instruction, operands } => {
                    let desc = self.instruction_desc(instruction).ok()?;
                    (desc.semantic == *semantic && operands_match_shape(operands, required_operands))
                        .then_some(action.clone())
                },
                _ => None,
            })
            .collect::<Vec<_>>();

        match matches.len() {
            1 => Ok(matches.remove(0)),
            0 => bail!("profile lowering rule {rule} does not emit {semantic:?} with required operand shape"),
            count => bail!("profile lowering rule {rule} has {count} emits matching {semantic:?} and operand shape"),
        }
    }

    fn select_lowering_actions(&self, rule: &str, width_expr: &str) -> anyhow::Result<SelectLoweringActions> {
        Ok(SelectLoweringActions {
            br_if: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::BrCond,
                &[("cond", "%vc"), ("then_pc", "then_label"), ("else_pc", "else_label")],
            )?,
            then_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Mov,
                &[("dst", "%vr"), ("src", "%vt"), ("width", width_expr)],
            )?,
            br: self.emit_action_for_shape(rule, &HandlerSemantic::Br, &[("target", "join_label")])?,
            else_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Mov,
                &[("dst", "%vr"), ("src", "%ve"), ("width", width_expr)],
            )?,
        })
    }

    fn emit_profile_action(&mut self, action: &LoweringAction, env: &LoweringEnv<'ctx>) -> anyhow::Result<()> {
        let LoweringAction::Emit { instruction, operands } = action else {
            bail!("lowering action is not an emit action");
        };
        let desc = self.instruction_desc(instruction)?.clone();
        let args = ProfileInstructionArgs::from_emit(&desc, operands, env)?;
        self.emit_profile_instruction(&desc, args)
            .with_context(|| format!("while emitting profile instruction {}", desc.name))
    }

    fn execute_lowering_rule(
        &mut self,
        rule: &str,
        mut env: LoweringEnv<'ctx>,
        selected_semantic: Option<HandlerSemantic>,
    ) -> anyhow::Result<LoweringEnv<'ctx>> {
        let actions = self.lowering_rule(rule)?.actions.clone();
        let mut emitted = 0_usize;

        // lowering.vm 的 action 顺序是语义的一部分：
        // materialize/vreg 先准备 VM 值，emit 产生 profile 指令，bind 再把 LLVM 结果挂到 VM 寄存器。
        for action in &actions {
            match action {
                LoweringAction::Materialize {
                    target,
                    source,
                    value_type,
                } => {
                    let value =
                        self.materialize_lowering_value(source, value_type.as_deref().unwrap_or("i64"), &env)?;
                    env.insert(target.clone(), LoweringValue::Reg(value));
                },
                LoweringAction::VReg { target, value_type } => {
                    if env.get(target).is_ok() {
                        continue;
                    }
                    let reg = self.builder.alloc_vreg()?;
                    let width = width_from_lowering_value_type(value_type, &env)?;
                    env.insert(target.clone(), LoweringValue::Reg(ValueBinding { reg, width }));
                },
                LoweringAction::Emit { instruction, .. } => {
                    let desc = self.instruction_desc(instruction)?;
                    if selected_semantic
                        .as_ref()
                        .is_some_and(|semantic| desc.semantic != *semantic)
                    {
                        continue;
                    }
                    self.emit_profile_action(action, &env)?;
                    emitted += 1;
                },
                LoweringAction::Bind { llvm_value, vm_value } => {
                    let LoweringValue::Reg(binding) = env.get(vm_value)? else {
                        bail!("lowering bind {llvm_value} = {vm_value} expected a VM register");
                    };
                    env.insert(llvm_value.clone(), LoweringValue::Reg(binding));
                    if let Some(key) = env.llvm_key(llvm_value) {
                        self.values.insert(key, binding);
                    }
                },
            }
        }

        if emitted == 0 {
            if let Some(semantic) = selected_semantic {
                bail!("profile lowering rule {rule} did not emit semantic {semantic:?}");
            }
            bail!("profile lowering rule {rule} did not emit any VM instruction");
        }

        Ok(env)
    }

    fn materialize_lowering_value(
        &mut self,
        source: &str,
        value_type: &str,
        env: &LoweringEnv<'ctx>,
    ) -> anyhow::Result<ValueBinding> {
        let value = match env.get(source) {
            Ok(value) => value,
            Err(error) => {
                if let Some(llvm_value) = env.llvm_source_value(source) {
                    return self.materialize_value(llvm_value);
                }
                return Err(error);
            },
        };

        match value {
            LoweringValue::Reg(binding) => Ok(binding),
            LoweringValue::Imm(value) => {
                let width = width_from_lowering_value_type(value_type, env)?;
                let reg = self.alloc_temporary_vreg()?;
                self.push_constant(reg, value, width)?;
                Ok(ValueBinding { reg, width })
            },
            LoweringValue::Label(_) => bail!("lowering materialize {source} cannot materialize a label"),
        }
    }

    fn emit_profile_instruction(&mut self, desc: &InstructionDesc, args: ProfileInstructionArgs) -> anyhow::Result<()> {
        // 这里是 profile semantic AST 到内部 VmInstruction 的最后一跳。内部 enum 只保存语义操作，
        // 但 push_profile 会额外记录 desc.name，避免同语义指令在 bytecode 阶段丢失 profile identity。
        match &desc.semantic {
            HandlerSemantic::MovImm => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let imm = args.imm("imm")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::MovImm { dst, imm, width }, desc.name.clone());
            },
            HandlerSemantic::ConstLoad => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let value = args.imm("index")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::ConstLoad { dst, value, width }, desc.name.clone());
            },
            HandlerSemantic::ReadCounter(kind) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::ReadCounter {
                        kind: *kind,
                        dst,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::AddXor) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let xor_rhs = self.profile_reg(desc, &args, "xor_rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperAddXor {
                        dst,
                        lhs,
                        rhs,
                        xor_rhs,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::IcmpBrIf) => {
                let pred = predicate_from_u64(args.imm("pred")?)?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                let then_label = args.label("then_pc")?;
                let else_label = args.label("else_pc")?;
                self.builder.push_profile(
                    VmInstruction::SuperIcmpBrIf {
                        pred,
                        lhs,
                        rhs,
                        width,
                        then_label,
                        else_label,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::GepLoad) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let base = self.profile_reg(desc, &args, "base")?;
                let offset = args.imm("offset")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperGepLoad {
                        dst,
                        base,
                        offset,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadAdd) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let addend = self.profile_reg(desc, &args, "addend")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadAdd {
                        dst,
                        ptr,
                        addend,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Mov => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::Mov { dst, src, width }, desc.name.clone());
            },
            HandlerSemantic::Bin(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::Bin {
                        op: *op,
                        dst,
                        lhs,
                        rhs,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::IntUnary(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let width = checked_intrinsic_integer_width(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::IntUnary {
                        op: *op,
                        dst,
                        src,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::IntTernary(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let third = self.profile_reg(desc, &args, "third")?;
                let width = checked_intrinsic_integer_width(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::IntTernary {
                        op: *op,
                        dst,
                        lhs,
                        rhs,
                        third,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::IntOverflow(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let overflow = self.profile_reg(desc, &args, "overflow")?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_intrinsic_integer_width(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::IntOverflow {
                        op: *op,
                        dst,
                        overflow,
                        lhs,
                        rhs,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::FloatBin(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_float_width(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::FloatBin {
                        op: *op,
                        dst,
                        lhs,
                        rhs,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::FloatUnary(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let width = checked_float_width(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::FloatUnary {
                        op: *op,
                        dst,
                        src,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::FloatTernary(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let third = self.profile_reg(desc, &args, "third")?;
                let width = checked_float_width(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::FloatTernary {
                        op: *op,
                        dst,
                        lhs,
                        rhs,
                        third,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::FloatCast(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let from_width = checked_width_u64(args.imm("from_width")?)?;
                let to_width = checked_width_u64(args.imm("to_width")?)?;
                self.validate_float_cast_widths(*op, from_width, to_width)?;
                self.builder.push_profile(
                    VmInstruction::FloatCast {
                        op: *op,
                        dst,
                        src,
                        from_width,
                        to_width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::FloatClass => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let mask = checked_fpclass_mask(args.imm("mask")?)?;
                let width = checked_float_width(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::FloatClass { dst, src, mask, width }, desc.name.clone());
            },
            HandlerSemantic::Icmp => {
                let pred = predicate_from_u64(args.imm("pred")?)?;
                let dst = self.profile_reg(desc, &args, "dst")?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::Icmp {
                        pred,
                        dst,
                        lhs,
                        rhs,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Fcmp => {
                let pred = float_predicate_from_u64(args.imm("pred")?)?;
                let dst = self.profile_reg(desc, &args, "dst")?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_float_width(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::Fcmp {
                        pred,
                        dst,
                        lhs,
                        rhs,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Cast(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let from_width = checked_width_u64(args.imm("from_width")?)?;
                let to_width = checked_width_u64(args.imm("to_width")?)?;
                self.builder.push_profile(
                    VmInstruction::Cast {
                        op: *op,
                        dst,
                        src,
                        from_width,
                        to_width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Alloca => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let bytes = args.imm("bytes")?;
                let align = u8::try_from(args.imm("align")?).context("alloca align does not fit in u8")?;
                self.builder
                    .push_profile(VmInstruction::Alloca { dst, bytes, align }, desc.name.clone());
            },
            HandlerSemantic::DynamicAlloca => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let count = self.profile_reg(desc, &args, "count")?;
                let elem_size = args.imm("elem_size")?;
                let align = u8::try_from(args.imm("align")?).context("dynamic alloca align does not fit in u8")?;
                self.builder.push_profile(
                    VmInstruction::DynamicAlloca {
                        dst,
                        count,
                        elem_size,
                        align,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Load => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::Load { dst, ptr, width }, desc.name.clone());
            },
            HandlerSemantic::VolatileLoad => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::VolatileLoad { dst, ptr, width }, desc.name.clone());
            },
            HandlerSemantic::Store => {
                let src = self.profile_reg(desc, &args, "src")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::Store { src, ptr, width }, desc.name.clone());
            },
            HandlerSemantic::VolatileStore => {
                let src = self.profile_reg(desc, &args, "src")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::VolatileStore { src, ptr, width }, desc.name.clone());
            },
            HandlerSemantic::MemcpyDynamic => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let len = self.profile_reg(desc, &args, "len")?;
                self.builder
                    .push_profile(VmInstruction::MemcpyDynamic { dst, src, len }, desc.name.clone());
            },
            HandlerSemantic::MemmoveDynamic => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let len = self.profile_reg(desc, &args, "len")?;
                self.builder
                    .push_profile(VmInstruction::MemmoveDynamic { dst, src, len }, desc.name.clone());
            },
            HandlerSemantic::MemsetDynamic => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let value = self.profile_reg(desc, &args, "value")?;
                let len = self.profile_reg(desc, &args, "len")?;
                self.builder
                    .push_profile(VmInstruction::MemsetDynamic { dst, value, len }, desc.name.clone());
            },
            HandlerSemantic::VolatileMemcpyDynamic => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let len = self.profile_reg(desc, &args, "len")?;
                self.builder.push_profile(
                    VmInstruction::VolatileMemcpyDynamic { dst, src, len },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::VolatileMemmoveDynamic => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let len = self.profile_reg(desc, &args, "len")?;
                self.builder.push_profile(
                    VmInstruction::VolatileMemmoveDynamic { dst, src, len },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::VolatileMemsetDynamic => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let value = self.profile_reg(desc, &args, "value")?;
                let len = self.profile_reg(desc, &args, "len")?;
                self.builder.push_profile(
                    VmInstruction::VolatileMemsetDynamic { dst, value, len },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::AtomicLoad => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_atomic_memory_width(args.imm("width")?)?;
                let ordering = memory_ordering_from_u64(args.imm("ordering")?)?;
                self.builder.push_profile(
                    VmInstruction::AtomicLoad {
                        dst,
                        ptr,
                        width,
                        ordering,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::AtomicStore => {
                let src = self.profile_reg(desc, &args, "src")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_atomic_memory_width(args.imm("width")?)?;
                let ordering = memory_ordering_from_u64(args.imm("ordering")?)?;
                self.builder.push_profile(
                    VmInstruction::AtomicStore {
                        src,
                        ptr,
                        width,
                        ordering,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::VolatileAtomicLoad => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_atomic_memory_width(args.imm("width")?)?;
                let ordering = memory_ordering_from_u64(args.imm("ordering")?)?;
                self.builder.push_profile(
                    VmInstruction::VolatileAtomicLoad {
                        dst,
                        ptr,
                        width,
                        ordering,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::VolatileAtomicStore => {
                let src = self.profile_reg(desc, &args, "src")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_atomic_memory_width(args.imm("width")?)?;
                let ordering = memory_ordering_from_u64(args.imm("ordering")?)?;
                self.builder.push_profile(
                    VmInstruction::VolatileAtomicStore {
                        src,
                        ptr,
                        width,
                        ordering,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::AtomicRmw(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let width = checked_atomic_memory_width(args.imm("width")?)?;
                let ordering = memory_ordering_from_u64(args.imm("ordering")?)?;
                self.builder.push_profile(
                    VmInstruction::AtomicRmw {
                        op: *op,
                        dst,
                        ptr,
                        src,
                        width,
                        ordering,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::VolatileAtomicRmw(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let width = checked_atomic_memory_width(args.imm("width")?)?;
                let ordering = memory_ordering_from_u64(args.imm("ordering")?)?;
                self.builder.push_profile(
                    VmInstruction::VolatileAtomicRmw {
                        op: *op,
                        dst,
                        ptr,
                        src,
                        width,
                        ordering,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::CmpXchg => {
                let old = self.profile_reg(desc, &args, "old")?;
                let success = self.profile_reg(desc, &args, "success")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let cmp = self.profile_reg(desc, &args, "cmp")?;
                let new = self.profile_reg(desc, &args, "new")?;
                let width = checked_atomic_memory_width(args.imm("width")?)?;
                let success_ordering = memory_ordering_from_u64(args.imm("success_ordering")?)?;
                let failure_ordering = memory_ordering_from_u64(args.imm("failure_ordering")?)?;
                self.builder.push_profile(
                    VmInstruction::CmpXchg {
                        old,
                        success,
                        ptr,
                        cmp,
                        new,
                        width,
                        success_ordering,
                        failure_ordering,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::VolatileCmpXchg => {
                let old = self.profile_reg(desc, &args, "old")?;
                let success = self.profile_reg(desc, &args, "success")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let cmp = self.profile_reg(desc, &args, "cmp")?;
                let new = self.profile_reg(desc, &args, "new")?;
                let width = checked_atomic_memory_width(args.imm("width")?)?;
                let success_ordering = memory_ordering_from_u64(args.imm("success_ordering")?)?;
                let failure_ordering = memory_ordering_from_u64(args.imm("failure_ordering")?)?;
                self.builder.push_profile(
                    VmInstruction::VolatileCmpXchg {
                        old,
                        success,
                        ptr,
                        cmp,
                        new,
                        width,
                        success_ordering,
                        failure_ordering,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Fence => {
                let ordering = memory_ordering_from_u64(args.imm("ordering")?)?;
                self.builder
                    .push_profile(VmInstruction::Fence { ordering }, desc.name.clone());
            },
            HandlerSemantic::Gep => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let base = self.profile_reg(desc, &args, "base")?;
                let offset = args.imm("offset")?;
                self.builder
                    .push_profile(VmInstruction::Gep { dst, base, offset }, desc.name.clone());
            },
            HandlerSemantic::Br => {
                let target = args.label("target")?;
                self.builder
                    .push_profile(VmInstruction::Br { target }, desc.name.clone());
            },
            HandlerSemantic::BrCond => {
                let cond = self.profile_reg(desc, &args, "cond")?;
                let then_label = args.label("then_pc")?;
                let else_label = args.label("else_pc")?;
                self.builder.push_profile(
                    VmInstruction::BrCond {
                        cond,
                        then_label,
                        else_label,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Ret => {
                let src = self.profile_reg(desc, &args, "src")?;
                self.builder.push_profile(VmInstruction::Ret { src }, desc.name.clone());
            },
            HandlerSemantic::Unreachable => {
                self.builder.push_profile(VmInstruction::Unreachable, desc.name.clone());
            },
            HandlerSemantic::Trap => {
                self.builder.push_profile(VmInstruction::Trap, desc.name.clone());
            },
            HandlerSemantic::SideEffect => {
                self.builder.push_profile(VmInstruction::SideEffect, desc.name.clone());
            },
            HandlerSemantic::CallNative | HandlerSemantic::Nop | HandlerSemantic::VmCall | HandlerSemantic::VmRet => {
                if desc.semantic == HandlerSemantic::Nop {
                    self.builder.push_profile(VmInstruction::Nop, desc.name.clone());
                    return Ok(());
                }
                if desc.semantic != HandlerSemantic::CallNative {
                    bail!(
                        "profile instruction {} is not supported by the LLVM translator emitter",
                        desc.name
                    );
                }
                let call_id = u16::try_from(args.imm("callee")?)
                    .with_context(|| format!("call_native callee operand in {} does not fit in u16", desc.name))?;
                let argc = usize::try_from(args.imm("argc")?)
                    .with_context(|| format!("call_native argc operand in {} does not fit in usize", desc.name))?;
                if argc > NATIVE_CALL_MAX_ARGS {
                    bail!("call_native supports at most {NATIVE_CALL_MAX_ARGS} arguments, got {argc}");
                }
                let ret_count = usize::try_from(args.imm("ret_count")?)
                    .with_context(|| format!("call_native ret_count operand in {} does not fit in usize", desc.name))?;
                if ret_count > NATIVE_CALL_MAX_RETURNS {
                    bail!("call_native supports at most {NATIVE_CALL_MAX_RETURNS} returns, got {ret_count}");
                }

                let mut call_args = Vec::with_capacity(argc);
                for index in 0..argc {
                    call_args.push(self.profile_reg(desc, &args, &format!("arg{index}"))?);
                }

                let mut returns = Vec::with_capacity(ret_count);
                for index in 0..ret_count {
                    returns.push(NativeReturn {
                        dst: self.profile_reg(desc, &args, &format!("ret{index}"))?,
                        width: checked_width_u64(args.imm(&format!("ret{index}_width"))?)?,
                    });
                }

                self.builder.push_profile(
                    VmInstruction::CallNative {
                        call_id,
                        args: call_args,
                        returns,
                    },
                    desc.name.clone(),
                );
            },
        }
        Ok(())
    }

    fn emit_profile_mov_imm(&mut self, dst: u8, imm: u64, width: u8) -> anyhow::Result<()> {
        let desc = self.instruction_desc_for_semantic(&HandlerSemantic::MovImm)?;
        let args = ProfileInstructionArgs::from_values([
            ("dst".to_owned(), LoweringValue::Reg(ValueBinding { reg: dst, width })),
            ("imm".to_owned(), LoweringValue::Imm(imm)),
            ("width".to_owned(), LoweringValue::Imm(width as u64)),
        ]);
        self.emit_profile_instruction(&desc, args)
    }

    fn emit_profile_mov_direct(&mut self, dst: u8, src: u8, width: u8) -> anyhow::Result<()> {
        let desc = self.instruction_desc("mov")?.clone();
        let args = ProfileInstructionArgs::from_values([
            ("dst".to_owned(), LoweringValue::Reg(ValueBinding { reg: dst, width })),
            ("src".to_owned(), LoweringValue::Reg(ValueBinding { reg: src, width })),
            ("width".to_owned(), LoweringValue::Imm(width as u64)),
        ]);
        self.emit_profile_instruction(&desc, args)
    }

    fn profile_reg(&mut self, desc: &InstructionDesc, args: &ProfileInstructionArgs, name: &str) -> anyhow::Result<u8> {
        let operand = desc
            .operand_descs
            .iter()
            .find(|operand| operand.name == name)
            .with_context(|| format!("profile instruction {} has no operand {name}", desc.name))?;
        match args.raw(name)? {
            LoweringValue::Reg(binding) => Ok(binding.reg),
            LoweringValue::Imm(value) if operand.kind == OperandKind::VReg => {
                let reg = self.alloc_temporary_vreg()?;
                self.push_constant(reg, value, width_from_operand_type(&operand.value_type))?;
                Ok(reg)
            },
            LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                bail!("profile instruction operand {name} expected an x register")
            },
        }
    }

    fn lower_binop(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let (rule, selected) = match instruction.get_opcode() {
            InstructionOpcode::Add => ("llvm.add.integer", None),
            InstructionOpcode::Sub => ("llvm.sub.integer", None),
            InstructionOpcode::Mul => ("llvm.mul.integer", None),
            InstructionOpcode::UDiv => ("llvm.udiv.integer", Some(HandlerSemantic::Bin(BinOp::UDiv))),
            InstructionOpcode::SDiv => ("llvm.sdiv.integer", Some(HandlerSemantic::Bin(BinOp::SDiv))),
            InstructionOpcode::URem => ("llvm.urem.integer", Some(HandlerSemantic::Bin(BinOp::URem))),
            InstructionOpcode::SRem => ("llvm.srem.integer", Some(HandlerSemantic::Bin(BinOp::SRem))),
            InstructionOpcode::Xor => ("llvm.bitops.integer", Some(HandlerSemantic::Bin(BinOp::Xor))),
            InstructionOpcode::And => ("llvm.bitops.integer", Some(HandlerSemantic::Bin(BinOp::And))),
            InstructionOpcode::Or => ("llvm.bitops.integer", Some(HandlerSemantic::Bin(BinOp::Or))),
            InstructionOpcode::Shl => ("llvm.shift.integer", Some(HandlerSemantic::Bin(BinOp::Shl))),
            InstructionOpcode::LShr => ("llvm.shift.integer", Some(HandlerSemantic::Bin(BinOp::LShr))),
            InstructionOpcode::AShr => ("llvm.shift.integer", Some(HandlerSemantic::Bin(BinOp::AShr))),
            opcode => bail!("unsupported binop opcode: {opcode:?}"),
        };
        let lhs = instruction_operand_value(instruction, 0)?;
        let rhs = instruction_operand_value(instruction, 1)?;
        let width = instruction_result_width(instruction)?.context("binop result has no scalar width")?;
        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(rule, env, selected)?;
        Ok(())
    }

    fn lower_float_binop(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let (rule, semantic) = match instruction.get_opcode() {
            InstructionOpcode::FAdd => ("llvm.fadd.float", HandlerSemantic::FloatBin(FloatBinOp::Add)),
            InstructionOpcode::FSub => ("llvm.fsub.float", HandlerSemantic::FloatBin(FloatBinOp::Sub)),
            InstructionOpcode::FMul => ("llvm.fmul.float", HandlerSemantic::FloatBin(FloatBinOp::Mul)),
            InstructionOpcode::FDiv => ("llvm.fdiv.float", HandlerSemantic::FloatBin(FloatBinOp::Div)),
            InstructionOpcode::FRem => ("llvm.frem.float", HandlerSemantic::FloatBin(FloatBinOp::Rem)),
            opcode => bail!("unsupported floating binop opcode: {opcode:?}"),
        };
        let lhs = instruction_operand_value(instruction, 0)?;
        let rhs = instruction_operand_value(instruction, 1)?;
        let width = instruction_result_width(instruction)?.context("floating binop result has no scalar width")?;
        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(rule, env, Some(semantic))?;
        Ok(())
    }

    fn lower_float_unary(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let (rule, semantic) = match instruction.get_opcode() {
            InstructionOpcode::FNeg => ("llvm.fneg.float", HandlerSemantic::FloatUnary(FloatUnaryOp::Neg)),
            opcode => bail!("unsupported floating unary opcode: {opcode:?}"),
        };
        let src = instruction_operand_value(instruction, 0)?;
        let width = instruction_result_width(instruction)?.context("floating unary result has no scalar width")?;
        let env = LoweringEnv::new()
            .llvm_source("%a", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(rule, env, Some(semantic))?;
        Ok(())
    }

    fn lower_float_cast(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let src = instruction_operand_value(instruction, 0)?;
        let src_width = value_width(src)?;
        let dst_width = instruction_result_width(instruction)?.context("floating cast result has no scalar width")?;
        let (rule, semantic) = match instruction.get_opcode() {
            InstructionOpcode::SIToFP => (
                "llvm.sitofp.float",
                HandlerSemantic::FloatCast(FloatCastOp::SignedIntToFloat),
            ),
            InstructionOpcode::UIToFP => (
                "llvm.uitofp.float",
                HandlerSemantic::FloatCast(FloatCastOp::UnsignedIntToFloat),
            ),
            InstructionOpcode::FPToSI => (
                "llvm.fptosi.float",
                HandlerSemantic::FloatCast(FloatCastOp::FloatToSignedInt),
            ),
            InstructionOpcode::FPToUI => (
                "llvm.fptoui.float",
                HandlerSemantic::FloatCast(FloatCastOp::FloatToUnsignedInt),
            ),
            InstructionOpcode::FPTrunc => (
                "llvm.fptrunc.float",
                HandlerSemantic::FloatCast(FloatCastOp::FloatTrunc),
            ),
            InstructionOpcode::FPExt => ("llvm.fpext.float", HandlerSemantic::FloatCast(FloatCastOp::FloatExt)),
            opcode => bail!("unsupported floating cast opcode: {opcode:?}"),
        };
        if let HandlerSemantic::FloatCast(op) = semantic {
            self.validate_float_cast_widths(op, src_width, dst_width)?;
        }
        let env = LoweringEnv::new()
            .llvm_source("%a", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", src_width as u64)
            .imm("type_width(%r)", dst_width as u64);
        self.execute_lowering_rule(rule, env, Some(semantic))?;
        Ok(())
    }

    fn validate_float_cast_widths(&self, op: FloatCastOp, from_width: u8, to_width: u8) -> anyhow::Result<()> {
        match op {
            FloatCastOp::SignedIntToFloat | FloatCastOp::UnsignedIntToFloat => {
                checked_float_width(to_width as u64)?;
            },
            FloatCastOp::FloatToSignedInt | FloatCastOp::FloatToUnsignedInt => {
                checked_float_width(from_width as u64)?;
            },
            FloatCastOp::FloatTrunc => {
                if from_width != 64 || to_width != 32 {
                    bail!("only double-to-float fptrunc is supported by vm_virtualize");
                }
            },
            FloatCastOp::FloatExt => {
                if from_width != 32 || to_width != 64 {
                    bail!("only float-to-double fpext is supported by vm_virtualize");
                }
            },
        }
        Ok(())
    }

    fn lower_alloca(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let allocated_type = instruction
            .get_allocated_type()
            .map_err(|err| anyhow::anyhow!("failed to read alloca type: {err}"))?;
        let element_size = self.target_data.get_store_size(&allocated_type);
        if element_size == 0 {
            bail!("zero-sized alloca is not supported");
        }
        let align = instruction
            .get_alignment()
            .ok()
            .and_then(|align| u8::try_from(align).ok())
            .unwrap_or(1);

        if let Some(count_value) = instruction_basic_operand(instruction, 0) {
            if !count_value.is_int_value() {
                bail!("dynamic alloca count must be an integer");
            }
            if let Some(count) = count_value.into_int_value().get_zero_extended_constant() {
                if count == 0 {
                    bail!("zero-count alloca is not supported");
                }
                let bytes = element_size.checked_mul(count).context("alloca byte size overflow")?;
                let env = LoweringEnv::new()
                    .llvm_value("%r", instruction_key(instruction))
                    .imm("alloc_size(%ty)", bytes)
                    .imm("alloc_align(%r)", align as u64);
                self.execute_lowering_rule("llvm.alloca.stack", env, Some(HandlerSemantic::Alloca))?;
                return Ok(());
            }

            let env = LoweringEnv::new()
                .llvm_source("%count", count_value)
                .llvm_value("%r", instruction_key(instruction))
                .imm("alloc_elem_size(%ty)", element_size)
                .imm("alloc_align(%r)", align as u64);
            self.execute_lowering_rule("llvm.alloca.dynamic", env, Some(HandlerSemantic::DynamicAlloca))?;
            return Ok(());
        }

        let bytes = element_size;
        let env = LoweringEnv::new()
            .llvm_value("%r", instruction_key(instruction))
            .imm("alloc_size(%ty)", bytes)
            .imm("alloc_align(%r)", align as u64);
        self.execute_lowering_rule("llvm.alloca.stack", env, Some(HandlerSemantic::Alloca))?;
        Ok(())
    }

    fn lower_load(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let is_volatile = memory_is_volatile(instruction, "load")?;
        let ordering = memory_ordering(instruction, "load")?;
        if ordering != AtomicOrdering::NotAtomic {
            return self.lower_atomic_load(instruction, ordering, is_volatile);
        }
        if matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_)
        ) {
            return self.lower_aggregate_load(instruction, is_volatile);
        }
        let ptr = instruction_operand_value(instruction, 0)?;
        let width = instruction_result_width(instruction)?.context("load result has no scalar width")?;
        let env = LoweringEnv::new()
            .llvm_source("%ptr", ptr)
            .llvm_value("%r", instruction_key(instruction))
            .imm("memory_width(%ptr)", width as u64);
        let (contract, semantic) = if is_volatile {
            ("llvm.memory.volatile.scalar", HandlerSemantic::VolatileLoad)
        } else {
            ("llvm.memory.scalar", HandlerSemantic::Load)
        };
        self.execute_lowering_rule(contract, env, Some(semantic))?;
        Ok(())
    }

    fn lower_store(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let is_volatile = memory_is_volatile(instruction, "store")?;
        let ordering = memory_ordering(instruction, "store")?;
        if ordering != AtomicOrdering::NotAtomic {
            return self.lower_atomic_store(instruction, ordering, is_volatile);
        }
        let src_value = instruction_operand_value(instruction, 0)?;
        if matches!(
            src_value.get_type(),
            BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_)
        ) {
            return self.lower_aggregate_store(instruction, src_value, is_volatile);
        }
        let src = self.materialize_operand(instruction, 0)?;
        let ptr = instruction_operand_value(instruction, 1)?;
        let env = LoweringEnv::new()
            .llvm_source("%ptr", ptr)
            .binding("%value", src)
            .binding("%vv", src)
            .imm("memory_width(%ptr)", src.width as u64);
        let (contract, semantic) = if is_volatile {
            ("llvm.memory.volatile.scalar", HandlerSemantic::VolatileStore)
        } else {
            ("llvm.memory.scalar", HandlerSemantic::Store)
        };
        self.execute_lowering_rule(contract, env, Some(semantic))?;
        Ok(())
    }

    fn lower_aggregate_load(&mut self, instruction: InstructionValue<'ctx>, is_volatile: bool) -> anyhow::Result<()> {
        let aggregate_type = instruction_aggregate_type(instruction).context("aggregate load result type")?;
        let fields = aggregate_memory_fields(&self.target_data, aggregate_type).context("aggregate load fields")?;
        if fields.is_empty() {
            bail!("empty aggregate load is not supported by vm_virtualize");
        }

        let ptr_value = instruction_operand_value(instruction, 0)?;
        let ptr = self.materialize_value(ptr_value)?;
        let (contract, load_semantic) = if is_volatile {
            ("llvm.memory.volatile.aggregate.load", HandlerSemantic::VolatileLoad)
        } else {
            ("llvm.memory.aggregate.load", HandlerSemantic::Load)
        };
        let direct_load = self.emit_action_for_shape(
            contract,
            &load_semantic,
            &[("dst", "%vf"), ("ptr", "%vp"), ("width", "field_width(%field)")],
        )?;
        let offset_load = self.emit_action_for_shape(
            contract,
            &load_semantic,
            &[("dst", "%vf"), ("ptr", "%addr"), ("width", "field_width(%field)")],
        )?;
        let gep = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vp"), ("offset", "field_offset(%field)")],
        )?;
        let mov = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vf"), ("width", "field_width(%field)")],
        )?;

        let mut loaded = Vec::with_capacity(fields.len());
        for field in fields {
            let tmp = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: field.info.width,
            };
            let (field_ptr, load_action) = if field.offset == 0 {
                (ptr, &direct_load)
            } else {
                (
                    self.emit_memory_intrinsic_gep(contract, &gep, "%vp", ptr, "field_offset(%field)", field.offset)?,
                    &offset_load,
                )
            };
            let load_env = LoweringEnv::new()
                .binding("%vp", ptr)
                .binding("%addr", field_ptr)
                .binding("%vf", tmp)
                .imm("field_width(%field)", field.info.width as u64)
                .imm("field_offset(%field)", field.offset);
            self.emit_profile_action(load_action, &load_env)?;

            let stable = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: field.info.width,
            };
            let mov_env = LoweringEnv::new()
                .binding("%vf", tmp)
                .binding("%vr", stable)
                .imm("field_width(%field)", field.info.width as u64);
            self.emit_profile_action(&mov, &mov_env)?;
            loaded.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: loaded });
        Ok(())
    }

    fn lower_aggregate_store(
        &mut self,
        instruction: InstructionValue<'ctx>,
        src_value: BasicValueEnum<'ctx>,
        is_volatile: bool,
    ) -> anyhow::Result<()> {
        if is_undef_or_poison_value(src_value) {
            bail!("aggregate store source must be frozen before VM materialization");
        }
        let fields =
            aggregate_memory_fields(&self.target_data, src_value.get_type()).context("aggregate store fields")?;
        if fields.is_empty() {
            bail!("empty aggregate store is not supported by vm_virtualize");
        }
        let aggregate = self
            .aggregates
            .get(&value_key(src_value))
            .cloned()
            .context("aggregate store source was not built by supported aggregate lowering")?;
        if aggregate.fields.len() != fields.len() {
            bail!(
                "aggregate store field count mismatch: value has {}, memory layout has {}",
                aggregate.fields.len(),
                fields.len()
            );
        }

        let ptr_value = instruction_operand_value(instruction, 1)?;
        let ptr = self.materialize_value(ptr_value)?;
        let (contract, store_semantic) = if is_volatile {
            ("llvm.memory.volatile.aggregate.store", HandlerSemantic::VolatileStore)
        } else {
            ("llvm.memory.aggregate.store", HandlerSemantic::Store)
        };
        let direct_store = self.emit_action_for_shape(
            contract,
            &store_semantic,
            &[("src", "%vf"), ("ptr", "%vp"), ("width", "field_width(%field)")],
        )?;
        let offset_store = self.emit_action_for_shape(
            contract,
            &store_semantic,
            &[("src", "%vf"), ("ptr", "%addr"), ("width", "field_width(%field)")],
        )?;
        let gep = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vp"), ("offset", "field_offset(%field)")],
        )?;

        for (index, field) in fields.into_iter().enumerate() {
            let source = aggregate
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("aggregate store field {index} is undef or unavailable"))?
                .binding;
            if source.width != field.info.width {
                bail!(
                    "aggregate store field {index} width mismatch: value is {}, memory field is {}",
                    source.width,
                    field.info.width
                );
            }
            let (field_ptr, store_action) = if field.offset == 0 {
                (ptr, &direct_store)
            } else {
                (
                    self.emit_memory_intrinsic_gep(contract, &gep, "%vp", ptr, "field_offset(%field)", field.offset)?,
                    &offset_store,
                )
            };
            let env = LoweringEnv::new()
                .binding("%vp", ptr)
                .binding("%addr", field_ptr)
                .binding("%vf", source)
                .imm("field_width(%field)", field.info.width as u64)
                .imm("field_offset(%field)", field.offset);
            self.emit_profile_action(store_action, &env)?;
        }

        Ok(())
    }

    fn lower_atomic_load(
        &mut self,
        instruction: InstructionValue<'ctx>,
        ordering: AtomicOrdering,
        is_volatile: bool,
    ) -> anyhow::Result<()> {
        ensure_default_atomic_syncscope(instruction, "load")?;
        let ptr = instruction_operand_value(instruction, 0)?;
        let width = instruction_result_width(instruction)?.context("atomic load result has no scalar width")?;
        ensure_atomic_load_store_value_type(instruction.get_type(), "load")?;
        ensure_naturally_aligned_atomic(instruction, "load", width)?;
        let ordering = atomic_ordering_for_load(ordering)?;
        let env = LoweringEnv::new()
            .llvm_source("%ptr", ptr)
            .llvm_value("%r", instruction_key(instruction))
            .imm("memory_width(%ptr)", width as u64)
            .imm("memory_ordering(%ptr)", ordering as u64);
        let (rule, semantic) = if is_volatile {
            ("llvm.atomic.volatile.load.scalar", HandlerSemantic::VolatileAtomicLoad)
        } else {
            ("llvm.atomic.load.scalar", HandlerSemantic::AtomicLoad)
        };
        self.execute_lowering_rule(rule, env, Some(semantic))?;
        Ok(())
    }

    fn lower_atomic_store(
        &mut self,
        instruction: InstructionValue<'ctx>,
        ordering: AtomicOrdering,
        is_volatile: bool,
    ) -> anyhow::Result<()> {
        ensure_default_atomic_syncscope(instruction, "store")?;
        let src_value = instruction_operand_value(instruction, 0)?;
        ensure_atomic_load_store_basic_value_type(src_value.get_type(), "store")?;
        let src = self.materialize_operand(instruction, 0)?;
        ensure_naturally_aligned_atomic(instruction, "store", src.width)?;
        let ordering = atomic_ordering_for_store(ordering)?;
        let ptr = instruction_operand_value(instruction, 1)?;
        let env = LoweringEnv::new()
            .llvm_source("%ptr", ptr)
            .binding("%value", src)
            .binding("%vv", src)
            .imm("memory_width(%ptr)", src.width as u64)
            .imm("memory_ordering(%ptr)", ordering as u64);
        let (rule, semantic) = if is_volatile {
            (
                "llvm.atomic.volatile.store.scalar",
                HandlerSemantic::VolatileAtomicStore,
            )
        } else {
            ("llvm.atomic.store.scalar", HandlerSemantic::AtomicStore)
        };
        self.execute_lowering_rule(rule, env, Some(semantic))?;
        Ok(())
    }

    fn lower_atomic_rmw(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let is_volatile = memory_is_volatile(instruction, "atomicrmw")?;
        ensure_default_atomic_syncscope(instruction, "atomicrmw")?;
        let ordering = memory_ordering(instruction, "atomicrmw")?;
        let op = instruction
            .get_atomic_rmw_bin_op()
            .context("atomicrmw operation cannot be read")?;
        let op = map_atomic_rmw_op(op)?;
        let ptr = instruction_operand_value(instruction, 0)?;
        let src_value = instruction_operand_value(instruction, 1)?;
        ensure_atomic_rmw_value_type(src_value.get_type(), op)?;
        let src = self.materialize_operand(instruction, 1)?;
        ensure_naturally_aligned_atomic(instruction, "atomicrmw", src.width)?;
        let ordering = atomic_ordering_for_rmw(ordering)?;
        let width = instruction_result_width(instruction)?.context("atomicrmw result has no scalar width")?;
        if width != src.width {
            bail!(
                "atomicrmw result width {} differs from operand width {}",
                width,
                src.width
            );
        }

        let env = LoweringEnv::new()
            .llvm_source("%ptr", ptr)
            .binding("%value", src)
            .binding("%vv", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("memory_width(%ptr)", width as u64)
            .imm("memory_ordering(%ptr)", ordering as u64);
        let (rule, semantic) = if is_volatile {
            (
                "llvm.atomic.volatile.rmw.scalar",
                HandlerSemantic::VolatileAtomicRmw(op),
            )
        } else {
            ("llvm.atomic.rmw.scalar", HandlerSemantic::AtomicRmw(op))
        };
        self.execute_lowering_rule(rule, env, Some(semantic))?;
        Ok(())
    }

    fn lower_cmpxchg(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let is_volatile = memory_is_volatile(instruction, "cmpxchg")?;
        ensure_default_atomic_syncscope(instruction, "cmpxchg")?;
        // LLVM weak cmpxchg 允许但不要求伪失败；runtime 发射 strong cmpxchg 是合法实现选择，
        // 同时避免为 weak bit 引入额外 profile ABI 和字节码 operand。

        let ptr = instruction_operand_value(instruction, 0)?;
        let cmp_value = instruction_operand_value(instruction, 1)?;
        let new_value = instruction_operand_value(instruction, 2)?;
        ensure_atomic_basic_value_type(cmp_value.get_type(), "cmpxchg")?;
        if cmp_value.get_type() != new_value.get_type() {
            bail!("cmpxchg compare and new value types differ");
        }
        let cmp = self.materialize_operand(instruction, 1)?;
        let new = self.materialize_operand(instruction, 2)?;
        if cmp.width != new.width {
            bail!(
                "cmpxchg compare width {} differs from new width {}",
                cmp.width,
                new.width
            );
        }
        ensure_naturally_aligned_atomic(instruction, "cmpxchg", cmp.width)?;
        // SAFETY: `instruction` 已由 opcode dispatch 限定为 cmpxchg；LLVM 这里只读取
        // success/failure ordering metadata。非法组合随后按 VMP 支持边界安全跳过。
        let success_ordering = unsafe { LLVMGetCmpXchgSuccessOrdering(instruction.as_value_ref()) }.into();
        // SAFETY: 同上，读取同一 live cmpxchg instruction 的 failure ordering metadata。
        let failure_ordering = unsafe { LLVMGetCmpXchgFailureOrdering(instruction.as_value_ref()) }.into();
        let success_ordering = cmpxchg_success_ordering(success_ordering)?;
        let failure_ordering = cmpxchg_failure_ordering(failure_ordering)?;
        ensure_cmpxchg_ordering(success_ordering, failure_ordering)?;

        let env = LoweringEnv::new()
            .llvm_source("%ptr", ptr)
            .binding("%vc", cmp)
            .binding("%vn", new)
            .imm("memory_width(%ptr)", cmp.width as u64)
            .imm("success_ordering(%ptr)", success_ordering as u64)
            .imm("failure_ordering(%ptr)", failure_ordering as u64);
        let (rule, semantic) = if is_volatile {
            ("llvm.volatile.cmpxchg.scalar", HandlerSemantic::VolatileCmpXchg)
        } else {
            ("llvm.cmpxchg.scalar", HandlerSemantic::CmpXchg)
        };
        let env = self.execute_lowering_rule(rule, env, Some(semantic))?;
        let old = match env.get("%old")? {
            LoweringValue::Reg(binding) => binding,
            LoweringValue::Imm(_) | LoweringValue::Label(_) => bail!("cmpxchg old result must be a register"),
        };
        let success = match env.get("%ok")? {
            LoweringValue::Reg(binding) => binding,
            LoweringValue::Imm(_) | LoweringValue::Label(_) => bail!("cmpxchg success result must be a register"),
        };
        if old.width != cmp.width {
            bail!(
                "cmpxchg old result width {} differs from compare width {}",
                old.width,
                cmp.width
            );
        }
        if success.width != 1 {
            bail!("cmpxchg success result must be i1, got i{}", success.width);
        }
        self.insert_aggregate_value(
            instruction_key(instruction),
            AggregateBinding {
                fields: vec![Some(AggregateField::owned(old)), Some(AggregateField::owned(success))],
            },
        );
        Ok(())
    }

    fn lower_fence(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        ensure_default_atomic_syncscope(instruction, "fence")?;
        let ordering = atomic_ordering_for_fence(memory_ordering(instruction, "fence")?)?;
        let env = LoweringEnv::new().imm("memory_ordering(%fence)", ordering as u64);
        self.execute_lowering_rule("llvm.fence", env, Some(HandlerSemantic::Fence))?;
        Ok(())
    }

    fn lower_gep(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let gep = GepInst::new(instruction);
        let base_value = gep
            .get_pointer_operand()
            .context("getelementptr has no pointer operand")?;
        let Some(offset) = gep.accumulate_constant_offset(self.module) else {
            let binding = self.ensure_result_binding(instruction)?;
            let base = self.materialize_value(base_value)?;
            return self.lower_dynamic_gep(instruction, gep, binding, base);
        };
        let env = LoweringEnv::new()
            .llvm_source("%base", base_value)
            .llvm_value("%r", instruction_key(instruction))
            .imm("constant_gep_offset(%r)", offset as u64);
        self.execute_lowering_rule("llvm.gep.constant", env, Some(HandlerSemantic::Gep))?;
        Ok(())
    }

    fn lower_dynamic_gep(
        &mut self,
        instruction: InstructionValue<'ctx>,
        gep: GepInst<'ctx>,
        dst: ValueBinding,
        base: ValueBinding,
    ) -> anyhow::Result<()> {
        let terms = self.gep_dynamic_terms(instruction, gep)?;
        let mut address = base.reg;

        // dynamic GEP 被拆成：base + sum(index_i * element_size_i) + constant_offset。
        // 每个乘加都必须通过 profile 中的 llvm.gep.dynamic rule emit，不能在这里绕过 ISA。
        if terms.dynamic.is_empty() {
            if terms.constant_offset != 0 {
                let gep_action = self.emit_action_for_shape(
                    "llvm.gep.constant",
                    &HandlerSemantic::Gep,
                    &[("dst", "%vr"), ("base", "%vb"), ("offset", "constant_gep_offset(%r)")],
                )?;
                let env = LoweringEnv::new()
                    .reg("%vr", dst.reg, 64)
                    .reg("%vb", address, 64)
                    .imm("constant_gep_offset(%r)", terms.constant_offset as u64);
                self.emit_profile_action(&gep_action, &env)?;
            } else if address != dst.reg {
                let action = self.emit_action_for_shape(
                    "llvm.phi.edge_move",
                    &HandlerSemantic::Mov,
                    &[("dst", "%vr"), ("src", "%vi"), ("width", "type_width(%r)")],
                )?;
                let env = LoweringEnv::new()
                    .reg("%vr", dst.reg, 64)
                    .reg("%vi", address, 64)
                    .imm("type_width(%r)", 64);
                self.emit_profile_action(&action, &env)?;
            }
            return Ok(());
        }

        let dynamic_len = terms.dynamic.len();
        let mul_action = self.emit_action_for_shape(
            "llvm.gep.dynamic",
            &HandlerSemantic::Bin(BinOp::Mul),
            &[
                ("dst", "%vs"),
                ("lhs", "%vi"),
                ("rhs", "element_size(%base)"),
                ("width", "64"),
            ],
        )?;
        let add_action = self.emit_action_for_shape(
            "llvm.gep.dynamic",
            &HandlerSemantic::Bin(BinOp::Add),
            &[("dst", "%vr"), ("lhs", "%vb"), ("rhs", "%vs"), ("width", "64")],
        )?;
        let gep_action = self.emit_action_for_shape(
            "llvm.gep.dynamic",
            &HandlerSemantic::Gep,
            &[("dst", "%vr"), ("base", "%vr"), ("offset", "constant_gep_offset(%r)")],
        )?;
        for (term_index, (index, scale)) in terms.dynamic.into_iter().enumerate() {
            let scale_reg = self.alloc_temporary_vreg()?;
            self.push_constant(scale_reg, scale, 64)?;
            let scaled = self.alloc_temporary_vreg()?;
            let mul_env = LoweringEnv::new()
                .binding("%vi", index)
                .reg("%vs", scaled, 64)
                .reg("element_size(%base)", scale_reg, 64)
                .imm("64", 64);
            self.emit_profile_action(&mul_action, &mul_env)?;
            let is_last = term_index + 1 == dynamic_len;
            let sum = if is_last { dst.reg } else { self.alloc_temporary_vreg()? };
            let add_env = LoweringEnv::new()
                .reg("%vb", address, 64)
                .reg("%vs", scaled, 64)
                .reg("%vr", sum, 64)
                .imm("64", 64);
            self.emit_profile_action(&add_action, &add_env)?;
            address = sum;
        }
        if terms.constant_offset != 0 {
            let env = LoweringEnv::new()
                .reg("%vr", dst.reg, 64)
                .reg("%vb", address, 64)
                .imm("constant_gep_offset(%r)", terms.constant_offset as u64);
            self.emit_profile_action(&gep_action, &env)?;
        }
        Ok(())
    }

    fn lower_call(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let call = CallInst::new(instruction);
        let Some(callee) = call.get_call_function() else {
            return self.lower_indirect_call(instruction);
        };
        if let Some(kind) = memory_intrinsic_kind(callee) {
            return self.lower_memory_intrinsic(instruction, kind);
        }
        if sideeffect_intrinsic(callee) {
            return self.lower_sideeffect_intrinsic(instruction);
        }
        if let Some(kind) = nop_intrinsic_kind(callee) {
            return self.lower_nop_intrinsic(instruction, kind);
        }
        if let Some(kind) = identity_intrinsic_kind(callee) {
            if kind == IdentityIntrinsicKind::ThreadLocalAddress {
                return self.lower_threadlocal_address_intrinsic(instruction, callee);
            }
            return self.lower_identity_intrinsic(instruction, kind);
        }
        if let Some(kind) = pointer_intrinsic_kind(callee) {
            return self.lower_pointer_intrinsic(instruction, kind);
        }
        if let Some(kind) = compile_time_intrinsic_kind(callee) {
            return self.lower_compile_time_intrinsic(instruction, kind);
        }
        if let Some(kind) = float_intrinsic_kind(callee) {
            return self.lower_float_intrinsic(instruction, kind);
        }
        if let Some(kind) = integer_intrinsic_kind(callee) {
            return match kind.arity() {
                1 => self.lower_integer_intrinsic(instruction, kind),
                2 if kind.is_overflow_intrinsic() => self.lower_integer_overflow_intrinsic(instruction, kind),
                2 if kind.is_binary_intrinsic() => self.lower_integer_binary_intrinsic(instruction, kind),
                2 => self.lower_integer_flagged_unary_intrinsic(instruction, kind),
                3 => self.lower_integer_ternary_intrinsic(instruction, kind),
                _ => bail!("unsupported integer intrinsic arity {}", kind.arity()),
            };
        }
        if let Some(kind) = counter_intrinsic_kind(callee) {
            return self.lower_counter_intrinsic(instruction, kind);
        }
        if let Some(kind) = trap_intrinsic_kind(callee) {
            return self.lower_trap_intrinsic(instruction, kind);
        }
        if callee.get_intrinsic_id() != 0 || callee.is_llvm_function() {
            bail!("LLVM intrinsic calls are not supported by vm_virtualize");
        }

        let target = native_call_target_for_direct_call(callee, instruction)?;
        let args = self.materialize_native_call_args(instruction, &target.params, 0)?;
        let call_action = self.emit_action_for_shape(
            "llvm.call.direct",
            &HandlerSemantic::CallNative,
            &[
                ("argc", "arg_count(%callee)"),
                ("arg0", "arg0"),
                ("ret_count", "return_count(%callee)"),
            ],
        )?;
        self.emit_native_bridge_call(instruction, target, args, &call_action)
    }

    fn lower_indirect_call(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let call_type_ref = unsafe { LLVMGetCalledFunctionType(instruction.as_value_ref()) };
        if call_type_ref.is_null() {
            bail!("indirect call function type is unavailable");
        }
        let call_type = unsafe { FunctionType::new(call_type_ref) };
        if call_type.is_var_arg() {
            bail!("varargs indirect calls are not supported by vm_virtualize");
        }
        ensure_supported_indirect_call_type(call_type)?;

        let callee_ref = unsafe { LLVMGetCalledValue(instruction.as_value_ref()) };
        if callee_ref.is_null() {
            bail!("indirect call callee operand is unavailable");
        }
        let callee = unsafe { BasicValueEnum::new(callee_ref) };
        let callee = self.materialize_value(callee)?;
        if callee.width != 64 {
            bail!("indirect call callee pointer must materialize as i64");
        }

        let adapter = self.emit_indirect_call_adapter(call_type)?;
        let target = native_call_target(adapter)?;
        let mut args = Vec::with_capacity(target.param_widths.len());
        args.push(callee);
        args.extend(self.materialize_native_call_args(instruction, &target.params[1..], 0)?);

        let call_action = self.emit_action_for_shape(
            "llvm.call.indirect",
            &HandlerSemantic::CallNative,
            &[
                ("argc", "arg_count(%callee)"),
                ("arg0", "arg0"),
                ("ret_count", "return_count(%callee)"),
            ],
        )?;
        self.emit_native_bridge_call(instruction, target, args, &call_action)
    }

    fn materialize_native_call_args(
        &mut self,
        instruction: InstructionValue<'ctx>,
        params: &[FunctionParamSlots],
        operand_offset: u32,
    ) -> anyhow::Result<Vec<ValueBinding>> {
        let mut args = Vec::new();
        for (index, slots) in params.iter().enumerate() {
            let operand_index = operand_offset + index as u32;
            let value = instruction_basic_operand(instruction, operand_index)
                .with_context(|| format!("missing native call argument {index}"))?;
            if !matches!(
                value.get_type(),
                BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_)
            ) {
                if slots.fields.len() != 1 {
                    bail!(
                        "native scalar argument {index} unexpectedly maps to {} fields",
                        slots.fields.len()
                    );
                }
                args.push(self.materialize_value(value)?);
                continue;
            }

            let binding = self
                .aggregate_operand(instruction, operand_index)
                .with_context(|| format!("native aggregate argument {index}"))?;
            if binding.fields.len() != slots.fields.len() {
                bail!(
                    "native aggregate argument {index} field count mismatch: signature has {}, operand has {}",
                    slots.fields.len(),
                    binding.fields.len()
                );
            }
            for (field_index, (field, expected)) in binding.fields.iter().zip(slots.fields.iter()).enumerate() {
                let field = field
                    .as_ref()
                    .with_context(|| format!("native aggregate argument {index} field {field_index} is undef"))?;
                if field.binding.width != expected.width {
                    bail!(
                        "native aggregate argument {index} field {field_index} width mismatch: value is {}, callee expects {}",
                        field.binding.width,
                        expected.width
                    );
                }
                args.push(field.binding);
            }
        }
        Ok(args)
    }

    fn emit_indirect_call_adapter(&mut self, call_type: FunctionType<'ctx>) -> anyhow::Result<FunctionValue<'ctx>> {
        let ctx = self.module.get_context();
        let direct_param_types = call_type.get_param_types();
        let mut adapter_param_types = Vec::with_capacity(direct_param_types.len() + 1);
        adapter_param_types.push(ctx.ptr_type(AddressSpace::default()).into());
        adapter_param_types.extend(direct_param_types.iter().copied());
        let adapter_type = match call_type.get_return_type() {
            Some(return_type) => return_type.fn_type(&adapter_param_types, false),
            None => ctx.void_type().fn_type(&adapter_param_types, false),
        };
        let function_name = self.function.get_name().to_str().unwrap_or("anon");
        let adapter = self.module.add_function(
            &format!(
                ".amice.vm.indirect_adapter.{}.{}",
                translator_symbol_suffix(function_name),
                self.native_calls.len()
            ),
            adapter_type,
            Some(Linkage::Private),
        );
        adapter.as_global_value().set_unnamed_address(UnnamedAddress::Global);

        let entry = ctx.append_basic_block(adapter, "entry");
        let builder = ctx.create_builder();
        builder.position_at_end(entry);
        let callee = adapter
            .get_nth_param(0)
            .ok_or_else(|| anyhow::anyhow!("missing indirect adapter callee parameter"))?
            .into_pointer_value();
        let args = direct_param_types
            .iter()
            .enumerate()
            .map(|(index, _)| {
                adapter
                    .get_nth_param((index + 1) as u32)
                    .ok_or_else(|| anyhow::anyhow!("missing indirect adapter argument {index}"))
                    .map(Into::into)
            })
            .collect::<anyhow::Result<Vec<BasicMetadataValueEnum<'ctx>>>>()?;
        let call = builder.build_indirect_call(call_type, callee, &args, "amice.vm.indirect.target")?;
        match call_type.get_return_type() {
            Some(_) => {
                let ret = call
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| anyhow::anyhow!("indirect adapter call should return a value"))?;
                builder.build_return(Some(&ret))?;
            },
            None => {
                builder.build_return(None)?;
            },
        }
        self.module.append_to_compiler_used(adapter.as_global_value());
        Ok(adapter)
    }

    fn emit_native_bridge_call(
        &mut self,
        instruction: InstructionValue<'ctx>,
        target: NativeCallTarget<'ctx>,
        args: Vec<ValueBinding>,
        call_action: &LoweringAction,
    ) -> anyhow::Result<()> {
        if args.len() != target.param_widths.len() {
            bail!(
                "native call argument count mismatch: materialized {}, callee expects {}",
                args.len(),
                target.param_widths.len()
            );
        }
        if target.param_widths.len() > self.native_arg_registers.len() {
            bail!(
                "profile native_call ABI maps {} argument registers but callee needs {}",
                self.native_arg_registers.len(),
                target.param_widths.len()
            );
        }
        if target.return_fields.len() > self.native_return_registers.len() {
            bail!(
                "profile native_call ABI maps {} return registers but callee needs {}",
                self.native_return_registers.len(),
                target.return_fields.len()
            );
        }

        let final_returns = self.native_call_final_returns(instruction, &target)?;
        let result_regs = final_returns.iter().map(|binding| binding.reg).collect::<HashSet<_>>();

        for (index, value) in args.iter().enumerate() {
            if value.width != target.param_widths[index] {
                bail!(
                    "native call argument {index} width mismatch: value is {}, callee expects {}",
                    value.width,
                    target.param_widths[index]
                );
            }
        }
        // `native_call` 是 profile 声明的 ABI 边界。把参数移动到 call register 可能覆盖无关的
        // VM SSA 值，因此 translator 会保存 profile 声明 native bridge 可能触碰的所有已定义 x
        // 寄存器，但排除此调用结果值拥有的寄存器。
        let saved = self.save_native_touched_registers(&result_regs)?;

        let arg_moves = args
            .iter()
            .enumerate()
            .map(|(index, value)| RegisterMove {
                dst: self.native_arg_registers[index],
                src: *value,
            })
            .collect::<Vec<_>>();
        self.emit_parallel_register_moves(arg_moves)?;

        let call_id = u16::try_from(self.native_calls.len()).context("native call table has too many entries")?;
        // `call_native` handler 的 record 已经携带 retN 目标寄存器。这里直接把 native thunk
        // 返回 tuple 写进最终 SSA 结果寄存器，避免生成紧跟在 call 后面的 VM 内部返回槽 copy。
        // native_return_registers 仍在上面的 ABI 覆盖检查中约束 profile 声明的返回槽容量。
        let call_return_slots = final_returns
            .iter()
            .map(|binding| NativeReturn {
                dst: binding.reg,
                width: binding.width,
            })
            .collect::<Vec<_>>();
        self.native_calls.push(target);
        let mut env = LoweringEnv::new()
            .imm("native_id(%callee)", call_id as u64)
            .imm("callee", call_id as u64)
            .imm("arg_count(%callee)", args.len() as u64)
            .imm("argc", args.len() as u64)
            .imm("return_count(%callee)", call_return_slots.len() as u64)
            .imm("ret_count", call_return_slots.len() as u64);
        for index in 0..NATIVE_CALL_MAX_ARGS {
            let reg = self.native_arg_registers.get(index).copied().unwrap_or(0);
            env = env.reg(format!("arg{index}"), reg, 64);
        }
        for index in 0..NATIVE_CALL_MAX_RETURNS {
            let ret = call_return_slots
                .get(index)
                .copied()
                .unwrap_or(NativeReturn { dst: 0, width: 64 });
            env = env
                .reg(format!("ret{index}"), ret.dst, ret.width)
                .imm(format!("ret{index}_width"), ret.width as u64);
        }
        self.emit_profile_action(call_action, &env)?;

        if final_returns.len() > 1 {
            self.insert_aggregate_value(
                instruction_key(instruction),
                AggregateBinding {
                    fields: final_returns.into_iter().map(AggregateField::owned).map(Some).collect(),
                },
            );
        }
        self.restore_native_touched_registers(saved, &result_regs)?;
        Ok(())
    }

    fn lower_integer_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: IntegerIntrinsicKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 1 {
            bail!("integer intrinsic {:?} expects exactly one argument", kind);
        }
        let src = self.materialize_operand(instruction, 0)?;
        let width = instruction_result_width(instruction)?.context("integer intrinsic result has no scalar width")?;
        if src.width != width {
            bail!(
                "integer intrinsic result width {} differs from operand width {}",
                width,
                src.width
            );
        }
        checked_intrinsic_integer_width(u64::from(width))?;

        let env = LoweringEnv::new()
            .binding("%value", src)
            .binding("%src", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_integer_flagged_unary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: IntegerIntrinsicKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 2 {
            bail!("integer intrinsic {:?} expects exactly two arguments", kind);
        }
        let flag_name = match kind {
            IntegerIntrinsicKind::CtLz | IntegerIntrinsicKind::CtTz => "is_zero_undef",
            IntegerIntrinsicKind::Abs => "is_int_min_poison",
            IntegerIntrinsicKind::CtPop
            | IntegerIntrinsicKind::SMax
            | IntegerIntrinsicKind::SMin
            | IntegerIntrinsicKind::UMax
            | IntegerIntrinsicKind::UMin
            | IntegerIntrinsicKind::UAddSat
            | IntegerIntrinsicKind::USubSat
            | IntegerIntrinsicKind::SAddSat
            | IntegerIntrinsicKind::SSubSat
            | IntegerIntrinsicKind::UShlSat
            | IntegerIntrinsicKind::SShlSat
            | IntegerIntrinsicKind::UAddOverflow
            | IntegerIntrinsicKind::SAddOverflow
            | IntegerIntrinsicKind::USubOverflow
            | IntegerIntrinsicKind::SSubOverflow
            | IntegerIntrinsicKind::UMulOverflow
            | IntegerIntrinsicKind::SMulOverflow
            | IntegerIntrinsicKind::BSwap
            | IntegerIntrinsicKind::BitReverse
            | IntegerIntrinsicKind::FShl
            | IntegerIntrinsicKind::FShr => bail!("integer intrinsic {:?} does not use a poison flag", kind),
        };
        let poison_flag = constant_int_operand(instruction, 1, &format!("integer intrinsic {flag_name} flag"))?;
        if poison_flag > 1 {
            bail!("integer intrinsic {flag_name} flag must be an i1 constant");
        }
        // `true` 只收窄 LLVM 定义域；被排除输入仍沿用 poison/UB 边界，handler 复用同一套计算。

        let src = self.materialize_operand(instruction, 0)?;
        let width = instruction_result_width(instruction)?.context("integer intrinsic result has no scalar width")?;
        if src.width != width {
            bail!(
                "integer intrinsic result width {} differs from operand width {}",
                width,
                src.width
            );
        }
        checked_intrinsic_integer_width(u64::from(width))?;

        let env = LoweringEnv::new()
            .binding("%value", src)
            .binding("%src", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_integer_binary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: IntegerIntrinsicKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 2 {
            bail!("integer intrinsic {:?} expects exactly two arguments", kind);
        }
        let lhs = self.materialize_operand(instruction, 0)?;
        let rhs = self.materialize_operand(instruction, 1)?;
        let width = instruction_result_width(instruction)?.context("integer intrinsic result has no scalar width")?;
        for (name, value) in [("lhs", lhs), ("rhs", rhs)] {
            if value.width != width {
                bail!(
                    "integer intrinsic result width {} differs from {name} operand width {}",
                    width,
                    value.width
                );
            }
        }
        checked_intrinsic_integer_width(u64::from(width))?;

        let env = LoweringEnv::new()
            .binding("%a", lhs)
            .binding("%b", rhs)
            .binding("%lhs", lhs)
            .binding("%rhs", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_integer_overflow_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: IntegerIntrinsicKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 2 {
            bail!("integer overflow intrinsic {:?} expects exactly two arguments", kind);
        }
        let AnyTypeEnum::StructType(return_type) = instruction.get_type() else {
            bail!("integer overflow intrinsic must return a two-field struct");
        };
        let fields = return_type
            .get_field_types()
            .into_iter()
            .enumerate()
            .map(|(index, ty)| return_field_from_type(ty).with_context(|| format!("overflow return field {index}")))
            .collect::<anyhow::Result<Vec<_>>>()?;
        if fields.len() != 2 {
            bail!("integer overflow intrinsic must return exactly two fields");
        }
        if fields[0].kind != ScalarKind::Integer || fields[1].kind != ScalarKind::Integer {
            bail!("integer overflow intrinsic fields must be integer scalars");
        }
        if fields[1].width != 1 {
            bail!("integer overflow intrinsic flag field must be i1");
        }

        let lhs = self.materialize_operand(instruction, 0)?;
        let rhs = self.materialize_operand(instruction, 1)?;
        let width = fields[0].width;
        for (name, value) in [("lhs", lhs), ("rhs", rhs)] {
            if value.width != width {
                bail!(
                    "integer overflow intrinsic value width {} differs from {name} operand width {}",
                    width,
                    value.width
                );
            }
        }
        checked_intrinsic_integer_width(u64::from(width))?;

        let env = LoweringEnv::new()
            .binding("%a", lhs)
            .binding("%b", rhs)
            .binding("%lhs", lhs)
            .binding("%rhs", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        let env = self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        let LoweringValue::Reg(value) = env.get("%vr")? else {
            bail!("integer overflow lowering must define %vr as the value result register");
        };
        let LoweringValue::Reg(overflow) = env.get("%vo")? else {
            bail!("integer overflow lowering must define %vo as the overflow flag register");
        };
        if value.width != width {
            bail!(
                "integer overflow lowering value register width {} differs from result width {}",
                value.width,
                width
            );
        }
        if overflow.width != 1 {
            bail!(
                "integer overflow lowering flag register width {} differs from i1",
                overflow.width
            );
        }
        self.insert_aggregate_value(
            instruction_key(instruction),
            AggregateBinding {
                fields: vec![
                    Some(AggregateField::owned(value)),
                    Some(AggregateField::owned(overflow)),
                ],
            },
        );
        Ok(())
    }

    fn lower_integer_ternary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: IntegerIntrinsicKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 3 {
            bail!("integer intrinsic {:?} expects exactly three arguments", kind);
        }
        let lhs = self.materialize_operand(instruction, 0)?;
        let rhs = self.materialize_operand(instruction, 1)?;
        let third = self.materialize_operand(instruction, 2)?;
        let width = instruction_result_width(instruction)?.context("integer intrinsic result has no scalar width")?;
        for (name, value) in [("lhs", lhs), ("rhs", rhs), ("third", third)] {
            if value.width != width {
                bail!(
                    "integer intrinsic result width {} differs from {name} operand width {}",
                    width,
                    value.width
                );
            }
        }
        checked_intrinsic_integer_width(u64::from(width))?;

        let env = LoweringEnv::new()
            .binding("%a", lhs)
            .binding("%b", rhs)
            .binding("%shift", third)
            .binding("%lhs", lhs)
            .binding("%rhs", rhs)
            .binding("%third", third)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_counter_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: CounterKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 0 {
            bail!("counter intrinsic {kind:?} expects exactly 0 arguments, got {actual_args}");
        }
        let width = instruction_result_width(instruction)?.context("counter intrinsic result has no scalar width")?;
        if width != 64 {
            bail!("counter intrinsic {kind:?} must return i64, got i{width}");
        }

        let env = LoweringEnv::new()
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64)
            .imm("width", width as u64);
        self.execute_lowering_rule(
            counter_intrinsic_lowering_rule(kind),
            env,
            Some(HandlerSemantic::ReadCounter(kind)),
        )?;
        Ok(())
    }

    fn lower_sideeffect_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 0 {
            bail!("llvm.sideeffect expects exactly 0 arguments, got {actual_args}");
        }
        self.execute_lowering_rule("llvm.sideeffect", LoweringEnv::new(), Some(HandlerSemantic::SideEffect))?;
        Ok(())
    }

    fn lower_memory_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: MemoryIntrinsicKind,
    ) -> anyhow::Result<()> {
        match kind {
            MemoryIntrinsicKind::Memcpy | MemoryIntrinsicKind::Memmove => {
                self.lower_memory_copy_intrinsic(instruction, kind)
            },
            MemoryIntrinsicKind::Memset => self.lower_memset_intrinsic(instruction),
        }
    }

    fn lower_nop_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: NopIntrinsicKind,
    ) -> anyhow::Result<()> {
        if let Some(expected) = kind.checked_arg_count() {
            let actual = instruction.get_num_operands().saturating_sub(1);
            if actual != expected {
                bail!(
                    "nop intrinsic {:?} expects exactly {expected} arguments, got {actual}",
                    kind
                );
            }
        }
        for &index in kind.constant_operand_indices() {
            let _ = constant_int_operand(instruction, index, &format!("nop intrinsic {:?} operand {index}", kind))?;
        }
        for &index in kind.pointer_operand_indices() {
            let value = instruction_operand_value(instruction, index)?;
            if !value.is_pointer_value() {
                bail!("nop intrinsic {:?} operand {index} must be a pointer", kind);
            }
        }
        let env = LoweringEnv::new();
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(HandlerSemantic::Nop))?;
        Ok(())
    }

    fn lower_identity_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: IdentityIntrinsicKind,
    ) -> anyhow::Result<()> {
        let expected_args = kind.arg_count();
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != expected_args {
            bail!(
                "identity intrinsic {:?} expects exactly {expected_args} arguments, got {actual_args}",
                kind
            );
        }

        if let Some(index) = kind.constant_length_operand_index() {
            let _ = constant_int_operand(instruction, index, &format!("identity intrinsic {:?} length", kind))?;
        }

        let value_operand_index = kind.value_operand_index();
        let src = self.materialize_operand(instruction, value_operand_index)?;
        let width = instruction_result_width(instruction)?.context("identity intrinsic result has no scalar width")?;
        if kind.is_scalar_copy() {
            let source = instruction_operand_value(instruction, value_operand_index)?;
            ensure_scalar_copy_shape(source, instruction.get_type(), kind)?;
            if src.width != width {
                bail!(
                    "identity intrinsic {:?} scalar copy width mismatch: result i{}, value i{}",
                    kind,
                    width,
                    src.width
                );
            }
        } else if kind.is_expect_hint() {
            let expected = instruction_operand_value(instruction, 1)?;
            let expected_width = value_width(expected)?;
            if src.width != width || expected_width != width {
                bail!(
                    "identity intrinsic {:?} width mismatch: result i{}, value i{}, expected i{}",
                    kind,
                    width,
                    src.width,
                    expected_width
                );
            }
        } else if kind.is_pointer_identity() {
            let source = instruction_operand_value(instruction, value_operand_index)?;
            if !source.is_pointer_value() || !matches!(instruction.get_type(), AnyTypeEnum::PointerType(_)) {
                bail!(
                    "identity intrinsic {:?} expects a pointer argument and pointer result",
                    kind
                );
            }
            if src.width != 64 || width != 64 {
                bail!(
                    "identity intrinsic {:?} pointer width mismatch: result i{}, value i{}",
                    kind,
                    width,
                    src.width
                );
            }
        } else if kind.is_integer_identity() {
            let source = instruction_operand_value(instruction, value_operand_index)?;
            if !source.is_int_value() || !matches!(instruction.get_type(), AnyTypeEnum::IntType(_)) {
                bail!(
                    "identity intrinsic {:?} expects an integer argument and integer result",
                    kind
                );
            }
            if src.width != width {
                bail!(
                    "identity intrinsic {:?} integer width mismatch: result i{}, value i{}",
                    kind,
                    width,
                    src.width
                );
            }
        }

        let env = LoweringEnv::new()
            .binding("%value", src)
            .binding("%src", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(HandlerSemantic::Mov))?;
        Ok(())
    }

    fn lower_threadlocal_address_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        callee: FunctionValue<'ctx>,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!("threadlocal.address intrinsic expects exactly 1 argument, got {actual_args}");
        }

        let source = instruction_operand_value(instruction, 0)?;
        let BasicValueEnum::PointerValue(global_ptr) = source else {
            bail!("threadlocal.address intrinsic expects a pointer global argument");
        };
        if !matches!(instruction.get_type(), AnyTypeEnum::PointerType(_)) {
            bail!("threadlocal.address intrinsic must return a pointer");
        }
        // LLVM 要求 operand 0 是 GlobalValue。把这个 operand 留在原生 thunk 里，
        // 避免把 TLS 模型下的符号地址错误固化进 VM const_pool。
        if unsafe { LLVMIsAGlobalValue(global_ptr.as_value_ref()) }.is_null() {
            bail!("threadlocal.address intrinsic operand must be a GlobalValue");
        }

        let thunk = self.emit_threadlocal_address_thunk(callee, global_ptr)?;
        let target = native_call_target(thunk)?;
        let call_result = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 64,
        };
        self.emit_no_arg_native_call_result(target, call_result)?;

        let width = instruction_result_width(instruction)?
            .context("threadlocal.address intrinsic result has no pointer width")?;
        if width != 64 {
            bail!("threadlocal.address intrinsic pointer width i{width} is not supported by vm_virtualize");
        }

        let env = LoweringEnv::new()
            .binding("%value", call_result)
            .binding("%src", call_result)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(
            IdentityIntrinsicKind::ThreadLocalAddress.lowering_rule(),
            env,
            Some(HandlerSemantic::Mov),
        )?;
        Ok(())
    }

    fn emit_threadlocal_address_thunk(
        &mut self,
        callee: FunctionValue<'ctx>,
        global_ptr: PointerValue<'ctx>,
    ) -> anyhow::Result<FunctionValue<'ctx>> {
        let ctx = self.module.get_context();
        let return_type = match callee.get_type().get_return_type() {
            Some(BasicTypeEnum::PointerType(return_type)) => return_type,
            _ => bail!("threadlocal.address thunk target must return a pointer"),
        };
        let thunk_type = return_type.fn_type(&[], false);
        let function_name = self.function.get_name().to_str().unwrap_or("anon");
        let thunk = self.module.add_function(
            &format!(
                ".amice.vm.tls_addr.{}.{}",
                translator_symbol_suffix(function_name),
                self.native_calls.len()
            ),
            thunk_type,
            Some(Linkage::Private),
        );
        thunk.as_global_value().set_unnamed_address(UnnamedAddress::Global);

        let entry = ctx.append_basic_block(thunk, "entry");
        let builder = ctx.create_builder();
        builder.position_at_end(entry);
        let args: [BasicMetadataValueEnum<'ctx>; 1] = [global_ptr.into()];
        let call = builder.build_call(callee, &args, "amice.vm.tls.addr")?;
        let ret = call
            .try_as_basic_value()
            .basic()
            .context("threadlocal.address thunk call should return a pointer")?;
        builder.build_return(Some(&ret))?;
        Ok(thunk)
    }

    fn emit_no_arg_native_call_result(
        &mut self,
        target: NativeCallTarget<'ctx>,
        result: ValueBinding,
    ) -> anyhow::Result<()> {
        if !target.param_widths.is_empty() {
            bail!("no-arg native bridge target unexpectedly has parameters");
        }
        if target.returns_void || target.return_fields.len() != 1 {
            bail!("no-arg native bridge target must return exactly one scalar value");
        }
        let field = target.return_fields[0];
        if field.width != result.width {
            bail!(
                "no-arg native bridge return width mismatch: destination is {}, callee returns {}",
                result.width,
                field.width
            );
        }
        if target.return_fields.len() > self.native_return_registers.len() {
            bail!(
                "profile native_call ABI maps {} return registers but callee needs {}",
                self.native_return_registers.len(),
                target.return_fields.len()
            );
        }

        let call_action = self.emit_action_for_shape(
            "llvm.call.direct",
            &HandlerSemantic::CallNative,
            &[
                ("argc", "arg_count(%callee)"),
                ("arg0", "arg0"),
                ("ret_count", "return_count(%callee)"),
            ],
        )?;
        let result_regs = HashSet::from([result.reg]);
        let saved = self.save_native_touched_registers(&result_regs)?;
        let call_id = u16::try_from(self.native_calls.len()).context("native call table has too many entries")?;
        self.native_calls.push(target);

        let mut env = LoweringEnv::new()
            .imm("native_id(%callee)", call_id as u64)
            .imm("callee", call_id as u64)
            .imm("arg_count(%callee)", 0)
            .imm("argc", 0)
            .imm("return_count(%callee)", 1)
            .imm("ret_count", 1);
        for index in 0..NATIVE_CALL_MAX_ARGS {
            let reg = self.native_arg_registers.get(index).copied().unwrap_or(0);
            env = env.reg(format!("arg{index}"), reg, 64);
        }
        for index in 0..NATIVE_CALL_MAX_RETURNS {
            let ret = if index == 0 {
                NativeReturn {
                    dst: result.reg,
                    width: result.width,
                }
            } else {
                NativeReturn { dst: 0, width: 64 }
            };
            env = env
                .reg(format!("ret{index}"), ret.dst, ret.width)
                .imm(format!("ret{index}_width"), ret.width as u64);
        }
        self.emit_profile_action(&call_action, &env)?;
        self.restore_native_touched_registers(saved, &result_regs)?;
        Ok(())
    }

    fn lower_pointer_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: PointerIntrinsicKind,
    ) -> anyhow::Result<()> {
        let expected_args = kind.arg_count();
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != expected_args {
            bail!(
                "pointer intrinsic {:?} expects exactly {expected_args} arguments, got {actual_args}",
                kind
            );
        }

        match kind {
            PointerIntrinsicKind::PtrMask => {
                let ptr_value = instruction_operand_value(instruction, 0)?;
                let mask_value = instruction_operand_value(instruction, 1)?;
                if !ptr_value.is_pointer_value() || !matches!(instruction.get_type(), AnyTypeEnum::PointerType(_)) {
                    bail!("ptrmask intrinsic expects a pointer argument and pointer result");
                }
                if !mask_value.is_int_value() {
                    bail!("ptrmask intrinsic mask must be an integer");
                }
                let mask_width = value_width(mask_value)?;
                if mask_width != 64 {
                    bail!("ptrmask intrinsic mask width i{mask_width} is not supported by vm_virtualize");
                }

                let ptr = self.materialize_operand(instruction, 0)?;
                let mask = self.materialize_operand(instruction, 1)?;
                let width =
                    instruction_result_width(instruction)?.context("ptrmask intrinsic result has no scalar width")?;
                if ptr.width != 64 || mask.width != 64 || width != 64 {
                    bail!(
                        "ptrmask intrinsic width mismatch: result i{}, pointer i{}, mask i{}",
                        width,
                        ptr.width,
                        mask.width
                    );
                }

                let env = LoweringEnv::new()
                    .binding("%ptr", ptr)
                    .binding("%mask", mask)
                    .binding("%value", ptr)
                    .binding("%src", ptr)
                    .llvm_value("%r", instruction_key(instruction))
                    .imm("type_width(%r)", width as u64);
                self.execute_lowering_rule(kind.lowering_rule(), env, Some(HandlerSemantic::Bin(BinOp::And)))?;
            },
        }
        Ok(())
    }

    fn lower_compile_time_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: CompileTimeIntrinsicKind,
    ) -> anyhow::Result<()> {
        if matches!(kind, CompileTimeIntrinsicKind::ObjectSize) {
            return self.lower_objectsize_intrinsic(instruction);
        }

        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!(
                "compile-time intrinsic {:?} expects exactly 1 argument, got {actual_args}",
                kind
            );
        }

        let width =
            instruction_result_width(instruction)?.context("compile-time intrinsic result has no scalar width")?;
        if width != 1 {
            bail!("compile-time intrinsic {:?} must return i1, got i{width}", kind);
        }

        let operand =
            instruction_basic_operand(instruction, 0).context("compile-time intrinsic missing value operand 0")?;
        let value = kind.result(operand)?;
        let dst = self.ensure_result_binding(instruction)?;
        self.push_constant(dst.reg, value, width)?;
        Ok(())
    }

    fn lower_objectsize_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.objectsize expects exactly 4 arguments, got {actual_args}");
        }
        for index in 1..=3 {
            let flag = constant_int_operand(instruction, index, &format!("llvm.objectsize immarg {index}"))?;
            if flag > 1 {
                bail!("llvm.objectsize immarg {index} must be i1");
            }
        }

        let width = instruction_result_width(instruction)?.context("llvm.objectsize result has no scalar width")?;
        let ptr = instruction_basic_operand(instruction, 0).context("llvm.objectsize missing pointer operand")?;
        let size = self.static_object_size(ptr)?;
        if width < 64 && u128::from(size) >= (1_u128 << width) {
            bail!("llvm.objectsize result {size} does not fit in i{width}");
        }

        let dst = self.ensure_result_binding(instruction)?;
        let env = LoweringEnv::new()
            .reg("%vr", dst.reg, width)
            .llvm_value("%r", instruction_key(instruction))
            .imm("object_size(%ptr)", size)
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule("llvm.objectsize.integer", env, Some(HandlerSemantic::MovImm))?;
        Ok(())
    }

    fn lower_float_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        match kind {
            FloatIntrinsicKind::FAbs
            | FloatIntrinsicKind::Sqrt
            | FloatIntrinsicKind::Canonicalize
            | FloatIntrinsicKind::Floor
            | FloatIntrinsicKind::Ceil
            | FloatIntrinsicKind::Trunc
            | FloatIntrinsicKind::Rint
            | FloatIntrinsicKind::NearbyInt
            | FloatIntrinsicKind::Round
            | FloatIntrinsicKind::RoundEven => self.lower_float_unary_intrinsic(instruction, kind),
            FloatIntrinsicKind::MinNum
            | FloatIntrinsicKind::MaxNum
            | FloatIntrinsicKind::Minimum
            | FloatIntrinsicKind::Maximum
            | FloatIntrinsicKind::CopySign => self.lower_float_binary_intrinsic(instruction, kind),
            FloatIntrinsicKind::Fma | FloatIntrinsicKind::FmulAdd => {
                self.lower_float_ternary_intrinsic(instruction, kind)
            },
            FloatIntrinsicKind::IsFpClass => self.lower_is_fpclass_intrinsic(instruction, kind),
        }
    }

    fn lower_float_unary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!(
                "floating intrinsic {:?} expects exactly 1 argument, got {actual_args}",
                kind
            );
        }

        let source = instruction_operand_value(instruction, 0).context("floating intrinsic missing operand 0")?;
        if !matches!(source.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!(
                "floating intrinsic {:?} only supports scalar float/double operands",
                kind
            );
        }
        let source_width = value_width(source).context("floating intrinsic source has unsupported float width")?;
        let result_width =
            instruction_result_width(instruction)?.context("floating intrinsic result has no scalar width")?;
        checked_float_width(source_width as u64)?;
        checked_float_width(result_width as u64)?;
        if source_width != result_width {
            bail!(
                "floating intrinsic {:?} width mismatch: source i{}, result i{}",
                kind,
                source_width,
                result_width
            );
        }

        let env = LoweringEnv::new()
            .llvm_source("%a", source)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", source_width as u64)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_float_binary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 2 {
            bail!(
                "floating intrinsic {:?} expects exactly 2 arguments, got {actual_args}",
                kind
            );
        }

        let lhs = instruction_operand_value(instruction, 0).context("floating intrinsic missing operand 0")?;
        let rhs = instruction_operand_value(instruction, 1).context("floating intrinsic missing operand 1")?;
        if !matches!(lhs.get_type(), BasicTypeEnum::FloatType(_))
            || !matches!(rhs.get_type(), BasicTypeEnum::FloatType(_))
        {
            bail!(
                "floating intrinsic {:?} only supports scalar float/double operands",
                kind
            );
        }
        let lhs_width = value_width(lhs).context("floating intrinsic lhs has unsupported float width")?;
        let rhs_width = value_width(rhs).context("floating intrinsic rhs has unsupported float width")?;
        let result_width =
            instruction_result_width(instruction)?.context("floating intrinsic result has no scalar width")?;
        checked_float_width(lhs_width as u64)?;
        checked_float_width(rhs_width as u64)?;
        checked_float_width(result_width as u64)?;
        if lhs_width != rhs_width || lhs_width != result_width {
            bail!(
                "floating intrinsic {:?} width mismatch: lhs i{}, rhs i{}, result i{}",
                kind,
                lhs_width,
                rhs_width,
                result_width
            );
        }

        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", lhs_width as u64)
            .imm("type_width(%b)", rhs_width as u64)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_float_ternary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 3 {
            bail!(
                "floating intrinsic {:?} expects exactly 3 arguments, got {actual_args}",
                kind
            );
        }

        let lhs = instruction_operand_value(instruction, 0).context("floating intrinsic missing operand 0")?;
        let rhs = instruction_operand_value(instruction, 1).context("floating intrinsic missing operand 1")?;
        let third = instruction_operand_value(instruction, 2).context("floating intrinsic missing operand 2")?;
        if !matches!(lhs.get_type(), BasicTypeEnum::FloatType(_))
            || !matches!(rhs.get_type(), BasicTypeEnum::FloatType(_))
            || !matches!(third.get_type(), BasicTypeEnum::FloatType(_))
        {
            bail!(
                "floating intrinsic {:?} only supports scalar float/double operands",
                kind
            );
        }
        let lhs_width = value_width(lhs).context("floating intrinsic lhs has unsupported float width")?;
        let rhs_width = value_width(rhs).context("floating intrinsic rhs has unsupported float width")?;
        let third_width = value_width(third).context("floating intrinsic third has unsupported float width")?;
        let result_width =
            instruction_result_width(instruction)?.context("floating intrinsic result has no scalar width")?;
        checked_float_width(lhs_width as u64)?;
        checked_float_width(rhs_width as u64)?;
        checked_float_width(third_width as u64)?;
        checked_float_width(result_width as u64)?;
        if lhs_width != rhs_width || lhs_width != third_width || lhs_width != result_width {
            bail!(
                "floating intrinsic {:?} width mismatch: lhs i{}, rhs i{}, third i{}, result i{}",
                kind,
                lhs_width,
                rhs_width,
                third_width,
                result_width
            );
        }

        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_source("%c", third)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", lhs_width as u64)
            .imm("type_width(%b)", rhs_width as u64)
            .imm("type_width(%c)", third_width as u64)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_is_fpclass_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 2 {
            bail!("llvm.is.fpclass expects exactly 2 arguments, got {actual_args}");
        }

        let result_width =
            instruction_result_width(instruction)?.context("llvm.is.fpclass result has no scalar width")?;
        if result_width != 1 {
            bail!("llvm.is.fpclass must return i1, got i{result_width}");
        }

        let source = instruction_operand_value(instruction, 0).context("llvm.is.fpclass missing float operand 0")?;
        if !matches!(source.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!("llvm.is.fpclass only supports scalar float/double operands");
        }
        let source_width = value_width(source).context("llvm.is.fpclass source has unsupported float width")?;
        checked_float_width(source_width as u64)?;
        let mask = checked_fpclass_mask(constant_int_operand(instruction, 1, "llvm.is.fpclass mask")?)?;

        let env = LoweringEnv::new()
            .llvm_source("%a", source)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", source_width as u64)
            .imm("type_width(%r)", result_width as u64)
            .imm("fpclass_mask(%r)", mask as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(HandlerSemantic::FloatClass))?;
        Ok(())
    }

    fn lower_memory_copy_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: MemoryIntrinsicKind,
    ) -> anyhow::Result<()> {
        let rule = kind.lowering_rule();
        let dst_value = instruction_operand_value(instruction, 0)?;
        let src_value = instruction_operand_value(instruction, 1)?;
        let is_volatile = memory_intrinsic_is_volatile(instruction, 3)?;

        let Some(len_value) = instruction_basic_operand(instruction, 2) else {
            bail!("missing memory intrinsic length operand");
        };
        if is_volatile {
            return self.lower_dynamic_memory_copy_intrinsic(kind, dst_value, src_value, len_value, true);
        }
        let len = if len_value.is_int_value() {
            len_value.into_int_value().get_zero_extended_constant()
        } else {
            None
        };
        let Some(len) = len else {
            return self.lower_dynamic_memory_copy_intrinsic(kind, dst_value, src_value, len_value, false);
        };

        if len > MAX_MEMORY_INTRINSIC_INLINE_BYTES {
            return self.lower_dynamic_memory_copy_intrinsic(kind, dst_value, src_value, len_value, false);
        }
        let dst = self.materialize_value(dst_value)?;
        let src = self.materialize_value(src_value)?;
        let direct_load = self.emit_action_for_shape(
            rule,
            &HandlerSemantic::Load,
            &[("dst", "%tmp"), ("ptr", "%vs"), ("width", "copy_width")],
        )?;
        let offset_load = self.emit_action_for_shape(
            rule,
            &HandlerSemantic::Load,
            &[("dst", "%tmp"), ("ptr", "%addr"), ("width", "copy_width")],
        )?;
        let direct_store = self.emit_action_for_shape(
            rule,
            &HandlerSemantic::Store,
            &[("src", "%tmp"), ("ptr", "%vd"), ("width", "copy_width")],
        )?;
        let offset_store = self.emit_action_for_shape(
            rule,
            &HandlerSemantic::Store,
            &[("src", "%tmp"), ("ptr", "%addr"), ("width", "copy_width")],
        )?;
        let src_gep = self.emit_action_for_shape(
            rule,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vs"), ("offset", "copy_offset")],
        )?;
        let dst_gep = self.emit_action_for_shape(
            rule,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vd"), ("offset", "copy_offset")],
        )?;

        let mut loaded = Vec::new();
        for chunk in memory_copy_chunks(len) {
            let value = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: chunk.width,
            };
            let (ptr, action) = if chunk.offset == 0 {
                (src, &direct_load)
            } else {
                (
                    self.emit_memory_intrinsic_gep(rule, &src_gep, "%vs", src, "copy_offset", chunk.offset)?,
                    &offset_load,
                )
            };
            let env = LoweringEnv::new()
                .binding("%vs", src)
                .binding("%addr", ptr)
                .binding("%tmp", value)
                .imm("copy_width", chunk.width as u64);
            self.emit_profile_action(action, &env)?;
            loaded.push(LoadedMemoryChunk { chunk, value });
        }

        for loaded in loaded {
            let (ptr, action) = if loaded.chunk.offset == 0 {
                (dst, &direct_store)
            } else {
                (
                    self.emit_memory_intrinsic_gep(rule, &dst_gep, "%vd", dst, "copy_offset", loaded.chunk.offset)?,
                    &offset_store,
                )
            };
            let env = LoweringEnv::new()
                .binding("%vd", dst)
                .binding("%addr", ptr)
                .binding("%tmp", loaded.value)
                .imm("copy_width", loaded.chunk.width as u64);
            self.emit_profile_action(action, &env)?;
        }

        Ok(())
    }

    fn lower_dynamic_memory_copy_intrinsic(
        &mut self,
        kind: MemoryIntrinsicKind,
        dst_value: BasicValueEnum<'ctx>,
        src_value: BasicValueEnum<'ctx>,
        len_value: BasicValueEnum<'ctx>,
        volatile: bool,
    ) -> anyhow::Result<()> {
        let env = LoweringEnv::new()
            .llvm_source("%dst", dst_value)
            .llvm_source("%src", src_value)
            .llvm_source("%len", len_value);
        let rule = kind.dynamic_lowering_rule(volatile);
        let semantic = kind.dynamic_semantic(volatile);
        self.execute_lowering_rule(rule, env, Some(semantic))?;
        Ok(())
    }

    fn lower_memset_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let rule = MemoryIntrinsicKind::Memset.lowering_rule();
        let dst_value = instruction_operand_value(instruction, 0)?;
        let fill_value = instruction_operand_value(instruction, 1)?;
        let fill_width = value_width(fill_value)?;
        if fill_width != 8 {
            bail!("llvm.memset value must be i8, got i{fill_width}");
        }
        let is_volatile = memory_intrinsic_is_volatile(instruction, 3)?;

        let Some(len_value) = instruction_basic_operand(instruction, 2) else {
            bail!("missing memory intrinsic length operand");
        };
        if is_volatile {
            return self.lower_dynamic_memset_intrinsic(dst_value, fill_value, len_value, true);
        }
        let len = if len_value.is_int_value() {
            len_value.into_int_value().get_zero_extended_constant()
        } else {
            None
        };
        let Some(len) = len else {
            return self.lower_dynamic_memset_intrinsic(dst_value, fill_value, len_value, false);
        };

        if len > MAX_MEMORY_INTRINSIC_INLINE_BYTES {
            return self.lower_dynamic_memset_intrinsic(dst_value, fill_value, len_value, false);
        }
        let dst = self.materialize_value(dst_value)?;
        let value = self.materialize_value(fill_value)?;

        let direct_store = self.emit_action_for_shape(
            rule,
            &HandlerSemantic::Store,
            &[("src", "%vv"), ("ptr", "%vd"), ("width", "set_width")],
        )?;
        let offset_store = self.emit_action_for_shape(
            rule,
            &HandlerSemantic::Store,
            &[("src", "%vv"), ("ptr", "%addr"), ("width", "set_width")],
        )?;
        let dst_gep = self.emit_action_for_shape(
            rule,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vd"), ("offset", "set_offset")],
        )?;

        for offset in 0..len {
            let (ptr, action) = if offset == 0 {
                (dst, &direct_store)
            } else {
                (
                    self.emit_memory_intrinsic_gep(rule, &dst_gep, "%vd", dst, "set_offset", offset)?,
                    &offset_store,
                )
            };
            let env = LoweringEnv::new()
                .binding("%vd", dst)
                .binding("%addr", ptr)
                .binding("%vv", value)
                .imm("set_width", 8);
            self.emit_profile_action(action, &env)?;
        }

        Ok(())
    }

    fn lower_dynamic_memset_intrinsic(
        &mut self,
        dst_value: BasicValueEnum<'ctx>,
        fill_value: BasicValueEnum<'ctx>,
        len_value: BasicValueEnum<'ctx>,
        volatile: bool,
    ) -> anyhow::Result<()> {
        let env = LoweringEnv::new()
            .llvm_source("%dst", dst_value)
            .llvm_source("%value", fill_value)
            .llvm_source("%len", len_value);
        let rule = MemoryIntrinsicKind::Memset.dynamic_lowering_rule(volatile);
        let semantic = MemoryIntrinsicKind::Memset.dynamic_semantic(volatile);
        self.execute_lowering_rule(rule, env, Some(semantic))?;
        Ok(())
    }

    fn emit_memory_intrinsic_gep(
        &mut self,
        rule: &str,
        action: &LoweringAction,
        base_expr: &str,
        base: ValueBinding,
        offset_expr: &str,
        offset: u64,
    ) -> anyhow::Result<ValueBinding> {
        let ptr = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 64,
        };
        let env = LoweringEnv::new()
            .binding(base_expr, base)
            .binding("%addr", ptr)
            .imm(offset_expr, offset);
        self.emit_profile_action(action, &env)
            .with_context(|| format!("while lowering {rule} offset {offset}"))?;
        Ok(ptr)
    }

    fn native_call_final_returns(
        &mut self,
        instruction: InstructionValue<'ctx>,
        target: &NativeCallTarget<'ctx>,
    ) -> anyhow::Result<Vec<ValueBinding>> {
        if target.returns_void {
            return Ok(Vec::new());
        }

        if target.return_fields.len() == 1 {
            let dst = self.ensure_result_binding(instruction)?;
            let field = target.return_fields[0];
            if field.width != dst.width {
                bail!(
                    "native call return width mismatch: destination is {}, callee returns {}",
                    dst.width,
                    field.width
                );
            }
            return Ok(vec![dst]);
        }

        target
            .return_fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let reg = self
                    .builder
                    .alloc_vreg_excluding(&self.native_touched_registers)
                    .with_context(|| format!("native aggregate return field {index}"))?;
                Ok(ValueBinding {
                    reg,
                    width: field.width,
                })
            })
            .collect()
    }

    fn save_native_touched_registers(&mut self, result_regs: &HashSet<u8>) -> anyhow::Result<Vec<(u8, u8)>> {
        let touched = self.native_touched_registers.clone();
        let mut seen = HashSet::new();
        let mut saves = Vec::new();
        let mut candidates = Vec::new();
        for (key, binding) in &self.values {
            if self.defined_values.contains(key)
                && !result_regs.contains(&binding.reg)
                && touched.contains(&binding.reg)
                && seen.insert(binding.reg)
            {
                candidates.push(binding.reg);
            }
        }
        for binding in self.aggregates.values() {
            for field in binding.fields.iter().flatten() {
                let reg = field.binding.reg;
                if !result_regs.contains(&reg) && touched.contains(&reg) && seen.insert(reg) {
                    candidates.push(reg);
                }
            }
        }

        for reg in candidates {
            let scratch = self.alloc_temporary_vreg_excluding(&touched)?;
            self.emit_profile_mov_direct(scratch, reg, 64)?;
            saves.push((reg, scratch));
        }

        Ok(saves)
    }

    fn emit_parallel_register_moves(&mut self, moves: Vec<RegisterMove>) -> anyhow::Result<()> {
        // 参数搬运可能形成 x1->x2、x2->x1 这种环。先把会被覆盖的源寄存器复制到 scratch，
        // 再执行目标写入，避免 native_call 参数准备阶段破坏尚未移动的源值。
        let clobbered = moves
            .iter()
            .filter(|mov| mov.dst != mov.src.reg)
            .map(|mov| mov.dst)
            .collect::<HashSet<_>>();
        let mut prepared = Vec::new();

        for mov in moves {
            if mov.dst == mov.src.reg {
                continue;
            }

            let src = if clobbered.contains(&mov.src.reg) {
                let scratch = self.alloc_temporary_vreg_excluding(&clobbered)?;
                self.emit_profile_mov_direct(scratch, mov.src.reg, mov.src.width)?;
                scratch
            } else {
                mov.src.reg
            };
            prepared.push((mov.dst, src, mov.src.width));
        }

        for (dst, src, width) in prepared {
            self.emit_profile_mov_direct(dst, src, width)?;
        }

        Ok(())
    }

    fn restore_native_touched_registers(
        &mut self,
        saves: Vec<(u8, u8)>,
        result_regs: &HashSet<u8>,
    ) -> anyhow::Result<()> {
        for (reg, scratch) in saves {
            if !result_regs.contains(&reg) {
                self.emit_profile_mov_direct(reg, scratch, 64)?;
            }
        }
        Ok(())
    }

    fn lower_icmp(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let lhs = instruction_operand_value(instruction, 0)?;
        let rhs = instruction_operand_value(instruction, 1)?;
        let pred = instruction
            .get_icmp_predicate()
            .context("icmp instruction has no predicate")?;
        let lhs_width = value_width(lhs)?;
        let rhs_width = value_width(rhs)?;
        if lhs_width != rhs_width {
            bail!("icmp operands have mismatched widths: {lhs_width} and {rhs_width}");
        }

        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("predicate(%r)", map_predicate(pred) as u64)
            .imm("operand_width(%a,%b)", lhs_width as u64);
        self.execute_lowering_rule("llvm.icmp.scalar", env, Some(HandlerSemantic::Icmp))?;
        Ok(())
    }

    fn lower_fcmp(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let lhs = instruction_operand_value(instruction, 0)?;
        let rhs = instruction_operand_value(instruction, 1)?;
        let pred = instruction
            .get_fcmp_predicate()
            .context("fcmp instruction has no predicate")?;
        let lhs_width = value_width(lhs)?;
        let rhs_width = value_width(rhs)?;
        if lhs_width != rhs_width {
            bail!("fcmp operands have mismatched widths: {lhs_width} and {rhs_width}");
        }

        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("predicate(%r)", map_float_predicate(pred) as u64)
            .imm("operand_width(%a,%b)", lhs_width as u64);
        self.execute_lowering_rule("llvm.fcmp.float", env, Some(HandlerSemantic::Fcmp))?;
        Ok(())
    }

    fn lower_cast(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let src = instruction_operand_value(instruction, 0)?;
        let src_width = value_width(src)?;
        let dst_width = instruction_result_width(instruction)?.context("cast result has no scalar width")?;
        let (rule, required) = match instruction.get_opcode() {
            InstructionOpcode::ZExt => ("llvm.cast.integer", HandlerSemantic::Cast(CastOp::ZExt)),
            InstructionOpcode::SExt => ("llvm.cast.integer", HandlerSemantic::Cast(CastOp::SExt)),
            InstructionOpcode::Trunc => ("llvm.cast.integer", HandlerSemantic::Cast(CastOp::Trunc)),
            InstructionOpcode::BitCast => {
                if src_width != dst_width {
                    bail!("scalar bitcast requires equal widths: source i{src_width}, result i{dst_width}");
                }
                ("llvm.cast.bitcast.scalar", HandlerSemantic::Cast(CastOp::Bitcast))
            },
            InstructionOpcode::PtrToInt => {
                if dst_width < src_width {
                    ("llvm.cast.pointer", HandlerSemantic::Cast(CastOp::Trunc))
                } else {
                    ("llvm.cast.pointer", HandlerSemantic::Cast(CastOp::Bitcast))
                }
            },
            InstructionOpcode::IntToPtr => {
                if src_width < dst_width {
                    ("llvm.cast.pointer", HandlerSemantic::Cast(CastOp::ZExt))
                } else {
                    ("llvm.cast.pointer", HandlerSemantic::Cast(CastOp::Bitcast))
                }
            },
            InstructionOpcode::AddrSpaceCast => ("llvm.cast.pointer", HandlerSemantic::Cast(CastOp::Bitcast)),
            opcode => bail!("unsupported cast opcode: {opcode:?}"),
        };
        let env = LoweringEnv::new()
            .llvm_source("%a", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", src_width as u64)
            .imm("type_width(%r)", dst_width as u64);
        self.execute_lowering_rule(rule, env, Some(required))?;
        Ok(())
    }

    fn lower_freeze(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let value = instruction_operand_value(instruction, 0)?;
        if matches!(
            value.get_type(),
            BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_)
        ) {
            return self.lower_aggregate_freeze(instruction, value);
        }

        let width = instruction_result_width(instruction)?.context("freeze result has no scalar width")?;
        let frozen = self.materialize_freeze_value(value, width)?;
        let env = LoweringEnv::new()
            .binding("%value", frozen)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule("llvm.freeze.scalar", env, Some(HandlerSemantic::Mov))?;
        Ok(())
    }

    fn lower_aggregate_freeze(
        &mut self,
        instruction: InstructionValue<'ctx>,
        value: BasicValueEnum<'ctx>,
    ) -> anyhow::Result<()> {
        let field_infos = return_fields_from_aggregate_type(value.get_type()).context("freeze aggregate fields")?;
        if field_infos.is_empty() {
            bail!("empty aggregate freeze is not supported");
        }

        let source = if is_undef_or_poison_value(value) {
            AggregateBinding {
                fields: vec![None; field_infos.len()],
            }
        } else {
            self.aggregates
                .get(&value_key(value))
                .cloned()
                .context("aggregate freeze operand was not built by supported aggregate lowering")?
        };
        if source.fields.len() != field_infos.len() {
            bail!(
                "aggregate freeze field count mismatch: value has {}, type has {}",
                source.fields.len(),
                field_infos.len()
            );
        }

        let mut frozen_fields = Vec::with_capacity(field_infos.len());
        for (index, info) in field_infos.into_iter().enumerate() {
            let src = match source.fields.get(index).copied().flatten() {
                Some(field) => field.binding,
                None => self.zero_aggregate_freeze_field(info)?,
            };
            if src.width != info.width {
                bail!(
                    "aggregate freeze field {index} width mismatch: value is {}, type expects {}",
                    src.width,
                    info.width
                );
            }
            let env = LoweringEnv::new()
                .binding("%value", src)
                .imm("type_width(%field)", info.width as u64);
            let env = self.execute_lowering_rule("llvm.aggregate.freeze", env, Some(HandlerSemantic::Mov))?;
            let stable = match env.get("%vr")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("aggregate freeze lowering must produce a field register")
                },
            };
            frozen_fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: frozen_fields });
        Ok(())
    }

    fn zero_aggregate_freeze_field(&mut self, info: ReturnField) -> anyhow::Result<ValueBinding> {
        let reg = self.alloc_temporary_vreg()?;
        self.push_constant(reg, 0, info.width)?;
        Ok(ValueBinding { reg, width: info.width })
    }

    fn lower_select(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        if matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_)
        ) {
            return self.lower_aggregate_select(instruction);
        }

        self.lower_scalar_select(instruction)
    }

    fn lower_scalar_select(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let dst = self.ensure_result_binding(instruction)?;
        let cond = self.materialize_operand(instruction, 0)?;
        let then_value = self.materialize_operand(instruction, 1)?;
        let else_value = self.materialize_operand(instruction, 2)?;
        let then_label = self.builder.new_label();
        let else_label = self.builder.new_label();
        let join_label = self.builder.new_label();
        let actions = self.select_lowering_actions("llvm.select.scalar", "type_width(%r)")?;

        let branch_env = LoweringEnv::new()
            .binding("%vc", cond)
            .label("then_label", then_label)
            .label("else_label", else_label);
        self.emit_profile_action(&actions.br_if, &branch_env)?;

        self.builder.bind_label(then_label);
        let then_env = LoweringEnv::new()
            .binding("%vr", dst)
            .binding("%vt", then_value)
            .imm("type_width(%r)", dst.width as u64)
            .label("join_label", join_label);
        self.emit_profile_action(&actions.then_mov, &then_env)?;
        self.emit_profile_action(&actions.br, &then_env)?;

        self.builder.bind_label(else_label);
        let else_env = LoweringEnv::new()
            .binding("%vr", dst)
            .binding("%ve", else_value)
            .imm("type_width(%r)", dst.width as u64)
            .label("join_label", join_label);
        self.emit_profile_action(&actions.else_mov, &else_env)?;
        self.emit_profile_action(&actions.br, &else_env)?;

        self.builder.bind_label(join_label);
        Ok(())
    }

    fn lower_aggregate_select(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let field_infos = return_fields_from_aggregate_type(instruction_aggregate_type(instruction)?)
            .context("select result fields")?;
        if field_infos.is_empty() {
            bail!("aggregate select has no scalar leaf fields");
        }

        let cond = self.materialize_operand(instruction, 0)?;
        let then_aggregate = self
            .aggregate_operand(instruction, 1)
            .context("select then aggregate operand")?;
        let else_aggregate = self
            .aggregate_operand(instruction, 2)
            .context("select else aggregate operand")?;
        if then_aggregate.fields.len() != field_infos.len() || else_aggregate.fields.len() != field_infos.len() {
            bail!(
                "aggregate select field count mismatch: type has {}, then has {}, else has {}",
                field_infos.len(),
                then_aggregate.fields.len(),
                else_aggregate.fields.len()
            );
        }

        let actions = self.select_lowering_actions("llvm.select.aggregate", "type_width(%field)")?;
        let mut field_moves = Vec::with_capacity(field_infos.len());
        let mut result_fields = Vec::with_capacity(field_infos.len());
        for (index, info) in field_infos.iter().copied().enumerate() {
            let then_field = then_aggregate
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("aggregate select then field {index} is undefined or unsupported"))?;
            let else_field = else_aggregate
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("aggregate select else field {index} is undefined or unsupported"))?;
            if then_field.binding.width != info.width || else_field.binding.width != info.width {
                bail!(
                    "aggregate select field {index} width mismatch: type i{}, then i{}, else i{}",
                    info.width,
                    then_field.binding.width,
                    else_field.binding.width
                );
            }

            let dst = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: info.width,
            };
            field_moves.push((info, dst, then_field.binding, else_field.binding));
            result_fields.push(Some(AggregateField::owned(dst)));
        }

        let then_label = self.builder.new_label();
        let else_label = self.builder.new_label();
        let join_label = self.builder.new_label();
        let branch_env = LoweringEnv::new()
            .binding("%vc", cond)
            .label("then_label", then_label)
            .label("else_label", else_label);
        self.emit_profile_action(&actions.br_if, &branch_env)?;

        self.builder.bind_label(then_label);
        for (info, dst, then_value, _) in &field_moves {
            let then_env = LoweringEnv::new()
                .binding("%vr", *dst)
                .binding("%vt", *then_value)
                .imm("type_width(%field)", info.width as u64)
                .label("join_label", join_label);
            self.emit_profile_action(&actions.then_mov, &then_env)?;
        }
        let then_br_env = LoweringEnv::new().label("join_label", join_label);
        self.emit_profile_action(&actions.br, &then_br_env)?;

        self.builder.bind_label(else_label);
        for (info, dst, _, else_value) in &field_moves {
            let else_env = LoweringEnv::new()
                .binding("%vr", *dst)
                .binding("%ve", *else_value)
                .imm("type_width(%field)", info.width as u64)
                .label("join_label", join_label);
            self.emit_profile_action(&actions.else_mov, &else_env)?;
        }
        let else_br_env = LoweringEnv::new().label("join_label", join_label);
        self.emit_profile_action(&actions.br, &else_br_env)?;

        self.builder.bind_label(join_label);
        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: result_fields });
        Ok(())
    }

    fn lower_insert_value(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let selection = aggregate_selection_from_instruction(instruction)?;
        let mut aggregate = self.aggregate_seed_from_operand(instruction, 0)?;
        let inserted = instruction_operand_value(instruction, 1)?;
        if selection.is_aggregate {
            return self.lower_insert_subaggregate(instruction, aggregate, selection, inserted);
        }

        let field = selection
            .fields
            .first()
            .copied()
            .context("insertvalue scalar selection has no field")?;
        let inserted_field = return_field_from_type(inserted.get_type()).context("insertvalue scalar field")?;
        if inserted_field != field {
            bail!(
                "insertvalue field type mismatch: inserted {:?} width {}, aggregate field {:?} width {}",
                inserted_field.kind,
                inserted_field.width,
                field.kind,
                field.width
            );
        }
        let env = LoweringEnv::new()
            .llvm_source("%field", inserted)
            .imm("type_width(%field)", inserted_field.width as u64);
        let env = self.execute_lowering_rule("llvm.aggregate.insert", env, Some(HandlerSemantic::Mov))?;
        let stable = match env.get("%r")? {
            LoweringValue::Reg(binding) => binding,
            LoweringValue::Imm(_) | LoweringValue::Label(_) => bail!("aggregate insert bind must produce a register"),
        };
        let slot = aggregate
            .fields
            .get_mut(selection.start)
            .with_context(|| format!("insertvalue field {} is out of range", selection.start))?;
        *slot = Some(AggregateField::owned(stable));
        self.insert_aggregate_value(instruction_key(instruction), aggregate);
        Ok(())
    }

    fn lower_insert_subaggregate(
        &mut self,
        instruction: InstructionValue<'ctx>,
        mut aggregate: AggregateBinding,
        selection: AggregateSelection,
        inserted: BasicValueEnum<'ctx>,
    ) -> anyhow::Result<()> {
        if is_undef_or_poison_value(inserted) {
            bail!("insertvalue subaggregate operand must be frozen before VM materialization");
        }
        let inserted_fields =
            return_fields_from_aggregate_type(inserted.get_type()).context("insertvalue subaggregate fields")?;
        if inserted_fields != selection.fields {
            bail!("insertvalue subaggregate field layout does not match selected aggregate field");
        }
        let inserted_aggregate = self
            .aggregates
            .get(&value_key(inserted))
            .cloned()
            .context("insertvalue subaggregate operand was not built by supported aggregate lowering")?;
        if inserted_aggregate.fields.len() != selection.fields.len() {
            bail!(
                "insertvalue subaggregate field count mismatch: value has {}, selection has {}",
                inserted_aggregate.fields.len(),
                selection.fields.len()
            );
        }

        for (relative, info) in selection.fields.iter().copied().enumerate() {
            let target_index = selection.start + relative;
            let replacement = match inserted_aggregate.fields.get(relative).copied().flatten() {
                Some(field) => {
                    if field.binding.width != info.width {
                        bail!(
                            "insertvalue subaggregate field {relative} width mismatch: value is {}, selected field is {}",
                            field.binding.width,
                            info.width
                        );
                    }
                    Some(AggregateField::owned(self.emit_aggregate_field_mov(
                        "llvm.aggregate.insert.subaggregate",
                        field.binding,
                        info.width,
                    )?))
                },
                None => None,
            };
            let slot = aggregate
                .fields
                .get_mut(target_index)
                .with_context(|| format!("insertvalue subaggregate field {target_index} is out of range"))?;
            *slot = replacement;
        }

        self.insert_aggregate_value(instruction_key(instruction), aggregate);
        Ok(())
    }

    fn lower_extract_value(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let selection = aggregate_selection_from_instruction(instruction)?;
        let aggregate = self.aggregate_operand(instruction, 0)?;
        if selection.is_aggregate {
            return self.lower_extract_subaggregate(instruction, aggregate, selection);
        }

        let field = selection
            .fields
            .first()
            .copied()
            .context("extractvalue scalar selection has no field")?;
        let src = aggregate
            .fields
            .get(selection.start)
            .copied()
            .flatten()
            .map(|field| field.binding)
            .with_context(|| format!("extractvalue field {} is out of range", selection.start))?;

        let result_width = instruction_result_width(instruction)?.context("extractvalue result has no scalar width")?;
        if result_width != field.width || src.width != field.width {
            bail!(
                "extractvalue field width mismatch: result i{}, value i{}, aggregate field i{}",
                result_width,
                src.width,
                field.width
            );
        }
        let env = LoweringEnv::new()
            .binding("%agg", src)
            .llvm_value("%r", instruction_key(instruction))
            .binding("field(%va)", src)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule("llvm.aggregate.extract", env, Some(HandlerSemantic::Mov))?;
        Ok(())
    }

    fn lower_extract_subaggregate(
        &mut self,
        instruction: InstructionValue<'ctx>,
        aggregate: AggregateBinding,
        selection: AggregateSelection,
    ) -> anyhow::Result<()> {
        let mut fields = Vec::with_capacity(selection.fields.len());
        for (relative, info) in selection.fields.iter().copied().enumerate() {
            let source_index = selection.start + relative;
            let field = match aggregate.fields.get(source_index).copied().flatten() {
                Some(field) => {
                    if field.binding.width != info.width {
                        bail!(
                            "extractvalue subaggregate field {relative} width mismatch: value is {}, selected field is {}",
                            field.binding.width,
                            info.width
                        );
                    }
                    Some(AggregateField::owned(self.emit_aggregate_field_mov(
                        "llvm.aggregate.extract.subaggregate",
                        field.binding,
                        info.width,
                    )?))
                },
                None => None,
            };
            fields.push(field);
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn emit_aggregate_field_mov(
        &mut self,
        rule: &str,
        source: ValueBinding,
        width: u8,
    ) -> anyhow::Result<ValueBinding> {
        let env = LoweringEnv::new()
            .binding("%field", source)
            .imm("type_width(%field)", width as u64);
        let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::Mov))?;
        match env.get("%r")? {
            LoweringValue::Reg(binding) => Ok(binding),
            LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                bail!("aggregate field lowering rule {rule} must bind %r to a register")
            },
        }
    }

    fn lower_branch(&mut self, block: BasicBlock<'ctx>, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let branch = instruction.into_branch_inst();
        if instruction.get_num_operands() == 1 {
            let target = branch
                .get_successor(0)
                .context("unconditional branch has no successor")?;
            self.lower_phi_moves(block, target)?;
            let action =
                self.emit_action_for_shape("llvm.br.control", &HandlerSemantic::Br, &[("target", "target_label")])?;
            let env = LoweringEnv::new().label("target_label", self.label_for_block(target)?);
            self.emit_profile_action(&action, &env)?;
            return Ok(());
        }

        let cond = self.materialize_operand(instruction, 0)?;
        let then_block = branch
            .get_successor(0)
            .context("conditional branch has no then successor")?;
        let else_block = branch
            .get_successor(1)
            .context("conditional branch has no else successor")?;
        let then_edge = self.builder.new_label();
        let else_edge = self.builder.new_label();
        let cond_action = self.emit_action_for_shape(
            "llvm.br.control",
            &HandlerSemantic::BrCond,
            &[("cond", "%vc"), ("then_pc", "then_label"), ("else_pc", "else_label")],
        )?;
        let br_action =
            self.emit_action_for_shape("llvm.br.control", &HandlerSemantic::Br, &[("target", "target_label")])?;

        let cond_env = LoweringEnv::new()
            .binding("%vc", cond)
            .label("then_label", then_edge)
            .label("else_label", else_edge);
        self.emit_profile_action(&cond_action, &cond_env)?;

        self.builder.bind_label(then_edge);
        self.lower_phi_moves(block, then_block)?;
        let then_env = LoweringEnv::new().label("target_label", self.label_for_block(then_block)?);
        self.emit_profile_action(&br_action, &then_env)?;

        self.builder.bind_label(else_edge);
        self.lower_phi_moves(block, else_block)?;
        let else_env = LoweringEnv::new().label("target_label", self.label_for_block(else_block)?);
        self.emit_profile_action(&br_action, &else_env)?;

        Ok(())
    }

    fn lower_switch(&mut self, block: BasicBlock<'ctx>, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let switch = SwitchInst::new(instruction);
        let cond = self.materialize_value(switch.get_condition())?;
        let cases = switch.get_cases();
        let default_block = switch.get_default_block();
        let br_action = self.emit_action_for_shape(
            "llvm.switch.control",
            &HandlerSemantic::Br,
            &[("target", "default_label")],
        )?;

        if cases.is_empty() {
            self.lower_phi_moves(block, default_block)?;
            let env = LoweringEnv::new().label("default_label", self.label_for_block(default_block)?);
            self.emit_profile_action(&br_action, &env)?;
            return Ok(());
        }

        let default_edge = self.builder.new_label();
        let last_case = cases.len().saturating_sub(1);
        let icmp_action = self.emit_action_for_shape(
            "llvm.switch.control",
            &HandlerSemantic::Icmp,
            &[
                ("pred", "eq"),
                ("dst", "%vm"),
                ("lhs", "%vc"),
                ("rhs", "%vk"),
                ("width", "type_width(%cond)"),
            ],
        )?;
        let br_if_action = self.emit_action_for_shape(
            "llvm.switch.control",
            &HandlerSemantic::BrCond,
            &[
                ("cond", "%vm"),
                ("then_pc", "case_label"),
                ("else_pc", "next_case_label"),
            ],
        )?;
        for (index, (case_value, case_block)) in cases.into_iter().enumerate() {
            let case = self.materialize_value(case_value)?;
            let matched = self.alloc_temporary_vreg()?;
            let icmp_env = LoweringEnv::new()
                .binding("%vc", cond)
                .binding("%vk", case)
                .reg("%vm", matched, 1)
                .imm("eq", CmpPredicate::Eq as u64)
                .imm("type_width(%cond)", cond.width as u64);
            self.emit_profile_action(&icmp_action, &icmp_env)?;

            let case_edge = self.builder.new_label();
            let next_edge = if index == last_case {
                default_edge
            } else {
                self.builder.new_label()
            };
            let br_if_env = LoweringEnv::new()
                .reg("%vm", matched, 1)
                .label("case_label", case_edge)
                .label("next_case_label", next_edge);
            self.emit_profile_action(&br_if_action, &br_if_env)?;

            self.builder.bind_label(case_edge);
            self.lower_phi_moves(block, case_block)?;
            let case_env = LoweringEnv::new().label("default_label", self.label_for_block(case_block)?);
            self.emit_profile_action(&br_action, &case_env)?;

            if index != last_case {
                self.builder.bind_label(next_edge);
            }
        }

        self.builder.bind_label(default_edge);
        self.lower_phi_moves(block, default_block)?;
        let env = LoweringEnv::new().label("default_label", self.label_for_block(default_block)?);
        self.emit_profile_action(&br_action, &env)?;
        Ok(())
    }

    fn lower_unreachable(&mut self) -> anyhow::Result<()> {
        self.execute_lowering_rule(
            "llvm.unreachable",
            LoweringEnv::new(),
            Some(HandlerSemantic::Unreachable),
        )?;
        Ok(())
    }

    fn lower_trap_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: TrapIntrinsicKind,
    ) -> anyhow::Result<()> {
        kind.validate(instruction)?;
        self.execute_lowering_rule("llvm.trap", LoweringEnv::new(), Some(HandlerSemantic::Trap))?;
        Ok(())
    }

    fn lower_return(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        if instruction.get_num_operands() == 0 {
            let env = LoweringEnv::new().reg("x0", 0, 64);
            self.execute_lowering_rule("llvm.ret.void", env, Some(HandlerSemantic::Ret))?;
            return Ok(());
        }

        if !self.aggregate_return_fields.is_empty() {
            let mov_action = self.emit_action_for_shape(
                "llvm.ret.aggregate",
                &HandlerSemantic::Mov,
                &[("dst", "ret_slot"), ("src", "%vf"), ("width", "field_width(%field)")],
            )?;
            let ret_action =
                self.emit_action_for_shape("llvm.ret.aggregate", &HandlerSemantic::Ret, &[("src", "ret0")])?;
            let aggregate = self.aggregate_operand(instruction, 0)?;
            let return_fields = self.aggregate_return_fields.clone();
            for (index, field) in return_fields.iter().enumerate() {
                let ret_reg = self
                    .return_registers
                    .get(index)
                    .copied()
                    .with_context(|| format!("missing ABI return register {index}"))?;
                let src = aggregate
                    .fields
                    .get(index)
                    .copied()
                    .flatten()
                    .map(|field| field.binding)
                    .with_context(|| format!("aggregate return field {index} is out of range"))?;
                if src.width != field.width {
                    bail!(
                        "aggregate return field {index} width mismatch: value is {}, return type expects {}",
                        src.width,
                        field.width
                    );
                }
                if src.reg != ret_reg {
                    let env = LoweringEnv::new()
                        .reg("ret_slot", ret_reg, src.width)
                        .binding("%vf", src)
                        .imm("field_width(%field)", src.width as u64);
                    self.emit_profile_action(&mov_action, &env)?;
                }
            }
            let src = self
                .return_registers
                .first()
                .copied()
                .context("aggregate return has no ABI return register")?;
            let env = LoweringEnv::new().reg("ret0", src, 64);
            self.emit_profile_action(&ret_action, &env)?;
            return Ok(());
        }

        let ret = instruction_operand_value(instruction, 0)?;
        let env = LoweringEnv::new().llvm_source("%value", ret);
        self.execute_lowering_rule("llvm.ret.scalar", env, Some(HandlerSemantic::Ret))?;
        Ok(())
    }

    fn lower_phi_moves(&mut self, from: BasicBlock<'ctx>, to: BasicBlock<'ctx>) -> anyhow::Result<()> {
        for phi in leading_phi_nodes(to) {
            if matches!(phi.get_type(), AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_)) {
                self.lower_aggregate_phi_move(phi, from)?;
                continue;
            }

            let dst = self
                .values
                .get(&instruction_key(phi))
                .copied()
                .context("missing destination binding for phi")?;
            let incoming = phi_incoming_value(phi, from)?;
            let src = self.materialize_value(incoming)?;
            let env = LoweringEnv::new()
                .binding("%incoming", src)
                .binding("%r", dst)
                .llvm_value("%r", instruction_key(phi))
                .binding("%vi", src)
                .binding("%vr", dst)
                .imm("type_width(%r)", dst.width as u64);
            self.execute_lowering_rule("llvm.phi.edge_move", env, Some(HandlerSemantic::Mov))?;
        }

        Ok(())
    }

    fn lower_aggregate_phi_move(&mut self, phi: InstructionValue<'ctx>, from: BasicBlock<'ctx>) -> anyhow::Result<()> {
        let field_infos = return_fields_from_aggregate_type(instruction_aggregate_type(phi)?)
            .context("aggregate phi result fields")?;
        let dst = self
            .aggregates
            .get(&instruction_key(phi))
            .cloned()
            .context("missing destination aggregate binding for phi")?;
        let incoming = phi_incoming_value(phi, from)?;
        let src = self
            .aggregates
            .get(&value_key(incoming))
            .cloned()
            .context("aggregate phi incoming value was not built by supported aggregate lowering")?;
        if dst.fields.len() != field_infos.len() || src.fields.len() != field_infos.len() {
            bail!(
                "aggregate phi field count mismatch: type has {}, dst has {}, incoming has {}",
                field_infos.len(),
                dst.fields.len(),
                src.fields.len()
            );
        }

        for (index, info) in field_infos.into_iter().enumerate() {
            let dst_field = dst
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("aggregate phi destination field {index} is unavailable"))?;
            let src_field = src
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("aggregate phi incoming field {index} is undefined or unsupported"))?;
            if dst_field.binding.width != info.width || src_field.binding.width != info.width {
                bail!(
                    "aggregate phi field {index} width mismatch: type i{}, dst i{}, incoming i{}",
                    info.width,
                    dst_field.binding.width,
                    src_field.binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%incoming_field", src_field.binding)
                .binding("%vr", dst_field.binding)
                .imm("type_width(%field)", info.width as u64);
            self.execute_lowering_rule("llvm.aggregate.phi.edge_move", env, Some(HandlerSemantic::Mov))?;
        }

        Ok(())
    }

    fn materialize_operand(&mut self, instruction: InstructionValue<'ctx>, index: u32) -> anyhow::Result<ValueBinding> {
        let value =
            instruction_basic_operand(instruction, index).with_context(|| format!("missing value operand {index}"))?;
        self.materialize_value(value)
    }

    fn aggregate_seed_from_operand(
        &self,
        instruction: InstructionValue<'ctx>,
        index: u32,
    ) -> anyhow::Result<AggregateBinding> {
        let value = instruction_basic_operand(instruction, index)
            .with_context(|| format!("missing aggregate operand {index}"))?;
        if let Some(binding) = self.aggregates.get(&value_key(value)) {
            return Ok(binding.clone());
        }

        Ok(AggregateBinding {
            fields: vec![None; aggregate_leaf_count(value.get_type())?],
        })
    }

    fn aggregate_operand(&self, instruction: InstructionValue<'ctx>, index: u32) -> anyhow::Result<AggregateBinding> {
        let value = instruction_basic_operand(instruction, index)
            .with_context(|| format!("missing aggregate operand {index}"))?;
        self.aggregates
            .get(&value_key(value))
            .cloned()
            .context("aggregate value was not built by supported insertvalue lowering")
    }

    fn materialize_value(&mut self, value: BasicValueEnum<'ctx>) -> anyhow::Result<ValueBinding> {
        if let Some(binding) = self.values.get(&value_key(value)).copied() {
            return Ok(binding);
        }

        if is_undef_or_poison_value(value) {
            bail!("undef/poison values must be frozen before VM materialization");
        }

        if value.is_int_value() {
            let int_value = value.into_int_value();
            if let Some(imm) = int_value.get_zero_extended_constant() {
                let width = checked_width(int_value.get_type().get_bit_width())?;
                let reg = self.alloc_temporary_vreg()?;
                self.push_constant(reg, imm, width)?;
                return Ok(ValueBinding { reg, width });
            }
            let value_ref = int_value.as_value_ref();
            if !unsafe { LLVMIsAConstantExpr(value_ref) }.is_null() {
                return self.materialize_constant_expr_integer(value_ref);
            }
        }

        if value.is_float_value() {
            let float_value = value.into_float_value();
            let width = float_type_width(float_value.get_type().as_type_ref())?;
            if let Some((constant, _lossy)) = float_value.get_constant() {
                let imm = match width {
                    32 => u64::from((constant as f32).to_bits()),
                    64 => constant.to_bits(),
                    _ => unreachable!("float_type_width only returns 32 or 64"),
                };
                let reg = self.alloc_temporary_vreg()?;
                self.push_constant(reg, imm, width)?;
                return Ok(ValueBinding { reg, width });
            }
        }

        if value.is_pointer_value() {
            let pointer_value = value.into_pointer_value();
            if pointer_value.is_null() {
                let reg = self.alloc_temporary_vreg()?;
                self.push_constant(reg, 0, 64)?;
                return Ok(ValueBinding { reg, width: 64 });
            }
            let pointer_ref = pointer_value.as_value_ref();
            if !unsafe { LLVMIsAGlobalValue(pointer_ref) }.is_null() {
                return self.materialize_global_pointer(pointer_value);
            }
            if !unsafe { LLVMIsAConstantExpr(pointer_ref) }.is_null() {
                return self.materialize_constant_expr_pointer(pointer_ref);
            }
            bail!(
                "non-null pointer constants other than GlobalValue or supported pointer cast/getelementptr constant expressions cannot be materialized"
            );
        }

        bail!(
            "only integer/float constants, global/null pointers, supported pointer cast/getelementptr constants, and previously lowered SSA values can be materialized"
        )
    }

    fn static_object_size(&self, value: BasicValueEnum<'ctx>) -> anyhow::Result<u64> {
        if !value.is_pointer_value() {
            bail!("llvm.objectsize operand must be a pointer");
        }
        let (total_size, offset) = self.static_object_base_and_offset(value.as_value_ref())?;
        if offset < 0 {
            bail!("llvm.objectsize negative object offset is not supported");
        }
        let offset = u64::try_from(offset).context("llvm.objectsize offset does not fit in u64")?;
        if offset > total_size {
            bail!("llvm.objectsize offset {offset} exceeds object size {total_size}");
        }
        Ok(total_size - offset)
    }

    fn static_object_base_and_offset(&self, value_ref: LLVMValueRef) -> anyhow::Result<(u64, i64)> {
        if value_ref.is_null() {
            bail!("llvm.objectsize pointer is null");
        }
        if unsafe { LLVMGetTypeKind(LLVMTypeOf(value_ref)) } != LLVMTypeKind::LLVMPointerTypeKind {
            bail!("llvm.objectsize base value must be a pointer");
        }

        if !unsafe { LLVMIsAAllocaInst(value_ref) }.is_null() {
            return self.static_alloca_object_size(value_ref).map(|size| (size, 0));
        }
        if !unsafe { LLVMIsAGlobalVariable(value_ref) }.is_null() {
            let value_type = unsafe { LLVMGlobalGetValueType(value_ref) };
            if value_type.is_null() {
                bail!("llvm.objectsize global value type is unavailable");
            }
            return store_size(&self.target_data, value_type).map(|size| (size, 0));
        }
        if !unsafe { LLVMIsAGetElementPtrInst(value_ref) }.is_null()
            || (!unsafe { LLVMIsAConstantExpr(value_ref) }.is_null()
                && unsafe { LLVMGetConstOpcode(value_ref) } == LLVMOpcode::LLVMGetElementPtr)
        {
            let (base_ref, offset) = self.static_gep_pointer_parts(value_ref)?;
            let (total_size, base_offset) = self.static_object_base_and_offset(base_ref)?;
            let offset = base_offset
                .checked_add(offset)
                .context("llvm.objectsize GEP offset overflow")?;
            return Ok((total_size, offset));
        }
        if !unsafe { LLVMIsAConstantExpr(value_ref) }.is_null() {
            match unsafe { LLVMGetConstOpcode(value_ref) } {
                LLVMOpcode::LLVMBitCast | LLVMOpcode::LLVMAddrSpaceCast => {
                    let operand = single_constant_expr_operand(value_ref, "llvm.objectsize pointer cast")?;
                    return self.static_object_base_and_offset(operand);
                },
                opcode => bail!("unsupported llvm.objectsize pointer constant expression opcode: {opcode:?}"),
            }
        }

        bail!("llvm.objectsize only supports static alloca, global, and constant-offset GEP operands")
    }

    fn static_alloca_object_size(&self, alloca_ref: LLVMValueRef) -> anyhow::Result<u64> {
        let allocated_type = unsafe { LLVMGetAllocatedType(alloca_ref) };
        if allocated_type.is_null() {
            bail!("llvm.objectsize alloca allocated type is unavailable");
        }
        let element_size = store_size(&self.target_data, allocated_type)?;
        let operand_count =
            usize::try_from(unsafe { LLVMGetNumOperands(alloca_ref) }).context("alloca operand count is negative")?;
        if operand_count == 0 {
            return Ok(element_size);
        }

        let count_ref = unsafe { LLVMGetOperand(alloca_ref, 0) };
        if count_ref.is_null() {
            return Ok(element_size);
        }
        if unsafe { LLVMGetTypeKind(LLVMTypeOf(count_ref)) } != LLVMTypeKind::LLVMIntegerTypeKind {
            bail!("llvm.objectsize alloca element count must be an integer");
        }
        let count = unsafe { BasicValueEnum::new(count_ref) }
            .into_int_value()
            .get_zero_extended_constant()
            .context("llvm.objectsize dynamic alloca count is not statically known")?;
        element_size
            .checked_mul(count)
            .context("llvm.objectsize alloca byte size overflow")
    }

    fn static_gep_pointer_parts(&self, gep_ref: LLVMValueRef) -> anyhow::Result<(LLVMValueRef, i64)> {
        let source_type = unsafe { LLVMGetGEPSourceElementType(gep_ref) };
        if source_type.is_null() {
            bail!("llvm.objectsize getelementptr source element type is unavailable");
        }

        let operand_count = usize::try_from(unsafe { LLVMGetNumOperands(gep_ref) })
            .context("getelementptr operand count is negative")?;
        if operand_count < 2 {
            bail!("llvm.objectsize getelementptr needs a base pointer and at least one index");
        }

        let base_ref = unsafe { LLVMGetOperand(gep_ref, 0) };
        if base_ref.is_null() || unsafe { LLVMGetTypeKind(LLVMTypeOf(base_ref)) } != LLVMTypeKind::LLVMPointerTypeKind {
            bail!("llvm.objectsize getelementptr base must be a pointer");
        }

        let mut current_type = source_type;
        let mut offset = 0_i64;
        for index_position in 0..(operand_count - 1) {
            let operand_index = u32::try_from(index_position + 1).context("getelementptr index position overflow")?;
            let index_ref = unsafe { LLVMGetOperand(gep_ref, operand_index) };
            let index_value = constant_int_ref(index_ref, "llvm.objectsize getelementptr index")?;
            let index = unsafe { BasicValueEnum::new(index_ref) };
            let step = gep_index_step(&self.target_data, current_type, index_position, index)?;
            let step_offset = match step.constant_offset {
                Some(offset) => offset,
                None => scaled_gep_offset(index_value, step.scale)?,
            };
            offset = offset
                .checked_add(step_offset)
                .context("llvm.objectsize getelementptr offset overflow")?;
            current_type = step.next_type;
        }

        Ok((base_ref, offset))
    }

    fn materialize_global_pointer(&mut self, pointer_value: PointerValue<'ctx>) -> anyhow::Result<ValueBinding> {
        let thunk = self.emit_global_address_thunk(pointer_value)?;
        let target = native_call_target(thunk)?;
        let call_result = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 64,
        };
        self.emit_no_arg_native_call_result(target, call_result)?;

        let result = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 64,
        };
        let env = LoweringEnv::new()
            .binding("%value", call_result)
            .binding("%src", call_result)
            .binding("%vr", result)
            .imm("type_width(%r)", 64);
        self.execute_lowering_rule("llvm.global.address.pointer", env, Some(HandlerSemantic::Mov))?;
        Ok(result)
    }

    fn materialize_constant_expr_pointer(&mut self, pointer_ref: LLVMValueRef) -> anyhow::Result<ValueBinding> {
        match unsafe { LLVMGetConstOpcode(pointer_ref) } {
            LLVMOpcode::LLVMIntToPtr => self.materialize_constexpr_inttoptr(pointer_ref),
            LLVMOpcode::LLVMGetElementPtr => self.materialize_constant_gep_pointer(pointer_ref),
            LLVMOpcode::LLVMBitCast | LLVMOpcode::LLVMAddrSpaceCast => {
                self.materialize_pointer_constant_expr_operand(pointer_ref)
            },
            opcode => bail!("unsupported pointer constant expression opcode: {opcode:?}"),
        }
    }

    fn materialize_constant_expr_integer(&mut self, value_ref: LLVMValueRef) -> anyhow::Result<ValueBinding> {
        match unsafe { LLVMGetConstOpcode(value_ref) } {
            LLVMOpcode::LLVMPtrToInt => self.materialize_constexpr_ptrtoint(value_ref),
            LLVMOpcode::LLVMAdd => self.materialize_constexpr_integer_binop(value_ref, BinOp::Add),
            LLVMOpcode::LLVMSub => self.materialize_constexpr_integer_binop(value_ref, BinOp::Sub),
            LLVMOpcode::LLVMMul => self.materialize_constexpr_integer_binop(value_ref, BinOp::Mul),
            LLVMOpcode::LLVMUDiv => self.materialize_constexpr_integer_binop(value_ref, BinOp::UDiv),
            LLVMOpcode::LLVMSDiv => self.materialize_constexpr_integer_binop(value_ref, BinOp::SDiv),
            LLVMOpcode::LLVMURem => self.materialize_constexpr_integer_binop(value_ref, BinOp::URem),
            LLVMOpcode::LLVMSRem => self.materialize_constexpr_integer_binop(value_ref, BinOp::SRem),
            LLVMOpcode::LLVMXor => self.materialize_constexpr_integer_binop(value_ref, BinOp::Xor),
            LLVMOpcode::LLVMAnd => self.materialize_constexpr_integer_binop(value_ref, BinOp::And),
            LLVMOpcode::LLVMOr => self.materialize_constexpr_integer_binop(value_ref, BinOp::Or),
            LLVMOpcode::LLVMShl => self.materialize_constexpr_integer_binop(value_ref, BinOp::Shl),
            LLVMOpcode::LLVMLShr => self.materialize_constexpr_integer_binop(value_ref, BinOp::LShr),
            LLVMOpcode::LLVMAShr => self.materialize_constexpr_integer_binop(value_ref, BinOp::AShr),
            LLVMOpcode::LLVMZExt => self.materialize_constexpr_integer_cast(value_ref, CastOp::ZExt),
            LLVMOpcode::LLVMSExt => self.materialize_constexpr_integer_cast(value_ref, CastOp::SExt),
            LLVMOpcode::LLVMTrunc => self.materialize_constexpr_integer_cast(value_ref, CastOp::Trunc),
            LLVMOpcode::LLVMBitCast => self.materialize_constexpr_integer_cast(value_ref, CastOp::Bitcast),
            opcode => bail!("unsupported integer constant expression opcode: {opcode:?}"),
        }
    }

    fn materialize_constexpr_integer_binop(
        &mut self,
        expr_ref: LLVMValueRef,
        op: BinOp,
    ) -> anyhow::Result<ValueBinding> {
        let (lhs_ref, rhs_ref) = binary_constant_expr_operands(expr_ref, "integer binop constant expression")?;
        for (name, operand_ref) in [("lhs", lhs_ref), ("rhs", rhs_ref)] {
            if unsafe { LLVMGetTypeKind(LLVMTypeOf(operand_ref)) } != LLVMTypeKind::LLVMIntegerTypeKind {
                bail!("integer binop constant expression {name} operand must be an integer");
            }
        }

        let lhs_value = unsafe { BasicValueEnum::new(lhs_ref) };
        let rhs_value = unsafe { BasicValueEnum::new(rhs_ref) };
        let result_value = unsafe { BasicValueEnum::new(expr_ref) };
        let lhs_width = value_width(lhs_value)?;
        let rhs_width = value_width(rhs_value)?;
        let dst_width = value_width(result_value)?;
        if lhs_width != dst_width || rhs_width != dst_width {
            bail!(
                "integer binop constant expression width mismatch: lhs i{}, rhs i{}, result i{}",
                lhs_width,
                rhs_width,
                dst_width
            );
        }

        let lhs = self.materialize_value(lhs_value)?;
        let rhs = self.materialize_value(rhs_value)?;
        let result = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: dst_width,
        };
        let env = LoweringEnv::new()
            .binding("%a", lhs)
            .binding("%b", rhs)
            .binding("%va", lhs)
            .binding("%vb", rhs)
            .binding("%vr", result)
            .imm("type_width(%r)", dst_width as u64);
        self.execute_lowering_rule("llvm.constexpr.integer.binop", env, Some(HandlerSemantic::Bin(op)))?;
        Ok(result)
    }

    fn materialize_constexpr_integer_cast(
        &mut self,
        expr_ref: LLVMValueRef,
        op: CastOp,
    ) -> anyhow::Result<ValueBinding> {
        let operand_ref = single_constant_expr_operand(expr_ref, "integer cast constant expression")?;
        if unsafe { LLVMGetTypeKind(LLVMTypeOf(operand_ref)) } != LLVMTypeKind::LLVMIntegerTypeKind {
            bail!("integer cast constant expression operand must be an integer");
        }

        let operand = unsafe { BasicValueEnum::new(operand_ref) };
        let src_width = value_width(operand)?;
        let dst_width = value_width(unsafe { BasicValueEnum::new(expr_ref) })?;
        match op {
            CastOp::ZExt | CastOp::SExt if src_width >= dst_width => {
                bail!("integer cast constant expression extension requires a wider result")
            },
            CastOp::Trunc if src_width <= dst_width => {
                bail!("integer cast constant expression trunc requires a narrower result")
            },
            CastOp::Bitcast if src_width != dst_width => {
                bail!("integer cast constant expression bitcast requires equal widths")
            },
            CastOp::ZExt | CastOp::SExt | CastOp::Trunc | CastOp::Bitcast => {},
        }

        let src = self.materialize_value(operand)?;
        let result = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: dst_width,
        };
        let env = LoweringEnv::new()
            .binding("%value", src)
            .binding("%a", src)
            .binding("%va", src)
            .binding("%vr", result)
            .imm("type_width(%value)", src_width as u64)
            .imm("type_width(%a)", src_width as u64)
            .imm("type_width(%r)", dst_width as u64);
        self.execute_lowering_rule("llvm.constexpr.integer.cast", env, Some(HandlerSemantic::Cast(op)))?;
        Ok(result)
    }

    fn materialize_constexpr_ptrtoint(&mut self, expr_ref: LLVMValueRef) -> anyhow::Result<ValueBinding> {
        let operand_ref = single_constant_expr_operand(expr_ref, "ptrtoint constant expression")?;
        if unsafe { LLVMGetTypeKind(LLVMTypeOf(operand_ref)) } != LLVMTypeKind::LLVMPointerTypeKind {
            bail!("ptrtoint constant expression operand must be a pointer");
        }

        let operand = unsafe { BasicValueEnum::new(operand_ref) };
        let src_width = value_width(operand)?;
        let src = self.materialize_value(operand)?;
        let dst_width = value_width(unsafe { BasicValueEnum::new(expr_ref) })?;
        let result = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: dst_width,
        };
        let semantic = if dst_width < src_width {
            HandlerSemantic::Cast(CastOp::Trunc)
        } else {
            HandlerSemantic::Cast(CastOp::Bitcast)
        };
        let env = LoweringEnv::new()
            .binding("%value", src)
            .binding("%a", src)
            .binding("%va", src)
            .binding("%vr", result)
            .imm("type_width(%value)", src_width as u64)
            .imm("type_width(%a)", src_width as u64)
            .imm("type_width(%r)", dst_width as u64);
        self.execute_lowering_rule("llvm.constexpr.ptrtoint", env, Some(semantic))?;
        Ok(result)
    }

    fn materialize_constexpr_inttoptr(&mut self, expr_ref: LLVMValueRef) -> anyhow::Result<ValueBinding> {
        let operand_ref = single_constant_expr_operand(expr_ref, "inttoptr constant expression")?;
        if unsafe { LLVMGetTypeKind(LLVMTypeOf(operand_ref)) } != LLVMTypeKind::LLVMIntegerTypeKind {
            bail!("inttoptr constant expression operand must be an integer");
        }

        let operand = unsafe { BasicValueEnum::new(operand_ref) };
        let src_width = value_width(operand)?;
        let src = self.materialize_value(operand)?;
        let dst_width = value_width(unsafe { BasicValueEnum::new(expr_ref) })?;
        let result = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: dst_width,
        };
        let semantic = if src_width < dst_width {
            HandlerSemantic::Cast(CastOp::ZExt)
        } else {
            HandlerSemantic::Cast(CastOp::Bitcast)
        };
        let env = LoweringEnv::new()
            .binding("%value", src)
            .binding("%a", src)
            .binding("%va", src)
            .binding("%vr", result)
            .imm("type_width(%value)", src_width as u64)
            .imm("type_width(%a)", src_width as u64)
            .imm("type_width(%r)", dst_width as u64);
        self.execute_lowering_rule("llvm.constexpr.inttoptr", env, Some(semantic))?;
        Ok(result)
    }

    fn materialize_pointer_constant_expr_operand(&mut self, expr_ref: LLVMValueRef) -> anyhow::Result<ValueBinding> {
        let operand_ref = single_constant_expr_operand(expr_ref, "pointer cast constant expression")?;
        if operand_ref.is_null()
            || unsafe { LLVMGetTypeKind(LLVMTypeOf(operand_ref)) } != LLVMTypeKind::LLVMPointerTypeKind
        {
            bail!("pointer cast constant expression operand must be a pointer");
        }
        let operand = unsafe { BasicValueEnum::new(operand_ref) };
        self.materialize_value(operand)
    }

    fn materialize_constant_gep_pointer(&mut self, gep_ref: LLVMValueRef) -> anyhow::Result<ValueBinding> {
        let (base_ref, offset) = self.constant_gep_pointer_parts(gep_ref)?;
        let base = self.materialize_value(unsafe { BasicValueEnum::new(base_ref) })?;
        if offset == 0 {
            return Ok(base);
        }

        let result = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 64,
        };
        let gep_action = self.emit_action_for_shape(
            "llvm.gep.constant",
            &HandlerSemantic::Gep,
            &[("dst", "%vr"), ("base", "%vb"), ("offset", "constant_gep_offset(%r)")],
        )?;
        let env = LoweringEnv::new()
            .binding("%vb", base)
            .binding("%vr", result)
            .imm("constant_gep_offset(%r)", offset as u64);
        self.emit_profile_action(&gep_action, &env)?;
        Ok(result)
    }

    fn constant_gep_pointer_parts(&self, gep_ref: LLVMValueRef) -> anyhow::Result<(LLVMValueRef, i64)> {
        let source_type = unsafe { LLVMGetGEPSourceElementType(gep_ref) };
        if source_type.is_null() {
            bail!("constant getelementptr source element type is unavailable");
        }

        let operand_count = usize::try_from(unsafe { LLVMGetNumOperands(gep_ref) })
            .context("constant getelementptr operand count is negative")?;
        if operand_count < 2 {
            bail!("constant getelementptr needs a base pointer and at least one index");
        }

        let base_ref = unsafe { LLVMGetOperand(gep_ref, 0) };
        if base_ref.is_null() || unsafe { LLVMGetTypeKind(LLVMTypeOf(base_ref)) } != LLVMTypeKind::LLVMPointerTypeKind {
            bail!("constant getelementptr base must be a pointer");
        }

        let mut current_type = source_type;
        let mut offset = 0_i64;
        for index_position in 0..(operand_count - 1) {
            let operand_index =
                u32::try_from(index_position + 1).context("constant getelementptr index position overflow")?;
            let index_ref = unsafe { LLVMGetOperand(gep_ref, operand_index) };
            let index_value = constant_int_ref(index_ref, "constant getelementptr index")?;
            let index = unsafe { BasicValueEnum::new(index_ref) };
            let step = gep_index_step(&self.target_data, current_type, index_position, index)?;
            let step_offset = match step.constant_offset {
                Some(offset) => offset,
                None => scaled_gep_offset(index_value, step.scale)?,
            };
            offset = offset
                .checked_add(step_offset)
                .context("constant getelementptr offset overflow")?;
            current_type = step.next_type;
        }

        Ok((base_ref, offset))
    }

    fn emit_global_address_thunk(&mut self, pointer_value: PointerValue<'ctx>) -> anyhow::Result<FunctionValue<'ctx>> {
        let ctx = self.module.get_context();
        let thunk_type = ctx
            .ptr_type(pointer_value.get_type().get_address_space())
            .fn_type(&[], false);
        let function_name = self.function.get_name().to_str().unwrap_or("anon");
        let thunk = self.module.add_function(
            &format!(
                ".amice.vm.global_addr.{}.{}",
                translator_symbol_suffix(function_name),
                self.native_calls.len()
            ),
            thunk_type,
            Some(Linkage::Private),
        );
        thunk.as_global_value().set_unnamed_address(UnnamedAddress::Global);

        let entry = ctx.append_basic_block(thunk, "entry");
        let builder = ctx.create_builder();
        builder.position_at_end(entry);
        builder.build_return(Some(&pointer_value))?;
        Ok(thunk)
    }

    fn materialize_freeze_value(&mut self, value: BasicValueEnum<'ctx>, width: u8) -> anyhow::Result<ValueBinding> {
        if is_undef_or_poison_value(value) {
            let reg = self.alloc_temporary_vreg()?;
            self.push_constant(reg, 0, width)?;
            return Ok(ValueBinding { reg, width });
        }

        self.materialize_value(value)
    }

    fn push_constant(&mut self, dst: u8, imm: u64, width: u8) -> anyhow::Result<()> {
        if should_use_const_pool(imm, width) {
            let env = LoweringEnv::new()
                .imm("%v", imm)
                .reg("%vr", dst, width)
                .imm("const_pool_index(%v)", imm)
                .imm("type_width(%v)", width as u64);
            self.execute_lowering_rule("llvm.const_pool.materialize", env, Some(HandlerSemantic::ConstLoad))?;
        } else {
            self.emit_profile_mov_imm(dst, imm, width)?;
        }
        Ok(())
    }

    fn label_for_block(&self, block: BasicBlock<'ctx>) -> anyhow::Result<LabelId> {
        self.labels
            .get(&block_key(block))
            .copied()
            .context("missing label for successor")
    }

    fn gep_dynamic_terms(
        &mut self,
        instruction: InstructionValue<'ctx>,
        gep: GepInst<'ctx>,
    ) -> anyhow::Result<GepTerms> {
        // SAFETY: `instruction` 是正在 lowering 的 live LLVM getelementptr 指令。
        // C API 只读取 module 拥有的类型元数据；null 会立刻处理，因此 opaque 或畸形 GEP 会安全跳过。
        let source_type = unsafe { LLVMGetGEPSourceElementType(instruction.as_value_ref()) };
        if source_type.is_null() {
            bail!("getelementptr source element type is unavailable");
        }

        let mut current_type = source_type;
        let mut constant_offset = 0_i64;
        let mut dynamic = Vec::new();

        for (index_position, index) in gep.get_indices().into_iter().enumerate() {
            let index = index.context("getelementptr index has no value")?;
            let step = gep_index_step(&self.target_data, current_type, index_position, index)?;
            if let Some(offset) = step.constant_offset {
                constant_offset = constant_offset
                    .checked_add(offset)
                    .context("getelementptr constant offset overflow")?;
            } else if let Some(constant) = index.into_int_value().get_sign_extended_constant() {
                constant_offset = constant_offset
                    .checked_add(scaled_gep_offset(constant, step.scale)?)
                    .context("getelementptr constant offset overflow")?;
            } else {
                dynamic.push((self.materialize_value(index)?, step.scale));
            }
            current_type = step.next_type;
        }

        Ok(GepTerms {
            constant_offset,
            dynamic,
        })
    }
}

fn native_call_target_for_direct_call<'ctx>(
    function: FunctionValue<'ctx>,
    instruction: InstructionValue<'ctx>,
) -> anyhow::Result<NativeCallTarget<'ctx>> {
    let fn_type = function.get_type();
    if !fn_type.is_var_arg() {
        return native_call_target(function);
    }

    let arg_count = instruction.get_num_operands().saturating_sub(1) as usize;
    let fixed_param_count = fn_type.get_param_types().len();
    if arg_count < fixed_param_count {
        bail!(
            "varargs native call passes {arg_count} arguments but callee declares {fixed_param_count} fixed parameters"
        );
    }

    let mut arg_types = Vec::with_capacity(arg_count);
    for index in 0..arg_count {
        let value = instruction_basic_operand(instruction, index as u32)
            .with_context(|| format!("missing varargs native call argument {index}"))?;
        arg_types.push(metadata_type_from_basic_type(value.get_type())?);
    }
    native_call_target_with_arg_types(function, arg_types)
}

fn native_call_target<'ctx>(function: FunctionValue<'ctx>) -> anyhow::Result<NativeCallTarget<'ctx>> {
    let fn_type = function.get_type();
    native_call_target_with_arg_types(function, fn_type.get_param_types())
}

fn native_call_target_with_arg_types<'ctx>(
    function: FunctionValue<'ctx>,
    arg_types: Vec<BasicMetadataTypeEnum<'ctx>>,
) -> anyhow::Result<NativeCallTarget<'ctx>> {
    let fn_type = function.get_type();
    let (returns_void, return_fields) = match fn_type.get_return_type() {
        None => (true, Vec::new()),
        Some(BasicTypeEnum::IntType(return_type)) => (
            false,
            vec![ReturnField {
                width: checked_width(return_type.get_bit_width())?,
                kind: ScalarKind::Integer,
            }],
        ),
        Some(BasicTypeEnum::PointerType(_)) => (
            false,
            vec![ReturnField {
                width: 64,
                kind: ScalarKind::Pointer,
            }],
        ),
        Some(BasicTypeEnum::FloatType(return_type)) => (
            false,
            vec![ReturnField {
                width: float_type_width(return_type.as_type_ref())?,
                kind: ScalarKind::Float,
            }],
        ),
        Some(BasicTypeEnum::StructType(return_type)) => {
            let fields = return_fields_from_aggregate_type(BasicTypeEnum::StructType(return_type))
                .context("native return fields")?;
            if fields.is_empty() {
                bail!("empty aggregate native call returns are not supported");
            }
            (false, fields)
        },
        Some(BasicTypeEnum::ArrayType(return_type)) => {
            let fields = return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(return_type))
                .context("native return fields")?;
            if fields.is_empty() {
                bail!("empty aggregate native call returns are not supported");
            }
            (false, fields)
        },
        Some(_) => {
            bail!(
                "only void, scalar integer, pointer, float, and direct struct/array native call returns are supported"
            )
        },
    };

    let mut param_widths = Vec::new();
    let mut params = Vec::with_capacity(arg_types.len());
    for (index, ty) in arg_types.iter().enumerate() {
        let start = param_widths.len();
        let fields = match ty {
            BasicMetadataTypeEnum::IntType(int_ty) => vec![ReturnField {
                width: checked_width(int_ty.get_bit_width())?,
                kind: ScalarKind::Integer,
            }],
            BasicMetadataTypeEnum::PointerType(_) => vec![ReturnField {
                width: 64,
                kind: ScalarKind::Pointer,
            }],
            BasicMetadataTypeEnum::FloatType(float_ty) => vec![ReturnField {
                width: float_type_width(float_ty.as_type_ref())?,
                kind: ScalarKind::Float,
            }],
            BasicMetadataTypeEnum::StructType(struct_ty) => {
                let fields = return_fields_from_aggregate_type(BasicTypeEnum::StructType(*struct_ty))
                    .with_context(|| format!("native aggregate parameter {index} fields"))?;
                if fields.is_empty() {
                    bail!("empty aggregate native call parameter {index} is not supported");
                }
                fields
            },
            BasicMetadataTypeEnum::ArrayType(array_ty) => {
                let fields = return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(*array_ty))
                    .with_context(|| format!("native aggregate parameter {index} fields"))?;
                if fields.is_empty() {
                    bail!("empty aggregate native call parameter {index} is not supported");
                }
                fields
            },
            _ => {
                bail!(
                    "only scalar integer, pointer, float, and direct struct/array native call parameters are supported"
                )
            },
        };
        for field in &fields {
            param_widths.push(field.width);
        }
        params.push(FunctionParamSlots { start, fields });
    }

    if param_widths.len() > NATIVE_CALL_MAX_ARGS {
        bail!(
            "only up to {NATIVE_CALL_MAX_ARGS} flattened scalar integer/pointer/float native call argument slots are supported, got {}",
            param_widths.len()
        );
    }

    Ok(NativeCallTarget {
        function,
        arg_types,
        param_widths,
        params,
        returns_void,
        return_fields,
    })
}

fn metadata_type_from_basic_type<'ctx>(ty: BasicTypeEnum<'ctx>) -> anyhow::Result<BasicMetadataTypeEnum<'ctx>> {
    match ty {
        BasicTypeEnum::IntType(ty) => Ok(ty.into()),
        BasicTypeEnum::PointerType(ty) => Ok(ty.into()),
        BasicTypeEnum::FloatType(ty) => Ok(ty.into()),
        BasicTypeEnum::StructType(ty) => Ok(ty.into()),
        BasicTypeEnum::ArrayType(ty) => Ok(ty.into()),
        BasicTypeEnum::VectorType(_) | BasicTypeEnum::ScalableVectorType(_) => {
            bail!("vector varargs native call arguments are not supported by vm_virtualize")
        },
    }
}

fn ensure_supported_indirect_call_type(call_type: FunctionType<'_>) -> anyhow::Result<()> {
    let param_types = call_type.get_param_types();
    let mut flattened_param_count = 1usize;
    for (index, ty) in param_types.iter().enumerate() {
        let leaf_count = match ty {
            BasicMetadataTypeEnum::IntType(int_ty) => {
                checked_width(int_ty.get_bit_width())
                    .with_context(|| format!("indirect call argument {index} integer width"))?;
                1
            },
            BasicMetadataTypeEnum::PointerType(_) => 1,
            BasicMetadataTypeEnum::FloatType(float_ty) => {
                float_type_width(float_ty.as_type_ref())
                    .with_context(|| format!("indirect call argument {index} float width"))?;
                1
            },
            BasicMetadataTypeEnum::StructType(struct_ty) => {
                let fields = return_fields_from_aggregate_type(BasicTypeEnum::StructType(*struct_ty))
                    .with_context(|| format!("indirect call aggregate argument {index} fields"))?;
                if fields.is_empty() {
                    bail!("empty aggregate indirect call argument {index} is not supported");
                }
                fields.len()
            },
            BasicMetadataTypeEnum::ArrayType(array_ty) => {
                let fields = return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(*array_ty))
                    .with_context(|| format!("indirect call aggregate argument {index} fields"))?;
                if fields.is_empty() {
                    bail!("empty aggregate indirect call argument {index} is not supported");
                }
                fields.len()
            },
            _ => bail!(
                "only scalar integer, pointer, float, and direct struct/array indirect call arguments are supported"
            ),
        };
        flattened_param_count += leaf_count;
    }
    if flattened_param_count > NATIVE_CALL_MAX_ARGS {
        bail!(
            "only up to {NATIVE_CALL_MAX_ARGS} flattened scalar integer/pointer/float indirect call argument slots are supported, got {flattened_param_count}"
        );
    }

    let return_count = match call_type.get_return_type() {
        None => 0,
        Some(BasicTypeEnum::IntType(return_type)) => {
            checked_width(return_type.get_bit_width()).context("indirect call integer return width")?;
            1
        },
        Some(BasicTypeEnum::PointerType(_)) => 1,
        Some(BasicTypeEnum::FloatType(return_type)) => {
            float_type_width(return_type.as_type_ref()).context("indirect call float return width")?;
            1
        },
        Some(BasicTypeEnum::StructType(return_type)) => {
            let fields = return_fields_from_aggregate_type(BasicTypeEnum::StructType(return_type))
                .context("indirect call return fields")?;
            if fields.is_empty() {
                bail!("empty aggregate indirect call returns are not supported");
            }
            fields.len()
        },
        Some(BasicTypeEnum::ArrayType(return_type)) => {
            let fields = return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(return_type))
                .context("indirect call return fields")?;
            if fields.is_empty() {
                bail!("empty aggregate indirect call returns are not supported");
            }
            fields.len()
        },
        Some(_) => {
            bail!(
                "only void, scalar integer, pointer, float, and direct struct/array indirect call returns are supported"
            )
        },
    };
    if return_count > NATIVE_CALL_MAX_RETURNS {
        bail!("indirect call returns {return_count} fields but call_native supports at most {NATIVE_CALL_MAX_RETURNS}");
    }
    Ok(())
}

fn translator_symbol_suffix(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn should_use_const_pool(imm: u64, width: u8) -> bool {
    width > 16 && imm > 0xff
}

fn x_register_list(name: &str, registers: &[VmRegister]) -> anyhow::Result<Vec<u8>> {
    registers
        .iter()
        .map(|register| match register {
            VmRegister::X(index) => Ok(*index),
            VmRegister::Q(index) => {
                bail!("{name} uses q{index}, but scalar native call lowering only supports x registers")
            },
        })
        .collect()
}

struct GepTerms {
    constant_offset: i64,
    dynamic: Vec<(ValueBinding, u64)>,
}

struct GepIndexStep {
    scale: u64,
    constant_offset: Option<i64>,
    next_type: LLVMTypeRef,
}

fn gep_index_step(
    target_data: &TargetData,
    current_type: LLVMTypeRef,
    index_position: usize,
    index: BasicValueEnum<'_>,
) -> anyhow::Result<GepIndexStep> {
    if !index.is_int_value() {
        bail!("getelementptr index is not an integer");
    }

    if index_position == 0 {
        return Ok(GepIndexStep {
            scale: store_size(target_data, current_type)?,
            constant_offset: None,
            next_type: current_type,
        });
    }

    // SAFETY: `current_type` 来自 LLVM 的 GEP source type，或来自 LLVM 先前返回的 element type。
    // match 只查询 type kind 元数据，不支持的形状会作为 safe-skip 错误返回。
    match unsafe { LLVMGetTypeKind(current_type) } {
        LLVMTypeKind::LLVMArrayTypeKind | LLVMTypeKind::LLVMVectorTypeKind => {
            // SAFETY: 上面的 type kind 保证对 array/vector 查询 element type 是有效的。
            // null 结果仍会在任何 size 计算前被拒绝。
            let element_type = unsafe { LLVMGetElementType(current_type) };
            if element_type.is_null() {
                bail!("getelementptr aggregate element type is unavailable");
            }
            Ok(GepIndexStep {
                scale: store_size(target_data, element_type)?,
                constant_offset: None,
                next_type: element_type,
            })
        },
        LLVMTypeKind::LLVMStructTypeKind => {
            let field = gep_struct_field_index(index)?;
            // SAFETY: `current_type` 已由 LLVM 标识为 struct type；这里只读取字段数量元数据。
            let field_count = unsafe { LLVMCountStructElementTypes(current_type) };
            if field >= field_count {
                bail!("getelementptr struct field index {field} is out of range {field_count}");
            }

            // SAFETY: `field` 已检查在 struct 字段范围内，LLVM 只返回该字段的 type 元数据。
            let element_type = unsafe { LLVMStructGetTypeAtIndex(current_type, field) };
            if element_type.is_null() {
                bail!("getelementptr struct field type is unavailable");
            }

            // SAFETY: `target_data` 来自当前 module data layout，`current_type` 是 struct type，
            // `field` 已做范围检查；LLVM 在这里仅查询该字段的 ABI 字节偏移。
            let offset = unsafe { LLVMOffsetOfElement(target_data.as_mut_ptr(), current_type, field) };
            Ok(GepIndexStep {
                scale: 0,
                constant_offset: Some(i64::try_from(offset).context("getelementptr struct field offset overflow")?),
                next_type: element_type,
            })
        },
        _ => Ok(GepIndexStep {
            scale: store_size(target_data, current_type)?,
            constant_offset: None,
            next_type: current_type,
        }),
    }
}

fn gep_struct_field_index(index: BasicValueEnum<'_>) -> anyhow::Result<u32> {
    let field = index
        .into_int_value()
        .get_sign_extended_constant()
        .context("getelementptr struct field index must be constant")?;
    if field < 0 {
        bail!("getelementptr struct field index must be non-negative");
    }
    u32::try_from(field).context("getelementptr struct field index is too large")
}

fn constant_int_ref(value: LLVMValueRef, context: &str) -> anyhow::Result<i64> {
    if value.is_null() {
        bail!("{context} is null");
    }
    if unsafe { LLVMIsAConstantInt(value) }.is_null() {
        bail!("{context} must be an integer constant");
    }
    Ok(unsafe { LLVMConstIntGetSExtValue(value) })
}

fn single_constant_expr_operand(value: LLVMValueRef, context: &str) -> anyhow::Result<LLVMValueRef> {
    if value.is_null() {
        bail!("{context} is null");
    }
    let operand_count = unsafe { LLVMGetNumOperands(value) };
    if operand_count != 1 {
        bail!("{context} expects exactly one operand");
    }
    let operand = unsafe { LLVMGetOperand(value, 0) };
    if operand.is_null() {
        bail!("{context} operand is null");
    }
    Ok(operand)
}

fn binary_constant_expr_operands(value: LLVMValueRef, context: &str) -> anyhow::Result<(LLVMValueRef, LLVMValueRef)> {
    if value.is_null() {
        bail!("{context} is null");
    }
    let operand_count = unsafe { LLVMGetNumOperands(value) };
    if operand_count != 2 {
        bail!("{context} expects exactly two operands");
    }
    let lhs = unsafe { LLVMGetOperand(value, 0) };
    let rhs = unsafe { LLVMGetOperand(value, 1) };
    if lhs.is_null() || rhs.is_null() {
        bail!("{context} operand is null");
    }
    Ok((lhs, rhs))
}

fn scaled_gep_offset(index: i64, scale: u64) -> anyhow::Result<i64> {
    let scale = i64::try_from(scale).context("getelementptr element size is too large")?;
    index
        .checked_mul(scale)
        .context("getelementptr constant offset overflow")
}

fn store_size(target_data: &TargetData, ty: LLVMTypeRef) -> anyhow::Result<u64> {
    // SAFETY: `target_data` 属于当前 module，`ty` 是从同一 context 取得的 LLVM type。
    // LLVM 在这里只读取 layout 元数据；零结果会被当作不支持处理，而不会继续用于 lowering。
    let size = unsafe { LLVMStoreSizeOfType(target_data.as_mut_ptr(), ty) };
    if size == 0 {
        bail!("zero-sized getelementptr element type is not supported");
    }
    Ok(size)
}

fn leading_phi_nodes(block: BasicBlock<'_>) -> Vec<InstructionValue<'_>> {
    block
        .get_instructions()
        .into_iter()
        .take_while(|instruction| instruction.get_opcode() == InstructionOpcode::Phi)
        .collect()
}

fn phi_incoming_value<'ctx>(
    phi: InstructionValue<'ctx>,
    predecessor: BasicBlock<'ctx>,
) -> anyhow::Result<BasicValueEnum<'ctx>> {
    let phi_value = PhiInst::new(phi).into_phi_value();
    phi_value
        .get_incomings()
        .find_map(|(value, block)| (block == predecessor).then_some(value))
        .context("phi has no incoming value for predecessor")
}

fn instruction_result_width(instruction: InstructionValue<'_>) -> anyhow::Result<Option<u8>> {
    if let Some(reason) = unsupported_control_flow_reason(instruction.get_opcode()) {
        bail!("{reason}");
    }

    match instruction.get_opcode() {
        InstructionOpcode::Br
        | InstructionOpcode::Return
        | InstructionOpcode::Store
        | InstructionOpcode::InsertValue => {
            return Ok(None);
        },
        _ => {},
    }

    match instruction.get_type() {
        AnyTypeEnum::IntType(int_ty) => checked_width(int_ty.get_bit_width()).map(Some),
        AnyTypeEnum::FloatType(float_ty) => float_type_width(float_ty.as_type_ref()).map(Some),
        AnyTypeEnum::PointerType(_) => Ok(Some(64)),
        AnyTypeEnum::VoidType(_) => Ok(None),
        AnyTypeEnum::StructType(_)
            if matches!(
                instruction.get_opcode(),
                InstructionOpcode::Call
                    | InstructionOpcode::AtomicCmpXchg
                    | InstructionOpcode::Freeze
                    | InstructionOpcode::ExtractValue
                    | InstructionOpcode::Load
                    | InstructionOpcode::Select
                    | InstructionOpcode::Phi
            ) =>
        {
            Ok(None)
        },
        AnyTypeEnum::ArrayType(_)
            if matches!(
                instruction.get_opcode(),
                InstructionOpcode::Call
                    | InstructionOpcode::Freeze
                    | InstructionOpcode::ExtractValue
                    | InstructionOpcode::Load
                    | InstructionOpcode::Select
                    | InstructionOpcode::Phi
            ) =>
        {
            Ok(None)
        },
        other => bail!("unsupported instruction result type: {other:?}"),
    }
}

fn instruction_has_aggregate_result(instruction: InstructionValue<'_>) -> bool {
    match instruction.get_opcode() {
        InstructionOpcode::InsertValue | InstructionOpcode::AtomicCmpXchg => true,
        InstructionOpcode::Freeze | InstructionOpcode::ExtractValue => matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_)
        ),
        InstructionOpcode::Load => matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_)
        ),
        InstructionOpcode::Call => matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_)
        ),
        InstructionOpcode::Select => matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_)
        ),
        InstructionOpcode::Phi => matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_)
        ),
        _ => false,
    }
}

fn memory_is_volatile(instruction: InstructionValue<'_>, kind: &str) -> anyhow::Result<bool> {
    instruction
        .get_volatile()
        .with_context(|| format!("{kind} volatile flag cannot be read"))
}

fn ensure_default_atomic_syncscope(instruction: InstructionValue<'_>, kind: &str) -> anyhow::Result<()> {
    // SAFETY: `instruction` 是当前 module 中的 live atomic/fence instruction；C API
    // 只读取 syncscope metadata，不访问用户内存。LLVM 21 在 LLVMContext.h 中定义
    // `SyncScope::System = 1`，其它 scope 先按保守边界跳过。
    let scope = unsafe { LLVMGetAtomicSyncScopeID(instruction.as_value_ref()) };
    if scope != LLVM_SYSTEM_SYNC_SCOPE_ID {
        bail!("{kind} non-default atomic syncscope is not supported by vm_virtualize");
    }
    Ok(())
}

fn memory_ordering(instruction: InstructionValue<'_>, kind: &str) -> anyhow::Result<AtomicOrdering> {
    instruction
        .get_atomic_ordering()
        .with_context(|| format!("{kind} atomic ordering cannot be read"))
}

fn ensure_atomic_load_store_value_type(ty: AnyTypeEnum<'_>, kind: &str) -> anyhow::Result<()> {
    match ty {
        AnyTypeEnum::IntType(_) | AnyTypeEnum::PointerType(_) | AnyTypeEnum::FloatType(_) => Ok(()),
        other => bail!("{kind} atomic memory type is not supported by vm_virtualize: {other:?}"),
    }
}

fn ensure_atomic_basic_value_type(ty: BasicTypeEnum<'_>, kind: &str) -> anyhow::Result<()> {
    match ty {
        BasicTypeEnum::IntType(_) | BasicTypeEnum::PointerType(_) => Ok(()),
        BasicTypeEnum::FloatType(_) => {
            bail!("{kind} atomic floating-point memory access is not supported by vm_virtualize")
        },
        other => bail!("{kind} atomic memory type is not supported by vm_virtualize: {other:?}"),
    }
}

fn ensure_atomic_rmw_value_type(ty: BasicTypeEnum<'_>, op: AtomicRmwOp) -> anyhow::Result<()> {
    match (op.is_floating_point(), ty) {
        (true, BasicTypeEnum::FloatType(float_ty)) => {
            checked_float_width(float_type_width(float_ty.as_type_ref())? as u64)?;
            Ok(())
        },
        (true, other) => bail!("floating atomicrmw operation {op:?} requires float/double operand, got {other:?}"),
        (false, BasicTypeEnum::IntType(_) | BasicTypeEnum::PointerType(_)) => Ok(()),
        (false, BasicTypeEnum::FloatType(_)) => {
            bail!("integer atomicrmw operation {op:?} cannot be applied to floating-point memory")
        },
        (false, other) => bail!("atomicrmw memory type is not supported by vm_virtualize: {other:?}"),
    }
}

fn ensure_atomic_load_store_basic_value_type(ty: BasicTypeEnum<'_>, kind: &str) -> anyhow::Result<()> {
    match ty {
        BasicTypeEnum::IntType(_) | BasicTypeEnum::PointerType(_) | BasicTypeEnum::FloatType(_) => Ok(()),
        other => bail!("{kind} atomic memory type is not supported by vm_virtualize: {other:?}"),
    }
}

fn ensure_naturally_aligned_atomic(instruction: InstructionValue<'_>, kind: &str, width: u8) -> anyhow::Result<()> {
    let bytes = match width {
        8 | 16 | 32 | 64 => u32::from(width) / 8,
        _ => bail!("{kind} atomic width i{width} is not supported by vm_virtualize"),
    };
    let alignment = instruction
        .alignment()
        .with_context(|| format!("{kind} atomic alignment cannot be read"))?;
    if alignment < bytes {
        bail!("{kind} atomic memory access requires natural alignment {bytes}, got {alignment}");
    }
    Ok(())
}

trait InstructionAlignment {
    fn alignment(self) -> anyhow::Result<u32>;
}

impl InstructionAlignment for InstructionValue<'_> {
    fn alignment(self) -> anyhow::Result<u32> {
        match self.get_opcode() {
            InstructionOpcode::AtomicRMW | InstructionOpcode::AtomicCmpXchg => {
                // SAFETY: `self` 是当前 module 中的 live atomic instruction；LLVMGetAlignment 只读取
                // atomicrmw/cmpxchg 的 alignment metadata。0 表示 IR 未声明可用对齐，调用方会拒绝。
                let alignment = unsafe { LLVMGetAlignment(self.as_value_ref()) };
                if alignment == 0 {
                    bail!("atomic instruction does not declare alignment");
                }
                Ok(alignment)
            },
            _ => self.get_alignment().map_err(|error| anyhow::anyhow!("{error}")),
        }
    }
}

fn atomic_ordering_for_load(ordering: AtomicOrdering) -> anyhow::Result<MemoryOrdering> {
    match ordering {
        AtomicOrdering::Unordered => Ok(MemoryOrdering::Unordered),
        AtomicOrdering::Monotonic => Ok(MemoryOrdering::Monotonic),
        AtomicOrdering::Acquire => Ok(MemoryOrdering::Acquire),
        AtomicOrdering::SequentiallyConsistent => Ok(MemoryOrdering::SequentiallyConsistent),
        AtomicOrdering::Release => bail!("load release atomic ordering is invalid for vm_virtualize"),
        AtomicOrdering::AcquireRelease => bail!("load acquire-release atomic ordering is invalid for vm_virtualize"),
        AtomicOrdering::NotAtomic => bail!("load atomic lowering received non-atomic ordering"),
    }
}

fn atomic_ordering_for_store(ordering: AtomicOrdering) -> anyhow::Result<MemoryOrdering> {
    match ordering {
        AtomicOrdering::Unordered => Ok(MemoryOrdering::Unordered),
        AtomicOrdering::Monotonic => Ok(MemoryOrdering::Monotonic),
        AtomicOrdering::Release => Ok(MemoryOrdering::Release),
        AtomicOrdering::SequentiallyConsistent => Ok(MemoryOrdering::SequentiallyConsistent),
        AtomicOrdering::Acquire => bail!("store acquire atomic ordering is invalid for vm_virtualize"),
        AtomicOrdering::AcquireRelease => bail!("store acquire-release atomic ordering is invalid for vm_virtualize"),
        AtomicOrdering::NotAtomic => bail!("store atomic lowering received non-atomic ordering"),
    }
}

fn atomic_ordering_for_fence(ordering: AtomicOrdering) -> anyhow::Result<MemoryOrdering> {
    match ordering {
        AtomicOrdering::Acquire => Ok(MemoryOrdering::Acquire),
        AtomicOrdering::Release => Ok(MemoryOrdering::Release),
        AtomicOrdering::AcquireRelease => Ok(MemoryOrdering::AcquireRelease),
        AtomicOrdering::SequentiallyConsistent => Ok(MemoryOrdering::SequentiallyConsistent),
        AtomicOrdering::Unordered => bail!("fence unordered atomic ordering is invalid for vm_virtualize"),
        AtomicOrdering::Monotonic => bail!("fence monotonic atomic ordering is invalid for vm_virtualize"),
        AtomicOrdering::NotAtomic => bail!("fence lowering received non-atomic ordering"),
    }
}

fn atomic_ordering_for_rmw(ordering: AtomicOrdering) -> anyhow::Result<MemoryOrdering> {
    match ordering {
        AtomicOrdering::Monotonic => Ok(MemoryOrdering::Monotonic),
        AtomicOrdering::Acquire => Ok(MemoryOrdering::Acquire),
        AtomicOrdering::Release => Ok(MemoryOrdering::Release),
        AtomicOrdering::AcquireRelease => Ok(MemoryOrdering::AcquireRelease),
        AtomicOrdering::SequentiallyConsistent => Ok(MemoryOrdering::SequentiallyConsistent),
        AtomicOrdering::Unordered => bail!("atomicrmw unordered atomic ordering is invalid for vm_virtualize"),
        AtomicOrdering::NotAtomic => bail!("atomicrmw lowering received non-atomic ordering"),
    }
}

fn cmpxchg_success_ordering(ordering: AtomicOrdering) -> anyhow::Result<MemoryOrdering> {
    match ordering {
        AtomicOrdering::Monotonic => Ok(MemoryOrdering::Monotonic),
        AtomicOrdering::Acquire => Ok(MemoryOrdering::Acquire),
        AtomicOrdering::Release => Ok(MemoryOrdering::Release),
        AtomicOrdering::AcquireRelease => Ok(MemoryOrdering::AcquireRelease),
        AtomicOrdering::SequentiallyConsistent => Ok(MemoryOrdering::SequentiallyConsistent),
        AtomicOrdering::Unordered => bail!("cmpxchg unordered success ordering is invalid for vm_virtualize"),
        AtomicOrdering::NotAtomic => bail!("cmpxchg lowering received non-atomic success ordering"),
    }
}

fn cmpxchg_failure_ordering(ordering: AtomicOrdering) -> anyhow::Result<MemoryOrdering> {
    match ordering {
        AtomicOrdering::Monotonic => Ok(MemoryOrdering::Monotonic),
        AtomicOrdering::Acquire => Ok(MemoryOrdering::Acquire),
        AtomicOrdering::SequentiallyConsistent => Ok(MemoryOrdering::SequentiallyConsistent),
        AtomicOrdering::Unordered => bail!("cmpxchg unordered failure ordering is invalid for vm_virtualize"),
        AtomicOrdering::Release => bail!("cmpxchg release failure ordering is invalid for vm_virtualize"),
        AtomicOrdering::AcquireRelease => {
            bail!("cmpxchg acquire-release failure ordering is invalid for vm_virtualize")
        },
        AtomicOrdering::NotAtomic => bail!("cmpxchg lowering received non-atomic failure ordering"),
    }
}

fn ensure_cmpxchg_ordering(success: MemoryOrdering, failure: MemoryOrdering) -> anyhow::Result<()> {
    if memory_ordering_rank(failure) > memory_ordering_rank(success) {
        bail!("cmpxchg failure ordering cannot be stronger than success ordering");
    }
    Ok(())
}

fn memory_ordering_rank(ordering: MemoryOrdering) -> u8 {
    match ordering {
        MemoryOrdering::Unordered => 1,
        MemoryOrdering::Monotonic => 2,
        MemoryOrdering::Acquire => 3,
        MemoryOrdering::Release => 4,
        MemoryOrdering::AcquireRelease => 5,
        MemoryOrdering::SequentiallyConsistent => 6,
    }
}

fn map_atomic_rmw_op(op: AtomicRMWBinOp) -> anyhow::Result<AtomicRmwOp> {
    match op {
        AtomicRMWBinOp::Xchg => Ok(AtomicRmwOp::Xchg),
        AtomicRMWBinOp::Add => Ok(AtomicRmwOp::Add),
        AtomicRMWBinOp::Sub => Ok(AtomicRmwOp::Sub),
        AtomicRMWBinOp::And => Ok(AtomicRmwOp::And),
        AtomicRMWBinOp::Or => Ok(AtomicRmwOp::Or),
        AtomicRMWBinOp::Xor => Ok(AtomicRmwOp::Xor),
        AtomicRMWBinOp::Nand => Ok(AtomicRmwOp::Nand),
        AtomicRMWBinOp::Max => Ok(AtomicRmwOp::Max),
        AtomicRMWBinOp::Min => Ok(AtomicRmwOp::Min),
        AtomicRMWBinOp::UMax => Ok(AtomicRmwOp::UMax),
        AtomicRMWBinOp::UMin => Ok(AtomicRmwOp::UMin),
        AtomicRMWBinOp::FAdd => Ok(AtomicRmwOp::FAdd),
        AtomicRMWBinOp::FSub => Ok(AtomicRmwOp::FSub),
        AtomicRMWBinOp::FMax => Ok(AtomicRmwOp::FMax),
        AtomicRMWBinOp::FMin => Ok(AtomicRmwOp::FMin),
        AtomicRMWBinOp::FMaximum => Ok(AtomicRmwOp::FMaximum),
        AtomicRMWBinOp::FMinimum => Ok(AtomicRmwOp::FMinimum),
        AtomicRMWBinOp::UIncWrap => Ok(AtomicRmwOp::UIncWrap),
        AtomicRMWBinOp::UDecWrap => Ok(AtomicRmwOp::UDecWrap),
        AtomicRMWBinOp::USubCond => Ok(AtomicRmwOp::USubCond),
        AtomicRMWBinOp::USubSat => Ok(AtomicRmwOp::USubSat),
    }
}

fn unsupported_control_flow_reason(opcode: InstructionOpcode) -> Option<&'static str> {
    match opcode {
        InstructionOpcode::Invoke => Some("invoke exception edges are not supported by vm_virtualize"),
        InstructionOpcode::CallBr => Some("callbr is not supported by vm_virtualize"),
        InstructionOpcode::IndirectBr => Some("indirectbr is not supported by vm_virtualize"),
        InstructionOpcode::LandingPad => Some("landingpad is not supported by vm_virtualize"),
        InstructionOpcode::Resume => Some("resume is not supported by vm_virtualize"),
        InstructionOpcode::CatchSwitch => Some("catchswitch is not supported by vm_virtualize"),
        InstructionOpcode::CatchRet => Some("catchret is not supported by vm_virtualize"),
        InstructionOpcode::CleanupRet => Some("cleanupret is not supported by vm_virtualize"),
        InstructionOpcode::CatchPad => Some("catchpad is not supported by vm_virtualize"),
        InstructionOpcode::CleanupPad => Some("cleanuppad is not supported by vm_virtualize"),
        InstructionOpcode::VAArg => Some("va_arg is not supported by vm_virtualize"),
        _ => None,
    }
}

fn return_field_from_type(ty: BasicTypeEnum<'_>) -> anyhow::Result<ReturnField> {
    match ty {
        BasicTypeEnum::IntType(int_ty) => Ok(ReturnField {
            width: checked_width(int_ty.get_bit_width())?,
            kind: ScalarKind::Integer,
        }),
        BasicTypeEnum::PointerType(_) => Ok(ReturnField {
            width: 64,
            kind: ScalarKind::Pointer,
        }),
        BasicTypeEnum::FloatType(float_ty) => Ok(ReturnField {
            width: float_type_width(float_ty.as_type_ref())?,
            kind: ScalarKind::Float,
        }),
        other => bail!("unsupported aggregate scalar leaf type: {other:?}"),
    }
}

fn return_fields_from_aggregate_type(ty: BasicTypeEnum<'_>) -> anyhow::Result<Vec<ReturnField>> {
    match ty {
        BasicTypeEnum::StructType(ty) => {
            let mut fields = Vec::new();
            for index in 0..ty.count_fields() {
                let field_ty = ty
                    .get_field_type_at_index(index)
                    .with_context(|| format!("aggregate struct field {index} is unavailable"))?;
                fields.extend(
                    return_fields_from_aggregate_type(field_ty)
                        .with_context(|| format!("aggregate struct field {index}"))?,
                );
            }
            Ok(fields)
        },
        BasicTypeEnum::ArrayType(ty) => {
            let element_ty = ty.get_element_type();
            let element_fields = return_fields_from_aggregate_type(element_ty).context("aggregate array element")?;
            let mut fields = Vec::new();
            for _ in 0..ty.len() {
                fields.extend(element_fields.iter().copied());
            }
            Ok(fields)
        },
        other => Ok(vec![return_field_from_type(other)?]),
    }
}

fn aggregate_leaf_count(ty: BasicTypeEnum<'_>) -> anyhow::Result<usize> {
    match ty {
        BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_) => Ok(return_fields_from_aggregate_type(ty)?.len()),
        other => bail!("unsupported aggregate value type: {other:?}"),
    }
}

fn instruction_aggregate_type(instruction: InstructionValue<'_>) -> anyhow::Result<BasicTypeEnum<'_>> {
    match instruction.get_type() {
        AnyTypeEnum::StructType(ty) => Ok(BasicTypeEnum::StructType(ty)),
        AnyTypeEnum::ArrayType(ty) => Ok(BasicTypeEnum::ArrayType(ty)),
        other => bail!("instruction result is not an aggregate: {other:?}"),
    }
}

fn aggregate_memory_fields<'ctx>(
    target_data: &TargetData,
    ty: BasicTypeEnum<'ctx>,
) -> anyhow::Result<Vec<AggregateMemoryField>> {
    let mut fields = Vec::new();
    collect_aggregate_memory_fields(target_data, ty, 0, &mut fields)?;
    Ok(fields)
}

fn collect_aggregate_memory_fields<'ctx>(
    target_data: &TargetData,
    ty: BasicTypeEnum<'ctx>,
    base_offset: u64,
    fields: &mut Vec<AggregateMemoryField>,
) -> anyhow::Result<()> {
    match ty {
        BasicTypeEnum::StructType(ty) => {
            for index in 0..ty.count_fields() {
                let field_ty = ty
                    .get_field_type_at_index(index)
                    .with_context(|| format!("aggregate memory struct field {index} is unavailable"))?;
                // LLVM data layout owns padding and packed-struct rules. The VM only sees byte offsets.
                let field_offset = unsafe { LLVMOffsetOfElement(target_data.as_mut_ptr(), ty.as_type_ref(), index) };
                let offset = base_offset
                    .checked_add(field_offset)
                    .context("aggregate memory struct field offset overflow")?;
                collect_aggregate_memory_fields(target_data, field_ty, offset, fields)
                    .with_context(|| format!("aggregate memory struct field {index}"))?;
            }
            Ok(())
        },
        BasicTypeEnum::ArrayType(ty) => {
            let element_ty = ty.get_element_type();
            let stride = store_size(target_data, element_ty.as_type_ref()).context("aggregate memory array stride")?;
            for index in 0..ty.len() {
                let element_offset = u64::from(index)
                    .checked_mul(stride)
                    .and_then(|offset| base_offset.checked_add(offset))
                    .context("aggregate memory array element offset overflow")?;
                collect_aggregate_memory_fields(target_data, element_ty, element_offset, fields)
                    .with_context(|| format!("aggregate memory array element {index}"))?;
            }
            Ok(())
        },
        other => {
            fields.push(AggregateMemoryField {
                offset: base_offset,
                info: return_field_from_type(other)?,
            });
            Ok(())
        },
    }
}

fn aggregate_selection_from_instruction(instruction: InstructionValue<'_>) -> anyhow::Result<AggregateSelection> {
    let value = instruction_operand_value(instruction, 0)?;
    let indices = aggregate_indices(instruction)?;
    aggregate_selection_at_indices(value.get_type(), &indices)
}

fn aggregate_indices(instruction: InstructionValue<'_>) -> anyhow::Result<Vec<u32>> {
    let indices = instruction.get_indices();
    if indices.is_empty() {
        bail!("aggregate instruction does not select a field");
    }
    Ok(indices)
}

fn aggregate_selection_at_indices(ty: BasicTypeEnum<'_>, indices: &[u32]) -> anyhow::Result<AggregateSelection> {
    let Some((index, rest)) = indices.split_first() else {
        return Ok(AggregateSelection {
            start: 0,
            fields: return_fields_from_aggregate_type(ty)?,
            is_aggregate: matches!(ty, BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_)),
        });
    };

    match ty {
        BasicTypeEnum::StructType(ty) => {
            if *index >= ty.count_fields() {
                bail!("aggregate struct index {index} is out of range");
            }

            let mut flattened_index = 0;
            for field_index in 0..*index {
                let field_ty = ty
                    .get_field_type_at_index(field_index)
                    .with_context(|| format!("aggregate struct field {field_index} is unavailable"))?;
                flattened_index += return_fields_from_aggregate_type(field_ty)
                    .with_context(|| format!("aggregate struct field {field_index}"))?
                    .len();
            }

            let field_ty = ty
                .get_field_type_at_index(*index)
                .with_context(|| format!("aggregate struct field {index} is unavailable"))?;
            let mut selection = aggregate_selection_at_indices(field_ty, rest)
                .with_context(|| format!("aggregate struct field {index}"))?;
            selection.start += flattened_index;
            Ok(selection)
        },
        BasicTypeEnum::ArrayType(ty) => {
            if *index >= ty.len() {
                bail!("aggregate array index {index} is out of range");
            }
            let element_ty = ty.get_element_type();
            let element_leaf_count = return_fields_from_aggregate_type(element_ty)
                .context("aggregate array element")?
                .len();
            let mut selection = aggregate_selection_at_indices(element_ty, rest).context("aggregate array element")?;
            selection.start += (*index as usize) * element_leaf_count;
            Ok(selection)
        },
        other => bail!("aggregate index descends into scalar field type: {other:?}"),
    }
}

fn instruction_value_operands(instruction: InstructionValue<'_>) -> Vec<BasicValueEnum<'_>> {
    let operand_count = match instruction.get_opcode() {
        InstructionOpcode::Call => instruction.get_num_operands().saturating_sub(1),
        _ => instruction.get_num_operands(),
    };

    (0..operand_count)
        .filter_map(|index| instruction_basic_operand(instruction, index))
        .collect()
}

fn instruction_operand_value(instruction: InstructionValue<'_>, index: u32) -> anyhow::Result<BasicValueEnum<'_>> {
    instruction_basic_operand(instruction, index).with_context(|| format!("missing value operand {index}"))
}

fn instruction_basic_operand(instruction: InstructionValue<'_>, index: u32) -> Option<BasicValueEnum<'_>> {
    // SAFETY: `instruction` is a live LLVM instruction from the current module. This helper only
    // inspects operand/type tags and skips non-BasicValue operands before constructing
    // `BasicValueEnum`; inkwell panics if metadata is converted as a basic value.
    unsafe {
        let value = LLVMGetOperand(instruction.as_value_ref(), index);
        if value.is_null() {
            return None;
        }
        let ty = LLVMTypeOf(value);
        if ty.is_null() {
            return None;
        }
        match LLVMGetTypeKind(ty) {
            LLVMTypeKind::LLVMVoidTypeKind
            | LLVMTypeKind::LLVMLabelTypeKind
            | LLVMTypeKind::LLVMMetadataTypeKind
            | LLVMTypeKind::LLVMTokenTypeKind
            | LLVMTypeKind::LLVMFunctionTypeKind => None,
            LLVMTypeKind::LLVMHalfTypeKind
            | LLVMTypeKind::LLVMBFloatTypeKind
            | LLVMTypeKind::LLVMFloatTypeKind
            | LLVMTypeKind::LLVMDoubleTypeKind
            | LLVMTypeKind::LLVMX86_FP80TypeKind
            | LLVMTypeKind::LLVMFP128TypeKind
            | LLVMTypeKind::LLVMPPC_FP128TypeKind
            | LLVMTypeKind::LLVMIntegerTypeKind
            | LLVMTypeKind::LLVMStructTypeKind
            | LLVMTypeKind::LLVMArrayTypeKind
            | LLVMTypeKind::LLVMPointerTypeKind
            | LLVMTypeKind::LLVMVectorTypeKind
            | LLVMTypeKind::LLVMScalableVectorTypeKind
            | LLVMTypeKind::LLVMX86_AMXTypeKind
            | LLVMTypeKind::LLVMTargetExtTypeKind => Some(BasicValueEnum::new(value)),
        }
    }
}

fn memory_intrinsic_kind(function: FunctionValue<'_>) -> Option<MemoryIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.memcpy.") || name.starts_with("llvm.memcpy.inline.") {
        Some(MemoryIntrinsicKind::Memcpy)
    } else if name.starts_with("llvm.memmove.") {
        Some(MemoryIntrinsicKind::Memmove)
    } else if name.starts_with("llvm.memset.") || name.starts_with("llvm.memset.inline.") {
        Some(MemoryIntrinsicKind::Memset)
    } else {
        None
    }
}

fn trap_intrinsic_kind(function: FunctionValue<'_>) -> Option<TrapIntrinsicKind> {
    match function.get_name().to_string_lossy().as_ref() {
        "llvm.trap" => Some(TrapIntrinsicKind::Trap),
        "llvm.debugtrap" => Some(TrapIntrinsicKind::DebugTrap),
        "llvm.ubsantrap" => Some(TrapIntrinsicKind::UbsanTrap),
        _ => None,
    }
}

fn counter_intrinsic_kind(function: FunctionValue<'_>) -> Option<CounterKind> {
    match function.get_name().to_string_lossy().as_ref() {
        "llvm.readcyclecounter" => Some(CounterKind::Cycle),
        "llvm.readsteadycounter" => Some(CounterKind::Steady),
        _ => None,
    }
}

fn sideeffect_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().as_ref() == "llvm.sideeffect"
}

fn counter_intrinsic_lowering_rule(kind: CounterKind) -> &'static str {
    match kind {
        CounterKind::Cycle => "llvm.readcyclecounter.integer",
        CounterKind::Steady => "llvm.readsteadycounter.integer",
    }
}

fn nop_intrinsic_kind(function: FunctionValue<'_>) -> Option<NopIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.lifetime.start.") {
        Some(NopIntrinsicKind::LifetimeStart)
    } else if name.starts_with("llvm.lifetime.end.") {
        Some(NopIntrinsicKind::LifetimeEnd)
    } else if name.starts_with("llvm.invariant.end.") {
        Some(NopIntrinsicKind::InvariantEnd)
    } else if name.starts_with("llvm.prefetch.") {
        Some(NopIntrinsicKind::Prefetch)
    } else if name == "llvm.experimental.noalias.scope.decl" {
        Some(NopIntrinsicKind::NoAliasScopeDecl)
    } else if name == "llvm.donothing" {
        Some(NopIntrinsicKind::DoNothing)
    } else if name == "llvm.assume" {
        Some(NopIntrinsicKind::Assume)
    } else if name.starts_with("llvm.dbg.") {
        Some(NopIntrinsicKind::Debug)
    } else if name.starts_with("llvm.var.annotation.") {
        Some(NopIntrinsicKind::VarAnnotation)
    } else if name == "llvm.codeview.annotation" {
        Some(NopIntrinsicKind::CodeViewAnnotation)
    } else {
        None
    }
}

fn identity_intrinsic_kind(function: FunctionValue<'_>) -> Option<IdentityIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.expect.with.probability.") {
        Some(IdentityIntrinsicKind::ExpectWithProbability)
    } else if name.starts_with("llvm.expect.") {
        Some(IdentityIntrinsicKind::Expect)
    } else if name.starts_with("llvm.ssa.copy.") {
        Some(IdentityIntrinsicKind::SsaCopyScalar)
    } else if name.starts_with("llvm.launder.invariant.group.") {
        Some(IdentityIntrinsicKind::LaunderInvariantGroup)
    } else if name.starts_with("llvm.strip.invariant.group.") {
        Some(IdentityIntrinsicKind::StripInvariantGroup)
    } else if name.starts_with("llvm.invariant.start.") {
        Some(IdentityIntrinsicKind::InvariantStart)
    } else if name.starts_with("llvm.annotation.") {
        Some(IdentityIntrinsicKind::AnnotationInteger)
    } else if name.starts_with("llvm.ptr.annotation.") {
        Some(IdentityIntrinsicKind::PtrAnnotationPointer)
    } else if name.starts_with("llvm.threadlocal.address.") {
        Some(IdentityIntrinsicKind::ThreadLocalAddress)
    } else {
        None
    }
}

fn pointer_intrinsic_kind(function: FunctionValue<'_>) -> Option<PointerIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.ptrmask.") {
        Some(PointerIntrinsicKind::PtrMask)
    } else {
        None
    }
}

fn compile_time_intrinsic_kind(function: FunctionValue<'_>) -> Option<CompileTimeIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.is.constant.") {
        Some(CompileTimeIntrinsicKind::IsConstant)
    } else if name.starts_with("llvm.objectsize.") {
        Some(CompileTimeIntrinsicKind::ObjectSize)
    } else {
        None
    }
}

fn float_intrinsic_kind(function: FunctionValue<'_>) -> Option<FloatIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.fabs.") {
        Some(FloatIntrinsicKind::FAbs)
    } else if name.starts_with("llvm.sqrt.") {
        Some(FloatIntrinsicKind::Sqrt)
    } else if name.starts_with("llvm.canonicalize.") {
        Some(FloatIntrinsicKind::Canonicalize)
    } else if name.starts_with("llvm.floor.") {
        Some(FloatIntrinsicKind::Floor)
    } else if name.starts_with("llvm.ceil.") {
        Some(FloatIntrinsicKind::Ceil)
    } else if name.starts_with("llvm.trunc.") {
        Some(FloatIntrinsicKind::Trunc)
    } else if name.starts_with("llvm.rint.") {
        Some(FloatIntrinsicKind::Rint)
    } else if name.starts_with("llvm.nearbyint.") {
        Some(FloatIntrinsicKind::NearbyInt)
    } else if name.starts_with("llvm.roundeven.") {
        Some(FloatIntrinsicKind::RoundEven)
    } else if name.starts_with("llvm.round.") {
        Some(FloatIntrinsicKind::Round)
    } else if name.starts_with("llvm.fma.") {
        Some(FloatIntrinsicKind::Fma)
    } else if name.starts_with("llvm.fmuladd.") {
        Some(FloatIntrinsicKind::FmulAdd)
    } else if name.starts_with("llvm.minnum.") {
        Some(FloatIntrinsicKind::MinNum)
    } else if name.starts_with("llvm.maxnum.") {
        Some(FloatIntrinsicKind::MaxNum)
    } else if name.starts_with("llvm.minimum.") {
        Some(FloatIntrinsicKind::Minimum)
    } else if name.starts_with("llvm.maximum.") {
        Some(FloatIntrinsicKind::Maximum)
    } else if name.starts_with("llvm.copysign.") {
        Some(FloatIntrinsicKind::CopySign)
    } else if name.starts_with("llvm.is.fpclass.") {
        Some(FloatIntrinsicKind::IsFpClass)
    } else {
        None
    }
}

fn integer_intrinsic_kind(function: FunctionValue<'_>) -> Option<IntegerIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.ctpop.") {
        Some(IntegerIntrinsicKind::CtPop)
    } else if name.starts_with("llvm.ctlz.") {
        Some(IntegerIntrinsicKind::CtLz)
    } else if name.starts_with("llvm.cttz.") {
        Some(IntegerIntrinsicKind::CtTz)
    } else if name.starts_with("llvm.abs.") {
        Some(IntegerIntrinsicKind::Abs)
    } else if name.starts_with("llvm.smax.") {
        Some(IntegerIntrinsicKind::SMax)
    } else if name.starts_with("llvm.smin.") {
        Some(IntegerIntrinsicKind::SMin)
    } else if name.starts_with("llvm.umax.") {
        Some(IntegerIntrinsicKind::UMax)
    } else if name.starts_with("llvm.umin.") {
        Some(IntegerIntrinsicKind::UMin)
    } else if name.starts_with("llvm.uadd.sat.") {
        Some(IntegerIntrinsicKind::UAddSat)
    } else if name.starts_with("llvm.usub.sat.") {
        Some(IntegerIntrinsicKind::USubSat)
    } else if name.starts_with("llvm.sadd.sat.") {
        Some(IntegerIntrinsicKind::SAddSat)
    } else if name.starts_with("llvm.ssub.sat.") {
        Some(IntegerIntrinsicKind::SSubSat)
    } else if name.starts_with("llvm.ushl.sat.") {
        Some(IntegerIntrinsicKind::UShlSat)
    } else if name.starts_with("llvm.sshl.sat.") {
        Some(IntegerIntrinsicKind::SShlSat)
    } else if name.starts_with("llvm.uadd.with.overflow.") {
        Some(IntegerIntrinsicKind::UAddOverflow)
    } else if name.starts_with("llvm.sadd.with.overflow.") {
        Some(IntegerIntrinsicKind::SAddOverflow)
    } else if name.starts_with("llvm.usub.with.overflow.") {
        Some(IntegerIntrinsicKind::USubOverflow)
    } else if name.starts_with("llvm.ssub.with.overflow.") {
        Some(IntegerIntrinsicKind::SSubOverflow)
    } else if name.starts_with("llvm.umul.with.overflow.") {
        Some(IntegerIntrinsicKind::UMulOverflow)
    } else if name.starts_with("llvm.smul.with.overflow.") {
        Some(IntegerIntrinsicKind::SMulOverflow)
    } else if name.starts_with("llvm.bswap.") {
        Some(IntegerIntrinsicKind::BSwap)
    } else if name.starts_with("llvm.bitreverse.") {
        Some(IntegerIntrinsicKind::BitReverse)
    } else if name.starts_with("llvm.fshl.") {
        Some(IntegerIntrinsicKind::FShl)
    } else if name.starts_with("llvm.fshr.") {
        Some(IntegerIntrinsicKind::FShr)
    } else {
        None
    }
}

fn constant_int_operand(instruction: InstructionValue<'_>, index: u32, name: &str) -> anyhow::Result<u64> {
    let value = instruction_operand_value(instruction, index)?;
    if is_undef_or_poison_value(value) {
        bail!("{name} cannot be undef or poison");
    }
    if !value.is_int_value() {
        bail!("{name} must be an integer constant");
    }
    value
        .into_int_value()
        .get_zero_extended_constant()
        .with_context(|| format!("{name} must be a compile-time constant"))
}

fn memory_intrinsic_is_volatile(instruction: InstructionValue<'_>, volatile_index: u32) -> anyhow::Result<bool> {
    let Some(value) = instruction_basic_operand(instruction, volatile_index) else {
        return Ok(false);
    };
    if !value.is_int_value() {
        bail!("memory intrinsic volatile flag must be an integer constant");
    }
    let Some(flag) = value.into_int_value().get_zero_extended_constant() else {
        bail!("memory intrinsic volatile flag must be a compile-time constant");
    };
    Ok(flag != 0)
}

fn memory_copy_chunks(len: u64) -> Vec<MemoryChunk> {
    let mut chunks = Vec::new();
    let mut offset = 0_u64;
    while offset < len {
        let remaining = len - offset;
        let (bytes, width) = if remaining >= 8 {
            (8, 64)
        } else if remaining >= 4 {
            (4, 32)
        } else if remaining >= 2 {
            (2, 16)
        } else {
            (1, 8)
        };
        chunks.push(MemoryChunk { offset, width });
        offset += bytes;
    }
    chunks
}

fn value_width(value: BasicValueEnum<'_>) -> anyhow::Result<u8> {
    match value.get_type() {
        BasicTypeEnum::IntType(int_ty) => checked_width(int_ty.get_bit_width()),
        BasicTypeEnum::FloatType(float_ty) => float_type_width(float_ty.as_type_ref()),
        BasicTypeEnum::PointerType(_) => Ok(64),
        other => bail!("unsupported scalar value type: {other:?}"),
    }
}

fn ensure_scalar_copy_shape(
    source: BasicValueEnum<'_>,
    result_type: AnyTypeEnum<'_>,
    kind: IdentityIntrinsicKind,
) -> anyhow::Result<()> {
    match (source.get_type(), result_type) {
        (BasicTypeEnum::IntType(source_type), AnyTypeEnum::IntType(result_type)) => {
            let source_width = checked_width(source_type.get_bit_width())?;
            let result_width = checked_width(result_type.get_bit_width())?;
            if source_width != result_width {
                bail!(
                    "identity intrinsic {:?} integer scalar copy width mismatch: result i{}, value i{}",
                    kind,
                    result_width,
                    source_width
                );
            }
        },
        (BasicTypeEnum::PointerType(_), AnyTypeEnum::PointerType(_)) => {},
        (BasicTypeEnum::FloatType(source_type), AnyTypeEnum::FloatType(result_type)) => {
            let source_width = float_type_width(source_type.as_type_ref())?;
            let result_width = float_type_width(result_type.as_type_ref())?;
            if source_width != result_width {
                bail!(
                    "identity intrinsic {:?} float scalar copy width mismatch: result i{}, value i{}",
                    kind,
                    result_width,
                    source_width
                );
            }
        },
        (source_type, result_type) => {
            bail!(
                "identity intrinsic {:?} only supports integer, pointer, float, and double scalar copies; got source {:?}, result {:?}",
                kind,
                source_type,
                result_type
            );
        },
    }
    Ok(())
}

/// 返回当前 VMP 标量浮点路径支持的 LLVM float value 位宽。
///
/// # Errors
/// 当 value 不是 `float` 或 `double` 时返回错误；更宽或特殊浮点类型暂不进入 x-register lowering。
pub fn float_value_width(value: amice_plugin::inkwell::values::FloatValue<'_>) -> anyhow::Result<u8> {
    float_type_width(value.get_type().as_type_ref())
}

/// 返回当前 VMP 标量浮点路径支持的 LLVM float type 位宽。
///
/// # Errors
/// 仅接受 LLVM `float` 和 `double`，其它浮点类型会作为 safe-skip 边界返回错误。
pub fn float_type_width(type_ref: LLVMTypeRef) -> anyhow::Result<u8> {
    // SAFETY: caller passes an LLVM type reference from the current module/context. This only
    // inspects the type kind and does not dereference user memory.
    match unsafe { LLVMGetTypeKind(type_ref) } {
        LLVMTypeKind::LLVMFloatTypeKind => Ok(32),
        LLVMTypeKind::LLVMDoubleTypeKind => Ok(64),
        other => bail!("unsupported floating point type kind: {other:?}"),
    }
}

fn is_undef_or_poison_value(value: BasicValueEnum<'_>) -> bool {
    value_is_undef(value) || value.is_poison()
}

fn value_is_undef(value: BasicValueEnum<'_>) -> bool {
    match value {
        BasicValueEnum::ArrayValue(value) => value.is_undef(),
        BasicValueEnum::IntValue(value) => value.is_undef(),
        BasicValueEnum::FloatValue(value) => value.is_undef(),
        BasicValueEnum::PointerValue(value) => value.is_undef(),
        BasicValueEnum::StructValue(value) => value.is_undef(),
        BasicValueEnum::VectorValue(value) => value.is_undef(),
        BasicValueEnum::ScalableVectorValue(value) => value.is_undef(),
    }
}

fn checked_width(width: u32) -> anyhow::Result<u8> {
    if matches!(width, 1 | 8 | 16 | 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("unsupported integer width: {width}")
    }
}

fn checked_width_u64(width: u64) -> anyhow::Result<u8> {
    u32::try_from(width)
        .context("integer width does not fit in u32")
        .and_then(checked_width)
}

fn checked_intrinsic_integer_width(width: u64) -> anyhow::Result<u8> {
    if matches!(width, 8 | 16 | 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("integer intrinsic width i{width} is not supported by vm_virtualize")
    }
}

fn checked_atomic_memory_width(width: u64) -> anyhow::Result<u8> {
    if matches!(width, 8 | 16 | 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("unsupported atomic memory width: {width}")
    }
}

fn checked_float_width(width: u64) -> anyhow::Result<u8> {
    if matches!(width, 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("unsupported floating point width: {width}")
    }
}

fn checked_fpclass_mask(mask: u64) -> anyhow::Result<u16> {
    if mask <= FPCLASS_ALL_FLAGS {
        Ok(mask as u16)
    } else {
        bail!("llvm.is.fpclass mask 0x{mask:x} exceeds supported FPClassTest bits 0x{FPCLASS_ALL_FLAGS:x}")
    }
}

fn width_from_operand_type(value_type: &str) -> u8 {
    value_type
        .strip_prefix('i')
        .and_then(|width| width.parse::<u8>().ok())
        .filter(|width| matches!(width, 1 | 8 | 16 | 32 | 64))
        .unwrap_or(64)
}

fn width_from_lowering_value_type(value_type: &str, env: &LoweringEnv<'_>) -> anyhow::Result<u8> {
    if let Some(width) = value_type.strip_prefix('i').and_then(|width| width.parse::<u64>().ok()) {
        return checked_width_u64(width);
    }

    if matches!(value_type, "integer" | "float" | "scalar" | "call_result" | "aggregate") {
        for expression in [
            "type_width(%r)",
            "memory_width(%ptr)",
            "type_width(%field)",
            "operand_width(%a,%b)",
        ] {
            if let Ok(LoweringValue::Imm(width)) = env.get(expression) {
                return checked_width_u64(width);
            }
        }
    }

    Ok(width_from_operand_type(value_type))
}

fn memory_ordering_from_u64(value: u64) -> anyhow::Result<MemoryOrdering> {
    match value {
        value if value == MemoryOrdering::Unordered as u64 => Ok(MemoryOrdering::Unordered),
        value if value == MemoryOrdering::Monotonic as u64 => Ok(MemoryOrdering::Monotonic),
        value if value == MemoryOrdering::Acquire as u64 => Ok(MemoryOrdering::Acquire),
        value if value == MemoryOrdering::Release as u64 => Ok(MemoryOrdering::Release),
        value if value == MemoryOrdering::AcquireRelease as u64 => Ok(MemoryOrdering::AcquireRelease),
        value if value == MemoryOrdering::SequentiallyConsistent as u64 => Ok(MemoryOrdering::SequentiallyConsistent),
        other => bail!("unsupported atomic memory ordering value {other}"),
    }
}

fn parse_u64_literal(value: &str) -> Option<u64> {
    value
        .strip_prefix("0x")
        .map_or_else(|| value.parse::<u64>().ok(), |hex| u64::from_str_radix(hex, 16).ok())
}

fn predicate_from_u64(value: u64) -> anyhow::Result<CmpPredicate> {
    match value {
        value if value == CmpPredicate::Eq as u64 => Ok(CmpPredicate::Eq),
        value if value == CmpPredicate::Ne as u64 => Ok(CmpPredicate::Ne),
        value if value == CmpPredicate::Ugt as u64 => Ok(CmpPredicate::Ugt),
        value if value == CmpPredicate::Uge as u64 => Ok(CmpPredicate::Uge),
        value if value == CmpPredicate::Ult as u64 => Ok(CmpPredicate::Ult),
        value if value == CmpPredicate::Ule as u64 => Ok(CmpPredicate::Ule),
        value if value == CmpPredicate::Sgt as u64 => Ok(CmpPredicate::Sgt),
        value if value == CmpPredicate::Sge as u64 => Ok(CmpPredicate::Sge),
        value if value == CmpPredicate::Slt as u64 => Ok(CmpPredicate::Slt),
        value if value == CmpPredicate::Sle as u64 => Ok(CmpPredicate::Sle),
        other => bail!("unsupported comparison predicate value {other}"),
    }
}

fn float_predicate_from_u64(value: u64) -> anyhow::Result<VmFloatPredicate> {
    match value {
        value if value == VmFloatPredicate::False as u64 => Ok(VmFloatPredicate::False),
        value if value == VmFloatPredicate::Oeq as u64 => Ok(VmFloatPredicate::Oeq),
        value if value == VmFloatPredicate::Ogt as u64 => Ok(VmFloatPredicate::Ogt),
        value if value == VmFloatPredicate::Oge as u64 => Ok(VmFloatPredicate::Oge),
        value if value == VmFloatPredicate::Olt as u64 => Ok(VmFloatPredicate::Olt),
        value if value == VmFloatPredicate::Ole as u64 => Ok(VmFloatPredicate::Ole),
        value if value == VmFloatPredicate::One as u64 => Ok(VmFloatPredicate::One),
        value if value == VmFloatPredicate::Ord as u64 => Ok(VmFloatPredicate::Ord),
        value if value == VmFloatPredicate::Uno as u64 => Ok(VmFloatPredicate::Uno),
        value if value == VmFloatPredicate::Ueq as u64 => Ok(VmFloatPredicate::Ueq),
        value if value == VmFloatPredicate::Ugt as u64 => Ok(VmFloatPredicate::Ugt),
        value if value == VmFloatPredicate::Uge as u64 => Ok(VmFloatPredicate::Uge),
        value if value == VmFloatPredicate::Ult as u64 => Ok(VmFloatPredicate::Ult),
        value if value == VmFloatPredicate::Ule as u64 => Ok(VmFloatPredicate::Ule),
        value if value == VmFloatPredicate::Une as u64 => Ok(VmFloatPredicate::Une),
        value if value == VmFloatPredicate::True as u64 => Ok(VmFloatPredicate::True),
        other => bail!("unsupported floating comparison predicate value {other}"),
    }
}

fn map_predicate(predicate: IntPredicate) -> CmpPredicate {
    match predicate {
        IntPredicate::EQ => CmpPredicate::Eq,
        IntPredicate::NE => CmpPredicate::Ne,
        IntPredicate::UGT => CmpPredicate::Ugt,
        IntPredicate::UGE => CmpPredicate::Uge,
        IntPredicate::ULT => CmpPredicate::Ult,
        IntPredicate::ULE => CmpPredicate::Ule,
        IntPredicate::SGT => CmpPredicate::Sgt,
        IntPredicate::SGE => CmpPredicate::Sge,
        IntPredicate::SLT => CmpPredicate::Slt,
        IntPredicate::SLE => CmpPredicate::Sle,
    }
}

fn map_float_predicate(predicate: LlvmFloatPredicate) -> VmFloatPredicate {
    match predicate {
        LlvmFloatPredicate::PredicateFalse => VmFloatPredicate::False,
        LlvmFloatPredicate::OEQ => VmFloatPredicate::Oeq,
        LlvmFloatPredicate::OGT => VmFloatPredicate::Ogt,
        LlvmFloatPredicate::OGE => VmFloatPredicate::Oge,
        LlvmFloatPredicate::OLT => VmFloatPredicate::Olt,
        LlvmFloatPredicate::OLE => VmFloatPredicate::Ole,
        LlvmFloatPredicate::ONE => VmFloatPredicate::One,
        LlvmFloatPredicate::ORD => VmFloatPredicate::Ord,
        LlvmFloatPredicate::UNO => VmFloatPredicate::Uno,
        LlvmFloatPredicate::UEQ => VmFloatPredicate::Ueq,
        LlvmFloatPredicate::UGT => VmFloatPredicate::Ugt,
        LlvmFloatPredicate::UGE => VmFloatPredicate::Uge,
        LlvmFloatPredicate::ULT => VmFloatPredicate::Ult,
        LlvmFloatPredicate::ULE => VmFloatPredicate::Ule,
        LlvmFloatPredicate::UNE => VmFloatPredicate::Une,
        LlvmFloatPredicate::PredicateTrue => VmFloatPredicate::True,
    }
}

fn value_key(value: BasicValueEnum<'_>) -> ValueKey {
    value.as_value_ref() as usize
}

fn instruction_key(instruction: InstructionValue<'_>) -> ValueKey {
    instruction.as_value_ref() as usize
}

fn block_key(block: BasicBlock<'_>) -> BlockKey {
    block.as_mut_ptr() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_control_flow_reason_covers_exception_pad_opcodes() {
        for (opcode, expected) in [
            (
                InstructionOpcode::CatchSwitch,
                "catchswitch is not supported by vm_virtualize",
            ),
            (
                InstructionOpcode::CatchPad,
                "catchpad is not supported by vm_virtualize",
            ),
            (
                InstructionOpcode::CatchRet,
                "catchret is not supported by vm_virtualize",
            ),
            (
                InstructionOpcode::CleanupPad,
                "cleanuppad is not supported by vm_virtualize",
            ),
            (
                InstructionOpcode::CleanupRet,
                "cleanupret is not supported by vm_virtualize",
            ),
        ] {
            assert_eq!(unsupported_control_flow_reason(opcode), Some(expected));
        }
        assert_eq!(unsupported_control_flow_reason(InstructionOpcode::Unreachable), None);
    }
}
