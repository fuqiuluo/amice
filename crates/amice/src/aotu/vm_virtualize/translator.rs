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

use amice_llvm::inkwell2::{
    CallInst, FunctionExt, GepInst, InstructionExt, ModuleExt, PhiInst, SwitchInst, metadata_string_from_value_ref,
};
use amice_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use amice_plugin::inkwell::basic_block::BasicBlock;
use amice_plugin::inkwell::llvm_sys::core::{
    LLVMConstIntGetSExtValue, LLVMCountStructElementTypes, LLVMGetAggregateElement, LLVMGetAlignment,
    LLVMGetAllocatedType, LLVMGetAtomicSyncScopeID, LLVMGetCalledFunctionType, LLVMGetCalledValue,
    LLVMGetCmpXchgFailureOrdering, LLVMGetCmpXchgSuccessOrdering, LLVMGetConstOpcode, LLVMGetElementType,
    LLVMGetGEPSourceElementType, LLVMGetMaskValue, LLVMGetNumMaskElements, LLVMGetNumOperandBundles,
    LLVMGetNumOperands, LLVMGetOperand, LLVMGetPointerAddressSpace, LLVMGetTypeKind, LLVMGetValueKind,
    LLVMGlobalGetValueType, LLVMIsAAddrSpaceCastInst, LLVMIsAAllocaInst, LLVMIsABitCastInst, LLVMIsAConstant,
    LLVMIsAConstantAggregateZero, LLVMIsAConstantDataVector, LLVMIsAConstantExpr, LLVMIsAConstantInt,
    LLVMIsAConstantVector, LLVMIsAGetElementPtrInst, LLVMIsAGlobalValue, LLVMIsAGlobalVariable, LLVMIsAInlineAsm,
    LLVMIsInBounds, LLVMStructGetTypeAtIndex, LLVMTypeOf,
};
use amice_plugin::inkwell::llvm_sys::prelude::{LLVMTypeRef, LLVMValueRef};
use amice_plugin::inkwell::llvm_sys::target::{LLVMOffsetOfElement, LLVMStoreSizeOfType};
use amice_plugin::inkwell::llvm_sys::{LLVMOpcode, LLVMTypeKind, LLVMValueKind};
use amice_plugin::inkwell::module::{Linkage, Module};
use amice_plugin::inkwell::targets::TargetData;
use amice_plugin::inkwell::types::{
    AnyTypeEnum, AsTypeRef, BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType,
};
use amice_plugin::inkwell::values::{
    AnyValue, AsValueRef, BasicMetadataValueEnum, BasicValueEnum, CallSiteValue, FunctionValue, InstructionOpcode,
    InstructionValue, LLVMTailCallKind, PointerValue, UnnamedAddress,
};
use amice_plugin::inkwell::{
    AddressSpace, AtomicOrdering, AtomicRMWBinOp, FloatPredicate as LlvmFloatPredicate, IntPredicate,
};
use amice_vm::abi::{AbiProfile, VmRegister};
use amice_vm::isa::{
    AtomicRmwOp, BinOp, CastOp, CmpPredicate, CounterKind, FloatBinOp, FloatCastOp, FloatIntBinOp,
    FloatPredicate as VmFloatPredicate, FloatRoundToIntOp, FloatTernaryOp, FloatUnaryOp, FpStateKind, HandlerSemantic,
    InstructionDesc, IntOverflowOp, IntTernaryOp, IntUnaryOp, IsaProfile, MemoryOrdering, OperandKind,
};
use amice_vm::profile::{LoweringAction, LoweringProfile, LoweringRule, lowering_match_pattern};
use amice_vm::{
    HOST_VM_MAX_ARGS, LabelId, NATIVE_CALL_MAX_ARGS, NATIVE_CALL_MAX_RETURNS, NativeReturn, VmFunction,
    VmFunctionBuilder, VmInstruction, fuse_superinstructions,
};
use anyhow::{Context, bail};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

type ValueKey = usize;
type BlockKey = usize;

const MAX_MEMORY_INTRINSIC_INLINE_BYTES: u64 = 64;
const LLVM_SINGLETHREAD_SYNC_SCOPE_ID: u32 = 0;
const LLVM_SYSTEM_SYNC_SCOPE_ID: u32 = 1;
const FPCLASS_ALL_FLAGS: u64 = 0x03ff;
const DEFAULT_X86_64_DATA_LAYOUT: &str =
    "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128";

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
    // 使用 x 寄存器保存 f16/f32/f64 的原始 bit。
    Float,
}

fn scalar_kind_prefix(kind: ScalarKind) -> &'static str {
    match kind {
        ScalarKind::Integer => "i",
        ScalarKind::Pointer => "ptr",
        ScalarKind::Float => "f",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryIntrinsicKind {
    Memcpy,
    Memmove,
    Memset,
}

#[derive(Debug, Clone, Copy)]
enum MaskedMemoryIntrinsicKind {
    Load,
    Store,
    ExpandLoad,
    CompressStore,
    Gather,
    Scatter,
    VpLoad,
    VpStore,
    VpGather,
    VpScatter,
    VpStridedLoad,
    VpStridedStore,
}

impl MaskedMemoryIntrinsicKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Load | Self::VpLoad => "llvm.memory.masked.vector.load",
            Self::Store | Self::VpStore => "llvm.memory.masked.vector.store",
            Self::ExpandLoad => "llvm.memory.masked.vector.expandload",
            Self::CompressStore => "llvm.memory.masked.vector.compressstore",
            Self::Gather | Self::VpGather => "llvm.memory.masked.vector.gather",
            Self::Scatter | Self::VpScatter => "llvm.memory.masked.vector.scatter",
            Self::VpStridedLoad => "llvm.memory.vp.strided.vector.load",
            Self::VpStridedStore => "llvm.memory.vp.strided.vector.store",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum StackIntrinsicKind {
    Save,
    Restore,
}

#[derive(Debug, Clone, Copy)]
enum FpStateIntrinsicKind {
    Get(FpStateKind),
    Set(FpStateKind),
    Reset(FpStateKind),
    SetRounding,
}

impl FpStateIntrinsicKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Get(FpStateKind::Env) => "llvm.get.fpenv.integer",
            Self::Set(FpStateKind::Env) => "llvm.set.fpenv.integer",
            Self::Reset(FpStateKind::Env) => "llvm.reset.fpenv",
            Self::Get(FpStateKind::Mode) => "llvm.get.fpmode.integer",
            Self::Set(FpStateKind::Mode) => "llvm.set.fpmode.integer",
            Self::Reset(FpStateKind::Mode) => "llvm.reset.fpmode",
            Self::SetRounding => "llvm.set.rounding.integer",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        match self {
            Self::Get(kind) => HandlerSemantic::ReadFpState(kind),
            Self::Set(kind) => HandlerSemantic::WriteFpState(kind),
            Self::Reset(kind) => HandlerSemantic::ResetFpState(kind),
            Self::SetRounding => HandlerSemantic::WriteRounding,
        }
    }
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
    NoAliasScopeDecl,
    DoNothing,
    FakeUse,
    Assume,
    Debug,
    VarAnnotation,
    CodeViewAnnotation,
}

#[derive(Debug, Clone, Copy)]
enum HardwareLoopIntrinsicKind {
    SetIterations,
    StartIterations,
    TestSetIterations,
    TestStartIterations,
}

impl HardwareLoopIntrinsicKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::SetIterations => "llvm.set.loop.iterations.integer",
            Self::StartIterations => "llvm.start.loop.iterations.integer",
            Self::TestSetIterations => "llvm.test.set.loop.iterations.integer",
            Self::TestStartIterations => "llvm.test.start.loop.iterations.integer",
        }
    }
}

impl NopIntrinsicKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::LifetimeStart => "llvm.lifetime.start",
            Self::LifetimeEnd => "llvm.lifetime.end",
            Self::InvariantEnd => "llvm.invariant.end",
            Self::NoAliasScopeDecl => "llvm.experimental.noalias.scope.decl",
            Self::DoNothing => "llvm.donothing",
            Self::FakeUse => "llvm.fake.use",
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
            Self::DoNothing => Some(0),
            Self::Assume => Some(1),
            Self::Debug | Self::NoAliasScopeDecl | Self::FakeUse | Self::CodeViewAnnotation => None,
            Self::VarAnnotation => Some(5),
        }
    }

    fn constant_operand_indices(self) -> &'static [u32] {
        match self {
            Self::LifetimeStart | Self::LifetimeEnd => &[0],
            Self::InvariantEnd => &[1],
            Self::Assume
            | Self::Debug
            | Self::NoAliasScopeDecl
            | Self::DoNothing
            | Self::FakeUse
            | Self::VarAnnotation
            | Self::CodeViewAnnotation => &[],
        }
    }

    fn pointer_operand_indices(self) -> &'static [u32] {
        match self {
            Self::LifetimeStart
            | Self::LifetimeEnd
            | Self::InvariantEnd
            | Self::NoAliasScopeDecl
            | Self::DoNothing
            | Self::FakeUse
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
    ArithmeticFenceScalar,
    LaunderInvariantGroup,
    StripInvariantGroup,
    PreserveArrayAccessIndex,
    PreserveUnionAccessIndex,
    PreserveStructAccessIndex,
    PreserveStaticOffset,
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
            Self::ArithmeticFenceScalar => "llvm.arithmetic.fence.scalar",
            Self::LaunderInvariantGroup => "llvm.launder.invariant.group.pointer",
            Self::StripInvariantGroup => "llvm.strip.invariant.group.pointer",
            Self::PreserveArrayAccessIndex => "llvm.preserve.array.access.index.pointer",
            Self::PreserveUnionAccessIndex => "llvm.preserve.union.access.index.pointer",
            Self::PreserveStructAccessIndex => "llvm.preserve.struct.access.index.pointer",
            Self::PreserveStaticOffset => "llvm.preserve.static.offset.pointer",
            Self::InvariantStart => "llvm.invariant.start.pointer",
            Self::AnnotationInteger => "llvm.annotation.integer",
            Self::PtrAnnotationPointer => "llvm.ptr.annotation.pointer",
            Self::ThreadLocalAddress => "llvm.threadlocal.address.pointer",
        }
    }

    fn vector_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::Expect => Some("llvm.vector.expect.integer"),
            Self::ExpectWithProbability => Some("llvm.vector.expect.with_probability.integer"),
            Self::SsaCopyScalar => Some("llvm.vector.ssa.copy"),
            Self::ArithmeticFenceScalar => Some("llvm.vector.arithmetic.fence"),
            Self::LaunderInvariantGroup
            | Self::StripInvariantGroup
            | Self::PreserveArrayAccessIndex
            | Self::PreserveUnionAccessIndex
            | Self::PreserveStructAccessIndex
            | Self::PreserveStaticOffset
            | Self::InvariantStart
            | Self::AnnotationInteger
            | Self::PtrAnnotationPointer
            | Self::ThreadLocalAddress => None,
        }
    }

    fn arg_count(self) -> u32 {
        match self {
            Self::SsaCopyScalar => 1,
            Self::ArithmeticFenceScalar => 1,
            Self::Expect => 2,
            Self::ExpectWithProbability => 3,
            Self::LaunderInvariantGroup | Self::StripInvariantGroup => 1,
            Self::PreserveArrayAccessIndex | Self::PreserveStructAccessIndex => 3,
            Self::PreserveUnionAccessIndex => 2,
            Self::PreserveStaticOffset => 1,
            Self::InvariantStart => 2,
            Self::AnnotationInteger => 4,
            Self::PtrAnnotationPointer => 5,
            Self::ThreadLocalAddress => 1,
        }
    }

    fn value_operand_index(self) -> u32 {
        match self {
            Self::SsaCopyScalar => 0,
            Self::ArithmeticFenceScalar => 0,
            Self::InvariantStart => 1,
            Self::Expect
            | Self::ExpectWithProbability
            | Self::LaunderInvariantGroup
            | Self::StripInvariantGroup
            | Self::PreserveArrayAccessIndex
            | Self::PreserveUnionAccessIndex
            | Self::PreserveStructAccessIndex
            | Self::PreserveStaticOffset
            | Self::AnnotationInteger
            | Self::PtrAnnotationPointer
            | Self::ThreadLocalAddress => 0,
        }
    }

    fn constant_operand_indices(self) -> &'static [u32] {
        match self {
            Self::InvariantStart => &[0],
            Self::PreserveArrayAccessIndex | Self::PreserveStructAccessIndex => &[1, 2],
            Self::PreserveUnionAccessIndex => &[1],
            Self::Expect
            | Self::ExpectWithProbability
            | Self::SsaCopyScalar
            | Self::ArithmeticFenceScalar
            | Self::LaunderInvariantGroup
            | Self::StripInvariantGroup
            | Self::PreserveStaticOffset
            | Self::AnnotationInteger
            | Self::PtrAnnotationPointer
            | Self::ThreadLocalAddress => &[],
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
                | Self::PreserveArrayAccessIndex
                | Self::PreserveUnionAccessIndex
                | Self::PreserveStructAccessIndex
                | Self::PreserveStaticOffset
                | Self::InvariantStart
                | Self::PtrAnnotationPointer
                | Self::ThreadLocalAddress
        )
    }

    fn is_integer_identity(self) -> bool {
        matches!(self, Self::AnnotationInteger)
    }

    fn is_scalar_copy(self) -> bool {
        matches!(self, Self::SsaCopyScalar | Self::ArithmeticFenceScalar)
    }
}

#[derive(Debug, Clone, Copy)]
enum PointerIntrinsicKind {
    PtrMask,
}

#[derive(Debug, Clone, Copy)]
enum VectorPermuteIntrinsicKind {
    Reverse,
    Splice,
    InsertSubvector,
    ExtractSubvector,
    Interleave(u8),
    Deinterleave(u8),
    Compress,
}

impl VectorPermuteIntrinsicKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Reverse => "llvm.vector.reverse.element",
            Self::Splice => "llvm.vector.splice.element",
            Self::InsertSubvector => "llvm.vector.insert.subvector.element",
            Self::ExtractSubvector => "llvm.vector.extract.subvector.element",
            Self::Interleave(_) => "llvm.vector.interleave.element",
            Self::Deinterleave(_) => "llvm.vector.deinterleave.element",
            Self::Compress => "llvm.experimental.vector.compress.element",
        }
    }

    fn arg_count(self) -> u32 {
        match self {
            Self::Reverse => 1,
            Self::Splice => 3,
            Self::InsertSubvector => 3,
            Self::ExtractSubvector => 2,
            Self::Interleave(factor) => u32::from(factor),
            Self::Deinterleave(_) => 1,
            Self::Compress => 3,
        }
    }
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
    AllowRuntimeCheck,
    AllowUbsanCheck,
    IsConstant,
    ObjectSize,
    WidenableCondition,
}

impl CompileTimeIntrinsicKind {
    fn result(self, value: BasicValueEnum<'_>) -> anyhow::Result<u64> {
        match self {
            Self::IsConstant => {
                ensure_is_constant_query_operand(value)?;
                // SAFETY: `value` 属于当前 live LLVM module。这里仅查询 Value 的常量分类，
                // 不读取用户内存，也不会把运行时值当作编译期常量折叠。
                Ok(u64::from(!unsafe { LLVMIsAConstant(value.as_value_ref()) }.is_null()))
            },
            Self::ObjectSize => bail!("llvm.objectsize needs target data and is handled by lower_objectsize_intrinsic"),
            Self::WidenableCondition => {
                bail!("llvm.experimental.widenable.condition has no operand and is handled separately")
            },
            Self::AllowRuntimeCheck | Self::AllowUbsanCheck => {
                bail!("LLVM runtime-check gate intrinsics are handled separately")
            },
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
    Sin,
    Cos,
    Exp,
    Exp2,
    Log,
    Log10,
    Log2,
    Fma,
    FmulAdd,
    MinNum,
    MaxNum,
    Minimum,
    Maximum,
    CopySign,
    Pow,
    PowI,
    IsFpClass,
    FPToSISat,
    FPToUISat,
    LRint,
    LLRint,
    LRound,
    LLRound,
    ConvertToFp16,
    ConvertFromFp16,
}

#[derive(Debug, Clone, Copy)]
enum ConstrainedFloatBinOpKind {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Debug, Clone, Copy)]
enum ConstrainedFloatCmpKind {
    Quiet,
    Signaling,
}

#[derive(Debug, Clone, Copy)]
enum ConstrainedFloatCastKind {
    SIToFP,
    UIToFP,
    FPToSI,
    FPToUI,
    FPTrunc,
    FPExt,
}

impl ConstrainedFloatBinOpKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Add => "llvm.constrained.fadd.float",
            Self::Sub => "llvm.constrained.fsub.float",
            Self::Mul => "llvm.constrained.fmul.float",
            Self::Div => "llvm.constrained.fdiv.float",
            Self::Rem => "llvm.constrained.frem.float",
        }
    }

    fn vector_lowering_rule(self) -> &'static str {
        match self {
            Self::Add => "llvm.constrained.vector.fadd.float",
            Self::Sub => "llvm.constrained.vector.fsub.float",
            Self::Mul => "llvm.constrained.vector.fmul.float",
            Self::Div => "llvm.constrained.vector.fdiv.float",
            Self::Rem => "llvm.constrained.vector.frem.float",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::FloatBin(match self {
            Self::Add => FloatBinOp::Add,
            Self::Sub => FloatBinOp::Sub,
            Self::Mul => FloatBinOp::Mul,
            Self::Div => FloatBinOp::Div,
            Self::Rem => FloatBinOp::Rem,
        })
    }
}

impl ConstrainedFloatCmpKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Quiet => "llvm.constrained.fcmp.float",
            Self::Signaling => "llvm.constrained.fcmps.float",
        }
    }

    fn vector_lowering_rule(self) -> &'static str {
        match self {
            Self::Quiet => "llvm.constrained.vector.fcmp.float",
            Self::Signaling => "llvm.constrained.vector.fcmps.float",
        }
    }
}

impl ConstrainedFloatCastKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::SIToFP => "llvm.constrained.sitofp.float",
            Self::UIToFP => "llvm.constrained.uitofp.float",
            Self::FPToSI => "llvm.constrained.fptosi.float",
            Self::FPToUI => "llvm.constrained.fptoui.float",
            Self::FPTrunc => "llvm.constrained.fptrunc.float",
            Self::FPExt => "llvm.constrained.fpext.float",
        }
    }

    fn vector_lowering_rule(self) -> &'static str {
        match self {
            Self::SIToFP => "llvm.constrained.vector.sitofp.float",
            Self::UIToFP => "llvm.constrained.vector.uitofp.float",
            Self::FPToSI => "llvm.constrained.vector.fptosi.float",
            Self::FPToUI => "llvm.constrained.vector.fptoui.float",
            Self::FPTrunc => "llvm.constrained.vector.fptrunc.float",
            Self::FPExt => "llvm.constrained.vector.fpext.float",
        }
    }

    fn op(self) -> FloatCastOp {
        match self {
            Self::SIToFP => FloatCastOp::SignedIntToFloat,
            Self::UIToFP => FloatCastOp::UnsignedIntToFloat,
            Self::FPToSI => FloatCastOp::FloatToSignedInt,
            Self::FPToUI => FloatCastOp::FloatToUnsignedInt,
            Self::FPTrunc => FloatCastOp::FloatTrunc,
            Self::FPExt => FloatCastOp::FloatExt,
        }
    }

    fn has_rounding_mode(self) -> bool {
        matches!(self, Self::SIToFP | Self::UIToFP | Self::FPTrunc)
    }

    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::FloatCast(self.op())
    }
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
            Self::Sin => "llvm.sin.float",
            Self::Cos => "llvm.cos.float",
            Self::Exp => "llvm.exp.float",
            Self::Exp2 => "llvm.exp2.float",
            Self::Log => "llvm.log.float",
            Self::Log10 => "llvm.log10.float",
            Self::Log2 => "llvm.log2.float",
            Self::Fma => "llvm.fma.float",
            Self::FmulAdd => "llvm.fmuladd.float",
            Self::MinNum => "llvm.minnum.float",
            Self::MaxNum => "llvm.maxnum.float",
            Self::Minimum => "llvm.minimum.float",
            Self::Maximum => "llvm.maximum.float",
            Self::CopySign => "llvm.copysign.float",
            Self::Pow => "llvm.pow.float",
            Self::PowI => "llvm.powi.float",
            Self::IsFpClass => "llvm.is.fpclass.float",
            Self::FPToSISat => "llvm.fptosi.sat.float",
            Self::FPToUISat => "llvm.fptoui.sat.float",
            Self::LRint => "llvm.lrint.float",
            Self::LLRint => "llvm.llrint.float",
            Self::LRound => "llvm.lround.float",
            Self::LLRound => "llvm.llround.float",
            Self::ConvertToFp16 => "llvm.convert.to.fp16",
            Self::ConvertFromFp16 => "llvm.convert.from.fp16",
        }
    }

    fn constrained_unary_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::FAbs => Some("llvm.constrained.fabs.float"),
            Self::Sqrt => Some("llvm.constrained.sqrt.float"),
            Self::Canonicalize => Some("llvm.constrained.canonicalize.float"),
            Self::Floor => Some("llvm.constrained.floor.float"),
            Self::Ceil => Some("llvm.constrained.ceil.float"),
            Self::Trunc => Some("llvm.constrained.trunc.float"),
            Self::Rint => Some("llvm.constrained.rint.float"),
            Self::NearbyInt => Some("llvm.constrained.nearbyint.float"),
            Self::Round => Some("llvm.constrained.round.float"),
            Self::RoundEven => Some("llvm.constrained.roundeven.float"),
            Self::Sin => Some("llvm.constrained.sin.float"),
            Self::Cos => Some("llvm.constrained.cos.float"),
            Self::Exp => Some("llvm.constrained.exp.float"),
            Self::Exp2 => Some("llvm.constrained.exp2.float"),
            Self::Log => Some("llvm.constrained.log.float"),
            Self::Log10 => Some("llvm.constrained.log10.float"),
            Self::Log2 => Some("llvm.constrained.log2.float"),
            Self::Fma
            | Self::FmulAdd
            | Self::MinNum
            | Self::MaxNum
            | Self::Minimum
            | Self::Maximum
            | Self::CopySign
            | Self::Pow
            | Self::PowI
            | Self::IsFpClass
            | Self::FPToSISat
            | Self::FPToUISat
            | Self::LRint
            | Self::LLRint
            | Self::LRound
            | Self::LLRound
            | Self::ConvertToFp16
            | Self::ConvertFromFp16 => None,
        }
    }

    fn constrained_vector_unary_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::FAbs => Some("llvm.constrained.vector.fabs.float"),
            Self::Sqrt => Some("llvm.constrained.vector.sqrt.float"),
            Self::Canonicalize => Some("llvm.constrained.vector.canonicalize.float"),
            Self::Floor => Some("llvm.constrained.vector.floor.float"),
            Self::Ceil => Some("llvm.constrained.vector.ceil.float"),
            Self::Trunc => Some("llvm.constrained.vector.trunc.float"),
            Self::Rint => Some("llvm.constrained.vector.rint.float"),
            Self::NearbyInt => Some("llvm.constrained.vector.nearbyint.float"),
            Self::Round => Some("llvm.constrained.vector.round.float"),
            Self::RoundEven => Some("llvm.constrained.vector.roundeven.float"),
            Self::Sin => Some("llvm.constrained.vector.sin.float"),
            Self::Cos => Some("llvm.constrained.vector.cos.float"),
            Self::Exp => Some("llvm.constrained.vector.exp.float"),
            Self::Exp2 => Some("llvm.constrained.vector.exp2.float"),
            Self::Log => Some("llvm.constrained.vector.log.float"),
            Self::Log10 => Some("llvm.constrained.vector.log10.float"),
            Self::Log2 => Some("llvm.constrained.vector.log2.float"),
            Self::Fma
            | Self::FmulAdd
            | Self::MinNum
            | Self::MaxNum
            | Self::Minimum
            | Self::Maximum
            | Self::CopySign
            | Self::Pow
            | Self::PowI
            | Self::IsFpClass
            | Self::FPToSISat
            | Self::FPToUISat
            | Self::LRint
            | Self::LLRint
            | Self::LRound
            | Self::LLRound
            | Self::ConvertToFp16
            | Self::ConvertFromFp16 => None,
        }
    }

    fn constrained_unary_has_rounding_mode(self) -> bool {
        matches!(
            self,
            Self::Sqrt
                | Self::Rint
                | Self::NearbyInt
                | Self::Sin
                | Self::Cos
                | Self::Exp
                | Self::Exp2
                | Self::Log
                | Self::Log10
                | Self::Log2
        )
    }

    fn constrained_binary_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::Pow => Some("llvm.constrained.pow.float"),
            Self::MinNum => Some("llvm.constrained.minnum.float"),
            Self::MaxNum => Some("llvm.constrained.maxnum.float"),
            Self::Minimum => Some("llvm.constrained.minimum.float"),
            Self::Maximum => Some("llvm.constrained.maximum.float"),
            Self::CopySign => Some("llvm.constrained.copysign.float"),
            Self::FAbs
            | Self::Sqrt
            | Self::Canonicalize
            | Self::Floor
            | Self::Ceil
            | Self::Trunc
            | Self::Rint
            | Self::NearbyInt
            | Self::Round
            | Self::RoundEven
            | Self::Sin
            | Self::Cos
            | Self::Exp
            | Self::Exp2
            | Self::Log
            | Self::Log10
            | Self::Log2
            | Self::Fma
            | Self::FmulAdd
            | Self::PowI
            | Self::IsFpClass
            | Self::FPToSISat
            | Self::FPToUISat
            | Self::LRint
            | Self::LLRint
            | Self::LRound
            | Self::LLRound
            | Self::ConvertToFp16
            | Self::ConvertFromFp16 => None,
        }
    }

    fn constrained_vector_binary_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::Pow => Some("llvm.constrained.vector.pow.float"),
            Self::MinNum => Some("llvm.constrained.vector.minnum.float"),
            Self::MaxNum => Some("llvm.constrained.vector.maxnum.float"),
            Self::Minimum => Some("llvm.constrained.vector.minimum.float"),
            Self::Maximum => Some("llvm.constrained.vector.maximum.float"),
            Self::CopySign => Some("llvm.constrained.vector.copysign.float"),
            Self::FAbs
            | Self::Sqrt
            | Self::Canonicalize
            | Self::Floor
            | Self::Ceil
            | Self::Trunc
            | Self::Rint
            | Self::NearbyInt
            | Self::Round
            | Self::RoundEven
            | Self::Sin
            | Self::Cos
            | Self::Exp
            | Self::Exp2
            | Self::Log
            | Self::Log10
            | Self::Log2
            | Self::Fma
            | Self::FmulAdd
            | Self::PowI
            | Self::IsFpClass
            | Self::FPToSISat
            | Self::FPToUISat
            | Self::LRint
            | Self::LLRint
            | Self::LRound
            | Self::LLRound
            | Self::ConvertToFp16
            | Self::ConvertFromFp16 => None,
        }
    }

    fn constrained_binary_has_rounding_mode(self) -> bool {
        matches!(self, Self::Pow)
    }

    fn constrained_float_int_binary_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::PowI => Some("llvm.constrained.powi.float"),
            Self::FAbs
            | Self::Sqrt
            | Self::Canonicalize
            | Self::Floor
            | Self::Ceil
            | Self::Trunc
            | Self::Rint
            | Self::NearbyInt
            | Self::Round
            | Self::RoundEven
            | Self::Sin
            | Self::Cos
            | Self::Exp
            | Self::Exp2
            | Self::Log
            | Self::Log10
            | Self::Log2
            | Self::Fma
            | Self::FmulAdd
            | Self::MinNum
            | Self::MaxNum
            | Self::Minimum
            | Self::Maximum
            | Self::CopySign
            | Self::Pow
            | Self::IsFpClass
            | Self::FPToSISat
            | Self::FPToUISat
            | Self::LRint
            | Self::LLRint
            | Self::LRound
            | Self::LLRound
            | Self::ConvertToFp16
            | Self::ConvertFromFp16 => None,
        }
    }

    fn constrained_vector_float_int_binary_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::PowI => Some("llvm.constrained.vector.powi.float"),
            Self::FAbs
            | Self::Sqrt
            | Self::Canonicalize
            | Self::Floor
            | Self::Ceil
            | Self::Trunc
            | Self::Rint
            | Self::NearbyInt
            | Self::Round
            | Self::RoundEven
            | Self::Sin
            | Self::Cos
            | Self::Exp
            | Self::Exp2
            | Self::Log
            | Self::Log10
            | Self::Log2
            | Self::Fma
            | Self::FmulAdd
            | Self::MinNum
            | Self::MaxNum
            | Self::Minimum
            | Self::Maximum
            | Self::CopySign
            | Self::Pow
            | Self::IsFpClass
            | Self::FPToSISat
            | Self::FPToUISat
            | Self::LRint
            | Self::LLRint
            | Self::LRound
            | Self::LLRound
            | Self::ConvertToFp16
            | Self::ConvertFromFp16 => None,
        }
    }

    fn constrained_ternary_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::Fma => Some("llvm.constrained.fma.float"),
            Self::FmulAdd => Some("llvm.constrained.fmuladd.float"),
            Self::FAbs
            | Self::Sqrt
            | Self::Canonicalize
            | Self::Floor
            | Self::Ceil
            | Self::Trunc
            | Self::Rint
            | Self::NearbyInt
            | Self::Round
            | Self::RoundEven
            | Self::Sin
            | Self::Cos
            | Self::Exp
            | Self::Exp2
            | Self::Log
            | Self::Log10
            | Self::Log2
            | Self::MinNum
            | Self::MaxNum
            | Self::Minimum
            | Self::Maximum
            | Self::CopySign
            | Self::Pow
            | Self::PowI
            | Self::IsFpClass
            | Self::FPToSISat
            | Self::FPToUISat
            | Self::LRint
            | Self::LLRint
            | Self::LRound
            | Self::LLRound
            | Self::ConvertToFp16
            | Self::ConvertFromFp16 => None,
        }
    }

    fn constrained_vector_ternary_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::Fma => Some("llvm.constrained.vector.fma.float"),
            Self::FmulAdd => Some("llvm.constrained.vector.fmuladd.float"),
            Self::FAbs
            | Self::Sqrt
            | Self::Canonicalize
            | Self::Floor
            | Self::Ceil
            | Self::Trunc
            | Self::Rint
            | Self::NearbyInt
            | Self::Round
            | Self::RoundEven
            | Self::Sin
            | Self::Cos
            | Self::Exp
            | Self::Exp2
            | Self::Log
            | Self::Log10
            | Self::Log2
            | Self::MinNum
            | Self::MaxNum
            | Self::Minimum
            | Self::Maximum
            | Self::CopySign
            | Self::Pow
            | Self::PowI
            | Self::IsFpClass
            | Self::FPToSISat
            | Self::FPToUISat
            | Self::LRint
            | Self::LLRint
            | Self::LRound
            | Self::LLRound
            | Self::ConvertToFp16
            | Self::ConvertFromFp16 => None,
        }
    }

    fn constrained_round_to_int_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::LRint => Some("llvm.constrained.lrint.float"),
            Self::LLRint => Some("llvm.constrained.llrint.float"),
            Self::LRound => Some("llvm.constrained.lround.float"),
            Self::LLRound => Some("llvm.constrained.llround.float"),
            Self::FAbs
            | Self::Sqrt
            | Self::Canonicalize
            | Self::Floor
            | Self::Ceil
            | Self::Trunc
            | Self::Rint
            | Self::NearbyInt
            | Self::Round
            | Self::RoundEven
            | Self::Sin
            | Self::Cos
            | Self::Exp
            | Self::Exp2
            | Self::Log
            | Self::Log10
            | Self::Log2
            | Self::Fma
            | Self::FmulAdd
            | Self::MinNum
            | Self::MaxNum
            | Self::Minimum
            | Self::Maximum
            | Self::CopySign
            | Self::Pow
            | Self::PowI
            | Self::IsFpClass
            | Self::FPToSISat
            | Self::FPToUISat
            | Self::ConvertToFp16
            | Self::ConvertFromFp16 => None,
        }
    }

    fn constrained_round_to_int_has_rounding_mode(self) -> bool {
        matches!(self, Self::LRint | Self::LLRint)
    }

    fn vector_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::FAbs => Some("llvm.vector.fabs.float"),
            Self::Sqrt => Some("llvm.vector.sqrt.float"),
            Self::Canonicalize => Some("llvm.vector.canonicalize.float"),
            Self::Floor => Some("llvm.vector.floor.float"),
            Self::Ceil => Some("llvm.vector.ceil.float"),
            Self::Trunc => Some("llvm.vector.trunc.float"),
            Self::Rint => Some("llvm.vector.rint.float"),
            Self::NearbyInt => Some("llvm.vector.nearbyint.float"),
            Self::Round => Some("llvm.vector.round.float"),
            Self::RoundEven => Some("llvm.vector.roundeven.float"),
            Self::Sin => Some("llvm.vector.sin.float"),
            Self::Cos => Some("llvm.vector.cos.float"),
            Self::Exp => Some("llvm.vector.exp.float"),
            Self::Exp2 => Some("llvm.vector.exp2.float"),
            Self::Log => Some("llvm.vector.log.float"),
            Self::Log10 => Some("llvm.vector.log10.float"),
            Self::Log2 => Some("llvm.vector.log2.float"),
            Self::MinNum => Some("llvm.vector.minnum.float"),
            Self::MaxNum => Some("llvm.vector.maxnum.float"),
            Self::Minimum => Some("llvm.vector.minimum.float"),
            Self::Maximum => Some("llvm.vector.maximum.float"),
            Self::CopySign => Some("llvm.vector.copysign.float"),
            Self::Pow => Some("llvm.vector.pow.float"),
            Self::PowI => Some("llvm.vector.powi.float"),
            Self::Fma => Some("llvm.vector.fma.float"),
            Self::FmulAdd => Some("llvm.vector.fmuladd.float"),
            Self::IsFpClass => Some("llvm.vector.is.fpclass.float"),
            Self::FPToSISat => Some("llvm.vector.fptosi.sat.float"),
            Self::FPToUISat => Some("llvm.vector.fptoui.sat.float"),
            Self::LRint => Some("llvm.vector.lrint.float"),
            Self::LLRint => Some("llvm.vector.llrint.float"),
            Self::LRound => Some("llvm.vector.lround.float"),
            Self::LLRound => Some("llvm.vector.llround.float"),
            Self::ConvertToFp16 | Self::ConvertFromFp16 => None,
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
            Self::Sin => HandlerSemantic::FloatUnary(FloatUnaryOp::Sin),
            Self::Cos => HandlerSemantic::FloatUnary(FloatUnaryOp::Cos),
            Self::Exp => HandlerSemantic::FloatUnary(FloatUnaryOp::Exp),
            Self::Exp2 => HandlerSemantic::FloatUnary(FloatUnaryOp::Exp2),
            Self::Log => HandlerSemantic::FloatUnary(FloatUnaryOp::Log),
            Self::Log10 => HandlerSemantic::FloatUnary(FloatUnaryOp::Log10),
            Self::Log2 => HandlerSemantic::FloatUnary(FloatUnaryOp::Log2),
            Self::Fma => HandlerSemantic::FloatTernary(FloatTernaryOp::Fma),
            Self::FmulAdd => HandlerSemantic::FloatTernary(FloatTernaryOp::MulAdd),
            Self::MinNum => HandlerSemantic::FloatBin(FloatBinOp::MinNum),
            Self::MaxNum => HandlerSemantic::FloatBin(FloatBinOp::MaxNum),
            Self::Minimum => HandlerSemantic::FloatBin(FloatBinOp::Minimum),
            Self::Maximum => HandlerSemantic::FloatBin(FloatBinOp::Maximum),
            Self::CopySign => HandlerSemantic::FloatBin(FloatBinOp::CopySign),
            Self::Pow => HandlerSemantic::FloatBin(FloatBinOp::Pow),
            Self::PowI => HandlerSemantic::FloatIntBin(FloatIntBinOp::PowI),
            Self::IsFpClass => HandlerSemantic::FloatClass,
            Self::FPToSISat => HandlerSemantic::FloatCast(FloatCastOp::FloatToSignedIntSat),
            Self::FPToUISat => HandlerSemantic::FloatCast(FloatCastOp::FloatToUnsignedIntSat),
            Self::LRint => HandlerSemantic::FloatRoundToInt(FloatRoundToIntOp::LRint),
            Self::LLRint => HandlerSemantic::FloatRoundToInt(FloatRoundToIntOp::LLRint),
            Self::LRound => HandlerSemantic::FloatRoundToInt(FloatRoundToIntOp::LRound),
            Self::LLRound => HandlerSemantic::FloatRoundToInt(FloatRoundToIntOp::LLRound),
            Self::ConvertToFp16 => HandlerSemantic::FloatCast(FloatCastOp::FloatTrunc),
            Self::ConvertFromFp16 => HandlerSemantic::FloatCast(FloatCastOp::FloatExt),
        }
    }

    fn accepts_half(self) -> bool {
        matches!(
            self,
            Self::FAbs
                | Self::Sqrt
                | Self::Canonicalize
                | Self::Floor
                | Self::Ceil
                | Self::Trunc
                | Self::Rint
                | Self::NearbyInt
                | Self::Round
                | Self::RoundEven
                | Self::Sin
                | Self::Cos
                | Self::Exp
                | Self::Exp2
                | Self::Log
                | Self::Log10
                | Self::Log2
                | Self::Fma
                | Self::FmulAdd
                | Self::MinNum
                | Self::MaxNum
                | Self::Minimum
                | Self::Maximum
                | Self::CopySign
                | Self::Pow
                | Self::PowI
                | Self::FPToSISat
                | Self::FPToUISat
                | Self::LRint
                | Self::LLRint
                | Self::LRound
                | Self::LLRound
        )
    }
}

#[derive(Debug, Clone, Copy)]
enum VectorReduceFloatKind {
    Add,
    Mul,
    Min,
    Max,
    Minimum,
    Maximum,
}

impl VectorReduceFloatKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Add => "llvm.vector.reduce.fadd.float",
            Self::Mul => "llvm.vector.reduce.fmul.float",
            Self::Min => "llvm.vector.reduce.fmin.float",
            Self::Max => "llvm.vector.reduce.fmax.float",
            Self::Minimum => "llvm.vector.reduce.fminimum.float",
            Self::Maximum => "llvm.vector.reduce.fmaximum.float",
        }
    }

    fn vp_lowering_rule(self) -> &'static str {
        match self {
            Self::Add => "llvm.vp.reduce.fadd.float",
            Self::Mul => "llvm.vp.reduce.fmul.float",
            Self::Min => "llvm.vp.reduce.fmin.float",
            Self::Max => "llvm.vp.reduce.fmax.float",
            Self::Minimum => "llvm.vp.reduce.fminimum.float",
            Self::Maximum => "llvm.vp.reduce.fmaximum.float",
        }
    }

    fn has_start_value(self) -> bool {
        matches!(self, Self::Add | Self::Mul)
    }

    fn source_operand_index(self) -> u32 {
        if self.has_start_value() { 1 } else { 0 }
    }

    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::FloatBin(match self {
            Self::Add => FloatBinOp::Add,
            Self::Mul => FloatBinOp::Mul,
            Self::Min => FloatBinOp::MinNum,
            Self::Max => FloatBinOp::MaxNum,
            Self::Minimum => FloatBinOp::Minimum,
            Self::Maximum => FloatBinOp::Maximum,
        })
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
    LoopDecrementReg,
    FShl,
    FShr,
}

#[derive(Debug, Clone, Copy)]
enum VectorReduceIntegerKind {
    Add,
    Mul,
    And,
    Or,
    Xor,
    SMax,
    SMin,
    UMax,
    UMin,
}

#[derive(Debug, Clone, Copy)]
enum VpIntegerBinopKind {
    Add,
    Sub,
    Mul,
    UDiv,
    SDiv,
    URem,
    SRem,
    SMax,
    SMin,
    UMax,
    UMin,
    UAddSat,
    USubSat,
    SAddSat,
    SSubSat,
    Xor,
    And,
    Or,
    Shl,
    LShr,
    AShr,
}

#[derive(Debug, Clone, Copy)]
enum VpIntegerTernaryKind {
    FShl,
    FShr,
}

#[derive(Debug, Clone, Copy)]
enum VpFloatBinopKind {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    MinNum,
    MaxNum,
    Minimum,
    Maximum,
    CopySign,
}

#[derive(Debug, Clone, Copy)]
enum VpFloatUnaryKind {
    Neg,
    Abs,
    Sqrt,
    Canonicalize,
    Floor,
    Ceil,
    RoundToZero,
    Rint,
    NearbyInt,
    Round,
    RoundEven,
    Sin,
    Cos,
    Exp,
    Exp2,
    Log,
    Log10,
    Log2,
}

#[derive(Debug, Clone, Copy)]
enum VpRoundToIntKind {
    LRint,
    LLRint,
}

#[derive(Debug, Clone, Copy)]
enum VpFloatTernaryKind {
    Fma,
    MulAdd,
}

#[derive(Debug, Clone, Copy)]
enum VpIntegerUnaryKind {
    CtPop,
    CtLz,
    CtTz,
    Abs,
    BSwap,
    BitReverse,
}

#[derive(Debug, Clone, Copy)]
enum VpIntegerCastKind {
    ZExt,
    SExt,
    Trunc,
}

#[derive(Debug, Clone, Copy)]
enum VpFloatCastKind {
    SIToFP,
    UIToFP,
    FPToSI,
    FPToUI,
    FPTrunc,
    FPExt,
}

#[derive(Debug, Clone, Copy)]
enum VpPointerCastKind {
    PtrToInt,
    IntToPtr,
}

impl VectorReduceIntegerKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Add => "llvm.vector.reduce.add.integer",
            Self::Mul => "llvm.vector.reduce.mul.integer",
            Self::And => "llvm.vector.reduce.and.integer",
            Self::Or => "llvm.vector.reduce.or.integer",
            Self::Xor => "llvm.vector.reduce.xor.integer",
            Self::SMax => "llvm.vector.reduce.smax.integer",
            Self::SMin => "llvm.vector.reduce.smin.integer",
            Self::UMax => "llvm.vector.reduce.umax.integer",
            Self::UMin => "llvm.vector.reduce.umin.integer",
        }
    }

    fn vp_lowering_rule(self) -> &'static str {
        match self {
            Self::Add => "llvm.vp.reduce.add.integer",
            Self::Mul => "llvm.vp.reduce.mul.integer",
            Self::And => "llvm.vp.reduce.and.integer",
            Self::Or => "llvm.vp.reduce.or.integer",
            Self::Xor => "llvm.vp.reduce.xor.integer",
            Self::SMax => "llvm.vp.reduce.smax.integer",
            Self::SMin => "llvm.vp.reduce.smin.integer",
            Self::UMax => "llvm.vp.reduce.umax.integer",
            Self::UMin => "llvm.vp.reduce.umin.integer",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::Bin(match self {
            Self::Add => BinOp::Add,
            Self::Mul => BinOp::Mul,
            Self::And => BinOp::And,
            Self::Or => BinOp::Or,
            Self::Xor => BinOp::Xor,
            Self::SMax => BinOp::SMax,
            Self::SMin => BinOp::SMin,
            Self::UMax => BinOp::UMax,
            Self::UMin => BinOp::UMin,
        })
    }
}

impl VpIntegerBinopKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Add => "llvm.vp.vector.add.integer",
            Self::Sub => "llvm.vp.vector.sub.integer",
            Self::Mul => "llvm.vp.vector.mul.integer",
            Self::UDiv => "llvm.vp.vector.udiv.integer",
            Self::SDiv => "llvm.vp.vector.sdiv.integer",
            Self::URem => "llvm.vp.vector.urem.integer",
            Self::SRem => "llvm.vp.vector.srem.integer",
            Self::SMax => "llvm.vp.vector.smax.integer",
            Self::SMin => "llvm.vp.vector.smin.integer",
            Self::UMax => "llvm.vp.vector.umax.integer",
            Self::UMin => "llvm.vp.vector.umin.integer",
            Self::UAddSat => "llvm.vp.vector.uadd.sat.integer",
            Self::USubSat => "llvm.vp.vector.usub.sat.integer",
            Self::SAddSat => "llvm.vp.vector.sadd.sat.integer",
            Self::SSubSat => "llvm.vp.vector.ssub.sat.integer",
            Self::Xor | Self::And | Self::Or => "llvm.vp.vector.bitops.integer",
            Self::Shl | Self::LShr | Self::AShr => "llvm.vp.vector.shift.integer",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::Bin(match self {
            Self::Add => BinOp::Add,
            Self::Sub => BinOp::Sub,
            Self::Mul => BinOp::Mul,
            Self::UDiv => BinOp::UDiv,
            Self::SDiv => BinOp::SDiv,
            Self::URem => BinOp::URem,
            Self::SRem => BinOp::SRem,
            Self::SMax => BinOp::SMax,
            Self::SMin => BinOp::SMin,
            Self::UMax => BinOp::UMax,
            Self::UMin => BinOp::UMin,
            Self::UAddSat => BinOp::UAddSat,
            Self::USubSat => BinOp::USubSat,
            Self::SAddSat => BinOp::SAddSat,
            Self::SSubSat => BinOp::SSubSat,
            Self::Xor => BinOp::Xor,
            Self::And => BinOp::And,
            Self::Or => BinOp::Or,
            Self::Shl => BinOp::Shl,
            Self::LShr => BinOp::LShr,
            Self::AShr => BinOp::AShr,
        })
    }
}

impl VpIntegerTernaryKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::FShl => "llvm.vp.vector.fshl.integer",
            Self::FShr => "llvm.vp.vector.fshr.integer",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::IntTernary(match self {
            Self::FShl => IntTernaryOp::FShl,
            Self::FShr => IntTernaryOp::FShr,
        })
    }
}

impl VpFloatBinopKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Add => "llvm.vp.vector.fadd.float",
            Self::Sub => "llvm.vp.vector.fsub.float",
            Self::Mul => "llvm.vp.vector.fmul.float",
            Self::Div => "llvm.vp.vector.fdiv.float",
            Self::Rem => "llvm.vp.vector.frem.float",
            Self::MinNum => "llvm.vp.vector.minnum.float",
            Self::MaxNum => "llvm.vp.vector.maxnum.float",
            Self::Minimum => "llvm.vp.vector.minimum.float",
            Self::Maximum => "llvm.vp.vector.maximum.float",
            Self::CopySign => "llvm.vp.vector.copysign.float",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::FloatBin(match self {
            Self::Add => FloatBinOp::Add,
            Self::Sub => FloatBinOp::Sub,
            Self::Mul => FloatBinOp::Mul,
            Self::Div => FloatBinOp::Div,
            Self::Rem => FloatBinOp::Rem,
            Self::MinNum => FloatBinOp::MinNum,
            Self::MaxNum => FloatBinOp::MaxNum,
            Self::Minimum => FloatBinOp::Minimum,
            Self::Maximum => FloatBinOp::Maximum,
            Self::CopySign => FloatBinOp::CopySign,
        })
    }
}

impl VpFloatUnaryKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Neg => "llvm.vp.vector.fneg.float",
            Self::Abs => "llvm.vp.vector.fabs.float",
            Self::Sqrt => "llvm.vp.vector.sqrt.float",
            Self::Canonicalize => "llvm.vp.vector.canonicalize.float",
            Self::Floor => "llvm.vp.vector.floor.float",
            Self::Ceil => "llvm.vp.vector.ceil.float",
            Self::RoundToZero => "llvm.vp.vector.roundtozero.float",
            Self::Rint => "llvm.vp.vector.rint.float",
            Self::NearbyInt => "llvm.vp.vector.nearbyint.float",
            Self::Round => "llvm.vp.vector.round.float",
            Self::RoundEven => "llvm.vp.vector.roundeven.float",
            Self::Sin => "llvm.vp.vector.sin.float",
            Self::Cos => "llvm.vp.vector.cos.float",
            Self::Exp => "llvm.vp.vector.exp.float",
            Self::Exp2 => "llvm.vp.vector.exp2.float",
            Self::Log => "llvm.vp.vector.log.float",
            Self::Log10 => "llvm.vp.vector.log10.float",
            Self::Log2 => "llvm.vp.vector.log2.float",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::FloatUnary(match self {
            Self::Neg => FloatUnaryOp::Neg,
            Self::Abs => FloatUnaryOp::Abs,
            Self::Sqrt => FloatUnaryOp::Sqrt,
            Self::Canonicalize => FloatUnaryOp::Canonicalize,
            Self::Floor => FloatUnaryOp::Floor,
            Self::Ceil => FloatUnaryOp::Ceil,
            Self::RoundToZero => FloatUnaryOp::Trunc,
            Self::Rint => FloatUnaryOp::Rint,
            Self::NearbyInt => FloatUnaryOp::NearbyInt,
            Self::Round => FloatUnaryOp::Round,
            Self::RoundEven => FloatUnaryOp::RoundEven,
            Self::Sin => FloatUnaryOp::Sin,
            Self::Cos => FloatUnaryOp::Cos,
            Self::Exp => FloatUnaryOp::Exp,
            Self::Exp2 => FloatUnaryOp::Exp2,
            Self::Log => FloatUnaryOp::Log,
            Self::Log10 => FloatUnaryOp::Log10,
            Self::Log2 => FloatUnaryOp::Log2,
        })
    }
}

impl VpRoundToIntKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::LRint => "llvm.vp.vector.lrint.float",
            Self::LLRint => "llvm.vp.vector.llrint.float",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::FloatRoundToInt(match self {
            Self::LRint => FloatRoundToIntOp::LRint,
            Self::LLRint => FloatRoundToIntOp::LLRint,
        })
    }
}

impl VpFloatTernaryKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Fma => "llvm.vp.vector.fma.float",
            Self::MulAdd => "llvm.vp.vector.fmuladd.float",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::FloatTernary(match self {
            Self::Fma => FloatTernaryOp::Fma,
            Self::MulAdd => FloatTernaryOp::MulAdd,
        })
    }
}

impl VpIntegerUnaryKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::CtPop => "llvm.vp.vector.ctpop.integer",
            Self::CtLz => "llvm.vp.vector.ctlz.integer",
            Self::CtTz => "llvm.vp.vector.cttz.integer",
            Self::Abs => "llvm.vp.vector.abs.integer",
            Self::BSwap => "llvm.vp.vector.bswap.integer",
            Self::BitReverse => "llvm.vp.vector.bitreverse.integer",
        }
    }

    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::IntUnary(match self {
            Self::CtPop => IntUnaryOp::CtPop,
            Self::CtLz => IntUnaryOp::CtLz,
            Self::CtTz => IntUnaryOp::CtTz,
            Self::Abs => IntUnaryOp::Abs,
            Self::BSwap => IntUnaryOp::BSwap,
            Self::BitReverse => IntUnaryOp::BitReverse,
        })
    }

    fn int_op(self) -> IntUnaryOp {
        match self {
            Self::CtPop => IntUnaryOp::CtPop,
            Self::CtLz => IntUnaryOp::CtLz,
            Self::CtTz => IntUnaryOp::CtTz,
            Self::Abs => IntUnaryOp::Abs,
            Self::BSwap => IntUnaryOp::BSwap,
            Self::BitReverse => IntUnaryOp::BitReverse,
        }
    }

    fn arity(self) -> u32 {
        if self.poison_flag_name().is_some() { 4 } else { 3 }
    }

    fn poison_flag_name(self) -> Option<&'static str> {
        match self {
            Self::CtPop | Self::BSwap | Self::BitReverse => None,
            Self::CtLz | Self::CtTz => Some("is_zero_undef"),
            Self::Abs => Some("is_int_min_poison"),
        }
    }

    fn mask_operand_index(self) -> u32 {
        if self.poison_flag_name().is_some() { 2 } else { 1 }
    }

    fn evl_operand_index(self) -> u32 {
        if self.poison_flag_name().is_some() { 3 } else { 2 }
    }
}

impl VpIntegerCastKind {
    fn semantic(self) -> HandlerSemantic {
        HandlerSemantic::Cast(match self {
            Self::ZExt => CastOp::ZExt,
            Self::SExt => CastOp::SExt,
            Self::Trunc => CastOp::Trunc,
        })
    }

    fn width_transition_is_valid(self, from: u8, to: u8) -> bool {
        match self {
            Self::ZExt | Self::SExt => from < to,
            Self::Trunc => from > to,
        }
    }
}

impl VpFloatCastKind {
    fn lowering_rule(self) -> &'static str {
        match self {
            Self::SIToFP => "llvm.vp.vector.sitofp.float",
            Self::UIToFP => "llvm.vp.vector.uitofp.float",
            Self::FPToSI => "llvm.vp.vector.fptosi.float",
            Self::FPToUI => "llvm.vp.vector.fptoui.float",
            Self::FPTrunc => "llvm.vp.vector.fptrunc.float",
            Self::FPExt => "llvm.vp.vector.fpext.float",
        }
    }

    fn op(self) -> FloatCastOp {
        match self {
            Self::SIToFP => FloatCastOp::SignedIntToFloat,
            Self::UIToFP => FloatCastOp::UnsignedIntToFloat,
            Self::FPToSI => FloatCastOp::FloatToSignedInt,
            Self::FPToUI => FloatCastOp::FloatToUnsignedInt,
            Self::FPTrunc => FloatCastOp::FloatTrunc,
            Self::FPExt => FloatCastOp::FloatExt,
        }
    }

    fn lane_kinds(self) -> (ScalarKind, ScalarKind) {
        match self {
            Self::SIToFP | Self::UIToFP => (ScalarKind::Integer, ScalarKind::Float),
            Self::FPToSI | Self::FPToUI => (ScalarKind::Float, ScalarKind::Integer),
            Self::FPTrunc | Self::FPExt => (ScalarKind::Float, ScalarKind::Float),
        }
    }
}

impl VpPointerCastKind {
    fn semantic_for_lane(
        self,
        index: usize,
        src_info: ReturnField,
        result_info: ReturnField,
    ) -> anyhow::Result<HandlerSemantic> {
        match self {
            Self::PtrToInt => {
                if src_info.kind != ScalarKind::Pointer || result_info.kind != ScalarKind::Integer {
                    bail!(
                        "llvm.vp.ptrtoint lane {index} requires pointer -> integer, got {}{} -> {}{}",
                        scalar_kind_prefix(src_info.kind),
                        src_info.width,
                        scalar_kind_prefix(result_info.kind),
                        result_info.width
                    );
                }
                if result_info.width < src_info.width {
                    Ok(HandlerSemantic::Cast(CastOp::Trunc))
                } else if result_info.width == src_info.width {
                    Ok(HandlerSemantic::Cast(CastOp::Bitcast))
                } else {
                    bail!(
                        "llvm.vp.ptrtoint lane {index} cannot widen pointer width {} to integer width {}",
                        src_info.width,
                        result_info.width
                    )
                }
            },
            Self::IntToPtr => {
                if src_info.kind != ScalarKind::Integer || result_info.kind != ScalarKind::Pointer {
                    bail!(
                        "llvm.vp.inttoptr lane {index} requires integer -> pointer, got {}{} -> {}{}",
                        scalar_kind_prefix(src_info.kind),
                        src_info.width,
                        scalar_kind_prefix(result_info.kind),
                        result_info.width
                    );
                }
                if src_info.width < result_info.width {
                    Ok(HandlerSemantic::Cast(CastOp::ZExt))
                } else if src_info.width == result_info.width {
                    Ok(HandlerSemantic::Cast(CastOp::Bitcast))
                } else {
                    bail!(
                        "llvm.vp.inttoptr lane {index} cannot narrow integer width {} to pointer width {}",
                        src_info.width,
                        result_info.width
                    )
                }
            },
        }
    }
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
            Self::LoopDecrementReg => "llvm.loop.decrement.reg.integer",
            Self::FShl => "llvm.fshl.integer",
            Self::FShr => "llvm.fshr.integer",
        }
    }

    fn vector_lowering_rule(self) -> Option<&'static str> {
        match self {
            Self::CtPop => Some("llvm.vector.ctpop.integer"),
            Self::CtLz => Some("llvm.vector.ctlz.integer"),
            Self::CtTz => Some("llvm.vector.cttz.integer"),
            Self::Abs => Some("llvm.vector.abs.integer"),
            Self::BSwap => Some("llvm.vector.bswap.integer"),
            Self::BitReverse => Some("llvm.vector.bitreverse.integer"),
            Self::SMax => Some("llvm.vector.smax.integer"),
            Self::SMin => Some("llvm.vector.smin.integer"),
            Self::UMax => Some("llvm.vector.umax.integer"),
            Self::UMin => Some("llvm.vector.umin.integer"),
            Self::UAddSat => Some("llvm.vector.uadd.sat.integer"),
            Self::USubSat => Some("llvm.vector.usub.sat.integer"),
            Self::SAddSat => Some("llvm.vector.sadd.sat.integer"),
            Self::SSubSat => Some("llvm.vector.ssub.sat.integer"),
            Self::UShlSat => Some("llvm.vector.ushl.sat.integer"),
            Self::SShlSat => Some("llvm.vector.sshl.sat.integer"),
            Self::FShl => Some("llvm.vector.fshl.integer"),
            Self::FShr => Some("llvm.vector.fshr.integer"),
            Self::UAddOverflow => Some("llvm.vector.uadd.with.overflow.integer"),
            Self::SAddOverflow => Some("llvm.vector.sadd.with.overflow.integer"),
            Self::USubOverflow => Some("llvm.vector.usub.with.overflow.integer"),
            Self::SSubOverflow => Some("llvm.vector.ssub.with.overflow.integer"),
            Self::UMulOverflow => Some("llvm.vector.umul.with.overflow.integer"),
            Self::SMulOverflow => Some("llvm.vector.smul.with.overflow.integer"),
            Self::LoopDecrementReg => None,
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
            Self::LoopDecrementReg => HandlerSemantic::Bin(BinOp::Sub),
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
            | Self::SMulOverflow
            | Self::LoopDecrementReg => 2,
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
                | Self::LoopDecrementReg
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
    pub return_is_aggregate: bool,
    // 每个原始宿主参数在扁平 VM 参数槽中的位置；direct aggregate 参数会占用多个槽。
    pub params: Vec<FunctionParamSlots>,
    // 直接 struct/array aggregate return 的叶子字段；空聚合返回时此列表为空但
    // `return_is_aggregate` 仍为 true。
    pub aggregate_return_fields: Vec<ReturnField>,
}

impl FunctionSignature {
    pub fn return_slot_count(&self) -> usize {
        if self.returns_void {
            0
        } else if self.return_is_aggregate {
            self.aggregate_return_fields.len()
        } else {
            1
        }
    }

    pub fn has_aggregate_return(&self) -> bool {
        self.return_is_aggregate
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
    pub return_is_aggregate: bool,
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
    if let Some(reason) = unsupported_function_attribute_reason(function) {
        bail!("{reason}");
    }

    let mut aggregate_return_fields = Vec::new();
    let (returns_void, return_width, return_kind, return_is_aggregate) = match fn_type.get_return_type() {
        None => (true, 64, ScalarKind::Integer, false),
        Some(BasicTypeEnum::IntType(return_type)) => (
            false,
            checked_width(return_type.get_bit_width())?,
            ScalarKind::Integer,
            false,
        ),
        Some(BasicTypeEnum::PointerType(_)) => (false, 64, ScalarKind::Pointer, false),
        Some(BasicTypeEnum::FloatType(return_type)) => (
            false,
            float_type_width(return_type.as_type_ref())?,
            ScalarKind::Float,
            false,
        ),
        Some(BasicTypeEnum::StructType(return_type)) => {
            aggregate_return_fields =
                return_fields_from_aggregate_type(BasicTypeEnum::StructType(return_type)).context("return fields")?;
            let return_width = aggregate_return_fields.first().map(|field| field.width).unwrap_or(64);
            (false, return_width, ScalarKind::Integer, true)
        },
        Some(BasicTypeEnum::ArrayType(return_type)) => {
            aggregate_return_fields =
                return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(return_type)).context("return fields")?;
            let return_width = aggregate_return_fields.first().map(|field| field.width).unwrap_or(64);
            (false, return_width, ScalarKind::Integer, true)
        },
        Some(BasicTypeEnum::VectorType(return_type)) => {
            aggregate_return_fields =
                vector_fields_from_type(BasicTypeEnum::VectorType(return_type)).context("return vector fields")?;
            let return_width = aggregate_return_fields.first().map(|field| field.width).unwrap_or(64);
            (false, return_width, ScalarKind::Integer, true)
        },
        Some(BasicTypeEnum::ScalableVectorType(_)) => {
            bail!("scalable vector returns are not supported by vm_virtualize function ABI")
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
                return_fields_from_aggregate_type(BasicTypeEnum::StructType(*struct_ty))
                    .with_context(|| format!("aggregate parameter {index} fields"))?
            },
            BasicMetadataTypeEnum::ArrayType(array_ty) => {
                return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(*array_ty))
                    .with_context(|| format!("aggregate parameter {index} fields"))?
            },
            BasicMetadataTypeEnum::VectorType(vector_ty) => {
                vector_fields_from_type(BasicTypeEnum::VectorType(*vector_ty))
                    .with_context(|| format!("vector parameter {index} fields"))?
            },
            BasicMetadataTypeEnum::ScalableVectorType(_) => {
                bail!("scalable vector parameters are not supported by vm_virtualize function ABI")
            },
            _ => {
                bail!(
                    "only scalar integer, pointer, half/float/double, and direct struct/array aggregate parameters are supported"
                )
            },
        };
        for field in &fields {
            param_widths.push(field.width);
        }
        params.push(FunctionParamSlots { start, fields });
    }

    if param_widths.len() > HOST_VM_MAX_ARGS {
        bail!(
            "only up to {HOST_VM_MAX_ARGS} flattened scalar integer/pointer/floating parameter slots are supported, got {}",
            param_widths.len(),
        );
    }

    Ok(FunctionSignature {
        return_width,
        param_widths,
        returns_void,
        return_is_pointer: return_kind == ScalarKind::Pointer,
        return_is_float: return_kind == ScalarKind::Float,
        return_is_aggregate,
        params,
        aggregate_return_fields,
    })
}

fn unsupported_function_attribute_reason(function: FunctionValue<'_>) -> Option<&'static str> {
    for (name, reason) in [
        ("naked", "naked functions are not supported by vm_virtualize"),
        (
            "returns_twice",
            "returns_twice functions are not supported by vm_virtualize",
        ),
        ("strictfp", "strictfp functions are not supported by vm_virtualize"),
        (
            "presplitcoroutine",
            "presplit coroutine functions are not supported by vm_virtualize",
        ),
        (
            "coro_only_destroy_when_complete",
            "coroutine lifetime function attributes are not supported by vm_virtualize",
        ),
        (
            "coro_elide_safe",
            "coroutine elide function attributes are not supported by vm_virtualize",
        ),
    ] {
        let kind = Attribute::get_named_enum_kind_id(name);
        if kind != 0 && function.get_enum_attribute(AttributeLoc::Function, kind).is_some() {
            return Some(reason);
        }
    }
    None
}

pub fn translate_function<'ctx>(
    module: &mut Module<'ctx>,
    function: FunctionValue<'ctx>,
    abi: &AbiProfile,
    lowering: &LoweringProfile,
    isa: &IsaProfile,
    emit_markers: bool,
) -> anyhow::Result<VmTranslation<'ctx>> {
    let signature = supported_signature(function)?;
    let name = function.get_name().to_str().unwrap_or("<invalid-name>").to_owned();

    let lowerer = FunctionLowerer::new(module, function, &name, &signature, abi, lowering, isa, emit_markers)?;
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
    non_integral_address_spaces: HashSet<u32>,
    builder: VmFunctionBuilder,
    // LLVM SSA value 到 VM x 寄存器的当前绑定。
    values: HashMap<ValueKey, ValueBinding>,
    // insertvalue/extractvalue 和多返回 native call 使用的临时 aggregate 绑定。
    aggregates: HashMap<ValueKey, AggregateBinding>,
    dynamic_allocas: HashMap<ValueKey, DynamicAllocaObject>,
    dynamic_alloca_geps: HashMap<ValueKey, DynamicAllocaGepObject>,
    dynamic_static_geps: HashMap<ValueKey, DynamicStaticGepObject>,
    // aggregate binding 可能在 insertvalue 链中共享字段寄存器；引用计数归零后才能释放。
    aggregate_reg_refs: HashMap<u8, usize>,
    // LLVM basic block 到 VM bytecode label 的映射。
    labels: HashMap<BlockKey, LabelId>,
    native_calls: Vec<NativeCallTarget<'ctx>>,
    return_registers: Vec<u8>,
    native_arg_registers: Vec<u8>,
    native_return_registers: Vec<u8>,
    native_touched_registers: HashSet<u8>,
    emit_markers: bool,
    aggregate_return: bool,
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
struct DynamicAllocaObject {
    count: ValueBinding,
    elem_size: u64,
}

#[derive(Debug, Clone, Copy)]
struct DynamicAllocaGepObject {
    object: DynamicAllocaObject,
    offset: ValueBinding,
}

#[derive(Debug, Clone, Copy)]
struct StaticObjectBase {
    total_size: u64,
    base_offset: u64,
}

#[derive(Debug, Clone, Copy)]
struct DynamicStaticGepObject {
    total_size: u64,
    offset: ValueBinding,
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
enum CompositePhiKind {
    Aggregate,
    Vector,
}

impl CompositePhiKind {
    fn name(self) -> &'static str {
        match self {
            Self::Aggregate => "aggregate",
            Self::Vector => "vector",
        }
    }

    fn lowering_rule(self) -> &'static str {
        match self {
            Self::Aggregate => "llvm.aggregate.phi.edge_move",
            Self::Vector => "llvm.vector.phi.edge_move",
        }
    }
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

#[derive(Debug, Clone)]
struct VpMergeLoweringActions {
    lane_mov: LoweringAction,
    pivot_icmp: LoweringAction,
    cond_br_if: LoweringAction,
    pivot_br_if: LoweringAction,
    then_mov: LoweringAction,
    br: LoweringAction,
    else_mov: LoweringAction,
}

#[derive(Debug, Clone)]
struct DynamicLaneActions {
    const_mov: LoweringAction,
    icmp: LoweringAction,
    br_if: LoweringAction,
    br: LoweringAction,
}

#[derive(Debug, Clone)]
struct ActiveLaneMaskActions {
    lane_mov: LoweringAction,
    add: LoweringAction,
    icmp: LoweringAction,
}

#[derive(Debug, Clone)]
struct GetVectorLengthActions {
    avl_zext: LoweringAction,
    vector_factor_mov: LoweringAction,
    icmp: LoweringAction,
    avl_trunc: LoweringAction,
    vector_factor_trunc: LoweringAction,
    br_if: LoweringAction,
    then_mov: LoweringAction,
    br: LoweringAction,
    else_mov: LoweringAction,
}

#[derive(Debug, Clone)]
struct CountTrailingZeroElementsActions {
    default_mov: LoweringAction,
    lane_mov: LoweringAction,
    br_if: LoweringAction,
    case_mov: LoweringAction,
    br: LoweringAction,
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
        emit_markers: bool,
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
        let layout = module_data_layout(module);
        let non_integral_address_spaces = non_integral_address_spaces_from_layout(&layout);
        let target_data = TargetData::create(&layout);

        let mut values = HashMap::new();
        let mut aggregates = HashMap::new();
        let function_params = function.get_params();
        for (index, value) in function_params.iter().enumerate() {
            ensure_no_non_integral_pointer_type_ref(
                &non_integral_address_spaces,
                value.get_type().as_type_ref(),
                &format!("function parameter {index}"),
            )?;
        }
        if let Some(return_type) = function.get_type().get_return_type() {
            ensure_no_non_integral_pointer_type_ref(
                &non_integral_address_spaces,
                return_type.as_type_ref(),
                "function return type",
            )?;
        }
        if function_params.len() != signature.params.len() {
            bail!(
                "function parameter count mismatch: LLVM has {}, signature has {}",
                function_params.len(),
                signature.params.len()
            );
        }
        for (index, (value, slots)) in function_params.into_iter().zip(signature.params.iter()).enumerate() {
            match value.get_type() {
                BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_) | BasicTypeEnum::VectorType(_) => {
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
            non_integral_address_spaces,
            builder,
            values,
            aggregates,
            dynamic_allocas: HashMap::new(),
            dynamic_alloca_geps: HashMap::new(),
            dynamic_static_geps: HashMap::new(),
            aggregate_reg_refs: HashMap::new(),
            labels: HashMap::new(),
            native_calls: Vec::new(),
            return_registers,
            native_arg_registers,
            native_return_registers,
            native_touched_registers,
            emit_markers,
            aggregate_return: signature.return_is_aggregate,
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

        let field_infos = instruction_composite_fields(instruction).context("composite result fields")?;
        if field_infos.is_empty() {
            let binding = AggregateBinding { fields: Vec::new() };
            self.insert_aggregate_value(key, binding.clone());
            return Ok(binding);
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
                InstructionOpcode::InsertElement => self.lower_insert_element(instruction)?,
                InstructionOpcode::ExtractElement => self.lower_extract_element(instruction)?,
                InstructionOpcode::ShuffleVector => self.lower_shuffle_vector(instruction)?,
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

    fn vp_merge_lowering_actions(&self) -> anyhow::Result<VpMergeLoweringActions> {
        let rule = "llvm.vp.merge.vector_condition";
        Ok(VpMergeLoweringActions {
            lane_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::MovImm,
                &[("dst", "%vk"), ("imm", "lane(%r)"), ("width", "type_width(%pivot)")],
            )?,
            pivot_icmp: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Icmp,
                &[
                    ("pred", "ult"),
                    ("dst", "%vm"),
                    ("lhs", "%vk"),
                    ("rhs", "%vp"),
                    ("width", "type_width(%pivot)"),
                ],
            )?,
            cond_br_if: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::BrCond,
                &[("cond", "%vc"), ("then_pc", "pivot_label"), ("else_pc", "else_label")],
            )?,
            pivot_br_if: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::BrCond,
                &[("cond", "%vm"), ("then_pc", "then_label"), ("else_pc", "else_label")],
            )?,
            then_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Mov,
                &[("dst", "%vr"), ("src", "%vt"), ("width", "type_width(%field)")],
            )?,
            br: self.emit_action_for_shape(rule, &HandlerSemantic::Br, &[("target", "join_label")])?,
            else_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Mov,
                &[("dst", "%vr"), ("src", "%ve"), ("width", "type_width(%field)")],
            )?,
        })
    }

    fn dynamic_lane_actions(&self, rule: &str) -> anyhow::Result<DynamicLaneActions> {
        Ok(DynamicLaneActions {
            const_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::MovImm,
                &[("dst", "%vk"), ("imm", "lane(%r)"), ("width", "type_width(%index)")],
            )?,
            icmp: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Icmp,
                &[
                    ("pred", "eq"),
                    ("dst", "%vm"),
                    ("lhs", "%vi"),
                    ("rhs", "%vk"),
                    ("width", "type_width(%index)"),
                ],
            )?,
            br_if: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::BrCond,
                &[
                    ("cond", "%vm"),
                    ("then_pc", "case_label"),
                    ("else_pc", "next_case_label"),
                ],
            )?,
            br: self.emit_action_for_shape(rule, &HandlerSemantic::Br, &[("target", "join_label")])?,
        })
    }

    fn active_lane_mask_actions(&self) -> anyhow::Result<ActiveLaneMaskActions> {
        let rule = "llvm.vector.get.active.lane.mask";
        Ok(ActiveLaneMaskActions {
            lane_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::MovImm,
                &[("dst", "%vl"), ("imm", "lane(%r)"), ("width", "operand_width(%a,%b)")],
            )?,
            add: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Bin(BinOp::Add),
                &[
                    ("dst", "%vi"),
                    ("lhs", "%vs"),
                    ("rhs", "%vl"),
                    ("width", "operand_width(%a,%b)"),
                ],
            )?,
            icmp: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Icmp,
                &[
                    ("pred", "ult"),
                    ("dst", "%vr"),
                    ("lhs", "%vi"),
                    ("rhs", "%ve"),
                    ("width", "operand_width(%a,%b)"),
                ],
            )?,
        })
    }

    fn get_vector_length_actions(&self) -> anyhow::Result<GetVectorLengthActions> {
        let rule = "llvm.experimental.get.vector.length.integer";
        Ok(GetVectorLengthActions {
            avl_zext: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Cast(CastOp::ZExt),
                &[
                    ("dst", "%vwide"),
                    ("src", "%va"),
                    ("from_width", "type_width(%avl)"),
                    ("to_width", "compare_width(%avl)"),
                ],
            )?,
            vector_factor_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::MovImm,
                &[
                    ("dst", "%vv"),
                    ("imm", "vector_factor(%r)"),
                    ("width", "compare_width(%avl)"),
                ],
            )?,
            icmp: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Icmp,
                &[
                    ("pred", "ult"),
                    ("dst", "%vc"),
                    ("lhs", "%vwide"),
                    ("rhs", "%vv"),
                    ("width", "compare_width(%avl)"),
                ],
            )?,
            avl_trunc: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Cast(CastOp::Trunc),
                &[
                    ("dst", "%vt"),
                    ("src", "%vwide"),
                    ("from_width", "compare_width(%avl)"),
                    ("to_width", "type_width(%r)"),
                ],
            )?,
            vector_factor_trunc: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Cast(CastOp::Trunc),
                &[
                    ("dst", "%ve"),
                    ("src", "%vv"),
                    ("from_width", "compare_width(%avl)"),
                    ("to_width", "type_width(%r)"),
                ],
            )?,
            br_if: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::BrCond,
                &[("cond", "%vc"), ("then_pc", "then_label"), ("else_pc", "else_label")],
            )?,
            then_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Mov,
                &[("dst", "%vr"), ("src", "%vt"), ("width", "type_width(%r)")],
            )?,
            br: self.emit_action_for_shape(rule, &HandlerSemantic::Br, &[("target", "join_label")])?,
            else_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Mov,
                &[("dst", "%vr"), ("src", "%ve"), ("width", "type_width(%r)")],
            )?,
        })
    }

    fn count_trailing_zero_elements_actions(&self) -> anyhow::Result<CountTrailingZeroElementsActions> {
        self.count_trailing_zero_elements_actions_for_rule("llvm.experimental.cttz.elts", "lane_count(%mask)")
    }

    fn vp_count_trailing_zero_elements_actions(&self) -> anyhow::Result<CountTrailingZeroElementsActions> {
        self.count_trailing_zero_elements_actions_for_rule("llvm.vp.cttz.elts", "active_lane_count(%mask,%evl)")
    }

    fn count_trailing_zero_elements_actions_for_rule(
        &self,
        rule: &str,
        default_imm: &str,
    ) -> anyhow::Result<CountTrailingZeroElementsActions> {
        Ok(CountTrailingZeroElementsActions {
            default_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::MovImm,
                &[("dst", "%vr"), ("imm", default_imm), ("width", "type_width(%r)")],
            )?,
            lane_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::MovImm,
                &[("dst", "%vk"), ("imm", "lane(%r)"), ("width", "type_width(%r)")],
            )?,
            br_if: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::BrCond,
                &[("cond", "%vm"), ("then_pc", "case_label"), ("else_pc", "join_label")],
            )?,
            case_mov: self.emit_action_for_shape(
                rule,
                &HandlerSemantic::Mov,
                &[("dst", "%vr"), ("src", "%vk"), ("width", "type_width(%r)")],
            )?,
            br: self.emit_action_for_shape(rule, &HandlerSemantic::Br, &[("target", "join_label")])?,
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
            HandlerSemantic::ReadVScale => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::ReadVScale { dst, width }, desc.name.clone());
            },
            HandlerSemantic::ReadRounding => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::ReadRounding { dst, width }, desc.name.clone());
            },
            HandlerSemantic::ReadFltRounds => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::ReadFltRounds { dst, width }, desc.name.clone());
            },
            HandlerSemantic::WriteRounding => {
                let src = self.profile_reg(desc, &args, "src")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::WriteRounding { src, width }, desc.name.clone());
            },
            HandlerSemantic::ReadFpState(kind) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let width = checked_fp_state_width(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::ReadFpState {
                        kind: *kind,
                        dst,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::WriteFpState(kind) => {
                let src = self.profile_reg(desc, &args, "src")?;
                let width = checked_fp_state_width(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::WriteFpState {
                        kind: *kind,
                        src,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::ResetFpState(kind) => {
                self.builder
                    .push_profile(VmInstruction::ResetFpState { kind: *kind }, desc.name.clone());
            },
            HandlerSemantic::ReadThreadPointer => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::ReadThreadPointer { dst, width }, desc.name.clone());
            },
            HandlerSemantic::StackSave => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                self.builder
                    .push_profile(VmInstruction::StackSave { dst }, desc.name.clone());
            },
            HandlerSemantic::StackRestore => {
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                self.builder
                    .push_profile(VmInstruction::StackRestore { ptr }, desc.name.clone());
            },
            HandlerSemantic::ClearCache => {
                let start = self.profile_reg(desc, &args, "start")?;
                let end = self.profile_reg(desc, &args, "end")?;
                self.builder
                    .push_profile(VmInstruction::ClearCache { start, end }, desc.name.clone());
            },
            HandlerSemantic::PseudoProbe => {
                let guid = args.imm("guid")?;
                let index = args.imm("index")?;
                let probe_type =
                    u32::try_from(args.imm("probe_type")?).context("pseudoprobe probe_type must fit in i32")?;
                let attributes = args.imm("attributes")?;
                self.builder.push_profile(
                    VmInstruction::PseudoProbe {
                        guid,
                        index,
                        probe_type,
                        attributes,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Prefetch => {
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let rw = checked_prefetch_rw(args.imm("rw")?)?;
                let locality = checked_prefetch_locality(args.imm("locality")?)?;
                let cache = checked_prefetch_cache(args.imm("cache")?)?;
                self.builder.push_profile(
                    VmInstruction::Prefetch {
                        ptr,
                        rw,
                        locality,
                        cache,
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
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadMul) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let factor = self.profile_reg(desc, &args, "factor")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadMul {
                        dst,
                        ptr,
                        factor,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadUDiv) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let divisor = self.profile_reg(desc, &args, "divisor")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadUDiv {
                        dst,
                        ptr,
                        divisor,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadSDiv) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let divisor = self.profile_reg(desc, &args, "divisor")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadSDiv {
                        dst,
                        ptr,
                        divisor,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadURem) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let divisor = self.profile_reg(desc, &args, "divisor")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadURem {
                        dst,
                        ptr,
                        divisor,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadSRem) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let divisor = self.profile_reg(desc, &args, "divisor")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadSRem {
                        dst,
                        ptr,
                        divisor,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadShl) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let shift = self.profile_reg(desc, &args, "shift")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadShl { dst, ptr, shift, width },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadLShr) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let shift = self.profile_reg(desc, &args, "shift")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadLShr { dst, ptr, shift, width },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadAShr) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let shift = self.profile_reg(desc, &args, "shift")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadAShr { dst, ptr, shift, width },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadSMax) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::SuperLoadSMax { dst, ptr, rhs, width }, desc.name.clone());
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadSMin) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::SuperLoadSMin { dst, ptr, rhs, width }, desc.name.clone());
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadUMax) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::SuperLoadUMax { dst, ptr, rhs, width }, desc.name.clone());
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadUMin) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::SuperLoadUMin { dst, ptr, rhs, width }, desc.name.clone());
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadUAddSat) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadUAddSat { dst, ptr, rhs, width },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadUSubSat) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadUSubSat { dst, ptr, rhs, width },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadSAddSat) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadSAddSat { dst, ptr, rhs, width },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadSSubSat) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadSSubSat { dst, ptr, rhs, width },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadUShlSat) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadUShlSat { dst, ptr, rhs, width },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadSShlSat) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadSShlSat { dst, ptr, rhs, width },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadAnd) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let and_rhs = self.profile_reg(desc, &args, "and_rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadAnd {
                        dst,
                        ptr,
                        and_rhs,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadOr) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let or_rhs = self.profile_reg(desc, &args, "or_rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadOr {
                        dst,
                        ptr,
                        or_rhs,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadSub) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let subtrahend = self.profile_reg(desc, &args, "subtrahend")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadSub {
                        dst,
                        ptr,
                        subtrahend,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Super(amice_vm::isa::SuperOp::LoadXor) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let xor_rhs = self.profile_reg(desc, &args, "xor_rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::SuperLoadXor {
                        dst,
                        ptr,
                        xor_rhs,
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
                let width = checked_int_unary_width(*op, args.imm("width")?)?;
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
            HandlerSemantic::FloatIntBin(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_float_width(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::FloatIntBin {
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
            HandlerSemantic::FloatRoundToInt(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let from_width = checked_float_width(args.imm("from_width")?)?;
                let to_width = checked_round_to_int_result_width(args.imm("to_width")?)?;
                self.builder.push_profile(
                    VmInstruction::FloatRoundToInt {
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
                let sync_scope = checked_supported_atomic_sync_scope(args.imm("sync_scope")?, "atomic load")?;
                self.builder.push_profile(
                    VmInstruction::AtomicLoad {
                        dst,
                        ptr,
                        width,
                        ordering,
                        sync_scope,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::AtomicStore => {
                let src = self.profile_reg(desc, &args, "src")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_atomic_memory_width(args.imm("width")?)?;
                let ordering = memory_ordering_from_u64(args.imm("ordering")?)?;
                let sync_scope = checked_supported_atomic_sync_scope(args.imm("sync_scope")?, "atomic store")?;
                self.builder.push_profile(
                    VmInstruction::AtomicStore {
                        src,
                        ptr,
                        width,
                        ordering,
                        sync_scope,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::VolatileAtomicLoad => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_atomic_memory_width(args.imm("width")?)?;
                let ordering = memory_ordering_from_u64(args.imm("ordering")?)?;
                let sync_scope = checked_supported_atomic_sync_scope(args.imm("sync_scope")?, "volatile atomic load")?;
                self.builder.push_profile(
                    VmInstruction::VolatileAtomicLoad {
                        dst,
                        ptr,
                        width,
                        ordering,
                        sync_scope,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::VolatileAtomicStore => {
                let src = self.profile_reg(desc, &args, "src")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_atomic_memory_width(args.imm("width")?)?;
                let ordering = memory_ordering_from_u64(args.imm("ordering")?)?;
                let sync_scope = checked_supported_atomic_sync_scope(args.imm("sync_scope")?, "volatile atomic store")?;
                self.builder.push_profile(
                    VmInstruction::VolatileAtomicStore {
                        src,
                        ptr,
                        width,
                        ordering,
                        sync_scope,
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
                let sync_scope = checked_supported_atomic_sync_scope(args.imm("sync_scope")?, "atomicrmw")?;
                self.builder.push_profile(
                    VmInstruction::AtomicRmw {
                        op: *op,
                        dst,
                        ptr,
                        src,
                        width,
                        ordering,
                        sync_scope,
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
                let sync_scope = checked_supported_atomic_sync_scope(args.imm("sync_scope")?, "volatile atomicrmw")?;
                self.builder.push_profile(
                    VmInstruction::VolatileAtomicRmw {
                        op: *op,
                        dst,
                        ptr,
                        src,
                        width,
                        ordering,
                        sync_scope,
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
                let sync_scope = checked_supported_atomic_sync_scope(args.imm("sync_scope")?, "cmpxchg")?;
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
                        sync_scope,
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
                let sync_scope = checked_supported_atomic_sync_scope(args.imm("sync_scope")?, "volatile cmpxchg")?;
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
                        sync_scope,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Fence => {
                let ordering = memory_ordering_from_u64(args.imm("ordering")?)?;
                let sync_scope = checked_supported_atomic_sync_scope(args.imm("sync_scope")?, "fence")?;
                self.builder
                    .push_profile(VmInstruction::Fence { ordering, sync_scope }, desc.name.clone());
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

    fn retain_dynamic_alloca_count(&mut self, count: ValueBinding) -> anyhow::Result<ValueBinding> {
        let stable = ValueBinding {
            reg: self.builder.alloc_vreg()?,
            width: count.width,
        };
        self.emit_profile_mov_direct(stable.reg, count.reg, count.width)?;
        Ok(stable)
    }

    fn retain_dynamic_gep_constant_offset(
        &mut self,
        instruction: InstructionValue<'ctx>,
        object: DynamicAllocaObject,
        byte_offset: u64,
    ) -> anyhow::Result<()> {
        let offset = ValueBinding {
            reg: self.builder.alloc_vreg()?,
            width: 64,
        };
        self.push_constant(offset.reg, byte_offset, 64)?;
        self.dynamic_alloca_geps
            .insert(instruction_key(instruction), DynamicAllocaGepObject { object, offset });
        Ok(())
    }

    fn retain_dynamic_gep_offset(
        &mut self,
        instruction: InstructionValue<'ctx>,
        object: DynamicAllocaObject,
        ptr: ValueBinding,
        base: ValueBinding,
    ) -> anyhow::Result<()> {
        let offset = ValueBinding {
            reg: self.builder.alloc_vreg()?,
            width: 64,
        };
        let action = self.emit_action_for_shape(
            "llvm.objectsize.dynamic_gep_offset",
            &HandlerSemantic::Bin(BinOp::Sub),
            &[("dst", "%vo"), ("lhs", "%vp"), ("rhs", "%vb"), ("width", "64")],
        )?;
        let env = LoweringEnv::new()
            .binding("%vp", ptr)
            .binding("%vb", base)
            .binding("%vo", offset)
            .imm("64", 64);
        self.emit_profile_action(&action, &env)?;
        self.dynamic_alloca_geps
            .insert(instruction_key(instruction), DynamicAllocaGepObject { object, offset });
        Ok(())
    }

    fn retain_chained_dynamic_gep_offset(
        &mut self,
        instruction: InstructionValue<'ctx>,
        object: DynamicAllocaGepObject,
        ptr: ValueBinding,
        base: ValueBinding,
    ) -> anyhow::Result<()> {
        let delta = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 64,
        };
        let delta_action = self.emit_action_for_shape(
            "llvm.objectsize.dynamic_gep_offset",
            &HandlerSemantic::Bin(BinOp::Sub),
            &[("dst", "%vo"), ("lhs", "%vp"), ("rhs", "%vb"), ("width", "64")],
        )?;
        let delta_env = LoweringEnv::new()
            .binding("%vp", ptr)
            .binding("%vb", base)
            .binding("%vo", delta)
            .imm("64", 64);
        self.emit_profile_action(&delta_action, &delta_env)?;

        let offset = ValueBinding {
            reg: self.builder.alloc_vreg()?,
            width: 64,
        };
        let accumulate_action = self.emit_action_for_shape(
            "llvm.objectsize.dynamic_gep_accumulate",
            &HandlerSemantic::Bin(BinOp::Add),
            &[("dst", "%vo"), ("lhs", "%vp"), ("rhs", "%vd"), ("width", "64")],
        )?;
        let accumulate_env = LoweringEnv::new()
            .binding("%vp", object.offset)
            .binding("%vd", delta)
            .binding("%vo", offset)
            .imm("64", 64);
        self.emit_profile_action(&accumulate_action, &accumulate_env)?;

        self.dynamic_alloca_geps.insert(
            instruction_key(instruction),
            DynamicAllocaGepObject {
                object: object.object,
                offset,
            },
        );
        Ok(())
    }

    fn retain_static_gep_constant_offset(
        &mut self,
        instruction: InstructionValue<'ctx>,
        object: StaticObjectBase,
        byte_offset: u64,
    ) -> anyhow::Result<()> {
        let Some(total_offset) = object.base_offset.checked_add(byte_offset) else {
            return Ok(());
        };
        if total_offset > object.total_size {
            return Ok(());
        }
        let offset = ValueBinding {
            reg: self.builder.alloc_vreg()?,
            width: 64,
        };
        self.push_constant(offset.reg, total_offset, 64)?;
        self.dynamic_static_geps.insert(
            instruction_key(instruction),
            DynamicStaticGepObject {
                total_size: object.total_size,
                offset,
            },
        );
        Ok(())
    }

    fn retain_static_gep_offset(
        &mut self,
        instruction: InstructionValue<'ctx>,
        object: StaticObjectBase,
        ptr: ValueBinding,
        base: ValueBinding,
    ) -> anyhow::Result<()> {
        if object.base_offset > object.total_size {
            return Ok(());
        }

        let delta = ValueBinding {
            reg: if object.base_offset == 0 {
                self.builder.alloc_vreg()?
            } else {
                self.alloc_temporary_vreg()?
            },
            width: 64,
        };
        let delta_action = self.emit_action_for_shape(
            "llvm.objectsize.dynamic_gep_offset",
            &HandlerSemantic::Bin(BinOp::Sub),
            &[("dst", "%vo"), ("lhs", "%vp"), ("rhs", "%vb"), ("width", "64")],
        )?;
        let delta_env = LoweringEnv::new()
            .binding("%vp", ptr)
            .binding("%vb", base)
            .binding("%vo", delta)
            .imm("64", 64);
        self.emit_profile_action(&delta_action, &delta_env)?;

        let offset = if object.base_offset == 0 {
            delta
        } else {
            let base_offset = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: 64,
            };
            self.push_constant(base_offset.reg, object.base_offset, 64)?;
            let offset = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: 64,
            };
            self.emit_static_gep_offset_accumulate(base_offset, delta, offset)?;
            offset
        };

        self.dynamic_static_geps.insert(
            instruction_key(instruction),
            DynamicStaticGepObject {
                total_size: object.total_size,
                offset,
            },
        );
        Ok(())
    }

    fn retain_chained_static_gep_offset(
        &mut self,
        instruction: InstructionValue<'ctx>,
        object: DynamicStaticGepObject,
        ptr: ValueBinding,
        base: ValueBinding,
    ) -> anyhow::Result<()> {
        let delta = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 64,
        };
        let delta_action = self.emit_action_for_shape(
            "llvm.objectsize.dynamic_gep_offset",
            &HandlerSemantic::Bin(BinOp::Sub),
            &[("dst", "%vo"), ("lhs", "%vp"), ("rhs", "%vb"), ("width", "64")],
        )?;
        let delta_env = LoweringEnv::new()
            .binding("%vp", ptr)
            .binding("%vb", base)
            .binding("%vo", delta)
            .imm("64", 64);
        self.emit_profile_action(&delta_action, &delta_env)?;

        let offset = ValueBinding {
            reg: self.builder.alloc_vreg()?,
            width: 64,
        };
        self.emit_static_gep_offset_accumulate(object.offset, delta, offset)?;
        self.dynamic_static_geps.insert(
            instruction_key(instruction),
            DynamicStaticGepObject {
                total_size: object.total_size,
                offset,
            },
        );
        Ok(())
    }

    fn emit_static_gep_offset_accumulate(
        &mut self,
        previous: ValueBinding,
        delta: ValueBinding,
        offset: ValueBinding,
    ) -> anyhow::Result<()> {
        let accumulate_action = self.emit_action_for_shape(
            "llvm.objectsize.dynamic_gep_accumulate",
            &HandlerSemantic::Bin(BinOp::Add),
            &[("dst", "%vo"), ("lhs", "%vp"), ("rhs", "%vd"), ("width", "64")],
        )?;
        let accumulate_env = LoweringEnv::new()
            .binding("%vp", previous)
            .binding("%vd", delta)
            .binding("%vo", offset)
            .imm("64", 64);
        self.emit_profile_action(&accumulate_action, &accumulate_env)
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
        let (rule, vector_rule, selected) = match instruction.get_opcode() {
            InstructionOpcode::Add => ("llvm.add.integer", "llvm.vector.add.integer", None),
            InstructionOpcode::Sub => ("llvm.sub.integer", "llvm.vector.sub.integer", None),
            InstructionOpcode::Mul => ("llvm.mul.integer", "llvm.vector.mul.integer", None),
            InstructionOpcode::UDiv => (
                "llvm.udiv.integer",
                "llvm.vector.udiv.integer",
                Some(HandlerSemantic::Bin(BinOp::UDiv)),
            ),
            InstructionOpcode::SDiv => (
                "llvm.sdiv.integer",
                "llvm.vector.sdiv.integer",
                Some(HandlerSemantic::Bin(BinOp::SDiv)),
            ),
            InstructionOpcode::URem => (
                "llvm.urem.integer",
                "llvm.vector.urem.integer",
                Some(HandlerSemantic::Bin(BinOp::URem)),
            ),
            InstructionOpcode::SRem => (
                "llvm.srem.integer",
                "llvm.vector.srem.integer",
                Some(HandlerSemantic::Bin(BinOp::SRem)),
            ),
            InstructionOpcode::Xor => (
                "llvm.bitops.integer",
                "llvm.vector.bitops.integer",
                Some(HandlerSemantic::Bin(BinOp::Xor)),
            ),
            InstructionOpcode::And => (
                "llvm.bitops.integer",
                "llvm.vector.bitops.integer",
                Some(HandlerSemantic::Bin(BinOp::And)),
            ),
            InstructionOpcode::Or => (
                "llvm.bitops.integer",
                "llvm.vector.bitops.integer",
                Some(HandlerSemantic::Bin(BinOp::Or)),
            ),
            InstructionOpcode::Shl => (
                "llvm.shift.integer",
                "llvm.vector.shift.integer",
                Some(HandlerSemantic::Bin(BinOp::Shl)),
            ),
            InstructionOpcode::LShr => (
                "llvm.shift.integer",
                "llvm.vector.shift.integer",
                Some(HandlerSemantic::Bin(BinOp::LShr)),
            ),
            InstructionOpcode::AShr => (
                "llvm.shift.integer",
                "llvm.vector.shift.integer",
                Some(HandlerSemantic::Bin(BinOp::AShr)),
            ),
            opcode => bail!("unsupported binop opcode: {opcode:?}"),
        };
        if matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)) {
            return self.lower_vector_integer_binop(instruction, vector_rule, selected);
        }
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

    fn lower_vector_integer_binop(
        &mut self,
        instruction: InstructionValue<'ctx>,
        rule: &str,
        selected: Option<HandlerSemantic>,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector integer binop result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vector integer binop fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("vector integer binop lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("vector integer binop rhs fields")?;
        let lhs = self.vector_operand(instruction, 0)?;
        let rhs = self.vector_operand(instruction, 1)?;
        if lhs.fields.len() != result_fields.len()
            || rhs.fields.len() != result_fields.len()
            || lhs_fields.len() != result_fields.len()
            || rhs_fields.len() != result_fields.len()
        {
            bail!(
                "vector integer binop lane count mismatch: result {}, lhs {}/{}, rhs {}/{}",
                result_fields.len(),
                lhs.fields.len(),
                lhs_fields.len(),
                rhs.fields.len(),
                rhs_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            if result_info.kind != ScalarKind::Integer
                || lhs_info.kind != ScalarKind::Integer
                || rhs_info.kind != ScalarKind::Integer
            {
                bail!(
                    "vector integer binop lane {index} requires integer lanes, got result {:?}, lhs {:?}, rhs {:?}",
                    result_info.kind,
                    lhs_info.kind,
                    rhs_info.kind
                );
            }
            if lhs_info.width != result_info.width || rhs_info.width != result_info.width {
                bail!(
                    "vector integer binop lane {index} width mismatch: result i{}, lhs i{}, rhs i{}",
                    result_info.width,
                    lhs_info.width,
                    rhs_info.width
                );
            }
            checked_intrinsic_integer_width(result_info.width as u64)?;
            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector integer binop lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector integer binop rhs lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != result_info.width || rhs_binding.width != result_info.width {
                bail!(
                    "vector integer binop lane {index} binding width mismatch: result i{}, lhs i{}, rhs i{}",
                    result_info.width,
                    lhs_binding.width,
                    rhs_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, selected.clone())?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector integer binop lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vector_integer_ternary(
        &mut self,
        instruction: InstructionValue<'ctx>,
        rule: &str,
        selected: Option<HandlerSemantic>,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector integer ternary result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vector integer ternary fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("vector integer ternary lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("vector integer ternary rhs fields")?;
        let third_fields = vector_fields_from_type(instruction_operand_value(instruction, 2)?.get_type())
            .context("vector integer ternary third fields")?;
        let lhs = self.vector_operand(instruction, 0)?;
        let rhs = self.vector_operand(instruction, 1)?;
        let third = self.vector_operand(instruction, 2)?;
        if lhs.fields.len() != result_fields.len()
            || rhs.fields.len() != result_fields.len()
            || third.fields.len() != result_fields.len()
            || lhs_fields.len() != result_fields.len()
            || rhs_fields.len() != result_fields.len()
            || third_fields.len() != result_fields.len()
        {
            bail!(
                "vector integer ternary lane count mismatch: result {}, lhs {}/{}, rhs {}/{}, third {}/{}",
                result_fields.len(),
                lhs.fields.len(),
                lhs_fields.len(),
                rhs.fields.len(),
                rhs_fields.len(),
                third.fields.len(),
                third_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            let third_info = third_fields[index];
            if result_info.kind != ScalarKind::Integer
                || lhs_info.kind != ScalarKind::Integer
                || rhs_info.kind != ScalarKind::Integer
                || third_info.kind != ScalarKind::Integer
            {
                bail!(
                    "vector integer ternary lane {index} requires integer lanes, got result {:?}, lhs {:?}, rhs {:?}, third {:?}",
                    result_info.kind,
                    lhs_info.kind,
                    rhs_info.kind,
                    third_info.kind
                );
            }
            if lhs_info.width != result_info.width
                || rhs_info.width != result_info.width
                || third_info.width != result_info.width
            {
                bail!(
                    "vector integer ternary lane {index} width mismatch: result i{}, lhs i{}, rhs i{}, third i{}",
                    result_info.width,
                    lhs_info.width,
                    rhs_info.width,
                    third_info.width
                );
            }
            checked_intrinsic_integer_width(result_info.width as u64)?;
            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector integer ternary lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector integer ternary rhs lane {index} is undefined or unsupported"))?;
            let third_binding = third
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector integer ternary third lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != result_info.width
                || rhs_binding.width != result_info.width
                || third_binding.width != result_info.width
            {
                bail!(
                    "vector integer ternary lane {index} binding width mismatch: result i{}, lhs i{}, rhs i{}, third i{}",
                    result_info.width,
                    lhs_binding.width,
                    rhs_binding.width,
                    third_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .binding("%shift_lane", third_binding)
                .binding("%third_lane", third_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, selected.clone())?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector integer ternary lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_float_binop(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let (rule, vector_rule, semantic) = match instruction.get_opcode() {
            InstructionOpcode::FAdd => (
                "llvm.fadd.float",
                "llvm.vector.fadd.float",
                HandlerSemantic::FloatBin(FloatBinOp::Add),
            ),
            InstructionOpcode::FSub => (
                "llvm.fsub.float",
                "llvm.vector.fsub.float",
                HandlerSemantic::FloatBin(FloatBinOp::Sub),
            ),
            InstructionOpcode::FMul => (
                "llvm.fmul.float",
                "llvm.vector.fmul.float",
                HandlerSemantic::FloatBin(FloatBinOp::Mul),
            ),
            InstructionOpcode::FDiv => (
                "llvm.fdiv.float",
                "llvm.vector.fdiv.float",
                HandlerSemantic::FloatBin(FloatBinOp::Div),
            ),
            InstructionOpcode::FRem => (
                "llvm.frem.float",
                "llvm.vector.frem.float",
                HandlerSemantic::FloatBin(FloatBinOp::Rem),
            ),
            opcode => bail!("unsupported floating binop opcode: {opcode:?}"),
        };
        if matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)) {
            return self.lower_vector_float_binop(instruction, vector_rule, semantic, |_| Ok(()));
        }
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

    fn lower_vector_float_binop(
        &mut self,
        instruction: InstructionValue<'ctx>,
        rule: &str,
        semantic: HandlerSemantic,
        validate_width: impl Fn(u8) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector floating binop result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vector floating binop fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("vector floating binop lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("vector floating binop rhs fields")?;
        let lhs = self.vector_operand(instruction, 0)?;
        let rhs = self.vector_operand(instruction, 1)?;
        if lhs.fields.len() != result_fields.len()
            || rhs.fields.len() != result_fields.len()
            || lhs_fields.len() != result_fields.len()
            || rhs_fields.len() != result_fields.len()
        {
            bail!(
                "vector floating binop lane count mismatch: result {}, lhs {}/{}, rhs {}/{}",
                result_fields.len(),
                lhs.fields.len(),
                lhs_fields.len(),
                rhs.fields.len(),
                rhs_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            if result_info.kind != ScalarKind::Float
                || lhs_info.kind != ScalarKind::Float
                || rhs_info.kind != ScalarKind::Float
            {
                bail!(
                    "vector floating binop lane {index} requires float lanes, got result {:?}, lhs {:?}, rhs {:?}",
                    result_info.kind,
                    lhs_info.kind,
                    rhs_info.kind
                );
            }
            if lhs_info.width != result_info.width || rhs_info.width != result_info.width {
                bail!(
                    "vector floating binop lane {index} width mismatch: result f{}, lhs f{}, rhs f{}",
                    result_info.width,
                    lhs_info.width,
                    rhs_info.width
                );
            }
            validate_width(result_info.width)?;
            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector floating binop lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector floating binop rhs lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != result_info.width || rhs_binding.width != result_info.width {
                bail!(
                    "vector floating binop lane {index} binding width mismatch: result f{}, lhs f{}, rhs f{}",
                    result_info.width,
                    lhs_binding.width,
                    rhs_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(semantic.clone()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector floating binop lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_float_unary(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let (rule, vector_rule, semantic) = match instruction.get_opcode() {
            InstructionOpcode::FNeg => (
                "llvm.fneg.float",
                "llvm.vector.fneg.float",
                HandlerSemantic::FloatUnary(FloatUnaryOp::Neg),
            ),
            opcode => bail!("unsupported floating unary opcode: {opcode:?}"),
        };
        if matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)) {
            return self.lower_vector_float_unary(instruction, vector_rule, semantic, |_| Ok(()));
        }
        let src = instruction_operand_value(instruction, 0)?;
        let width = instruction_result_width(instruction)?.context("floating unary result has no scalar width")?;
        let env = LoweringEnv::new()
            .llvm_source("%a", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(rule, env, Some(semantic))?;
        Ok(())
    }

    fn lower_vector_float_unary(
        &mut self,
        instruction: InstructionValue<'ctx>,
        rule: &str,
        semantic: HandlerSemantic,
        validate_width: impl Fn(u8) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector floating unary result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vector floating unary fields")?;
        let src_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("vector floating unary source fields")?;
        let src = self.vector_operand(instruction, 0)?;
        if src.fields.len() != result_fields.len() || src_fields.len() != result_fields.len() {
            bail!(
                "vector floating unary lane count mismatch: result {}, source {}/{}",
                result_fields.len(),
                src.fields.len(),
                src_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if result_info.kind != ScalarKind::Float || src_info.kind != ScalarKind::Float {
                bail!(
                    "vector floating unary lane {index} requires floating lanes, got result {:?}, source {:?}",
                    result_info.kind,
                    src_info.kind
                );
            }
            if src_info.width != result_info.width {
                bail!(
                    "vector floating unary lane {index} width mismatch: result f{}, source f{}",
                    result_info.width,
                    src_info.width
                );
            }
            validate_width(result_info.width)?;
            let src_binding = src
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector floating unary source lane {index} is undefined or unsupported"))?;
            if src_binding.width != result_info.width {
                bail!(
                    "vector floating unary lane {index} binding width mismatch: result f{}, source binding i{}",
                    result_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(semantic.clone()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector floating unary lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_float_cast(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let src = instruction_operand_value(instruction, 0)?;
        if matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)) {
            return match instruction.get_opcode() {
                InstructionOpcode::SIToFP
                | InstructionOpcode::UIToFP
                | InstructionOpcode::FPToSI
                | InstructionOpcode::FPToUI
                | InstructionOpcode::FPTrunc
                | InstructionOpcode::FPExt => self.lower_vector_float_cast(instruction, src),
                opcode => bail!("unsupported vector floating cast opcode: {opcode:?}"),
            };
        }
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
            FloatCastOp::FloatToSignedIntSat | FloatCastOp::FloatToUnsignedIntSat => {
                checked_float_width(from_width as u64)?;
                checked_saturating_float_to_int_width(to_width as u64)?;
            },
            FloatCastOp::FloatTrunc => {
                checked_float_width(from_width as u64)?;
                checked_float_width(to_width as u64)?;
                if from_width <= to_width {
                    bail!("only narrowing half/float/double fptrunc is supported by vm_virtualize");
                }
            },
            FloatCastOp::FloatExt => {
                checked_float_width(from_width as u64)?;
                checked_float_width(to_width as u64)?;
                if from_width >= to_width {
                    bail!("only widening half/float/double fpext is supported by vm_virtualize");
                }
            },
        }
        Ok(())
    }

    fn lower_vector_float_cast(
        &mut self,
        instruction: InstructionValue<'ctx>,
        src: BasicValueEnum<'ctx>,
    ) -> anyhow::Result<()> {
        let (rule, op) = match instruction.get_opcode() {
            InstructionOpcode::SIToFP => ("llvm.vector.sitofp.float", FloatCastOp::SignedIntToFloat),
            InstructionOpcode::UIToFP => ("llvm.vector.uitofp.float", FloatCastOp::UnsignedIntToFloat),
            InstructionOpcode::FPToSI => ("llvm.vector.fptosi.float", FloatCastOp::FloatToSignedInt),
            InstructionOpcode::FPToUI => ("llvm.vector.fptoui.float", FloatCastOp::FloatToUnsignedInt),
            InstructionOpcode::FPTrunc => ("llvm.vector.fptrunc.float", FloatCastOp::FloatTrunc),
            InstructionOpcode::FPExt => ("llvm.vector.fpext.float", FloatCastOp::FloatExt),
            opcode => bail!("unsupported vector floating cast opcode: {opcode:?}"),
        };
        self.lower_vector_float_cast_with_rule(instruction, src, rule, op)
    }

    fn lower_vector_float_cast_with_rule(
        &mut self,
        instruction: InstructionValue<'ctx>,
        src: BasicValueEnum<'ctx>,
        rule: &'static str,
        op: FloatCastOp,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector floating cast result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("vector floating cast result fields")?;
        let src_fields = vector_fields_from_type(src.get_type()).context("vector floating cast source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "vector floating cast requires equal lane counts, got source {} and result {}",
                src_fields.len(),
                result_fields.len()
            );
        }

        let (source_kind, result_kind) = match op {
            FloatCastOp::SignedIntToFloat | FloatCastOp::UnsignedIntToFloat => (ScalarKind::Integer, ScalarKind::Float),
            FloatCastOp::FloatToSignedInt
            | FloatCastOp::FloatToUnsignedInt
            | FloatCastOp::FloatToSignedIntSat
            | FloatCastOp::FloatToUnsignedIntSat => (ScalarKind::Float, ScalarKind::Integer),
            FloatCastOp::FloatTrunc | FloatCastOp::FloatExt => (ScalarKind::Float, ScalarKind::Float),
        };

        let src_vector = self.vector_operand(instruction, 0)?;
        if src_vector.fields.len() != src_fields.len() {
            bail!(
                "vector floating cast source field count mismatch: type has {}, value has {}",
                src_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if src_info.kind != source_kind || result_info.kind != result_kind {
                bail!(
                    "vector floating cast lane {index} requires {:?} -> {:?}, got source {:?} and result {:?}",
                    source_kind,
                    result_kind,
                    src_info.kind,
                    result_info.kind
                );
            }
            self.validate_float_cast_widths(op, src_info.width, result_info.width)?;
            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector floating cast source lane {index} is undefined or unsupported"))?;
            if src_binding.width != src_info.width {
                bail!(
                    "vector floating cast lane {index} binding width mismatch: source type {}{}, binding i{}",
                    scalar_kind_prefix(src_info.kind),
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", src_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::FloatCast(op)))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector floating cast lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_alloca(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let allocated_type = instruction
            .get_allocated_type()
            .map_err(|err| anyhow::anyhow!("failed to read alloca type: {err}"))?;
        let element_size = self.target_data.get_store_size(&allocated_type);
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
            let count = self.materialize_value(count_value)?;
            self.execute_lowering_rule("llvm.alloca.dynamic", env, Some(HandlerSemantic::DynamicAlloca))?;
            let count = self.retain_dynamic_alloca_count(count)?;
            self.dynamic_allocas.insert(
                instruction_key(instruction),
                DynamicAllocaObject {
                    count,
                    elem_size: element_size,
                },
            );
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
        if matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)) {
            return self.lower_vector_load(instruction, is_volatile);
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
        if matches!(src_value.get_type(), BasicTypeEnum::VectorType(_)) {
            return self.lower_vector_store(instruction, src_value, is_volatile);
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
            if is_volatile {
                let ptr_value = instruction_operand_value(instruction, 0)?;
                let env = LoweringEnv::new().llvm_source("%ptr", ptr_value);
                self.execute_lowering_rule(
                    "llvm.memory.volatile.empty_aggregate.load",
                    env,
                    Some(HandlerSemantic::SideEffect),
                )?;
                self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: Vec::new() });
                return Ok(());
            }
            self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: Vec::new() });
            return Ok(());
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
            if is_volatile {
                let ptr_value = instruction_operand_value(instruction, 1)?;
                let env = LoweringEnv::new().llvm_source("%ptr", ptr_value);
                self.execute_lowering_rule(
                    "llvm.memory.volatile.empty_aggregate.store",
                    env,
                    Some(HandlerSemantic::SideEffect),
                )?;
                return Ok(());
            }
            return Ok(());
        }
        let aggregate = if let Some(binding) = self.aggregates.get(&value_key(src_value)).cloned() {
            binding
        } else if let Some(binding) = self.constant_aggregate_binding(src_value, false)? {
            binding
        } else {
            bail!("aggregate store source was not built by supported aggregate lowering");
        };
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

    fn lower_vector_load(&mut self, instruction: InstructionValue<'ctx>, is_volatile: bool) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(vector_type) = instruction.get_type() else {
            bail!("vector load result must be a fixed vector");
        };
        let fields = vector_memory_fields(&self.target_data, BasicTypeEnum::VectorType(vector_type))
            .context("fixed vector load memory fields")?;
        let ptr_value = instruction_operand_value(instruction, 0)?;
        let ptr = self.materialize_value(ptr_value)?;
        let (contract, semantic) = if is_volatile {
            ("llvm.memory.volatile.vector.load", HandlerSemantic::VolatileLoad)
        } else {
            ("llvm.memory.vector.load", HandlerSemantic::Load)
        };
        let direct_load = self.emit_action_for_shape(
            contract,
            &semantic,
            &[("dst", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let offset_load = self.emit_action_for_shape(
            contract,
            &semantic,
            &[("dst", "%vf"), ("ptr", "%addr"), ("width", "lane_width(%lane)")],
        )?;
        let gep = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vp"), ("offset", "lane_offset(%lane)")],
        )?;
        let mov = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vf"), ("width", "lane_width(%lane)")],
        )?;

        let mut loaded = Vec::with_capacity(fields.len());
        for field in fields {
            let tmp = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: field.info.width,
            };
            let (lane_ptr, load_action) = if field.offset == 0 {
                (ptr, &direct_load)
            } else {
                (
                    self.emit_memory_intrinsic_gep(contract, &gep, "%vp", ptr, "lane_offset(%lane)", field.offset)?,
                    &offset_load,
                )
            };
            let load_env = LoweringEnv::new()
                .binding("%vp", ptr)
                .binding("%addr", lane_ptr)
                .binding("%vf", tmp)
                .imm("lane_width(%lane)", field.info.width as u64)
                .imm("lane_offset(%lane)", field.offset);
            self.emit_profile_action(load_action, &load_env)?;

            let stable = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: field.info.width,
            };
            let mov_env = LoweringEnv::new()
                .binding("%vf", tmp)
                .binding("%vr", stable)
                .imm("lane_width(%lane)", field.info.width as u64);
            self.emit_profile_action(&mov, &mov_env)?;
            loaded.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: loaded });
        Ok(())
    }

    fn lower_vector_store(
        &mut self,
        instruction: InstructionValue<'ctx>,
        src_value: BasicValueEnum<'ctx>,
        is_volatile: bool,
    ) -> anyhow::Result<()> {
        if is_undef_or_poison_value(src_value) {
            bail!("vector store source must be frozen before VM materialization");
        }
        let fields = vector_memory_fields(&self.target_data, src_value.get_type())
            .context("fixed vector store memory fields")?;
        let vector = if let Some(binding) = self.aggregates.get(&value_key(src_value)).cloned() {
            binding
        } else if let Some(binding) = self.constant_vector_binding(src_value, false, false)? {
            binding
        } else {
            bail!("vector store source was not built by supported vector lowering");
        };
        if vector.fields.len() != fields.len() {
            bail!(
                "vector store lane count mismatch: value has {}, memory layout has {}",
                vector.fields.len(),
                fields.len()
            );
        }

        let ptr_value = instruction_operand_value(instruction, 1)?;
        let ptr = self.materialize_value(ptr_value)?;
        let (contract, semantic) = if is_volatile {
            ("llvm.memory.volatile.vector.store", HandlerSemantic::VolatileStore)
        } else {
            ("llvm.memory.vector.store", HandlerSemantic::Store)
        };
        let direct_store = self.emit_action_for_shape(
            contract,
            &semantic,
            &[("src", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let offset_store = self.emit_action_for_shape(
            contract,
            &semantic,
            &[("src", "%vf"), ("ptr", "%addr"), ("width", "lane_width(%lane)")],
        )?;
        let gep = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vp"), ("offset", "lane_offset(%lane)")],
        )?;

        for (index, field) in fields.into_iter().enumerate() {
            let source = vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("vector store lane {index} is undef or unavailable"))?
                .binding;
            if source.width != field.info.width {
                bail!(
                    "vector store lane {index} width mismatch: value is {}, memory lane is {}",
                    source.width,
                    field.info.width
                );
            }
            let (lane_ptr, store_action) = if field.offset == 0 {
                (ptr, &direct_store)
            } else {
                (
                    self.emit_memory_intrinsic_gep(contract, &gep, "%vp", ptr, "lane_offset(%lane)", field.offset)?,
                    &offset_store,
                )
            };
            let env = LoweringEnv::new()
                .binding("%vp", ptr)
                .binding("%addr", lane_ptr)
                .binding("%vf", source)
                .imm("lane_width(%lane)", field.info.width as u64)
                .imm("lane_offset(%lane)", field.offset);
            self.emit_profile_action(store_action, &env)?;
        }

        Ok(())
    }

    fn lower_masked_memory_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: MaskedMemoryIntrinsicKind,
    ) -> anyhow::Result<()> {
        match kind {
            MaskedMemoryIntrinsicKind::Load => self.lower_masked_vector_load(instruction),
            MaskedMemoryIntrinsicKind::Store => self.lower_masked_vector_store(instruction),
            MaskedMemoryIntrinsicKind::ExpandLoad => self.lower_masked_vector_expandload(instruction),
            MaskedMemoryIntrinsicKind::CompressStore => self.lower_masked_vector_compressstore(instruction),
            MaskedMemoryIntrinsicKind::Gather => self.lower_masked_vector_gather(instruction),
            MaskedMemoryIntrinsicKind::Scatter => self.lower_masked_vector_scatter(instruction),
            MaskedMemoryIntrinsicKind::VpLoad => self.lower_vp_vector_load(instruction),
            MaskedMemoryIntrinsicKind::VpStore => self.lower_vp_vector_store(instruction),
            MaskedMemoryIntrinsicKind::VpGather => self.lower_vp_vector_gather(instruction),
            MaskedMemoryIntrinsicKind::VpScatter => self.lower_vp_vector_scatter(instruction),
            MaskedMemoryIntrinsicKind::VpStridedLoad => self.lower_vp_strided_vector_load(instruction),
            MaskedMemoryIntrinsicKind::VpStridedStore => self.lower_vp_strided_vector_store(instruction),
        }
    }

    fn lower_masked_vector_load(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.masked.load expects exactly 4 arguments, got {actual_args}");
        }
        let AnyTypeEnum::VectorType(vector_type) = instruction.get_type() else {
            bail!("llvm.masked.load result must be a fixed vector");
        };
        let fields = vector_memory_fields(&self.target_data, BasicTypeEnum::VectorType(vector_type))
            .context("llvm.masked.load fixed vector memory fields")?;
        let mask_value = instruction_operand_value(instruction, 2)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.masked.load mask")?;
        let passthru_value = instruction_operand_value(instruction, 3)?;
        let passthru = if is_undef_or_poison_value(passthru_value) {
            AggregateBinding {
                fields: vec![None; fields.len()],
            }
        } else {
            self.vector_operand(instruction, 3)
                .context("llvm.masked.load passthru vector")?
        };
        if passthru.fields.len() != fields.len() {
            bail!(
                "llvm.masked.load passthru lane count mismatch: value has {}, result has {}",
                passthru.fields.len(),
                fields.len()
            );
        }

        let _ = constant_int_operand(instruction, 1, "llvm.masked.load alignment")?;
        let ptr_value = instruction_operand_value(instruction, 0)?;
        let ptr = self.materialize_value(ptr_value)?;
        let contract = MaskedMemoryIntrinsicKind::Load.lowering_rule();
        let direct_load = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Load,
            &[("dst", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let offset_load = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Load,
            &[("dst", "%vf"), ("ptr", "%addr"), ("width", "lane_width(%lane)")],
        )?;
        let gep = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vp"), ("offset", "lane_offset(%lane)")],
        )?;
        let loaded_mov = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vf"), ("width", "lane_width(%lane)")],
        )?;
        let passthru_mov = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vpass"), ("width", "lane_width(%lane)")],
        )?;

        let mut loaded = Vec::with_capacity(fields.len());
        for (index, field) in fields.into_iter().enumerate() {
            let stable = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: field.info.width,
            };
            if mask[index] {
                let tmp = ValueBinding {
                    reg: self.alloc_temporary_vreg()?,
                    width: field.info.width,
                };
                let (lane_ptr, load_action) = if field.offset == 0 {
                    (ptr, &direct_load)
                } else {
                    (
                        self.emit_memory_intrinsic_gep(contract, &gep, "%vp", ptr, "lane_offset(%lane)", field.offset)?,
                        &offset_load,
                    )
                };
                let load_env = LoweringEnv::new()
                    .binding("%vp", ptr)
                    .binding("%addr", lane_ptr)
                    .binding("%vf", tmp)
                    .imm("lane_width(%lane)", field.info.width as u64)
                    .imm("lane_offset(%lane)", field.offset);
                self.emit_profile_action(load_action, &load_env)?;

                let mov_env = LoweringEnv::new()
                    .binding("%vf", tmp)
                    .binding("%vr", stable)
                    .imm("lane_width(%lane)", field.info.width as u64);
                self.emit_profile_action(&loaded_mov, &mov_env)?;
                loaded.push(Some(AggregateField::owned(stable)));
                continue;
            }

            let Some(passthru_lane) = passthru.fields.get(index).copied().flatten() else {
                loaded.push(None);
                continue;
            };
            let passthru_lane = passthru_lane.binding;
            if passthru_lane.width != field.info.width {
                bail!(
                    "llvm.masked.load passthru lane {index} width mismatch: value is {}, memory lane is {}",
                    passthru_lane.width,
                    field.info.width
                );
            }
            let env = LoweringEnv::new()
                .binding("%vpass", passthru_lane)
                .binding("%vr", stable)
                .imm("lane_width(%lane)", field.info.width as u64);
            self.emit_profile_action(&passthru_mov, &env)?;
            loaded.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: loaded });
        Ok(())
    }

    fn lower_vp_vector_load(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 3 {
            bail!("llvm.vp.load expects exactly 3 arguments, got {actual_args}");
        }
        let AnyTypeEnum::VectorType(vector_type) = instruction.get_type() else {
            bail!("llvm.vp.load result must be a fixed vector");
        };
        let fields = vector_memory_fields(&self.target_data, BasicTypeEnum::VectorType(vector_type))
            .context("llvm.vp.load fixed vector memory fields")?;
        let evl = constant_int_operand(instruction, 2, "llvm.vp.load EVL")?;
        let mask_value = instruction_operand_value(instruction, 1)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.vp.load mask")?;

        let ptr_value = instruction_operand_value(instruction, 0)?;
        let ptr = self.materialize_value(ptr_value)?;
        let contract = MaskedMemoryIntrinsicKind::VpLoad.lowering_rule();
        let direct_load = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Load,
            &[("dst", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let offset_load = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Load,
            &[("dst", "%vf"), ("ptr", "%addr"), ("width", "lane_width(%lane)")],
        )?;
        let gep = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vp"), ("offset", "lane_offset(%lane)")],
        )?;
        let loaded_mov = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vf"), ("width", "lane_width(%lane)")],
        )?;

        let mut loaded = Vec::with_capacity(fields.len());
        for (index, field) in fields.into_iter().enumerate() {
            if !mask[index] || index as u64 >= evl {
                loaded.push(None);
                continue;
            }
            let stable = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: field.info.width,
            };
            let tmp = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: field.info.width,
            };
            let (lane_ptr, load_action) = if field.offset == 0 {
                (ptr, &direct_load)
            } else {
                (
                    self.emit_memory_intrinsic_gep(contract, &gep, "%vp", ptr, "lane_offset(%lane)", field.offset)?,
                    &offset_load,
                )
            };
            let load_env = LoweringEnv::new()
                .binding("%vp", ptr)
                .binding("%addr", lane_ptr)
                .binding("%vf", tmp)
                .imm("lane_width(%lane)", field.info.width as u64)
                .imm("lane_offset(%lane)", field.offset);
            self.emit_profile_action(load_action, &load_env)?;

            let mov_env = LoweringEnv::new()
                .binding("%vf", tmp)
                .binding("%vr", stable)
                .imm("lane_width(%lane)", field.info.width as u64);
            self.emit_profile_action(&loaded_mov, &mov_env)?;
            loaded.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: loaded });
        Ok(())
    }

    fn lower_vp_strided_vector_load(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.experimental.vp.strided.load expects exactly 4 arguments, got {actual_args}");
        }
        let AnyTypeEnum::VectorType(result_type) = instruction.get_type() else {
            bail!("llvm.experimental.vp.strided.load result must be a fixed vector");
        };
        let fields = vector_byte_addressable_fields(BasicTypeEnum::VectorType(result_type))
            .context("llvm.experimental.vp.strided.load fixed vector lane fields")?;
        let stride_value = instruction_operand_value(instruction, 1)?;
        let BasicTypeEnum::IntType(stride_type) = stride_value.get_type() else {
            bail!("llvm.experimental.vp.strided.load stride must be an integer");
        };
        checked_width(stride_type.get_bit_width())?;
        let evl = constant_int_operand(instruction, 3, "llvm.experimental.vp.strided.load EVL")?;
        let mask_value = instruction_operand_value(instruction, 2)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.experimental.vp.strided.load mask")?;

        let base_value = instruction_operand_value(instruction, 0)?;
        let base = self.materialize_value(base_value)?;

        let contract = MaskedMemoryIntrinsicKind::VpStridedLoad.lowering_rule();
        let stride = self.materialize_value(stride_value)?;
        let stride = self.extend_vp_strided_stride(contract, stride)?;
        let mul = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Bin(BinOp::Mul),
            &[("dst", "%vo"), ("lhs", "%vwide"), ("rhs", "%vi"), ("width", "64")],
        )?;
        let add = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Bin(BinOp::Add),
            &[("dst", "%addr"), ("lhs", "%vp"), ("rhs", "%vo"), ("width", "64")],
        )?;
        let direct_load = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Load,
            &[("dst", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let strided_load = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Load,
            &[("dst", "%vf"), ("ptr", "%addr"), ("width", "lane_width(%lane)")],
        )?;
        let loaded_mov = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vf"), ("width", "lane_width(%lane)")],
        )?;

        let mut loaded = Vec::with_capacity(fields.len());
        for (index, field) in fields.into_iter().enumerate() {
            if !mask[index] || index as u64 >= evl {
                loaded.push(None);
                continue;
            }
            let stable = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: field.width,
            };
            let tmp = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: field.width,
            };
            let lane_ptr = self.emit_vp_strided_lane_address(contract, &mul, &add, base, stride, index)?;
            let load_env = LoweringEnv::new()
                .binding("%vp", base)
                .binding("%addr", lane_ptr)
                .binding("%vf", tmp)
                .imm("lane_width(%lane)", field.width as u64);
            self.emit_profile_action(if index == 0 { &direct_load } else { &strided_load }, &load_env)
                .with_context(|| format!("while lowering {contract} lane {index}"))?;

            let mov_env = LoweringEnv::new()
                .binding("%vf", tmp)
                .binding("%vr", stable)
                .imm("lane_width(%lane)", field.width as u64);
            self.emit_profile_action(&loaded_mov, &mov_env)?;
            loaded.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: loaded });
        Ok(())
    }

    fn lower_masked_vector_store(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.masked.store expects exactly 4 arguments, got {actual_args}");
        }
        let src_value = instruction_operand_value(instruction, 0)?;
        let fields =
            vector_memory_fields(&self.target_data, src_value.get_type()).context("llvm.masked.store memory fields")?;
        let mask_value = instruction_operand_value(instruction, 3)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.masked.store mask")?;
        if !mask.iter().any(|enabled| *enabled) {
            return Ok(());
        }
        if is_undef_or_poison_value(src_value) {
            bail!("llvm.masked.store source must be defined for enabled lanes");
        }
        let vector = self
            .aggregates
            .get(&value_key(src_value))
            .cloned()
            .context("llvm.masked.store source was not built by supported vector lowering")?;
        if vector.fields.len() != fields.len() {
            bail!(
                "llvm.masked.store lane count mismatch: value has {}, memory layout has {}",
                vector.fields.len(),
                fields.len()
            );
        }

        let _ = constant_int_operand(instruction, 2, "llvm.masked.store alignment")?;
        let ptr_value = instruction_operand_value(instruction, 1)?;
        let ptr = self.materialize_value(ptr_value)?;
        let contract = MaskedMemoryIntrinsicKind::Store.lowering_rule();
        let direct_store = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Store,
            &[("src", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let offset_store = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Store,
            &[("src", "%vf"), ("ptr", "%addr"), ("width", "lane_width(%lane)")],
        )?;
        let gep = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vp"), ("offset", "lane_offset(%lane)")],
        )?;

        for (index, field) in fields.into_iter().enumerate() {
            if !mask[index] {
                continue;
            }
            let source = vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.masked.store enabled lane {index} is undef or unavailable"))?
                .binding;
            if source.width != field.info.width {
                bail!(
                    "llvm.masked.store lane {index} width mismatch: value is {}, memory lane is {}",
                    source.width,
                    field.info.width
                );
            }
            let (lane_ptr, store_action) = if field.offset == 0 {
                (ptr, &direct_store)
            } else {
                (
                    self.emit_memory_intrinsic_gep(contract, &gep, "%vp", ptr, "lane_offset(%lane)", field.offset)?,
                    &offset_store,
                )
            };
            let env = LoweringEnv::new()
                .binding("%vp", ptr)
                .binding("%addr", lane_ptr)
                .binding("%vf", source)
                .imm("lane_width(%lane)", field.info.width as u64)
                .imm("lane_offset(%lane)", field.offset);
            self.emit_profile_action(store_action, &env)?;
        }

        Ok(())
    }

    fn lower_vp_vector_store(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.vp.store expects exactly 4 arguments, got {actual_args}");
        }
        let src_value = instruction_operand_value(instruction, 0)?;
        let fields =
            vector_memory_fields(&self.target_data, src_value.get_type()).context("llvm.vp.store memory fields")?;
        let evl = constant_int_operand(instruction, 3, "llvm.vp.store EVL")?;
        let mask_value = instruction_operand_value(instruction, 2)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.vp.store mask")?;
        if !mask
            .iter()
            .enumerate()
            .any(|(index, enabled)| *enabled && (index as u64) < evl)
        {
            return Ok(());
        }
        if is_undef_or_poison_value(src_value) {
            bail!("llvm.vp.store source must be defined for enabled lanes");
        }
        let vector = self
            .vector_operand(instruction, 0)
            .context("llvm.vp.store source vector")?;
        if vector.fields.len() != fields.len() {
            bail!(
                "llvm.vp.store lane count mismatch: value has {}, memory layout has {}",
                vector.fields.len(),
                fields.len()
            );
        }

        let ptr_value = instruction_operand_value(instruction, 1)?;
        let ptr = self.materialize_value(ptr_value)?;
        let contract = MaskedMemoryIntrinsicKind::VpStore.lowering_rule();
        let direct_store = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Store,
            &[("src", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let offset_store = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Store,
            &[("src", "%vf"), ("ptr", "%addr"), ("width", "lane_width(%lane)")],
        )?;
        let gep = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vp"), ("offset", "lane_offset(%lane)")],
        )?;

        for (index, field) in fields.into_iter().enumerate() {
            if !mask[index] || index as u64 >= evl {
                continue;
            }
            let source = vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.vp.store enabled lane {index} is undef or unavailable"))?
                .binding;
            if source.width != field.info.width {
                bail!(
                    "llvm.vp.store lane {index} width mismatch: value is {}, memory lane is {}",
                    source.width,
                    field.info.width
                );
            }
            let (lane_ptr, store_action) = if field.offset == 0 {
                (ptr, &direct_store)
            } else {
                (
                    self.emit_memory_intrinsic_gep(contract, &gep, "%vp", ptr, "lane_offset(%lane)", field.offset)?,
                    &offset_store,
                )
            };
            let env = LoweringEnv::new()
                .binding("%vp", ptr)
                .binding("%addr", lane_ptr)
                .binding("%vf", source)
                .imm("lane_width(%lane)", field.info.width as u64)
                .imm("lane_offset(%lane)", field.offset);
            self.emit_profile_action(store_action, &env)?;
        }

        Ok(())
    }

    fn lower_vp_strided_vector_store(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 5 {
            bail!("llvm.experimental.vp.strided.store expects exactly 5 arguments, got {actual_args}");
        }
        let src_value = instruction_operand_value(instruction, 0)?;
        let fields = vector_byte_addressable_fields(src_value.get_type())
            .context("llvm.experimental.vp.strided.store lane fields")?;
        let stride_value = instruction_operand_value(instruction, 2)?;
        let BasicTypeEnum::IntType(stride_type) = stride_value.get_type() else {
            bail!("llvm.experimental.vp.strided.store stride must be an integer");
        };
        checked_width(stride_type.get_bit_width())?;
        let evl = constant_int_operand(instruction, 4, "llvm.experimental.vp.strided.store EVL")?;
        let mask_value = instruction_operand_value(instruction, 3)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.experimental.vp.strided.store mask")?;
        if !mask
            .iter()
            .enumerate()
            .any(|(index, enabled)| *enabled && (index as u64) < evl)
        {
            return Ok(());
        }
        if is_undef_or_poison_value(src_value) {
            bail!("llvm.experimental.vp.strided.store source must be defined for enabled lanes");
        }
        let vector = self
            .vector_operand(instruction, 0)
            .context("llvm.experimental.vp.strided.store source vector")?;
        if vector.fields.len() != fields.len() {
            bail!(
                "llvm.experimental.vp.strided.store lane count mismatch: value has {}, memory layout has {}",
                vector.fields.len(),
                fields.len()
            );
        }

        let base_value = instruction_operand_value(instruction, 1)?;
        let base = self.materialize_value(base_value)?;

        let contract = MaskedMemoryIntrinsicKind::VpStridedStore.lowering_rule();
        let stride = self.materialize_value(stride_value)?;
        let stride = self.extend_vp_strided_stride(contract, stride)?;
        let mul = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Bin(BinOp::Mul),
            &[("dst", "%vo"), ("lhs", "%vwide"), ("rhs", "%vi"), ("width", "64")],
        )?;
        let add = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Bin(BinOp::Add),
            &[("dst", "%addr"), ("lhs", "%vp"), ("rhs", "%vo"), ("width", "64")],
        )?;
        let direct_store = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Store,
            &[("src", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let strided_store = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Store,
            &[("src", "%vf"), ("ptr", "%addr"), ("width", "lane_width(%lane)")],
        )?;

        for (index, field) in fields.into_iter().enumerate() {
            if !mask[index] || index as u64 >= evl {
                continue;
            }
            let source = vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| {
                    format!("llvm.experimental.vp.strided.store enabled source lane {index} is undef or unavailable")
                })?
                .binding;
            if source.width != field.width {
                bail!(
                    "llvm.experimental.vp.strided.store lane {index} width mismatch: value is {}, memory lane is {}",
                    source.width,
                    field.width
                );
            }
            let lane_ptr = self.emit_vp_strided_lane_address(contract, &mul, &add, base, stride, index)?;
            let env = LoweringEnv::new()
                .binding("%vp", base)
                .binding("%addr", lane_ptr)
                .binding("%vf", source)
                .imm("lane_width(%lane)", field.width as u64);
            self.emit_profile_action(if index == 0 { &direct_store } else { &strided_store }, &env)
                .with_context(|| format!("while lowering {contract} lane {index}"))?;
        }

        Ok(())
    }

    fn lower_masked_vector_expandload(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 3 {
            bail!("llvm.masked.expandload expects exactly 3 arguments, got {actual_args}");
        }
        let AnyTypeEnum::VectorType(vector_type) = instruction.get_type() else {
            bail!("llvm.masked.expandload result must be a fixed vector");
        };
        let fields = vector_memory_fields(&self.target_data, BasicTypeEnum::VectorType(vector_type))
            .context("llvm.masked.expandload fixed vector memory fields")?;
        let mask_value = instruction_operand_value(instruction, 1)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.masked.expandload mask")?;
        let passthru_value = instruction_operand_value(instruction, 2)?;
        let passthru = if is_undef_or_poison_value(passthru_value) {
            AggregateBinding {
                fields: vec![None; fields.len()],
            }
        } else {
            self.vector_operand(instruction, 2)
                .context("llvm.masked.expandload passthru vector")?
        };
        if passthru.fields.len() != fields.len() {
            bail!(
                "llvm.masked.expandload passthru lane count mismatch: value has {}, result has {}",
                passthru.fields.len(),
                fields.len()
            );
        }

        let ptr_value = instruction_operand_value(instruction, 0)?;
        let ptr = self.materialize_value(ptr_value)?;
        let contract = MaskedMemoryIntrinsicKind::ExpandLoad.lowering_rule();
        let direct_load = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Load,
            &[("dst", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let offset_load = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Load,
            &[("dst", "%vf"), ("ptr", "%addr"), ("width", "lane_width(%lane)")],
        )?;
        let gep = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vp"), ("offset", "active_offset(%lane)")],
        )?;
        let loaded_mov = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vf"), ("width", "lane_width(%lane)")],
        )?;
        let passthru_mov = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vpass"), ("width", "lane_width(%lane)")],
        )?;

        let mut active_offset = 0_u64;
        let mut loaded = Vec::with_capacity(fields.len());
        for (index, field) in fields.into_iter().enumerate() {
            let stable = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: field.info.width,
            };
            if mask[index] {
                let tmp = ValueBinding {
                    reg: self.alloc_temporary_vreg()?,
                    width: field.info.width,
                };
                let (lane_ptr, load_action) = if active_offset == 0 {
                    (ptr, &direct_load)
                } else {
                    (
                        self.emit_memory_intrinsic_gep(
                            contract,
                            &gep,
                            "%vp",
                            ptr,
                            "active_offset(%lane)",
                            active_offset,
                        )?,
                        &offset_load,
                    )
                };
                let load_env = LoweringEnv::new()
                    .binding("%vp", ptr)
                    .binding("%addr", lane_ptr)
                    .binding("%vf", tmp)
                    .imm("lane_width(%lane)", field.info.width as u64)
                    .imm("active_offset(%lane)", active_offset);
                self.emit_profile_action(load_action, &load_env)?;

                let mov_env = LoweringEnv::new()
                    .binding("%vf", tmp)
                    .binding("%vr", stable)
                    .imm("lane_width(%lane)", field.info.width as u64);
                self.emit_profile_action(&loaded_mov, &mov_env)?;
                active_offset = active_offset
                    .checked_add(u64::from(field.info.width / 8))
                    .context("llvm.masked.expandload active lane offset overflow")?;
                loaded.push(Some(AggregateField::owned(stable)));
                continue;
            }

            let Some(passthru_lane) = passthru.fields.get(index).copied().flatten() else {
                loaded.push(None);
                continue;
            };
            let passthru_lane = passthru_lane.binding;
            if passthru_lane.width != field.info.width {
                bail!(
                    "llvm.masked.expandload passthru lane {index} width mismatch: value is {}, memory lane is {}",
                    passthru_lane.width,
                    field.info.width
                );
            }
            let env = LoweringEnv::new()
                .binding("%vpass", passthru_lane)
                .binding("%vr", stable)
                .imm("lane_width(%lane)", field.info.width as u64);
            self.emit_profile_action(&passthru_mov, &env)?;
            loaded.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: loaded });
        Ok(())
    }

    fn lower_masked_vector_compressstore(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 3 {
            bail!("llvm.masked.compressstore expects exactly 3 arguments, got {actual_args}");
        }
        let src_value = instruction_operand_value(instruction, 0)?;
        let fields = vector_memory_fields(&self.target_data, src_value.get_type())
            .context("llvm.masked.compressstore memory fields")?;
        let mask_value = instruction_operand_value(instruction, 2)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.masked.compressstore mask")?;
        if !mask.iter().any(|enabled| *enabled) {
            return Ok(());
        }
        if is_undef_or_poison_value(src_value) {
            bail!("llvm.masked.compressstore source must be defined for enabled lanes");
        }
        let vector = self
            .vector_operand(instruction, 0)
            .context("llvm.masked.compressstore source vector")?;
        if vector.fields.len() != fields.len() {
            bail!(
                "llvm.masked.compressstore lane count mismatch: value has {}, memory layout has {}",
                vector.fields.len(),
                fields.len()
            );
        }

        let ptr_value = instruction_operand_value(instruction, 1)?;
        let ptr = self.materialize_value(ptr_value)?;
        let contract = MaskedMemoryIntrinsicKind::CompressStore.lowering_rule();
        let direct_store = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Store,
            &[("src", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let offset_store = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Store,
            &[("src", "%vf"), ("ptr", "%addr"), ("width", "lane_width(%lane)")],
        )?;
        let gep = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Gep,
            &[("dst", "%addr"), ("base", "%vp"), ("offset", "active_offset(%lane)")],
        )?;

        let mut active_offset = 0_u64;
        for (index, field) in fields.into_iter().enumerate() {
            if !mask[index] {
                continue;
            }
            let source = vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.masked.compressstore enabled lane {index} is undef or unavailable"))?
                .binding;
            if source.width != field.info.width {
                bail!(
                    "llvm.masked.compressstore lane {index} width mismatch: value is {}, memory lane is {}",
                    source.width,
                    field.info.width
                );
            }
            let (lane_ptr, store_action) = if active_offset == 0 {
                (ptr, &direct_store)
            } else {
                (
                    self.emit_memory_intrinsic_gep(contract, &gep, "%vp", ptr, "active_offset(%lane)", active_offset)?,
                    &offset_store,
                )
            };
            let env = LoweringEnv::new()
                .binding("%vp", ptr)
                .binding("%addr", lane_ptr)
                .binding("%vf", source)
                .imm("lane_width(%lane)", field.info.width as u64)
                .imm("active_offset(%lane)", active_offset);
            self.emit_profile_action(store_action, &env)?;
            active_offset = active_offset
                .checked_add(u64::from(field.info.width / 8))
                .context("llvm.masked.compressstore active lane offset overflow")?;
        }

        Ok(())
    }

    fn lower_masked_vector_gather(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.masked.gather expects exactly 4 arguments, got {actual_args}");
        }
        let AnyTypeEnum::VectorType(result_type) = instruction.get_type() else {
            bail!("llvm.masked.gather result must be a fixed vector");
        };
        let fields = vector_byte_addressable_fields(BasicTypeEnum::VectorType(result_type))
            .context("llvm.masked.gather fixed vector lane fields")?;
        let pointer_value = instruction_operand_value(instruction, 0)?;
        ensure_pointer_vector_lanes(
            pointer_value.get_type(),
            fields.len(),
            "llvm.masked.gather pointer vector",
        )?;
        let pointer_vector = self
            .vector_operand(instruction, 0)
            .context("llvm.masked.gather pointer vector")?;
        if pointer_vector.fields.len() != fields.len() {
            bail!(
                "llvm.masked.gather pointer lane count mismatch: pointer vector has {}, result has {}",
                pointer_vector.fields.len(),
                fields.len()
            );
        }
        let mask_value = instruction_operand_value(instruction, 2)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.masked.gather mask")?;
        let passthru_value = instruction_operand_value(instruction, 3)?;
        let passthru = if is_undef_or_poison_value(passthru_value) {
            AggregateBinding {
                fields: vec![None; fields.len()],
            }
        } else {
            self.vector_operand(instruction, 3)
                .context("llvm.masked.gather passthru vector")?
        };
        if passthru.fields.len() != fields.len() {
            bail!(
                "llvm.masked.gather passthru lane count mismatch: value has {}, result has {}",
                passthru.fields.len(),
                fields.len()
            );
        }

        let _ = constant_int_operand(instruction, 1, "llvm.masked.gather alignment")?;
        let contract = MaskedMemoryIntrinsicKind::Gather.lowering_rule();
        let load = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Load,
            &[("dst", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let loaded_mov = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vf"), ("width", "lane_width(%lane)")],
        )?;
        let passthru_mov = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vpass"), ("width", "lane_width(%lane)")],
        )?;

        let mut loaded = Vec::with_capacity(fields.len());
        for (index, field) in fields.into_iter().enumerate() {
            let stable = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: field.width,
            };
            if mask[index] {
                let ptr = pointer_vector
                    .fields
                    .get(index)
                    .copied()
                    .flatten()
                    .with_context(|| {
                        format!("llvm.masked.gather enabled pointer lane {index} is undef or unavailable")
                    })?
                    .binding;
                if ptr.width != 64 {
                    bail!(
                        "llvm.masked.gather pointer lane {index} must be i64 pointer bits, got i{}",
                        ptr.width
                    );
                }
                let tmp = ValueBinding {
                    reg: self.alloc_temporary_vreg()?,
                    width: field.width,
                };
                let load_env = LoweringEnv::new()
                    .binding("%vp", ptr)
                    .binding("%vf", tmp)
                    .imm("lane_width(%lane)", field.width as u64);
                self.emit_profile_action(&load, &load_env)?;

                let mov_env = LoweringEnv::new()
                    .binding("%vf", tmp)
                    .binding("%vr", stable)
                    .imm("lane_width(%lane)", field.width as u64);
                self.emit_profile_action(&loaded_mov, &mov_env)?;
                loaded.push(Some(AggregateField::owned(stable)));
                continue;
            }

            let Some(passthru_lane) = passthru.fields.get(index).copied().flatten() else {
                loaded.push(None);
                continue;
            };
            let passthru_lane = passthru_lane.binding;
            if passthru_lane.width != field.width {
                bail!(
                    "llvm.masked.gather passthru lane {index} width mismatch: value is {}, memory lane is {}",
                    passthru_lane.width,
                    field.width
                );
            }
            let env = LoweringEnv::new()
                .binding("%vpass", passthru_lane)
                .binding("%vr", stable)
                .imm("lane_width(%lane)", field.width as u64);
            self.emit_profile_action(&passthru_mov, &env)?;
            loaded.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: loaded });
        Ok(())
    }

    fn lower_vp_vector_gather(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 3 {
            bail!("llvm.vp.gather expects exactly 3 arguments, got {actual_args}");
        }
        let AnyTypeEnum::VectorType(result_type) = instruction.get_type() else {
            bail!("llvm.vp.gather result must be a fixed vector");
        };
        let fields = vector_byte_addressable_fields(BasicTypeEnum::VectorType(result_type))
            .context("llvm.vp.gather fixed vector lane fields")?;
        let evl = constant_int_operand(instruction, 2, "llvm.vp.gather EVL")?;
        let pointer_value = instruction_operand_value(instruction, 0)?;
        ensure_pointer_vector_lanes(pointer_value.get_type(), fields.len(), "llvm.vp.gather pointer vector")?;
        let pointer_vector = self
            .vector_operand(instruction, 0)
            .context("llvm.vp.gather pointer vector")?;
        if pointer_vector.fields.len() != fields.len() {
            bail!(
                "llvm.vp.gather pointer lane count mismatch: pointer vector has {}, result has {}",
                pointer_vector.fields.len(),
                fields.len()
            );
        }
        let mask_value = instruction_operand_value(instruction, 1)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.vp.gather mask")?;

        let contract = MaskedMemoryIntrinsicKind::VpGather.lowering_rule();
        let load = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Load,
            &[("dst", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;
        let loaded_mov = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vf"), ("width", "lane_width(%lane)")],
        )?;

        let mut loaded = Vec::with_capacity(fields.len());
        for (index, field) in fields.into_iter().enumerate() {
            if !mask[index] || index as u64 >= evl {
                loaded.push(None);
                continue;
            }
            let ptr = pointer_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.vp.gather enabled pointer lane {index} is undef or unavailable"))?
                .binding;
            if ptr.width != 64 {
                bail!(
                    "llvm.vp.gather pointer lane {index} must be i64 pointer bits, got i{}",
                    ptr.width
                );
            }

            let stable = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: field.width,
            };
            let tmp = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: field.width,
            };
            let load_env = LoweringEnv::new()
                .binding("%vp", ptr)
                .binding("%vf", tmp)
                .imm("lane_width(%lane)", field.width as u64);
            self.emit_profile_action(&load, &load_env)?;

            let mov_env = LoweringEnv::new()
                .binding("%vf", tmp)
                .binding("%vr", stable)
                .imm("lane_width(%lane)", field.width as u64);
            self.emit_profile_action(&loaded_mov, &mov_env)?;
            loaded.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: loaded });
        Ok(())
    }

    fn lower_masked_vector_scatter(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.masked.scatter expects exactly 4 arguments, got {actual_args}");
        }
        let src_value = instruction_operand_value(instruction, 0)?;
        let fields = vector_byte_addressable_fields(src_value.get_type()).context("llvm.masked.scatter lane fields")?;
        let mask_value = instruction_operand_value(instruction, 3)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.masked.scatter mask")?;
        if !mask.iter().any(|enabled| *enabled) {
            return Ok(());
        }
        if is_undef_or_poison_value(src_value) {
            bail!("llvm.masked.scatter source must be defined for enabled lanes");
        }
        let vector = self
            .aggregates
            .get(&value_key(src_value))
            .cloned()
            .context("llvm.masked.scatter source was not built by supported vector lowering")?;
        if vector.fields.len() != fields.len() {
            bail!(
                "llvm.masked.scatter lane count mismatch: value has {}, memory layout has {}",
                vector.fields.len(),
                fields.len()
            );
        }
        let pointer_vector = self
            .vector_operand(instruction, 1)
            .context("llvm.masked.scatter pointer vector")?;
        let pointer_value = instruction_operand_value(instruction, 1)?;
        ensure_pointer_vector_lanes(
            pointer_value.get_type(),
            fields.len(),
            "llvm.masked.scatter pointer vector",
        )?;
        if pointer_vector.fields.len() != fields.len() {
            bail!(
                "llvm.masked.scatter pointer lane count mismatch: pointer vector has {}, value has {}",
                pointer_vector.fields.len(),
                fields.len()
            );
        }

        let _ = constant_int_operand(instruction, 2, "llvm.masked.scatter alignment")?;
        let contract = MaskedMemoryIntrinsicKind::Scatter.lowering_rule();
        let store = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Store,
            &[("src", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;

        for (index, field) in fields.into_iter().enumerate() {
            if !mask[index] {
                continue;
            }
            let source = vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.masked.scatter enabled source lane {index} is undef or unavailable"))?
                .binding;
            if source.width != field.width {
                bail!(
                    "llvm.masked.scatter lane {index} width mismatch: value is {}, memory lane is {}",
                    source.width,
                    field.width
                );
            }
            let ptr = pointer_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.masked.scatter enabled pointer lane {index} is undef or unavailable"))?
                .binding;
            if ptr.width != 64 {
                bail!(
                    "llvm.masked.scatter pointer lane {index} must be i64 pointer bits, got i{}",
                    ptr.width
                );
            }
            let env = LoweringEnv::new()
                .binding("%vp", ptr)
                .binding("%vf", source)
                .imm("lane_width(%lane)", field.width as u64);
            self.emit_profile_action(&store, &env)?;
        }

        Ok(())
    }

    fn lower_vp_vector_scatter(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.vp.scatter expects exactly 4 arguments, got {actual_args}");
        }
        let src_value = instruction_operand_value(instruction, 0)?;
        let fields = vector_byte_addressable_fields(src_value.get_type()).context("llvm.vp.scatter lane fields")?;
        let evl = constant_int_operand(instruction, 3, "llvm.vp.scatter EVL")?;
        let mask_value = instruction_operand_value(instruction, 2)?;
        let mask = constant_i1_vector_mask(mask_value, fields.len(), "llvm.vp.scatter mask")?;
        if !mask
            .iter()
            .enumerate()
            .any(|(index, enabled)| *enabled && (index as u64) < evl)
        {
            return Ok(());
        }
        if is_undef_or_poison_value(src_value) {
            bail!("llvm.vp.scatter source must be defined for enabled lanes");
        }
        let vector = self
            .vector_operand(instruction, 0)
            .context("llvm.vp.scatter source vector")?;
        if vector.fields.len() != fields.len() {
            bail!(
                "llvm.vp.scatter lane count mismatch: value has {}, memory layout has {}",
                vector.fields.len(),
                fields.len()
            );
        }
        let pointer_value = instruction_operand_value(instruction, 1)?;
        ensure_pointer_vector_lanes(pointer_value.get_type(), fields.len(), "llvm.vp.scatter pointer vector")?;
        let pointer_vector = self
            .vector_operand(instruction, 1)
            .context("llvm.vp.scatter pointer vector")?;
        if pointer_vector.fields.len() != fields.len() {
            bail!(
                "llvm.vp.scatter pointer lane count mismatch: pointer vector has {}, value has {}",
                pointer_vector.fields.len(),
                fields.len()
            );
        }

        let contract = MaskedMemoryIntrinsicKind::VpScatter.lowering_rule();
        let store = self.emit_action_for_shape(
            contract,
            &HandlerSemantic::Store,
            &[("src", "%vf"), ("ptr", "%vp"), ("width", "lane_width(%lane)")],
        )?;

        for (index, field) in fields.into_iter().enumerate() {
            if !mask[index] || index as u64 >= evl {
                continue;
            }
            let source = vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.vp.scatter enabled source lane {index} is undef or unavailable"))?
                .binding;
            if source.width != field.width {
                bail!(
                    "llvm.vp.scatter lane {index} width mismatch: value is {}, memory lane is {}",
                    source.width,
                    field.width
                );
            }
            let ptr = pointer_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.vp.scatter enabled pointer lane {index} is undef or unavailable"))?
                .binding;
            if ptr.width != 64 {
                bail!(
                    "llvm.vp.scatter pointer lane {index} must be i64 pointer bits, got i{}",
                    ptr.width
                );
            }
            let env = LoweringEnv::new()
                .binding("%vp", ptr)
                .binding("%vf", source)
                .imm("lane_width(%lane)", field.width as u64);
            self.emit_profile_action(&store, &env)?;
        }

        Ok(())
    }

    fn lower_atomic_load(
        &mut self,
        instruction: InstructionValue<'ctx>,
        ordering: AtomicOrdering,
        is_volatile: bool,
    ) -> anyhow::Result<()> {
        let sync_scope = atomic_sync_scope(instruction, "load")?;
        let ptr = instruction_operand_value(instruction, 0)?;
        let width = instruction_result_width(instruction)?.context("atomic load result has no scalar width")?;
        ensure_atomic_load_store_value_type(instruction.get_type(), "load")?;
        ensure_naturally_aligned_atomic(instruction, "load", width)?;
        let ordering = atomic_ordering_for_load(ordering)?;
        let env = LoweringEnv::new()
            .llvm_source("%ptr", ptr)
            .llvm_value("%r", instruction_key(instruction))
            .imm("memory_width(%ptr)", width as u64)
            .imm("memory_ordering(%ptr)", ordering as u64)
            .imm("sync_scope(%ptr)", sync_scope as u64);
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
        let sync_scope = atomic_sync_scope(instruction, "store")?;
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
            .imm("memory_ordering(%ptr)", ordering as u64)
            .imm("sync_scope(%ptr)", sync_scope as u64);
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
        let sync_scope = atomic_sync_scope(instruction, "atomicrmw")?;
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
            .imm("memory_ordering(%ptr)", ordering as u64)
            .imm("sync_scope(%ptr)", sync_scope as u64);
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
        let sync_scope = atomic_sync_scope(instruction, "cmpxchg")?;
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
            .imm("failure_ordering(%ptr)", failure_ordering as u64)
            .imm("sync_scope(%ptr)", sync_scope as u64);
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
        let ordering = atomic_ordering_for_fence(memory_ordering(instruction, "fence")?)?;
        // SAFETY: `instruction` 是当前 module 中的 live fence instruction；C API 只读取
        // syncscope ID。runtime 只会按有限常量 case 生成 LLVM fence，不把动态值传给 LLVM。
        let sync_scope = atomic_sync_scope(instruction, "fence")?;
        let env = LoweringEnv::new()
            .imm("memory_ordering(%fence)", ordering as u64)
            .imm("sync_scope(%fence)", sync_scope as u64);
        self.execute_lowering_rule("llvm.fence", env, Some(HandlerSemantic::Fence))?;
        Ok(())
    }

    fn lower_gep(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let gep = GepInst::new(instruction);
        let base_value = gep
            .get_pointer_operand()
            .context("getelementptr has no pointer operand")?;
        let dynamic_base = if gep_is_inbounds(instruction) {
            self.dynamic_alloca_object(base_value)?
        } else {
            None
        };
        let dynamic_gep_base = if gep_is_inbounds(instruction) && dynamic_base.is_none() {
            self.dynamic_alloca_gep_object(base_value)?
        } else {
            None
        };
        let static_base = if gep_is_inbounds(instruction) && dynamic_base.is_none() && dynamic_gep_base.is_none() {
            self.static_object_base(base_value)?
        } else {
            None
        };
        let static_gep_base = if gep_is_inbounds(instruction)
            && dynamic_base.is_none()
            && dynamic_gep_base.is_none()
            && static_base.is_none()
        {
            self.dynamic_static_gep_object(base_value)?
        } else {
            None
        };
        let Some(offset) = gep.accumulate_constant_offset(self.module) else {
            let binding = self.ensure_result_binding(instruction)?;
            let base = self.materialize_value(base_value)?;
            self.lower_dynamic_gep(instruction, gep, binding, base)?;
            if let Some(object) = dynamic_base {
                self.retain_dynamic_gep_offset(instruction, object, binding, base)?;
            } else if let Some(object) = dynamic_gep_base {
                self.retain_chained_dynamic_gep_offset(instruction, object, binding, base)?;
            } else if let Some(object) = static_base {
                self.retain_static_gep_offset(instruction, object, binding, base)?;
            } else if let Some(object) = static_gep_base {
                self.retain_chained_static_gep_offset(instruction, object, binding, base)?;
            }
            return Ok(());
        };
        let env = LoweringEnv::new()
            .llvm_source("%base", base_value)
            .llvm_value("%r", instruction_key(instruction))
            .imm("constant_gep_offset(%r)", offset as u64);
        let env = self.execute_lowering_rule("llvm.gep.constant", env, Some(HandlerSemantic::Gep))?;
        if let Some(object) = dynamic_base {
            self.retain_dynamic_gep_constant_offset(instruction, object, offset)?;
        } else if let Some(object) = dynamic_gep_base {
            let LoweringValue::Reg(binding) = env.get("%r")? else {
                bail!("constant dynamic alloca GEP result must be a register");
            };
            let LoweringValue::Reg(base) = env.get("%vb")? else {
                bail!("constant dynamic alloca GEP base must be a register");
            };
            self.retain_chained_dynamic_gep_offset(instruction, object, binding, base)?;
        } else if let Some(object) = static_base {
            self.retain_static_gep_constant_offset(instruction, object, offset)?;
        } else if let Some(object) = static_gep_base {
            let LoweringValue::Reg(binding) = env.get("%r")? else {
                bail!("constant static object GEP result must be a register");
            };
            let LoweringValue::Reg(base) = env.get("%vb")? else {
                bail!("constant static object GEP base must be a register");
            };
            self.retain_chained_static_gep_offset(instruction, object, binding, base)?;
        }
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
        let sext_action = self.emit_action_for_shape(
            "llvm.gep.dynamic",
            &HandlerSemantic::Cast(CastOp::SExt),
            &[
                ("dst", "%vx"),
                ("src", "%vi"),
                ("from_width", "type_width(%index)"),
                ("to_width", "64"),
            ],
        )?;
        let mul_action = self.emit_action_for_shape(
            "llvm.gep.dynamic",
            &HandlerSemantic::Bin(BinOp::Mul),
            &[
                ("dst", "%vs"),
                ("lhs", "%vx"),
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
            let extended_index = if index.width == 64 {
                index
            } else if index.width < 64 {
                let reg = self.alloc_temporary_vreg()?;
                let env = LoweringEnv::new()
                    .binding("%vi", index)
                    .reg("%vx", reg, 64)
                    .imm("type_width(%index)", index.width as u64)
                    .imm("64", 64);
                self.emit_profile_action(&sext_action, &env)?;
                ValueBinding { reg, width: 64 }
            } else {
                bail!(
                    "dynamic getelementptr index width i{} is not supported by vm_virtualize",
                    index.width
                );
            };
            let scale_reg = self.alloc_temporary_vreg()?;
            self.push_constant(scale_reg, scale, 64)?;
            let scaled = self.alloc_temporary_vreg()?;
            let mul_env = LoweringEnv::new()
                .binding("%vx", extended_index)
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
        if call_uses_inline_asm(instruction) {
            bail!("inline asm calls are not supported by vm_virtualize");
        }
        if call_uses_musttail(instruction) {
            bail!("musttail calls are not supported by vm_virtualize");
        }
        let has_operand_bundles = call_has_operand_bundles(instruction);

        let call = CallInst::new(instruction);
        let Some(callee) = call.get_call_function() else {
            if has_operand_bundles {
                bail!("call operand bundles are not supported by vm_virtualize");
            }
            return self.lower_indirect_call(instruction);
        };
        if has_operand_bundles && !matches!(nop_intrinsic_kind(callee), Some(NopIntrinsicKind::Assume)) {
            bail!("call operand bundles are not supported by vm_virtualize");
        }
        if let Some(kind) = memory_intrinsic_kind(callee) {
            return self.lower_memory_intrinsic(instruction, kind);
        }
        if let Some(kind) = masked_memory_intrinsic_kind(callee) {
            return self.lower_masked_memory_intrinsic(instruction, kind);
        }
        if sideeffect_intrinsic(callee) {
            return self.lower_sideeffect_intrinsic(instruction);
        }
        if let Some(kind) = stack_intrinsic_kind(callee) {
            return self.lower_stack_intrinsic(instruction, kind);
        }
        if clear_cache_intrinsic(callee) {
            return self.lower_clear_cache_intrinsic(instruction);
        }
        if pseudoprobe_intrinsic(callee) {
            return self.lower_pseudoprobe_intrinsic(instruction);
        }
        if prefetch_intrinsic(callee) {
            return self.lower_prefetch_intrinsic(instruction);
        }
        if let Some(kind) = nop_intrinsic_kind(callee) {
            if has_operand_bundles && matches!(kind, NopIntrinsicKind::Assume) {
                return self.lower_assume_intrinsic_with_operand_bundles(instruction);
            }
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
        if let Some(kind) = vector_permute_intrinsic_kind(callee) {
            return self.lower_vector_permute_intrinsic(instruction, kind);
        }
        if experimental_vp_splice_intrinsic(callee) {
            return self.lower_experimental_vp_splice_intrinsic(instruction);
        }
        if experimental_vector_extract_last_active_intrinsic(callee) {
            return self.lower_vector_extract_last_active_intrinsic(instruction);
        }
        if get_active_lane_mask_intrinsic(callee) {
            return self.lower_get_active_lane_mask_intrinsic(instruction);
        }
        if experimental_get_vector_length_intrinsic(callee) {
            return self.lower_experimental_get_vector_length_intrinsic(instruction);
        }
        if experimental_cttz_elts_intrinsic(callee) {
            return self.lower_cttz_elts_intrinsic(instruction);
        }
        if vp_cttz_elts_intrinsic(callee) {
            return self.lower_vp_cttz_elts_intrinsic(instruction);
        }
        if vp_select_intrinsic(callee) {
            return self.lower_vp_select_intrinsic(instruction);
        }
        if vp_merge_intrinsic(callee) {
            return self.lower_vp_merge_intrinsic(instruction);
        }
        if experimental_vp_reverse_intrinsic(callee) {
            return self.lower_experimental_vp_reverse_intrinsic(instruction);
        }
        if experimental_vp_splat_intrinsic(callee) {
            return self.lower_experimental_vp_splat_intrinsic(instruction);
        }
        if vp_is_fpclass_intrinsic(callee) {
            return self.lower_vp_is_fpclass_intrinsic(instruction);
        }
        if let Some(kind) = vp_pointer_cast_intrinsic_kind(callee) {
            return self.lower_vp_pointer_cast_intrinsic(instruction, kind);
        }
        if let Some(kind) = vp_integer_cast_intrinsic_kind(callee) {
            return self.lower_vp_integer_cast_intrinsic(instruction, kind);
        }
        if let Some(kind) = vp_float_cast_intrinsic_kind(callee) {
            return self.lower_vp_float_cast_intrinsic(instruction, kind);
        }
        if stepvector_intrinsic(callee) {
            return self.lower_stepvector_intrinsic(instruction);
        }
        if let Some(kind) = compile_time_intrinsic_kind(callee) {
            return self.lower_compile_time_intrinsic(instruction, kind);
        }
        if let Some(kind) = float_intrinsic_kind(callee) {
            return self.lower_float_intrinsic(instruction, kind);
        }
        if let Some(kind) = constrained_float_unary_intrinsic_kind(callee) {
            return self.lower_constrained_float_unary_intrinsic(instruction, kind);
        }
        if let Some(kind) = constrained_float_binop_intrinsic_kind(callee) {
            return self.lower_constrained_float_binop_intrinsic(instruction, kind);
        }
        if let Some(kind) = constrained_float_binary_intrinsic_kind(callee) {
            return self.lower_constrained_float_binary_intrinsic(instruction, kind);
        }
        if let Some(kind) = constrained_float_int_binary_intrinsic_kind(callee) {
            return self.lower_constrained_float_int_binary_intrinsic(instruction, kind);
        }
        if let Some(kind) = constrained_float_ternary_intrinsic_kind(callee) {
            return self.lower_constrained_float_ternary_intrinsic(instruction, kind);
        }
        if let Some(kind) = constrained_round_to_int_intrinsic_kind(callee) {
            return self.lower_constrained_round_to_int_intrinsic(instruction, kind);
        }
        if let Some(kind) = constrained_float_cmp_intrinsic_kind(callee) {
            return self.lower_constrained_float_cmp_intrinsic(instruction, kind);
        }
        if let Some(kind) = constrained_float_cast_intrinsic_kind(callee) {
            return self.lower_constrained_float_cast_intrinsic(instruction, kind);
        }
        if let Some(kind) = vp_reduce_float_intrinsic_kind(callee) {
            return self.lower_vp_reduce_float_intrinsic(instruction, kind);
        }
        if let Some(kind) = vector_reduce_float_intrinsic_kind(callee) {
            return self.lower_vector_reduce_float_intrinsic(instruction, kind);
        }
        if let Some(kind) = hardware_loop_intrinsic_kind(callee) {
            return self.lower_hardware_loop_intrinsic(instruction, kind);
        }
        if loop_decrement_intrinsic(callee) {
            return self.lower_loop_decrement_intrinsic(instruction);
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
        if let Some(kind) = vp_integer_unary_intrinsic_kind(callee) {
            return self.lower_vp_integer_unary_intrinsic(instruction, kind);
        }
        if let Some(kind) = vp_float_unary_intrinsic_kind(callee) {
            return self.lower_vp_float_unary_intrinsic(instruction, kind);
        }
        if let Some(kind) = vp_round_to_int_intrinsic_kind(callee) {
            return self.lower_vp_round_to_int_intrinsic(instruction, kind);
        }
        if let Some(kind) = vp_float_ternary_intrinsic_kind(callee) {
            return self.lower_vp_float_ternary_intrinsic(instruction, kind);
        }
        if vp_icmp_intrinsic(callee) {
            return self.lower_vp_icmp_intrinsic(instruction);
        }
        if vp_fcmp_intrinsic(callee) {
            return self.lower_vp_fcmp_intrinsic(instruction);
        }
        if let Some(kind) = vp_float_binop_intrinsic_kind(callee) {
            return self.lower_vp_float_binop_intrinsic(instruction, kind);
        }
        if let Some(kind) = vp_integer_binop_intrinsic_kind(callee) {
            return self.lower_vp_integer_binop_intrinsic(instruction, kind);
        }
        if let Some(kind) = vp_integer_ternary_intrinsic_kind(callee) {
            return self.lower_vp_integer_ternary_intrinsic(instruction, kind);
        }
        if let Some(kind) = vp_reduce_integer_intrinsic_kind(callee) {
            return self.lower_vp_reduce_integer_intrinsic(instruction, kind);
        }
        if let Some(kind) = vector_reduce_integer_intrinsic_kind(callee) {
            return self.lower_vector_reduce_integer_intrinsic(instruction, kind);
        }
        if let Some(kind) = counter_intrinsic_kind(callee) {
            return self.lower_counter_intrinsic(instruction, kind);
        }
        if vscale_intrinsic(callee) {
            return self.lower_vscale_intrinsic(instruction);
        }
        if get_rounding_intrinsic(callee) {
            return self.lower_get_rounding_intrinsic(instruction);
        }
        if flt_rounds_intrinsic(callee) {
            return self.lower_flt_rounds_intrinsic(instruction);
        }
        if let Some(kind) = fp_state_intrinsic_kind(callee) {
            return self.lower_fp_state_intrinsic(instruction, kind);
        }
        if thread_pointer_intrinsic(callee) {
            return self.lower_thread_pointer_intrinsic(instruction);
        }
        if let Some(kind) = trap_intrinsic_kind(callee) {
            return self.lower_trap_intrinsic(instruction, kind);
        }
        if let Some(reason) = unsupported_stack_introspection_intrinsic_reason(callee) {
            bail!("{reason}");
        }
        if let Some(reason) = unsupported_target_state_intrinsic_reason(callee) {
            bail!("{reason}");
        }
        if let Some(reason) = unsupported_target_specific_intrinsic_reason(callee) {
            bail!("{reason}");
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

        let adapter = self.emit_indirect_call_adapter(instruction, call_type)?;
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
                BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_) | BasicTypeEnum::VectorType(_)
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

            if slots.fields.is_empty() {
                let expected_fields = aggregate_leaf_count(value.get_type())
                    .with_context(|| format!("native empty aggregate argument {index} fields"))?;
                if expected_fields != 0 {
                    bail!(
                        "native aggregate argument {index} field count mismatch: signature has 0, operand has {expected_fields}"
                    );
                }
                continue;
            }
            let binding = if matches!(value.get_type(), BasicTypeEnum::VectorType(_)) {
                self.vector_operand(instruction, operand_index)
                    .with_context(|| format!("native vector argument {index}"))?
            } else {
                self.aggregate_operand_or_constant(instruction, operand_index)
                    .with_context(|| format!("native aggregate argument {index}"))?
            };
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

    fn emit_indirect_call_adapter(
        &mut self,
        instruction: InstructionValue<'ctx>,
        call_type: FunctionType<'ctx>,
    ) -> anyhow::Result<FunctionValue<'ctx>> {
        let ctx = self.module.get_context();
        let source_call = CallInst::from(instruction).into_call_site_value();
        let direct_param_types = call_type.get_param_types();
        let mut adapter_param_types = Vec::with_capacity(direct_param_types.len() + 1);
        adapter_param_types.push(ctx.ptr_type(AddressSpace::default()).into());
        adapter_param_types.extend(direct_param_types.iter().copied());
        let adapter_type = match call_type.get_return_type() {
            Some(return_type) => return_type.fn_type(&adapter_param_types, false),
            None => ctx.void_type().fn_type(&adapter_param_types, false),
        };
        let function_name = self.function.get_name().to_str().unwrap_or("anon");
        let adapter_name = translator_private_symbol_name(
            self.emit_markers,
            ".amice.vm.indirect_adapter",
            "ia",
            function_name,
            self.native_calls.len(),
        );
        let adapter = self
            .module
            .add_function(&adapter_name, adapter_type, Some(Linkage::Private));
        adapter.as_global_value().set_unnamed_address(UnnamedAddress::Global);
        copy_indirect_call_attributes_to_adapter(adapter, source_call);

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
        copy_call_site_attributes(call, source_call);
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
        let return_is_aggregate = target.return_is_aggregate;
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

        if return_is_aggregate {
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

    fn lower_hardware_loop_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: HardwareLoopIntrinsicKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 1 {
            bail!("{kind:?} expects exactly one argument");
        }
        let value = instruction_operand_value(instruction, 0).context("hardware loop intrinsic missing counter")?;
        if matches!(value.get_type(), BasicTypeEnum::VectorType(_)) {
            bail!("hardware loop intrinsics only support scalar integer counters");
        }
        if !matches!(value.get_type(), BasicTypeEnum::IntType(_)) {
            bail!("hardware loop intrinsic counter must be an integer");
        }
        let counter = self.materialize_operand(instruction, 0)?;
        checked_intrinsic_integer_width(u64::from(counter.width))?;

        let env = LoweringEnv::new()
            .binding("%counter", counter)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%counter)", counter.width as u64)
            .imm("ne", CmpPredicate::Ne as u64);

        match kind {
            HardwareLoopIntrinsicKind::SetIterations => {
                if !matches!(instruction.get_type(), AnyTypeEnum::VoidType(_)) {
                    bail!("llvm.set.loop.iterations must return void");
                }
                self.execute_lowering_rule(kind.lowering_rule(), env, Some(HandlerSemantic::Nop))?;
            },
            HardwareLoopIntrinsicKind::StartIterations => {
                let result_width =
                    instruction_result_width(instruction)?.context("llvm.start.loop.iterations result has no width")?;
                if result_width != counter.width {
                    bail!(
                        "llvm.start.loop.iterations result width {result_width} differs from counter width {}",
                        counter.width
                    );
                }
                self.execute_lowering_rule(kind.lowering_rule(), env, Some(HandlerSemantic::Mov))?;
            },
            HardwareLoopIntrinsicKind::TestSetIterations => {
                let result_width = instruction_result_width(instruction)?
                    .context("llvm.test.set.loop.iterations result has no width")?;
                if result_width != 1 {
                    bail!("llvm.test.set.loop.iterations result must be i1, got i{result_width}");
                }
                self.execute_lowering_rule(kind.lowering_rule(), env, None)?;
            },
            HardwareLoopIntrinsicKind::TestStartIterations => {
                let AnyTypeEnum::StructType(return_type) = instruction.get_type() else {
                    bail!("llvm.test.start.loop.iterations must return a two-field struct");
                };
                let fields = return_type
                    .get_field_types()
                    .into_iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        return_field_from_type(ty)
                            .with_context(|| format!("test.start.loop.iterations return field {index}"))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                if fields.len() != 2 {
                    bail!("llvm.test.start.loop.iterations must return exactly two fields");
                }
                if fields[0].kind != ScalarKind::Integer || fields[0].width != counter.width {
                    bail!(
                        "llvm.test.start.loop.iterations value field must be i{}, got {:?}{}",
                        counter.width,
                        fields[0].kind,
                        fields[0].width
                    );
                }
                if fields[1].kind != ScalarKind::Integer || fields[1].width != 1 {
                    bail!(
                        "llvm.test.start.loop.iterations flag field must be i1, got {:?}{}",
                        fields[1].kind,
                        fields[1].width
                    );
                }

                let env = self.execute_lowering_rule(kind.lowering_rule(), env, None)?;
                let LoweringValue::Reg(value) = env.get("%vr")? else {
                    bail!("llvm.test.start.loop.iterations lowering must define %vr as the value result register");
                };
                let LoweringValue::Reg(test) = env.get("%vt")? else {
                    bail!("llvm.test.start.loop.iterations lowering must define %vt as the test result register");
                };
                if value.width != counter.width {
                    bail!(
                        "llvm.test.start.loop.iterations value register width {} differs from counter width {}",
                        value.width,
                        counter.width
                    );
                }
                if test.width != 1 {
                    bail!(
                        "llvm.test.start.loop.iterations test register width {} differs from i1",
                        test.width
                    );
                }
                self.insert_aggregate_value(
                    instruction_key(instruction),
                    AggregateBinding {
                        fields: vec![Some(AggregateField::owned(value)), Some(AggregateField::owned(test))],
                    },
                );
            },
        }

        Ok(())
    }

    fn lower_loop_decrement_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 1 {
            bail!("llvm.loop.decrement expects exactly one argument");
        }
        let value = instruction_operand_value(instruction, 0).context("llvm.loop.decrement missing operand 0")?;
        if matches!(value.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            bail!("llvm.loop.decrement only supports scalar integer counters");
        }
        if !matches!(value.get_type(), BasicTypeEnum::IntType(_)) {
            bail!("llvm.loop.decrement counter must be an integer");
        }
        let result_width = instruction_result_width(instruction)?.context("llvm.loop.decrement result has no width")?;
        if result_width != 1 {
            bail!("llvm.loop.decrement result must be i1, got i{result_width}");
        }
        let counter = self.materialize_operand(instruction, 0)?;
        checked_intrinsic_integer_width(u64::from(counter.width))?;

        let env = LoweringEnv::new()
            .binding("%counter", counter)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%counter)", counter.width as u64)
            .imm("ne", CmpPredicate::Ne as u64);
        self.execute_lowering_rule("llvm.loop.decrement.integer", env, None)?;
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
        let value = instruction_operand_value(instruction, 0).context("integer intrinsic missing operand 0")?;
        if matches!(value.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!("integer intrinsic {:?} does not support fixed vector lowering", kind);
            };
            return self.lower_vector_integer_unary_intrinsic(instruction, rule, kind.semantic());
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
        checked_integer_intrinsic_kind_width(kind, u64::from(width))?;

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
            | IntegerIntrinsicKind::LoopDecrementReg
            | IntegerIntrinsicKind::FShl
            | IntegerIntrinsicKind::FShr => bail!("integer intrinsic {:?} does not use a poison flag", kind),
        };
        let poison_flag = constant_int_operand(instruction, 1, &format!("integer intrinsic {flag_name} flag"))?;
        if poison_flag > 1 {
            bail!("integer intrinsic {flag_name} flag must be an i1 constant");
        }
        // `true` 只收窄 LLVM 定义域；被排除输入仍沿用 poison/UB 边界，handler 复用同一套计算。

        let value = instruction_operand_value(instruction, 0).context("integer intrinsic missing operand 0")?;
        if matches!(value.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!("integer intrinsic {:?} does not support fixed vector lowering", kind);
            };
            return self.lower_vector_integer_unary_intrinsic(instruction, rule, kind.semantic());
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
        checked_integer_intrinsic_kind_width(kind, u64::from(width))?;

        let env = LoweringEnv::new()
            .binding("%value", src)
            .binding("%src", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_vector_integer_unary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        rule: &str,
        semantic: HandlerSemantic,
    ) -> anyhow::Result<()> {
        let HandlerSemantic::IntUnary(op) = semantic else {
            bail!("vector integer unary intrinsic requires an int_unary semantic");
        };
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector integer unary intrinsic result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("vector integer unary intrinsic result fields")?;
        let src_value =
            instruction_operand_value(instruction, 0).context("vector integer unary intrinsic missing operand 0")?;
        let src_fields =
            vector_fields_from_type(src_value.get_type()).context("vector integer unary intrinsic source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "vector integer unary intrinsic requires equal lane counts, got source {} and result {}",
                src_fields.len(),
                result_fields.len()
            );
        }

        let src_vector = self.vector_operand(instruction, 0)?;
        if src_vector.fields.len() != src_fields.len() {
            bail!(
                "vector integer unary intrinsic source field count mismatch: type has {}, value has {}",
                src_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if result_info.kind != ScalarKind::Integer || src_info.kind != ScalarKind::Integer {
                bail!(
                    "vector integer unary intrinsic lane {index} requires integer lanes, got result {:?}, source {:?}",
                    result_info.kind,
                    src_info.kind
                );
            }
            if src_info.width != result_info.width {
                bail!(
                    "vector integer unary intrinsic lane {index} width mismatch: result i{}, source i{}",
                    result_info.width,
                    src_info.width
                );
            }
            checked_int_unary_width(op, result_info.width as u64)?;
            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| {
                    format!("vector integer unary intrinsic source lane {index} is undefined or unsupported")
                })?;
            if src_binding.width != src_info.width {
                bail!(
                    "vector integer unary intrinsic lane {index} binding width mismatch: source type i{}, binding i{}",
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%value_lane", src_binding)
                .binding("%src_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::IntUnary(op)))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector integer unary intrinsic lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_integer_binop_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VpIntegerBinopKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 4 {
            bail!("vp integer binop {:?} expects exactly four arguments", kind);
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vp integer binop result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vp integer binop result fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("vp integer binop lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("vp integer binop rhs fields")?;
        if lhs_fields.len() != result_fields.len() || rhs_fields.len() != result_fields.len() {
            bail!(
                "vp integer binop lane count mismatch: result {}, lhs {}, rhs {}",
                result_fields.len(),
                lhs_fields.len(),
                rhs_fields.len()
            );
        }

        let mask_value = instruction_operand_value(instruction, 2).context("vp integer binop missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp integer mask")?;
        let evl = constant_int_operand(instruction, 3, "llvm.vp integer evl")?;
        let lhs = self.vector_seed_from_operand(instruction, 0)?;
        let rhs = self.vector_seed_from_operand(instruction, 1)?;
        if lhs.fields.len() != result_fields.len() || rhs.fields.len() != result_fields.len() {
            bail!(
                "vp integer binop binding lane count mismatch: result {}, lhs {}, rhs {}",
                result_fields.len(),
                lhs.fields.len(),
                rhs.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            if result_info.kind != ScalarKind::Integer
                || lhs_info.kind != ScalarKind::Integer
                || rhs_info.kind != ScalarKind::Integer
            {
                bail!(
                    "vp integer binop lane {index} requires integer lanes, got result {:?}, lhs {:?}, rhs {:?}",
                    result_info.kind,
                    lhs_info.kind,
                    rhs_info.kind
                );
            }
            if lhs_info.width != result_info.width || rhs_info.width != result_info.width {
                bail!(
                    "vp integer binop lane {index} width mismatch: result i{}, lhs i{}, rhs i{}",
                    result_info.width,
                    lhs_info.width,
                    rhs_info.width
                );
            }
            checked_intrinsic_integer_width(result_info.width as u64)?;

            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp integer binop lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp integer binop rhs lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != result_info.width || rhs_binding.width != result_info.width {
                bail!(
                    "vp integer binop lane {index} binding width mismatch: result i{}, lhs i{}, rhs i{}",
                    result_info.width,
                    lhs_binding.width,
                    rhs_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vp integer binop lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_integer_ternary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VpIntegerTernaryKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 5 {
            bail!("vp integer ternary {:?} expects exactly five arguments", kind);
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vp integer ternary result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("vp integer ternary result fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("vp integer ternary lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("vp integer ternary rhs fields")?;
        let third_fields = vector_fields_from_type(instruction_operand_value(instruction, 2)?.get_type())
            .context("vp integer ternary third fields")?;
        if lhs_fields.len() != result_fields.len()
            || rhs_fields.len() != result_fields.len()
            || third_fields.len() != result_fields.len()
        {
            bail!(
                "vp integer ternary lane count mismatch: result {}, lhs {}, rhs {}, third {}",
                result_fields.len(),
                lhs_fields.len(),
                rhs_fields.len(),
                third_fields.len()
            );
        }

        let mask_value = instruction_operand_value(instruction, 3).context("vp integer ternary missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp integer ternary mask")?;
        let evl = constant_int_operand(instruction, 4, "llvm.vp integer ternary evl")?;
        let lhs = self.vector_seed_from_operand(instruction, 0)?;
        let rhs = self.vector_seed_from_operand(instruction, 1)?;
        let third = self.vector_seed_from_operand(instruction, 2)?;
        if lhs.fields.len() != result_fields.len()
            || rhs.fields.len() != result_fields.len()
            || third.fields.len() != result_fields.len()
        {
            bail!(
                "vp integer ternary binding lane count mismatch: result {}, lhs {}, rhs {}, third {}",
                result_fields.len(),
                lhs.fields.len(),
                rhs.fields.len(),
                third.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            let third_info = third_fields[index];
            if result_info.kind != ScalarKind::Integer
                || lhs_info.kind != ScalarKind::Integer
                || rhs_info.kind != ScalarKind::Integer
                || third_info.kind != ScalarKind::Integer
            {
                bail!(
                    "vp integer ternary lane {index} requires integer lanes, got result {:?}, lhs {:?}, rhs {:?}, third {:?}",
                    result_info.kind,
                    lhs_info.kind,
                    rhs_info.kind,
                    third_info.kind
                );
            }
            if lhs_info.width != result_info.width
                || rhs_info.width != result_info.width
                || third_info.width != result_info.width
            {
                bail!(
                    "vp integer ternary lane {index} width mismatch: result i{}, lhs i{}, rhs i{}, third i{}",
                    result_info.width,
                    lhs_info.width,
                    rhs_info.width,
                    third_info.width
                );
            }
            checked_intrinsic_integer_width(result_info.width as u64)?;

            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp integer ternary lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp integer ternary rhs lane {index} is undefined or unsupported"))?;
            let third_binding = third
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp integer ternary third lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != result_info.width
                || rhs_binding.width != result_info.width
                || third_binding.width != result_info.width
            {
                bail!(
                    "vp integer ternary lane {index} binding width mismatch: result i{}, lhs i{}, rhs i{}, third i{}",
                    result_info.width,
                    lhs_binding.width,
                    rhs_binding.width,
                    third_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .binding("%shift_lane", third_binding)
                .binding("%third_lane", third_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vp integer ternary lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_float_binop_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VpFloatBinopKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 4 {
            bail!("vp floating binop {:?} expects exactly four arguments", kind);
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vp floating binop result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vp floating binop result fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("vp floating binop lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("vp floating binop rhs fields")?;
        if lhs_fields.len() != result_fields.len() || rhs_fields.len() != result_fields.len() {
            bail!(
                "vp floating binop lane count mismatch: result {}, lhs {}, rhs {}",
                result_fields.len(),
                lhs_fields.len(),
                rhs_fields.len()
            );
        }

        let mask_value = instruction_operand_value(instruction, 2).context("vp floating binop missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp floating mask")?;
        let evl = constant_int_operand(instruction, 3, "llvm.vp floating evl")?;
        let lhs = self.vector_seed_from_operand(instruction, 0)?;
        let rhs = self.vector_seed_from_operand(instruction, 1)?;
        if lhs.fields.len() != result_fields.len() || rhs.fields.len() != result_fields.len() {
            bail!(
                "vp floating binop binding lane count mismatch: result {}, lhs {}, rhs {}",
                result_fields.len(),
                lhs.fields.len(),
                rhs.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            if result_info.kind != ScalarKind::Float
                || lhs_info.kind != ScalarKind::Float
                || rhs_info.kind != ScalarKind::Float
            {
                bail!(
                    "vp floating binop lane {index} requires float lanes, got result {:?}, lhs {:?}, rhs {:?}",
                    result_info.kind,
                    lhs_info.kind,
                    rhs_info.kind
                );
            }
            if lhs_info.width != result_info.width || rhs_info.width != result_info.width {
                bail!(
                    "vp floating binop lane {index} width mismatch: result f{}, lhs f{}, rhs f{}",
                    result_info.width,
                    lhs_info.width,
                    rhs_info.width
                );
            }
            checked_float_width(result_info.width as u64)?;

            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp floating binop lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp floating binop rhs lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != result_info.width || rhs_binding.width != result_info.width {
                bail!(
                    "vp floating binop lane {index} binding width mismatch: result f{}, lhs f{}, rhs f{}",
                    result_info.width,
                    lhs_binding.width,
                    rhs_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vp floating binop lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_float_unary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VpFloatUnaryKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 3 {
            bail!("vp floating unary {:?} expects exactly three arguments", kind);
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vp floating unary result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vp floating unary result fields")?;
        let src_value = instruction_operand_value(instruction, 0).context("vp floating unary missing source")?;
        let src_fields = vector_fields_from_type(src_value.get_type()).context("vp floating unary source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "vp floating unary lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_fields.len()
            );
        }

        let mask_value = instruction_operand_value(instruction, 1).context("vp floating unary missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp floating unary mask")?;
        let evl = constant_int_operand(instruction, 2, "llvm.vp floating unary evl")?;
        let src_vector = self.vector_seed_from_operand(instruction, 0)?;
        if src_vector.fields.len() != result_fields.len() {
            bail!(
                "vp floating unary binding lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if result_info.kind != ScalarKind::Float || src_info.kind != ScalarKind::Float {
                bail!(
                    "vp floating unary lane {index} requires float lanes, got result {:?}, source {:?}",
                    result_info.kind,
                    src_info.kind
                );
            }
            if src_info.width != result_info.width {
                bail!(
                    "vp floating unary lane {index} width mismatch: result f{}, source f{}",
                    result_info.width,
                    src_info.width
                );
            }
            checked_float_width(result_info.width as u64)?;

            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp floating unary source lane {index} is undefined or unsupported"))?;
            if src_binding.width != src_info.width {
                bail!(
                    "vp floating unary lane {index} binding width mismatch: source type f{}, binding i{}",
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .binding("%value_lane", src_binding)
                .binding("%src_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vp floating unary lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_round_to_int_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VpRoundToIntKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 3 {
            bail!("vp round-to-int {:?} expects exactly three arguments", kind);
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vp round-to-int result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vp round-to-int result fields")?;
        let src_value = instruction_operand_value(instruction, 0).context("vp round-to-int missing source")?;
        let src_fields = vector_fields_from_type(src_value.get_type()).context("vp round-to-int source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "vp round-to-int lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_fields.len()
            );
        }

        let mask_value = instruction_operand_value(instruction, 1).context("vp round-to-int missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp round-to-int mask")?;
        let evl = constant_int_operand(instruction, 2, "llvm.vp round-to-int evl")?;
        let src_vector = self.vector_seed_from_operand(instruction, 0)?;
        if src_vector.fields.len() != result_fields.len() {
            bail!(
                "vp round-to-int binding lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if src_info.kind != ScalarKind::Float || result_info.kind != ScalarKind::Integer {
                bail!(
                    "vp round-to-int lane {index} requires float -> integer, got source {:?} and result {:?}",
                    src_info.kind,
                    result_info.kind
                );
            }
            checked_float_width(src_info.width as u64)?;
            checked_round_to_int_result_width(result_info.width as u64)?;

            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp round-to-int source lane {index} is undefined or unsupported"))?;
            if src_binding.width != src_info.width {
                bail!(
                    "vp round-to-int lane {index} binding width mismatch: source type f{}, binding i{}",
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", src_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vp round-to-int lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_float_ternary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VpFloatTernaryKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 5 {
            bail!("vp floating ternary {:?} expects exactly five arguments", kind);
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vp floating ternary result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("vp floating ternary result fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("vp floating ternary lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("vp floating ternary rhs fields")?;
        let third_fields = vector_fields_from_type(instruction_operand_value(instruction, 2)?.get_type())
            .context("vp floating ternary third fields")?;
        if lhs_fields.len() != result_fields.len()
            || rhs_fields.len() != result_fields.len()
            || third_fields.len() != result_fields.len()
        {
            bail!(
                "vp floating ternary lane count mismatch: result {}, lhs {}, rhs {}, third {}",
                result_fields.len(),
                lhs_fields.len(),
                rhs_fields.len(),
                third_fields.len()
            );
        }

        let mask_value = instruction_operand_value(instruction, 3).context("vp floating ternary missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp floating ternary mask")?;
        let evl = constant_int_operand(instruction, 4, "llvm.vp floating ternary evl")?;
        let lhs = self.vector_seed_from_operand(instruction, 0)?;
        let rhs = self.vector_seed_from_operand(instruction, 1)?;
        let third = self.vector_seed_from_operand(instruction, 2)?;
        if lhs.fields.len() != result_fields.len()
            || rhs.fields.len() != result_fields.len()
            || third.fields.len() != result_fields.len()
        {
            bail!(
                "vp floating ternary binding lane count mismatch: result {}, lhs {}, rhs {}, third {}",
                result_fields.len(),
                lhs.fields.len(),
                rhs.fields.len(),
                third.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            let third_info = third_fields[index];
            if result_info.kind != ScalarKind::Float
                || lhs_info.kind != ScalarKind::Float
                || rhs_info.kind != ScalarKind::Float
                || third_info.kind != ScalarKind::Float
            {
                bail!(
                    "vp floating ternary lane {index} requires float lanes, got result {:?}, lhs {:?}, rhs {:?}, third {:?}",
                    result_info.kind,
                    lhs_info.kind,
                    rhs_info.kind,
                    third_info.kind
                );
            }
            if lhs_info.width != result_info.width
                || rhs_info.width != result_info.width
                || third_info.width != result_info.width
            {
                bail!(
                    "vp floating ternary lane {index} width mismatch: result f{}, lhs f{}, rhs f{}, third f{}",
                    result_info.width,
                    lhs_info.width,
                    rhs_info.width,
                    third_info.width
                );
            }
            checked_float_width(result_info.width as u64)?;

            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp floating ternary lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp floating ternary rhs lane {index} is undefined or unsupported"))?;
            let third_binding = third
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp floating ternary third lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != result_info.width
                || rhs_binding.width != result_info.width
                || third_binding.width != result_info.width
            {
                bail!(
                    "vp floating ternary lane {index} binding width mismatch: result f{}, lhs f{}, rhs f{}, third f{}",
                    result_info.width,
                    lhs_binding.width,
                    rhs_binding.width,
                    third_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .binding("%c_lane", third_binding)
                .binding("%third_lane", third_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vp floating ternary lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_fcmp_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 5 {
            bail!("llvm.vp.fcmp expects exactly five arguments");
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("llvm.vp.fcmp result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("llvm.vp.fcmp result fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("llvm.vp.fcmp lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("llvm.vp.fcmp rhs fields")?;
        if lhs_fields.len() != result_fields.len() || rhs_fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.fcmp lane count mismatch: result {}, lhs {}, rhs {}",
                result_fields.len(),
                lhs_fields.len(),
                rhs_fields.len()
            );
        }

        let predicate_name = metadata_string_operand(instruction, 2, "llvm.vp.fcmp predicate")?;
        let predicate = float_predicate_from_metadata_name(&predicate_name)?;
        let mask_value = instruction_operand_value(instruction, 3).context("llvm.vp.fcmp missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp.fcmp mask")?;
        let evl = constant_int_operand(instruction, 4, "llvm.vp.fcmp evl")?;
        let lhs = self.vector_seed_from_operand(instruction, 0)?;
        let rhs = self.vector_seed_from_operand(instruction, 1)?;
        if lhs.fields.len() != result_fields.len() || rhs.fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.fcmp binding lane count mismatch: result {}, lhs {}, rhs {}",
                result_fields.len(),
                lhs.fields.len(),
                rhs.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            if result_info.kind != ScalarKind::Integer || result_info.width != 1 {
                bail!(
                    "llvm.vp.fcmp lane {index} result must be i1, got {:?} i{}",
                    result_info.kind,
                    result_info.width
                );
            }
            if lhs_info.kind != ScalarKind::Float || rhs_info.kind != ScalarKind::Float {
                bail!(
                    "llvm.vp.fcmp lane {index} requires floating lanes, got lhs {:?}, rhs {:?}",
                    lhs_info.kind,
                    rhs_info.kind
                );
            }
            if lhs_info.width != rhs_info.width {
                bail!(
                    "llvm.vp.fcmp lane {index} operand width mismatch: lhs f{}, rhs f{}",
                    lhs_info.width,
                    rhs_info.width
                );
            }
            checked_float_width(lhs_info.width as u64)?;

            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("llvm.vp.fcmp lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("llvm.vp.fcmp rhs lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != lhs_info.width || rhs_binding.width != rhs_info.width {
                bail!(
                    "llvm.vp.fcmp lane {index} binding width mismatch: lhs type f{}, lhs binding i{}, rhs type f{}, rhs binding i{}",
                    lhs_info.width,
                    lhs_binding.width,
                    rhs_info.width,
                    rhs_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .imm("lane(%r)", index as u64)
                .imm("predicate(%r)", predicate as u64)
                .imm("operand_width(%a,%b)", lhs_info.width as u64);
            let env = self.execute_lowering_rule("llvm.vp.vector.fcmp.float", env, Some(HandlerSemantic::Fcmp))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("llvm.vp.fcmp lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_icmp_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 5 {
            bail!("llvm.vp.icmp expects exactly five arguments");
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("llvm.vp.icmp result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("llvm.vp.icmp result fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("llvm.vp.icmp lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("llvm.vp.icmp rhs fields")?;
        if lhs_fields.len() != result_fields.len() || rhs_fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.icmp lane count mismatch: result {}, lhs {}, rhs {}",
                result_fields.len(),
                lhs_fields.len(),
                rhs_fields.len()
            );
        }

        let predicate_name = metadata_string_operand(instruction, 2, "llvm.vp.icmp predicate")?;
        let predicate = predicate_from_metadata_name(&predicate_name)?;
        let mask_value = instruction_operand_value(instruction, 3).context("llvm.vp.icmp missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp.icmp mask")?;
        let evl = constant_int_operand(instruction, 4, "llvm.vp.icmp evl")?;
        let lhs = self.vector_seed_from_operand(instruction, 0)?;
        let rhs = self.vector_seed_from_operand(instruction, 1)?;
        if lhs.fields.len() != result_fields.len() || rhs.fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.icmp binding lane count mismatch: result {}, lhs {}, rhs {}",
                result_fields.len(),
                lhs.fields.len(),
                rhs.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            if result_info.kind != ScalarKind::Integer || result_info.width != 1 {
                bail!(
                    "llvm.vp.icmp lane {index} result must be i1, got {:?} i{}",
                    result_info.kind,
                    result_info.width
                );
            }
            let rule = match (lhs_info.kind, rhs_info.kind) {
                (ScalarKind::Integer, ScalarKind::Integer) => "llvm.vp.vector.icmp.integer",
                (ScalarKind::Pointer, ScalarKind::Pointer) => "llvm.vp.vector.icmp.pointer",
                (lhs_kind, rhs_kind) => {
                    bail!(
                        "llvm.vp.icmp lane {index} requires matching integer or pointer lanes, got lhs {lhs_kind:?}, rhs {rhs_kind:?}"
                    )
                },
            };
            if lhs_info.width != rhs_info.width {
                bail!(
                    "llvm.vp.icmp lane {index} operand width mismatch: lhs {}{}, rhs {}{}",
                    scalar_kind_prefix(lhs_info.kind),
                    lhs_info.width,
                    scalar_kind_prefix(rhs_info.kind),
                    rhs_info.width
                );
            }

            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("llvm.vp.icmp lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("llvm.vp.icmp rhs lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != lhs_info.width || rhs_binding.width != rhs_info.width {
                bail!(
                    "llvm.vp.icmp lane {index} binding width mismatch: lhs type {}{}, lhs binding i{}, rhs type {}{}, rhs binding i{}",
                    scalar_kind_prefix(lhs_info.kind),
                    lhs_info.width,
                    lhs_binding.width,
                    scalar_kind_prefix(rhs_info.kind),
                    rhs_info.width,
                    rhs_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .imm("lane(%r)", index as u64)
                .imm("predicate(%r)", predicate as u64)
                .imm("operand_width(%a,%b)", lhs_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::Icmp))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("llvm.vp.icmp lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_integer_unary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VpIntegerUnaryKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != kind.arity() {
            bail!("vp integer unary {:?} expects exactly {} arguments", kind, kind.arity());
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vp integer unary result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vp integer unary result fields")?;
        let src_value = instruction_operand_value(instruction, 0).context("vp integer unary missing source")?;
        let src_fields = vector_fields_from_type(src_value.get_type()).context("vp integer unary source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "vp integer unary lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_fields.len()
            );
        }

        if let Some(flag_name) = kind.poison_flag_name() {
            let poison_flag = constant_int_operand(instruction, 1, &format!("llvm.vp integer unary {flag_name} flag"))?;
            if poison_flag > 1 {
                bail!("llvm.vp integer unary {flag_name} flag must be an i1 constant");
            }
            // `true` 只收窄 LLVM 定义域；被排除输入仍按 poison/UB 边界处理，VM handler 复用同一套逐 lane 计算。
        }

        let mask_value = instruction_operand_value(instruction, kind.mask_operand_index())
            .context("vp integer unary missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp integer unary mask")?;
        let evl = constant_int_operand(instruction, kind.evl_operand_index(), "llvm.vp integer unary evl")?;
        let src_vector = self.vector_seed_from_operand(instruction, 0)?;
        if src_vector.fields.len() != result_fields.len() {
            bail!(
                "vp integer unary binding lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if result_info.kind != ScalarKind::Integer || src_info.kind != ScalarKind::Integer {
                bail!(
                    "vp integer unary lane {index} requires integer lanes, got result {:?}, source {:?}",
                    result_info.kind,
                    src_info.kind
                );
            }
            if src_info.width != result_info.width {
                bail!(
                    "vp integer unary lane {index} width mismatch: result i{}, source i{}",
                    result_info.width,
                    src_info.width
                );
            }
            checked_int_unary_width(kind.int_op(), result_info.width as u64)?;

            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp integer unary source lane {index} is undefined or unsupported"))?;
            if src_binding.width != src_info.width {
                bail!(
                    "vp integer unary lane {index} binding width mismatch: source type i{}, binding i{}",
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%value_lane", src_binding)
                .binding("%src_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vp integer unary lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
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
        let lhs_value = instruction_operand_value(instruction, 0).context("integer intrinsic missing operand 0")?;
        let rhs_value = instruction_operand_value(instruction, 1).context("integer intrinsic missing operand 1")?;
        if matches!(lhs_value.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(rhs_value.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!("integer intrinsic {:?} does not support fixed vector lowering", kind);
            };
            return self.lower_vector_integer_binop(instruction, rule, Some(kind.semantic()));
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
        let lhs_value = instruction_operand_value(instruction, 0).context("integer overflow intrinsic missing lhs")?;
        let rhs_value = instruction_operand_value(instruction, 1).context("integer overflow intrinsic missing rhs")?;
        if matches!(lhs_value.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(rhs_value.get_type(), BasicTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!(
                    "integer overflow intrinsic {:?} does not support fixed vector lowering",
                    kind
                );
            };
            return self.lower_vector_integer_overflow_intrinsic(instruction, rule, kind);
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

    fn lower_vector_integer_overflow_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        rule: &str,
        kind: IntegerIntrinsicKind,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::StructType(return_type) = instruction.get_type() else {
            bail!("vector integer overflow intrinsic must return a two-field struct");
        };
        if return_type.count_fields() != 2 {
            bail!("vector integer overflow intrinsic must return exactly two fields");
        }
        let value_type = return_type
            .get_field_type_at_index(0)
            .context("vector overflow value result field is unavailable")?;
        let flag_type = return_type
            .get_field_type_at_index(1)
            .context("vector overflow flag result field is unavailable")?;
        let value_fields = vector_fields_from_type(value_type).context("vector overflow value result fields")?;
        let flag_fields = vector_fields_from_type(flag_type).context("vector overflow flag result fields")?;
        if value_fields.len() != flag_fields.len() {
            bail!(
                "vector integer overflow lane count mismatch: value {}, flag {}",
                value_fields.len(),
                flag_fields.len()
            );
        }

        let lhs_value = instruction_operand_value(instruction, 0).context("vector overflow missing lhs operand")?;
        let rhs_value = instruction_operand_value(instruction, 1).context("vector overflow missing rhs operand")?;
        let lhs_fields = vector_fields_from_type(lhs_value.get_type()).context("vector overflow lhs fields")?;
        let rhs_fields = vector_fields_from_type(rhs_value.get_type()).context("vector overflow rhs fields")?;
        if lhs_fields.len() != value_fields.len() || rhs_fields.len() != value_fields.len() {
            bail!(
                "vector integer overflow operand lane count mismatch: result {}, lhs {}, rhs {}",
                value_fields.len(),
                lhs_fields.len(),
                rhs_fields.len()
            );
        }

        let lhs = self.vector_operand(instruction, 0)?;
        let rhs = self.vector_operand(instruction, 1)?;
        if lhs.fields.len() != value_fields.len() || rhs.fields.len() != value_fields.len() {
            bail!(
                "vector integer overflow binding lane count mismatch: result {}, lhs {}, rhs {}",
                value_fields.len(),
                lhs.fields.len(),
                rhs.fields.len()
            );
        }

        let mut value_results = Vec::with_capacity(value_fields.len());
        let mut flag_results = Vec::with_capacity(flag_fields.len());
        for (index, (value_info, flag_info)) in value_fields
            .iter()
            .copied()
            .zip(flag_fields.iter().copied())
            .enumerate()
        {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            if value_info.kind != ScalarKind::Integer
                || flag_info.kind != ScalarKind::Integer
                || lhs_info.kind != ScalarKind::Integer
                || rhs_info.kind != ScalarKind::Integer
            {
                bail!(
                    "vector integer overflow lane {index} requires integer lanes, got value {:?}, flag {:?}, lhs {:?}, rhs {:?}",
                    value_info.kind,
                    flag_info.kind,
                    lhs_info.kind,
                    rhs_info.kind
                );
            }
            if flag_info.width != 1 {
                bail!(
                    "vector integer overflow flag lane {index} must be i1, got i{}",
                    flag_info.width
                );
            }
            if lhs_info.width != value_info.width || rhs_info.width != value_info.width {
                bail!(
                    "vector integer overflow lane {index} width mismatch: value i{}, lhs i{}, rhs i{}",
                    value_info.width,
                    lhs_info.width,
                    rhs_info.width
                );
            }
            checked_intrinsic_integer_width(value_info.width as u64)?;

            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector integer overflow lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector integer overflow rhs lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != value_info.width || rhs_binding.width != value_info.width {
                bail!(
                    "vector integer overflow lane {index} binding width mismatch: value i{}, lhs i{}, rhs i{}",
                    value_info.width,
                    lhs_binding.width,
                    rhs_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", value_info.width as u64)
                .imm("type_width(%r)", value_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(kind.semantic()))?;
            let value = match env.get("%vr")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector integer overflow lowering must define %vr as a lane register")
                },
            };
            let overflow = match env.get("%vo")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector integer overflow lowering must define %vo as an overflow lane register")
                },
            };
            if value.width != value_info.width {
                bail!(
                    "vector integer overflow lane {index} value width mismatch: type i{}, register i{}",
                    value_info.width,
                    value.width
                );
            }
            if overflow.width != 1 {
                bail!(
                    "vector integer overflow lane {index} flag width mismatch: expected i1, register i{}",
                    overflow.width
                );
            }
            value_results.push(Some(AggregateField::owned(value)));
            flag_results.push(Some(AggregateField::owned(overflow)));
        }

        let mut fields = value_results;
        fields.extend(flag_results);
        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_select_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 4 {
            bail!("llvm.vp.select expects exactly four arguments");
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("llvm.vp.select result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("llvm.vp.select result fields")?;
        let then_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("llvm.vp.select then fields")?;
        let else_fields = vector_fields_from_type(instruction_operand_value(instruction, 2)?.get_type())
            .context("llvm.vp.select else fields")?;
        if then_fields.len() != result_fields.len() || else_fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.select lane count mismatch: result {}, then {}, else {}",
                result_fields.len(),
                then_fields.len(),
                else_fields.len()
            );
        }

        let cond_value = instruction_operand_value(instruction, 0).context("llvm.vp.select missing condition")?;
        if !matches!(cond_value.get_type(), BasicTypeEnum::VectorType(_)) {
            bail!("llvm.vp.select currently requires a fixed <N x i1> condition");
        }
        let cond_fields = vector_fields_from_type(cond_value.get_type()).context("llvm.vp.select condition fields")?;
        if cond_fields.len() != result_fields.len() || cond_fields.iter().any(|field| field.width != 1) {
            bail!(
                "llvm.vp.select vector condition must be <N x i1> matching result lanes: cond {}, result {}",
                cond_fields.len(),
                result_fields.len()
            );
        }
        let cond_vector = self
            .vector_seed_from_operand(instruction, 0)
            .context("llvm.vp.select vector condition operand")?;

        let evl = constant_int_operand(instruction, 3, "llvm.vp.select evl")?;
        let then_vector = self
            .vector_seed_from_operand(instruction, 1)
            .context("llvm.vp.select then vector operand")?;
        let else_vector = self
            .vector_seed_from_operand(instruction, 2)
            .context("llvm.vp.select else vector operand")?;
        if then_vector.fields.len() != result_fields.len() || else_vector.fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.select binding lane count mismatch: result {}, then {}, else {}",
                result_fields.len(),
                then_vector.fields.len(),
                else_vector.fields.len()
            );
        }
        if cond_vector.fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.select condition binding lane count mismatch: result {}, cond {}",
                result_fields.len(),
                cond_vector.fields.len()
            );
        }

        let actions = self.select_lowering_actions("llvm.vp.select.vector_condition", "type_width(%field)")?;
        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, info) in result_fields.iter().copied().enumerate() {
            let then_info = then_fields[index];
            let else_info = else_fields[index];
            if then_info != info || else_info != info {
                bail!(
                    "llvm.vp.select lane {index} type mismatch: result {:?} i{}, then {:?} i{}, else {:?} i{}",
                    info.kind,
                    info.width,
                    then_info.kind,
                    then_info.width,
                    else_info.kind,
                    else_info.width
                );
            }
            if index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let cond_field = cond_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.vp.select condition lane {index} is undefined"))?;
            if cond_field.binding.width != 1 {
                bail!(
                    "llvm.vp.select condition lane {index} must be i1, got i{}",
                    cond_field.binding.width
                );
            }
            let then_field = then_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.vp.select then lane {index} is undefined or unsupported"))?;
            let else_field = else_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.vp.select else lane {index} is undefined or unsupported"))?;
            if then_field.binding.width != info.width || else_field.binding.width != info.width {
                bail!(
                    "llvm.vp.select lane {index} binding width mismatch: result i{}, then i{}, else i{}",
                    info.width,
                    then_field.binding.width,
                    else_field.binding.width
                );
            }

            let dst = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: info.width,
            };
            let then_label = self.builder.new_label();
            let else_label = self.builder.new_label();
            let join_label = self.builder.new_label();
            let branch_env = LoweringEnv::new()
                .binding("%vc", cond_field.binding)
                .label("then_label", then_label)
                .label("else_label", else_label);
            self.emit_profile_action(&actions.br_if, &branch_env)?;

            self.builder.bind_label(then_label);
            let then_env = LoweringEnv::new()
                .binding("%vr", dst)
                .binding("%vt", then_field.binding)
                .imm("type_width(%field)", info.width as u64)
                .label("join_label", join_label);
            self.emit_profile_action(&actions.then_mov, &then_env)?;
            self.emit_profile_action(&actions.br, &then_env)?;

            self.builder.bind_label(else_label);
            let else_env = LoweringEnv::new()
                .binding("%vr", dst)
                .binding("%ve", else_field.binding)
                .imm("type_width(%field)", info.width as u64)
                .label("join_label", join_label);
            self.emit_profile_action(&actions.else_mov, &else_env)?;
            self.emit_profile_action(&actions.br, &else_env)?;

            self.builder.bind_label(join_label);
            fields.push(Some(AggregateField::owned(dst)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_merge_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 4 {
            bail!("llvm.vp.merge expects exactly four arguments");
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("llvm.vp.merge result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("llvm.vp.merge result fields")?;
        let then_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("llvm.vp.merge then fields")?;
        let else_fields = vector_fields_from_type(instruction_operand_value(instruction, 2)?.get_type())
            .context("llvm.vp.merge else fields")?;
        if then_fields.len() != result_fields.len() || else_fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.merge lane count mismatch: result {}, then {}, else {}",
                result_fields.len(),
                then_fields.len(),
                else_fields.len()
            );
        }

        let cond_value = instruction_operand_value(instruction, 0).context("llvm.vp.merge missing condition")?;
        if !matches!(cond_value.get_type(), BasicTypeEnum::VectorType(_)) {
            bail!("llvm.vp.merge currently requires a fixed <N x i1> condition");
        }
        let cond_fields = vector_fields_from_type(cond_value.get_type()).context("llvm.vp.merge condition fields")?;
        if cond_fields.len() != result_fields.len() || cond_fields.iter().any(|field| field.width != 1) {
            bail!(
                "llvm.vp.merge vector condition must be <N x i1> matching result lanes: cond {}, result {}",
                cond_fields.len(),
                result_fields.len()
            );
        }
        let cond_vector = self
            .vector_seed_from_operand(instruction, 0)
            .context("llvm.vp.merge vector condition operand")?;

        let pivot_value = instruction_operand_value(instruction, 3).context("llvm.vp.merge missing pivot")?;
        if !pivot_value.is_int_value() {
            bail!("llvm.vp.merge pivot must be an integer");
        }
        let pivot_width = checked_intrinsic_integer_width(value_width(pivot_value)? as u64)?;
        let pivot = self
            .materialize_value(pivot_value)
            .context("llvm.vp.merge pivot materialization")?;
        if pivot.width != pivot_width {
            bail!(
                "llvm.vp.merge pivot binding width mismatch: type i{}, register i{}",
                pivot_width,
                pivot.width
            );
        }

        let then_vector = self
            .vector_seed_from_operand(instruction, 1)
            .context("llvm.vp.merge then vector operand")?;
        let else_vector = self
            .vector_seed_from_operand(instruction, 2)
            .context("llvm.vp.merge else vector operand")?;
        if then_vector.fields.len() != result_fields.len() || else_vector.fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.merge binding lane count mismatch: result {}, then {}, else {}",
                result_fields.len(),
                then_vector.fields.len(),
                else_vector.fields.len()
            );
        }
        if cond_vector.fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.merge condition binding lane count mismatch: result {}, cond {}",
                result_fields.len(),
                cond_vector.fields.len()
            );
        }

        let actions = self.vp_merge_lowering_actions()?;
        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, info) in result_fields.iter().copied().enumerate() {
            let then_info = then_fields[index];
            let else_info = else_fields[index];
            if then_info != info || else_info != info {
                bail!(
                    "llvm.vp.merge lane {index} type mismatch: result {:?} i{}, then {:?} i{}, else {:?} i{}",
                    info.kind,
                    info.width,
                    then_info.kind,
                    then_info.width,
                    else_info.kind,
                    else_info.width
                );
            }

            let cond_field = cond_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.vp.merge condition lane {index} is undefined"))?;
            if cond_field.binding.width != 1 {
                bail!(
                    "llvm.vp.merge condition lane {index} must be i1, got i{}",
                    cond_field.binding.width
                );
            }
            let then_field = then_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.vp.merge then lane {index} is undefined or unsupported"))?;
            let else_field = else_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("llvm.vp.merge else lane {index} is undefined or unsupported"))?;
            if then_field.binding.width != info.width || else_field.binding.width != info.width {
                bail!(
                    "llvm.vp.merge lane {index} binding width mismatch: result i{}, then i{}, else i{}",
                    info.width,
                    then_field.binding.width,
                    else_field.binding.width
                );
            }

            let lane_key = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: pivot_width,
            };
            let pivot_match = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: 1,
            };
            let dst = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: info.width,
            };
            let pivot_label = self.builder.new_label();
            let then_label = self.builder.new_label();
            let else_label = self.builder.new_label();
            let join_label = self.builder.new_label();
            let env = LoweringEnv::new()
                .binding("%vc", cond_field.binding)
                .binding("%vp", pivot)
                .binding("%vk", lane_key)
                .binding("%vm", pivot_match)
                .binding("%vt", then_field.binding)
                .binding("%ve", else_field.binding)
                .binding("%vr", dst)
                .imm("lane(%r)", mask_integer_to_width(index as u64, pivot_width))
                .imm("type_width(%pivot)", pivot_width as u64)
                .imm("type_width(%field)", info.width as u64)
                .imm("ult", CmpPredicate::Ult as u64)
                .label("pivot_label", pivot_label)
                .label("then_label", then_label)
                .label("else_label", else_label)
                .label("join_label", join_label);

            self.emit_profile_action(&actions.lane_mov, &env)?;
            self.emit_profile_action(&actions.pivot_icmp, &env)?;
            self.emit_profile_action(&actions.cond_br_if, &env)?;

            self.builder.bind_label(pivot_label);
            self.emit_profile_action(&actions.pivot_br_if, &env)?;
            self.builder.release_vreg(lane_key.reg);
            self.builder.release_vreg(pivot_match.reg);

            self.builder.bind_label(then_label);
            self.emit_profile_action(&actions.then_mov, &env)?;
            self.emit_profile_action(&actions.br, &env)?;

            self.builder.bind_label(else_label);
            self.emit_profile_action(&actions.else_mov, &env)?;

            self.builder.bind_label(join_label);
            fields.push(Some(AggregateField::owned(dst)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_experimental_vp_splice_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 6 {
            bail!("llvm.experimental.vp.splice expects exactly six arguments, got {actual_args}");
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("llvm.experimental.vp.splice result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("llvm.experimental.vp.splice result fields")?;
        let lane_count = result_fields.len();
        if lane_count == 0 {
            bail!("zero-lane llvm.experimental.vp.splice is not supported by vm_virtualize");
        }

        let first_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("llvm.experimental.vp.splice lhs fields")?;
        let second_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("llvm.experimental.vp.splice rhs fields")?;
        if first_fields.len() != lane_count || second_fields.len() != lane_count {
            bail!(
                "llvm.experimental.vp.splice operand lane count mismatch: lhs {}, rhs {}, result {}",
                first_fields.len(),
                second_fields.len(),
                lane_count
            );
        }

        let imm_value = instruction_operand_value(instruction, 2)?;
        if value_width(imm_value)? != 32 {
            bail!("llvm.experimental.vp.splice immarg must be i32");
        }
        let imm = signed_constant_int_operand(instruction, 2, "llvm.experimental.vp.splice immarg")?;
        let mask_value =
            instruction_operand_value(instruction, 3).context("llvm.experimental.vp.splice missing mask")?;
        let lane_mask = constant_i1_vector_mask(mask_value, lane_count, "llvm.experimental.vp.splice mask")?;
        let evl1 = constant_int_operand(instruction, 4, "llvm.experimental.vp.splice evl1")?;
        let evl2 = constant_int_operand(instruction, 5, "llvm.experimental.vp.splice evl2")?;
        let evl1 = usize::try_from(evl1).context("llvm.experimental.vp.splice evl1 does not fit usize")?;
        let evl2 = usize::try_from(evl2).context("llvm.experimental.vp.splice evl2 does not fit usize")?;
        if evl1 > lane_count || evl2 > lane_count {
            bail!("llvm.experimental.vp.splice EVL values must be <= VL {lane_count}, got evl1 {evl1}, evl2 {evl2}");
        }
        let evl1_i64 = i64::try_from(evl1).context("llvm.experimental.vp.splice evl1 overflow")?;
        if !(-evl1_i64..evl1_i64).contains(&imm) {
            bail!("llvm.experimental.vp.splice immarg {imm} violates -evl1 <= imm < evl1 for evl1 {evl1}");
        }

        let mask = experimental_vp_splice_mask(lane_count, imm, evl1, evl2, &lane_mask)?;
        let lhs = self.vector_seed_from_operand(instruction, 0)?;
        let rhs = self.vector_seed_from_operand(instruction, 1)?;
        self.lower_vector_lane_permutation(
            instruction_key(instruction),
            result_fields,
            vec![(lhs, first_fields), (rhs, second_fields)],
            mask,
            "llvm.experimental.vp.splice.element",
            "llvm.experimental.vp.splice",
        )
    }

    fn lower_experimental_vp_reverse_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 3 {
            bail!("llvm.experimental.vp.reverse expects exactly three arguments");
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("llvm.experimental.vp.reverse result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("llvm.experimental.vp.reverse result fields")?;
        let lane_count = result_fields.len();
        let source_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("llvm.experimental.vp.reverse source fields")?;
        if source_fields.len() != lane_count {
            bail!(
                "llvm.experimental.vp.reverse source/result lane count mismatch: source {}, result {lane_count}",
                source_fields.len()
            );
        }
        let mask_value =
            instruction_operand_value(instruction, 1).context("llvm.experimental.vp.reverse missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, lane_count, "llvm.experimental.vp.reverse mask")?;
        let evl = constant_int_operand(instruction, 2, "llvm.experimental.vp.reverse evl")?;
        let source = self
            .vector_seed_from_operand(instruction, 0)
            .context("llvm.experimental.vp.reverse source vector")?;
        if source.fields.len() != lane_count {
            bail!(
                "llvm.experimental.vp.reverse binding lane count mismatch: source {}, result {lane_count}",
                source.fields.len()
            );
        }

        let lane_mask = (0..lane_count)
            .map(|index| (mask[index] && (index as u64) < evl).then_some(lane_count - 1 - index))
            .collect();
        self.lower_vector_lane_permutation(
            instruction_key(instruction),
            result_fields,
            vec![(source, source_fields)],
            lane_mask,
            "llvm.experimental.vp.reverse.element",
            "llvm.experimental.vp.reverse",
        )
    }

    fn lower_experimental_vp_splat_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 3 {
            bail!("llvm.experimental.vp.splat expects exactly three arguments");
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("llvm.experimental.vp.splat result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("llvm.experimental.vp.splat result fields")?;
        let lane_count = result_fields.len();
        let scalar_value =
            instruction_operand_value(instruction, 0).context("llvm.experimental.vp.splat missing scalar")?;
        let scalar_field =
            return_field_from_type(scalar_value.get_type()).context("llvm.experimental.vp.splat scalar field")?;
        if result_fields.iter().any(|field| *field != scalar_field) {
            bail!(
                "llvm.experimental.vp.splat scalar lane type mismatch: scalar {:?} i{}",
                scalar_field.kind,
                scalar_field.width
            );
        }
        let mask_value =
            instruction_operand_value(instruction, 1).context("llvm.experimental.vp.splat missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, lane_count, "llvm.experimental.vp.splat mask")?;
        let evl = constant_int_operand(instruction, 2, "llvm.experimental.vp.splat evl")?;
        let scalar = self
            .materialize_value(scalar_value)
            .context("llvm.experimental.vp.splat scalar materialization")?;
        if scalar.width != scalar_field.width {
            bail!(
                "llvm.experimental.vp.splat scalar width mismatch: value i{}, type i{}",
                scalar.width,
                scalar_field.width
            );
        }

        let mut fields = Vec::with_capacity(lane_count);
        for (index, field) in result_fields.iter().copied().enumerate() {
            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }
            let env = LoweringEnv::new()
                .binding("%value", scalar)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", field.width as u64)
                .imm("type_width(%r)", field.width as u64);
            let env =
                self.execute_lowering_rule("llvm.experimental.vp.splat.element", env, Some(HandlerSemantic::Mov))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("llvm.experimental.vp.splat lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_pointer_cast_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VpPointerCastKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 3 {
            bail!("VP pointer cast {:?} expects exactly three arguments", kind);
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("VP pointer cast result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("VP pointer cast result fields")?;
        let src_value = instruction_operand_value(instruction, 0).context("VP pointer cast missing source")?;
        self.ensure_no_non_integral_pointer_type_ref(src_value.get_type().as_type_ref(), "VP pointer cast source")?;
        self.ensure_no_non_integral_pointer_type_ref(result_ty.as_type_ref(), "VP pointer cast result")?;
        let src_fields = vector_fields_from_type(src_value.get_type()).context("VP pointer cast source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "VP pointer cast lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_fields.len()
            );
        }

        let mask_value = instruction_operand_value(instruction, 1).context("VP pointer cast missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp pointer cast mask")?;
        let evl = constant_int_operand(instruction, 2, "llvm.vp pointer cast evl")?;
        let src_vector = self
            .vector_seed_from_operand(instruction, 0)
            .context("VP pointer cast source vector operand")?;
        if src_vector.fields.len() != result_fields.len() {
            bail!(
                "VP pointer cast binding lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            let semantic = kind.semantic_for_lane(index, src_info, result_info)?;
            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("VP pointer cast source lane {index} is undefined or unsupported"))?;
            if src_binding.width != src_info.width {
                bail!(
                    "VP pointer cast lane {index} binding width mismatch: source type i{}, binding i{}",
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", src_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule("llvm.vp.vector.cast.pointer", env, Some(semantic))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("VP pointer cast lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_integer_cast_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VpIntegerCastKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 3 {
            bail!("VP integer cast {:?} expects exactly three arguments", kind);
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("VP integer cast result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("VP integer cast result fields")?;
        let src_value = instruction_operand_value(instruction, 0).context("VP integer cast missing source")?;
        let src_fields = vector_fields_from_type(src_value.get_type()).context("VP integer cast source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "VP integer cast lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_fields.len()
            );
        }

        let mask_value = instruction_operand_value(instruction, 1).context("VP integer cast missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp integer cast mask")?;
        let evl = constant_int_operand(instruction, 2, "llvm.vp integer cast evl")?;
        let src_vector = self
            .vector_seed_from_operand(instruction, 0)
            .context("VP integer cast source vector operand")?;
        if src_vector.fields.len() != result_fields.len() {
            bail!(
                "VP integer cast binding lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if src_info.kind != ScalarKind::Integer || result_info.kind != ScalarKind::Integer {
                bail!(
                    "VP integer cast lane {index} requires integer lanes, got source {:?} and result {:?}",
                    src_info.kind,
                    result_info.kind
                );
            }
            if !kind.width_transition_is_valid(src_info.width, result_info.width) {
                bail!(
                    "VP integer cast lane {index} has invalid width transition i{} -> i{} for {:?}",
                    src_info.width,
                    result_info.width,
                    kind
                );
            }
            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("VP integer cast source lane {index} is undefined or unsupported"))?;
            if src_binding.width != src_info.width {
                bail!(
                    "VP integer cast lane {index} binding width mismatch: source type i{}, binding i{}",
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", src_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule("llvm.vp.vector.cast.integer", env, Some(kind.semantic()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("VP integer cast lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_float_cast_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VpFloatCastKind,
    ) -> anyhow::Result<()> {
        if instruction.get_num_operands().saturating_sub(1) != 3 {
            bail!("VP floating cast {:?} expects exactly three arguments", kind);
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("VP floating cast result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("VP floating cast result fields")?;
        let src_value = instruction_operand_value(instruction, 0).context("VP floating cast missing source")?;
        let src_fields = vector_fields_from_type(src_value.get_type()).context("VP floating cast source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "VP floating cast lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_fields.len()
            );
        }

        let mask_value = instruction_operand_value(instruction, 1).context("VP floating cast missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp floating cast mask")?;
        let evl = constant_int_operand(instruction, 2, "llvm.vp floating cast evl")?;
        let src_vector = self
            .vector_seed_from_operand(instruction, 0)
            .context("VP floating cast source vector operand")?;
        if src_vector.fields.len() != result_fields.len() {
            bail!(
                "VP floating cast binding lane count mismatch: result {}, source {}",
                result_fields.len(),
                src_vector.fields.len()
            );
        }

        let (source_kind, result_kind) = kind.lane_kinds();
        let op = kind.op();
        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if src_info.kind != source_kind || result_info.kind != result_kind {
                bail!(
                    "VP floating cast lane {index} requires {:?} -> {:?}, got source {:?} and result {:?}",
                    source_kind,
                    result_kind,
                    src_info.kind,
                    result_info.kind
                );
            }
            self.validate_float_cast_widths(op, src_info.width, result_info.width)?;
            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("VP floating cast source lane {index} is undefined or unsupported"))?;
            if src_binding.width != src_info.width {
                bail!(
                    "VP floating cast lane {index} binding width mismatch: source type {}{}, binding i{}",
                    scalar_kind_prefix(src_info.kind),
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", src_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(kind.lowering_rule(), env, Some(HandlerSemantic::FloatCast(op)))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("VP floating cast lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
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
        let lhs_value = instruction_operand_value(instruction, 0).context("integer intrinsic missing operand 0")?;
        let rhs_value = instruction_operand_value(instruction, 1).context("integer intrinsic missing operand 1")?;
        let third_value = instruction_operand_value(instruction, 2).context("integer intrinsic missing operand 2")?;
        if matches!(lhs_value.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(rhs_value.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(third_value.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!("integer intrinsic {:?} does not support fixed vector lowering", kind);
            };
            return self.lower_vector_integer_ternary(instruction, rule, Some(kind.semantic()));
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

    fn lower_vector_reduce_integer_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VectorReduceIntegerKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!("vector integer reduction {kind:?} expects exactly one argument, got {actual_args}");
        }

        let src_value =
            instruction_operand_value(instruction, 0).context("vector integer reduction missing source vector")?;
        let src_fields = vector_fields_from_type(src_value.get_type()).context("vector integer reduction source")?;
        if src_fields.is_empty() {
            bail!("vector integer reduction requires at least one fixed vector lane");
        }
        let result_width =
            instruction_result_width(instruction)?.context("vector integer reduction result has no scalar width")?;
        checked_intrinsic_integer_width(result_width as u64)?;
        if src_fields
            .iter()
            .any(|field| field.kind != ScalarKind::Integer || field.width != result_width)
        {
            bail!("vector integer reduction requires integer lanes matching the scalar result width");
        }

        let src_vector = self.vector_operand(instruction, 0)?;
        if src_vector.fields.len() != src_fields.len() {
            bail!(
                "vector integer reduction source lane count mismatch: type has {}, value has {}",
                src_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut lanes = Vec::with_capacity(src_fields.len());
        for (index, field) in src_vector.fields.iter().copied().enumerate() {
            let binding = field
                .map(|field| field.binding)
                .with_context(|| format!("vector integer reduction lane {index} is undefined or unsupported"))?;
            if binding.width != result_width {
                bail!(
                    "vector integer reduction lane {index} binding width mismatch: result i{}, lane i{}",
                    result_width,
                    binding.width
                );
            }
            lanes.push(binding);
        }

        let result_key = instruction_key(instruction);
        let mut acc = lanes[0];
        for (index, lane) in lanes.into_iter().enumerate().skip(1) {
            let env = LoweringEnv::new()
                .binding("%acc", acc)
                .binding("%lane", lane)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%r)", result_width as u64);
            let env = self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
            acc = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector integer reduction lowering must produce a scalar result register")
                },
            };
        }
        self.values.insert(result_key, acc);
        Ok(())
    }

    fn lower_vp_reduce_integer_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VectorReduceIntegerKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("vp integer reduction {kind:?} expects exactly four arguments, got {actual_args}");
        }

        let start_value =
            instruction_operand_value(instruction, 0).context("vp integer reduction missing start value")?;
        if !matches!(start_value.get_type(), BasicTypeEnum::IntType(_)) {
            bail!("vp integer reduction start value must be a scalar integer");
        }
        let src_value =
            instruction_operand_value(instruction, 1).context("vp integer reduction missing source vector")?;
        let src_fields = vector_fields_from_type(src_value.get_type()).context("vp integer reduction source")?;
        if src_fields.is_empty() {
            bail!("vp integer reduction requires at least one fixed vector lane");
        }
        let result_width =
            instruction_result_width(instruction)?.context("vp integer reduction result has no scalar width")?;
        checked_intrinsic_integer_width(result_width as u64)?;
        let start_width = value_width(start_value).context("vp integer reduction start value width")?;
        if start_width != result_width {
            bail!(
                "vp integer reduction start width i{} does not match result width i{}",
                start_width,
                result_width
            );
        }
        if src_fields
            .iter()
            .any(|field| field.kind != ScalarKind::Integer || field.width != result_width)
        {
            bail!("vp integer reduction requires integer lanes matching the scalar result width");
        }

        let mask_value = instruction_operand_value(instruction, 2).context("vp integer reduction missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, src_fields.len(), "llvm.vp.reduce integer mask")?;
        let evl = constant_int_operand(instruction, 3, "llvm.vp.reduce integer evl")?;
        let src_vector = self.vector_seed_from_operand(instruction, 1)?;
        if src_vector.fields.len() != src_fields.len() {
            bail!(
                "vp integer reduction source lane count mismatch: type has {}, value has {}",
                src_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut acc = self.materialize_operand(instruction, 0)?;
        if acc.width != result_width {
            bail!(
                "vp integer reduction start binding width mismatch: result i{}, accumulator i{}",
                result_width,
                acc.width
            );
        }

        for (index, is_enabled) in mask.into_iter().enumerate() {
            if !is_enabled || index as u64 >= evl {
                continue;
            }
            let lane = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp integer reduction lane {index} is undefined or unsupported"))?;
            if lane.width != result_width {
                bail!(
                    "vp integer reduction lane {index} binding width mismatch: result i{}, lane i{}",
                    result_width,
                    lane.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%acc", acc)
                .binding("%lane", lane)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%r)", result_width as u64);
            let env = self.execute_lowering_rule(kind.vp_lowering_rule(), env, Some(kind.semantic()))?;
            acc = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vp integer reduction lowering must produce a scalar result register")
                },
            };
        }

        self.values.insert(instruction_key(instruction), acc);
        Ok(())
    }

    fn lower_vector_reduce_float_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VectorReduceFloatKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        let expected_args = if kind.has_start_value() { 2 } else { 1 };
        if actual_args != expected_args {
            bail!("vector float reduction {kind:?} expects {expected_args} arguments, got {actual_args}");
        }

        let src_index = kind.source_operand_index();
        let src_value = instruction_operand_value(instruction, src_index)
            .context("vector float reduction missing source vector")?;
        let src_fields = vector_fields_from_type(src_value.get_type()).context("vector float reduction source")?;
        if src_fields.is_empty() {
            bail!("vector float reduction requires at least one fixed vector lane");
        }
        let result_width =
            instruction_result_width(instruction)?.context("vector float reduction result has no scalar width")?;
        checked_intrinsic_float_width(result_width as u64)?;
        if src_fields
            .iter()
            .any(|field| field.kind != ScalarKind::Float || field.width != result_width)
        {
            bail!("vector float reduction requires floating lanes matching the scalar result width");
        }

        let src_vector = self.vector_operand(instruction, src_index)?;
        if src_vector.fields.len() != src_fields.len() {
            bail!(
                "vector float reduction source lane count mismatch: type has {}, value has {}",
                src_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut lanes = Vec::with_capacity(src_fields.len());
        for (index, field) in src_vector.fields.iter().copied().enumerate() {
            let lane = field
                .map(|field| field.binding)
                .with_context(|| format!("vector float reduction lane {index} is undefined or unsupported"))?;
            if lane.width != result_width {
                bail!(
                    "vector float reduction lane {index} binding width mismatch: result f{}, lane f{}",
                    result_width,
                    lane.width
                );
            }
            lanes.push(lane);
        }

        let (mut acc, first_lane) = if kind.has_start_value() {
            let start_value =
                instruction_operand_value(instruction, 0).context("vector float reduction missing accumulator")?;
            if !matches!(start_value.get_type(), BasicTypeEnum::FloatType(_)) {
                bail!("vector float reduction accumulator must be a scalar floating-point value");
            }
            let start_width = value_width(start_value).context("vector float reduction accumulator width")?;
            checked_intrinsic_float_width(start_width as u64)?;
            if start_width != result_width {
                bail!(
                    "vector float reduction accumulator width f{} does not match result width f{}",
                    start_width,
                    result_width
                );
            }

            let acc = self.materialize_operand(instruction, 0)?;
            if acc.width != result_width {
                bail!(
                    "vector float reduction accumulator binding width mismatch: result f{}, accumulator f{}",
                    result_width,
                    acc.width
                );
            }
            (acc, 0)
        } else {
            (lanes[0], 1)
        };

        for (index, lane) in lanes.into_iter().enumerate().skip(first_lane) {
            let env = LoweringEnv::new()
                .binding("%acc", acc)
                .binding("%lane", lane)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%r)", result_width as u64);
            let env = self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
            acc = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector float reduction lowering must produce a scalar result register")
                },
            };
        }
        self.values.insert(instruction_key(instruction), acc);
        Ok(())
    }

    fn lower_vp_reduce_float_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VectorReduceFloatKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("vp float reduction {kind:?} expects exactly four arguments, got {actual_args}");
        }

        let start_value =
            instruction_operand_value(instruction, 0).context("vp float reduction missing start value")?;
        if !matches!(start_value.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!("vp float reduction start value must be a scalar floating-point value");
        }
        let src_value =
            instruction_operand_value(instruction, 1).context("vp float reduction missing source vector")?;
        let src_fields = vector_fields_from_type(src_value.get_type()).context("vp float reduction source")?;
        if src_fields.is_empty() {
            bail!("vp float reduction requires at least one fixed vector lane");
        }
        let result_width =
            instruction_result_width(instruction)?.context("vp float reduction result has no scalar width")?;
        checked_intrinsic_float_width(result_width as u64)?;
        let start_width = value_width(start_value).context("vp float reduction start value width")?;
        if start_width != result_width {
            bail!(
                "vp float reduction start width f{} does not match result width f{}",
                start_width,
                result_width
            );
        }
        if src_fields
            .iter()
            .any(|field| field.kind != ScalarKind::Float || field.width != result_width)
        {
            bail!("vp float reduction requires floating lanes matching the scalar result width");
        }

        let mask_value = instruction_operand_value(instruction, 2).context("vp float reduction missing mask")?;
        let mask = constant_i1_vector_mask(mask_value, src_fields.len(), "llvm.vp.reduce floating mask")?;
        let evl = constant_int_operand(instruction, 3, "llvm.vp.reduce floating evl")?;
        let src_vector = self.vector_seed_from_operand(instruction, 1)?;
        if src_vector.fields.len() != src_fields.len() {
            bail!(
                "vp float reduction source lane count mismatch: type has {}, value has {}",
                src_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut acc = self.materialize_operand(instruction, 0)?;
        if acc.width != result_width {
            bail!(
                "vp float reduction start binding width mismatch: result f{}, accumulator f{}",
                result_width,
                acc.width
            );
        }

        for (index, is_enabled) in mask.into_iter().enumerate() {
            if !is_enabled || index as u64 >= evl {
                continue;
            }
            let lane = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vp float reduction lane {index} is undefined or unsupported"))?;
            if lane.width != result_width {
                bail!(
                    "vp float reduction lane {index} binding width mismatch: result f{}, lane f{}",
                    result_width,
                    lane.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%acc", acc)
                .binding("%lane", lane)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%r)", result_width as u64);
            let env = self.execute_lowering_rule(kind.vp_lowering_rule(), env, Some(kind.semantic()))?;
            acc = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vp float reduction lowering must produce a scalar result register")
                },
            };
        }

        self.values.insert(instruction_key(instruction), acc);
        Ok(())
    }

    fn lower_stepvector_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 0 {
            bail!("llvm.stepvector expects exactly 0 arguments, got {actual_args}");
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("llvm.stepvector result must be a fixed vector");
        };
        let fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("llvm.stepvector result fields")?;
        let action = self.emit_action_for_shape(
            "llvm.vector.step",
            &HandlerSemantic::MovImm,
            &[
                ("dst", "%vr"),
                ("imm", "lane_index(%lane)"),
                ("width", "lane_width(%lane)"),
            ],
        )?;

        let mut lanes = Vec::with_capacity(fields.len());
        for (index, field) in fields.into_iter().enumerate() {
            if field.kind != ScalarKind::Integer || field.width < 8 {
                bail!(
                    "llvm.stepvector lane {index} must be an integer with width at least 8, got {:?}{}",
                    field.kind,
                    field.width
                );
            }
            let stable = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: field.width,
            };
            let env = LoweringEnv::new()
                .binding("%vr", stable)
                .imm("lane_index(%lane)", mask_integer_to_width(index as u64, field.width))
                .imm("lane_width(%lane)", field.width as u64);
            self.emit_profile_action(&action, &env)?;
            lanes.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: lanes });
        Ok(())
    }

    fn lower_get_active_lane_mask_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 2 {
            bail!("llvm.get.active.lane.mask expects exactly 2 arguments, got {actual_args}");
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("llvm.get.active.lane.mask result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("llvm.get.active.lane.mask result fields")?;
        let start = instruction_operand_value(instruction, 0)?;
        let end = instruction_operand_value(instruction, 1)?;
        if !start.is_int_value() || !end.is_int_value() {
            bail!("llvm.get.active.lane.mask start/end operands must be integers");
        }
        let start_width = value_width(start)?;
        let end_width = value_width(end)?;
        if start_width != end_width {
            bail!("llvm.get.active.lane.mask start/end width mismatch: i{start_width} and i{end_width}");
        }
        let index_width = checked_intrinsic_integer_width(start_width as u64)?;
        let start_binding = self.materialize_value(start)?;
        let end_binding = self.materialize_value(end)?;
        let actions = self.active_lane_mask_actions()?;

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, field) in result_fields.into_iter().enumerate() {
            if field.kind != ScalarKind::Integer || field.width != 1 {
                bail!(
                    "llvm.get.active.lane.mask lane {index} result must be i1, got {:?} i{}",
                    field.kind,
                    field.width
                );
            }
            let lane = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: index_width,
            };
            let absolute_index = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: index_width,
            };
            let active = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: 1,
            };
            let env = LoweringEnv::new()
                .binding("%vs", start_binding)
                .binding("%ve", end_binding)
                .binding("%vl", lane)
                .binding("%vi", absolute_index)
                .binding("%vr", active)
                .imm("lane(%r)", mask_integer_to_width(index as u64, index_width))
                .imm("operand_width(%a,%b)", index_width as u64)
                .imm("ult", CmpPredicate::Ult as u64);
            self.emit_profile_action(&actions.lane_mov, &env)?;
            self.emit_profile_action(&actions.add, &env)?;
            self.emit_profile_action(&actions.icmp, &env)?;
            self.builder.release_vreg(lane.reg);
            self.builder.release_vreg(absolute_index.reg);
            fields.push(Some(AggregateField::owned(active)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_cttz_elts_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 2 {
            bail!("llvm.experimental.cttz.elts expects exactly 2 arguments, got {actual_args}");
        }
        let zero_is_poison = constant_int_operand(instruction, 1, "llvm.experimental.cttz.elts zero_is_poison")?;
        if zero_is_poison > 1 {
            bail!("llvm.experimental.cttz.elts zero_is_poison must be i1");
        }

        let result_width =
            instruction_result_width(instruction)?.context("llvm.experimental.cttz.elts result has no scalar width")?;
        let result_width = checked_intrinsic_integer_width(result_width as u64)?;
        let mask_value = instruction_operand_value(instruction, 0)?;
        let mask_fields =
            vector_fields_from_type(mask_value.get_type()).context("llvm.experimental.cttz.elts mask fields")?;
        if mask_fields.is_empty() {
            bail!("llvm.experimental.cttz.elts mask must have at least one lane");
        }
        for (index, field) in mask_fields.iter().enumerate() {
            if field.kind != ScalarKind::Integer || field.width != 1 {
                bail!(
                    "llvm.experimental.cttz.elts mask lane {index} must be i1, got {:?} i{}",
                    field.kind,
                    field.width
                );
            }
        }
        let lane_count = mask_fields.len() as u64;
        if result_width < 64 && lane_count >= (1_u64 << result_width) {
            bail!("llvm.experimental.cttz.elts result i{result_width} cannot represent lane count {lane_count}");
        }

        let mask = self.vector_operand(instruction, 0)?;
        if mask.fields.len() != mask_fields.len() {
            bail!(
                "llvm.experimental.cttz.elts mask binding lane count mismatch: type has {}, value has {}",
                mask_fields.len(),
                mask.fields.len()
            );
        }

        let actions = self.count_trailing_zero_elements_actions()?;
        let result = ValueBinding {
            reg: self.builder.alloc_vreg()?,
            width: result_width,
        };
        let default_env = LoweringEnv::new()
            .binding("%vr", result)
            .imm("lane_count(%mask)", lane_count)
            .imm("type_width(%r)", result_width as u64);
        self.emit_profile_action(&actions.default_mov, &default_env)?;

        for lane_index in (0..mask_fields.len()).rev() {
            let mask_lane = mask
                .fields
                .get(lane_index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("llvm.experimental.cttz.elts mask lane {lane_index} is undefined"))?;
            if mask_lane.width != 1 {
                bail!(
                    "llvm.experimental.cttz.elts mask lane {lane_index} binding must be i1, got i{}",
                    mask_lane.width
                );
            }

            let lane_value = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: result_width,
            };
            let case_label = self.builder.new_label();
            let join_label = self.builder.new_label();
            let env = LoweringEnv::new()
                .binding("%vm", mask_lane)
                .binding("%vk", lane_value)
                .binding("%vr", result)
                .imm("lane(%r)", mask_integer_to_width(lane_index as u64, result_width))
                .imm("type_width(%r)", result_width as u64)
                .label("case_label", case_label)
                .label("join_label", join_label);
            self.emit_profile_action(&actions.lane_mov, &env)?;
            self.emit_profile_action(&actions.br_if, &env)?;
            self.builder.bind_label(case_label);
            self.emit_profile_action(&actions.case_mov, &env)?;
            self.emit_profile_action(&actions.br, &env)?;
            self.builder.bind_label(join_label);
            self.builder.release_vreg(lane_value.reg);
        }

        self.values.insert(instruction_key(instruction), result);
        Ok(())
    }

    fn lower_experimental_get_vector_length_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 3 {
            bail!("llvm.experimental.get.vector.length expects exactly 3 arguments, got {actual_args}");
        }
        let vector_factor = constant_int_operand(instruction, 1, "llvm.experimental.get.vector.length VF")?;
        let scalable = constant_int_operand(instruction, 2, "llvm.experimental.get.vector.length scalable flag")?;
        if scalable > 1 {
            bail!("llvm.experimental.get.vector.length scalable flag must be i1");
        }
        if scalable != 0 {
            bail!("llvm.experimental.get.vector.length scalable=true is not supported by vm_virtualize");
        }

        let result_width = instruction_result_width(instruction)?
            .context("llvm.experimental.get.vector.length result has no scalar width")?;
        if result_width != 32 {
            bail!("llvm.experimental.get.vector.length result must be i32, got i{result_width}");
        }
        let avl = self.materialize_operand(instruction, 0)?;
        checked_intrinsic_integer_width(avl.width as u64)?;
        let compare_width = avl.width.max(result_width);
        checked_intrinsic_integer_width(compare_width as u64)?;

        let actions = self.get_vector_length_actions()?;
        let avl_for_compare = if avl.width < compare_width {
            let widened = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: compare_width,
            };
            let zext_env = LoweringEnv::new()
                .binding("%va", avl)
                .binding("%vwide", widened)
                .imm("type_width(%avl)", avl.width as u64)
                .imm("compare_width(%avl)", compare_width as u64);
            self.emit_profile_action(&actions.avl_zext, &zext_env)?;
            widened
        } else {
            avl
        };
        let vector_factor_full = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: compare_width,
        };
        let cond = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 1,
        };
        let avl_truncated = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: result_width,
        };
        let vector_factor_truncated = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: result_width,
        };
        let result = ValueBinding {
            reg: self.builder.alloc_vreg()?,
            width: result_width,
        };

        let then_label = self.builder.new_label();
        let else_label = self.builder.new_label();
        let join_label = self.builder.new_label();
        let env = LoweringEnv::new()
            .binding("%va", avl)
            .binding("%vwide", avl_for_compare)
            .binding("%vv", vector_factor_full)
            .binding("%vc", cond)
            .binding("%vt", avl_truncated)
            .binding("%ve", vector_factor_truncated)
            .binding("%vr", result)
            .imm("vector_factor(%r)", mask_integer_to_width(vector_factor, compare_width))
            .imm("type_width(%avl)", avl.width as u64)
            .imm("compare_width(%avl)", compare_width as u64)
            .imm("type_width(%r)", result_width as u64)
            .imm("ult", CmpPredicate::Ult as u64)
            .label("then_label", then_label)
            .label("else_label", else_label)
            .label("join_label", join_label);
        self.emit_profile_action(&actions.vector_factor_mov, &env)?;
        self.emit_profile_action(&actions.icmp, &env)?;
        self.emit_profile_action(&actions.avl_trunc, &env)?;
        self.emit_profile_action(&actions.vector_factor_trunc, &env)?;
        self.emit_profile_action(&actions.br_if, &env)?;
        self.builder.bind_label(then_label);
        self.emit_profile_action(&actions.then_mov, &env)?;
        self.emit_profile_action(&actions.br, &env)?;
        self.builder.bind_label(else_label);
        self.emit_profile_action(&actions.else_mov, &env)?;
        self.builder.bind_label(join_label);

        self.builder.release_vreg(vector_factor_full.reg);
        self.builder.release_vreg(cond.reg);
        self.builder.release_vreg(avl_truncated.reg);
        self.builder.release_vreg(vector_factor_truncated.reg);
        if avl_for_compare.reg != avl.reg {
            self.builder.release_vreg(avl_for_compare.reg);
        }
        self.values.insert(instruction_key(instruction), result);
        Ok(())
    }

    fn lower_vp_cttz_elts_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.vp.cttz.elts expects exactly 4 arguments, got {actual_args}");
        }
        let zero_is_poison = constant_int_operand(instruction, 1, "llvm.vp.cttz.elts zero_is_poison")?;
        if zero_is_poison > 1 {
            bail!("llvm.vp.cttz.elts zero_is_poison must be i1");
        }

        let result_width =
            instruction_result_width(instruction)?.context("llvm.vp.cttz.elts result has no scalar width")?;
        let result_width = checked_intrinsic_integer_width(result_width as u64)?;
        let mask_value = instruction_operand_value(instruction, 0)?;
        let mask_fields = vector_fields_from_type(mask_value.get_type()).context("llvm.vp.cttz.elts mask fields")?;
        if mask_fields.is_empty() {
            bail!("llvm.vp.cttz.elts mask must have at least one lane");
        }
        for (index, field) in mask_fields.iter().enumerate() {
            if field.kind != ScalarKind::Integer || field.width != 1 {
                bail!(
                    "llvm.vp.cttz.elts mask lane {index} must be i1, got {:?} i{}",
                    field.kind,
                    field.width
                );
            }
        }
        let lane_count = mask_fields.len() as u64;
        let vp_mask_value = instruction_operand_value(instruction, 2).context("llvm.vp.cttz.elts missing VP mask")?;
        let vp_mask = constant_i1_vector_mask(vp_mask_value, mask_fields.len(), "llvm.vp.cttz.elts VP mask")?;
        let evl = constant_int_operand(instruction, 3, "llvm.vp.cttz.elts EVL")?;
        let active_lane_count = lane_count.min(evl);
        if result_width < 64 && active_lane_count >= (1_u64 << result_width) {
            bail!("llvm.vp.cttz.elts result i{result_width} cannot represent active lane count {active_lane_count}");
        }

        let mask = self.vector_operand(instruction, 0)?;
        if mask.fields.len() != mask_fields.len() {
            bail!(
                "llvm.vp.cttz.elts mask binding lane count mismatch: type has {}, value has {}",
                mask_fields.len(),
                mask.fields.len()
            );
        }

        let actions = self.vp_count_trailing_zero_elements_actions()?;
        let result = ValueBinding {
            reg: self.builder.alloc_vreg()?,
            width: result_width,
        };
        let default_env = LoweringEnv::new()
            .binding("%vr", result)
            .imm("active_lane_count(%mask,%evl)", active_lane_count)
            .imm("type_width(%r)", result_width as u64);
        self.emit_profile_action(&actions.default_mov, &default_env)?;

        for lane_index in (0..mask_fields.len()).rev() {
            if !vp_mask[lane_index] || lane_index as u64 >= evl {
                continue;
            }
            let mask_lane = mask
                .fields
                .get(lane_index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("llvm.vp.cttz.elts mask lane {lane_index} is undefined"))?;
            if mask_lane.width != 1 {
                bail!(
                    "llvm.vp.cttz.elts mask lane {lane_index} binding must be i1, got i{}",
                    mask_lane.width
                );
            }

            let lane_value = ValueBinding {
                reg: self.alloc_temporary_vreg()?,
                width: result_width,
            };
            let case_label = self.builder.new_label();
            let join_label = self.builder.new_label();
            let env = LoweringEnv::new()
                .binding("%vm", mask_lane)
                .binding("%vk", lane_value)
                .binding("%vr", result)
                .imm("lane(%r)", mask_integer_to_width(lane_index as u64, result_width))
                .imm("type_width(%r)", result_width as u64)
                .label("case_label", case_label)
                .label("join_label", join_label);
            self.emit_profile_action(&actions.lane_mov, &env)?;
            self.emit_profile_action(&actions.br_if, &env)?;
            self.builder.bind_label(case_label);
            self.emit_profile_action(&actions.case_mov, &env)?;
            self.emit_profile_action(&actions.br, &env)?;
            self.builder.bind_label(join_label);
            self.builder.release_vreg(lane_value.reg);
        }

        self.values.insert(instruction_key(instruction), result);
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

    fn lower_vscale_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 0 {
            bail!("llvm.vscale expects exactly 0 arguments, got {actual_args}");
        }
        let width = instruction_result_width(instruction)?.context("llvm.vscale result has no scalar width")?;
        checked_width(u32::from(width)).context("llvm.vscale result width")?;

        let env = LoweringEnv::new()
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64)
            .imm("width", width as u64);
        self.execute_lowering_rule("llvm.vscale.integer", env, Some(HandlerSemantic::ReadVScale))?;
        Ok(())
    }

    fn lower_get_rounding_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 0 {
            bail!("llvm.get.rounding expects exactly 0 arguments, got {actual_args}");
        }
        let width = instruction_result_width(instruction)?.context("llvm.get.rounding result has no scalar width")?;
        if width != 32 {
            bail!("llvm.get.rounding must return i32, got i{width}");
        }

        let env = LoweringEnv::new()
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64)
            .imm("width", width as u64);
        self.execute_lowering_rule("llvm.get.rounding.integer", env, Some(HandlerSemantic::ReadRounding))?;
        Ok(())
    }

    fn lower_flt_rounds_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 0 {
            bail!("llvm.flt.rounds expects exactly 0 arguments, got {actual_args}");
        }
        let width = instruction_result_width(instruction)?.context("llvm.flt.rounds result has no scalar width")?;
        if width != 32 {
            bail!("llvm.flt.rounds must return i32, got i{width}");
        }

        let env = LoweringEnv::new()
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64)
            .imm("width", width as u64);
        self.execute_lowering_rule("llvm.flt.rounds.integer", env, Some(HandlerSemantic::ReadFltRounds))?;
        Ok(())
    }

    fn lower_fp_state_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FpStateIntrinsicKind,
    ) -> anyhow::Result<()> {
        match kind {
            FpStateIntrinsicKind::Get(state_kind) => self.lower_get_fp_state_intrinsic(instruction, state_kind),
            FpStateIntrinsicKind::Set(state_kind) => self.lower_set_fp_state_intrinsic(instruction, state_kind),
            FpStateIntrinsicKind::Reset(state_kind) => self.lower_reset_fp_state_intrinsic(instruction, state_kind),
            FpStateIntrinsicKind::SetRounding => self.lower_set_rounding_intrinsic(instruction),
        }
    }

    fn lower_get_fp_state_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FpStateKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 0 {
            bail!("llvm.get.{:?} expects exactly 0 arguments, got {actual_args}", kind);
        }
        let width =
            instruction_result_width(instruction)?.context("floating-point state result has no scalar width")?;
        checked_fp_state_width(width as u64)?;

        let intrinsic = FpStateIntrinsicKind::Get(kind);
        let env = LoweringEnv::new()
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64)
            .imm("width", width as u64);
        self.execute_lowering_rule(intrinsic.lowering_rule(), env, Some(intrinsic.semantic()))?;
        Ok(())
    }

    fn lower_set_fp_state_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FpStateKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!("llvm.set.{:?} expects exactly 1 argument, got {actual_args}", kind);
        }
        if instruction_result_width(instruction)?.is_some() {
            bail!("llvm.set.{:?} must return void", kind);
        }
        let value = instruction_basic_operand(instruction, 0).context("missing floating-point state value")?;
        let width = checked_fp_state_width(value_width(value)? as u64)?;

        let intrinsic = FpStateIntrinsicKind::Set(kind);
        let env = LoweringEnv::new()
            .llvm_source("%value", value)
            .imm("type_width(%value)", width as u64)
            .imm("width", width as u64);
        self.execute_lowering_rule(intrinsic.lowering_rule(), env, Some(intrinsic.semantic()))?;
        Ok(())
    }

    fn lower_reset_fp_state_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FpStateKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 0 {
            bail!("llvm.reset.{:?} expects exactly 0 arguments, got {actual_args}", kind);
        }
        if instruction_result_width(instruction)?.is_some() {
            bail!("llvm.reset.{:?} must return void", kind);
        }
        let intrinsic = FpStateIntrinsicKind::Reset(kind);
        self.execute_lowering_rule(
            intrinsic.lowering_rule(),
            LoweringEnv::new(),
            Some(intrinsic.semantic()),
        )?;
        Ok(())
    }

    fn lower_set_rounding_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!("llvm.set.rounding expects exactly 1 argument, got {actual_args}");
        }
        if instruction_result_width(instruction)?.is_some() {
            bail!("llvm.set.rounding must return void");
        }
        let value = instruction_basic_operand(instruction, 0).context("missing rounding mode value")?;
        let width = value_width(value)?;
        if width != 32 {
            bail!("llvm.set.rounding argument must be i32, got i{width}");
        }

        let env = LoweringEnv::new()
            .llvm_source("%value", value)
            .imm("type_width(%value)", width as u64)
            .imm("width", width as u64);
        self.execute_lowering_rule(
            FpStateIntrinsicKind::SetRounding.lowering_rule(),
            env,
            Some(FpStateIntrinsicKind::SetRounding.semantic()),
        )?;
        Ok(())
    }

    fn lower_thread_pointer_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 0 {
            bail!("llvm.thread.pointer expects exactly 0 arguments, got {actual_args}");
        }
        let Some(width) = instruction_result_width(instruction)? else {
            bail!("llvm.thread.pointer must return a pointer scalar");
        };
        if width != 64 {
            bail!("llvm.thread.pointer must return a 64-bit pointer, got width {width}");
        }

        let env = LoweringEnv::new()
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64)
            .imm("width", width as u64);
        self.execute_lowering_rule(
            "llvm.thread.pointer.pointer",
            env,
            Some(HandlerSemantic::ReadThreadPointer),
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

    fn lower_stack_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: StackIntrinsicKind,
    ) -> anyhow::Result<()> {
        match kind {
            StackIntrinsicKind::Save => self.lower_stacksave_intrinsic(instruction),
            StackIntrinsicKind::Restore => self.lower_stackrestore_intrinsic(instruction),
        }
    }

    fn lower_stacksave_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 0 {
            bail!("llvm.stacksave expects exactly 0 arguments, got {actual_args}");
        }
        if !matches!(instruction.get_type(), AnyTypeEnum::PointerType(_)) {
            bail!("llvm.stacksave must return a pointer");
        }

        let env = LoweringEnv::new()
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", 64)
            .imm("width", 64);
        self.execute_lowering_rule("llvm.stacksave.pointer", env, Some(HandlerSemantic::StackSave))?;
        Ok(())
    }

    fn lower_stackrestore_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!("llvm.stackrestore expects exactly 1 argument, got {actual_args}");
        }
        let ptr_operand = instruction_operand_value(instruction, 0)?;
        if !ptr_operand.is_pointer_value() {
            bail!("llvm.stackrestore operand must be a pointer");
        }
        if !matches!(instruction.get_type(), AnyTypeEnum::VoidType(_)) {
            bail!("llvm.stackrestore must return void");
        }

        let ptr = self.materialize_operand(instruction, 0)?;
        if ptr.width != 64 {
            bail!("llvm.stackrestore pointer operand must materialize as i64");
        }
        let env = LoweringEnv::new().binding("%ptr", ptr);
        self.execute_lowering_rule("llvm.stackrestore", env, Some(HandlerSemantic::StackRestore))?;
        Ok(())
    }

    fn lower_clear_cache_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 2 {
            bail!("llvm.clear_cache expects exactly 2 arguments, got {actual_args}");
        }
        if !matches!(instruction.get_type(), AnyTypeEnum::VoidType(_)) {
            bail!("llvm.clear_cache must return void");
        }
        for index in 0..2 {
            let operand = instruction_operand_value(instruction, index)?;
            if !operand.is_pointer_value() {
                bail!("llvm.clear_cache operand {index} must be a pointer");
            }
        }

        let start = self.materialize_operand(instruction, 0)?;
        let end = self.materialize_operand(instruction, 1)?;
        for (name, value) in [("start", start), ("end", end)] {
            if value.width != 64 {
                bail!("llvm.clear_cache {name} pointer operand must materialize as i64");
            }
        }
        let env = LoweringEnv::new().binding("%start", start).binding("%end", end);
        self.execute_lowering_rule("llvm.clear_cache", env, Some(HandlerSemantic::ClearCache))?;
        Ok(())
    }

    fn lower_pseudoprobe_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.pseudoprobe expects exactly 4 arguments, got {actual_args}");
        }
        if !matches!(instruction.get_type(), AnyTypeEnum::VoidType(_)) {
            bail!("llvm.pseudoprobe must return void");
        }

        let guid = constant_int_operand(instruction, 0, "llvm.pseudoprobe guid")?;
        let index = constant_int_operand(instruction, 1, "llvm.pseudoprobe index")?;
        let probe_type = constant_int_operand(instruction, 2, "llvm.pseudoprobe probe_type")?;
        if probe_type > u32::MAX as u64 {
            bail!("llvm.pseudoprobe probe_type must fit in i32, got {probe_type}");
        }
        let attributes = constant_int_operand(instruction, 3, "llvm.pseudoprobe attributes")?;
        let env = LoweringEnv::new()
            .imm("guid", guid)
            .imm("index", index)
            .imm("probe_type", probe_type)
            .imm("attributes", attributes);
        self.execute_lowering_rule("llvm.pseudoprobe", env, Some(HandlerSemantic::PseudoProbe))?;
        Ok(())
    }

    fn lower_prefetch_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.prefetch expects exactly 4 arguments, got {actual_args}");
        }
        if !matches!(instruction.get_type(), AnyTypeEnum::VoidType(_)) {
            bail!("llvm.prefetch must return void");
        }
        let ptr_operand = instruction_operand_value(instruction, 0)?;
        if !ptr_operand.is_pointer_value() {
            bail!("llvm.prefetch operand 0 must be a pointer");
        }
        let ptr = self.materialize_operand(instruction, 0)?;
        if ptr.width != 64 {
            bail!("llvm.prefetch pointer operand must materialize as i64");
        }
        let rw = checked_prefetch_rw(constant_int_operand(instruction, 1, "llvm.prefetch rw")?)?;
        let locality = checked_prefetch_locality(constant_int_operand(instruction, 2, "llvm.prefetch locality")?)?;
        let cache = checked_prefetch_cache(constant_int_operand(instruction, 3, "llvm.prefetch cache")?)?;
        let env = LoweringEnv::new()
            .binding("%ptr", ptr)
            .imm("prefetch_rw(%r)", u64::from(rw))
            .imm("prefetch_locality(%r)", u64::from(locality))
            .imm("prefetch_cache(%r)", u64::from(cache));
        self.execute_lowering_rule("llvm.prefetch", env, Some(HandlerSemantic::Prefetch))?;
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

    fn lower_assume_intrinsic_with_operand_bundles(
        &mut self,
        instruction: InstructionValue<'ctx>,
    ) -> anyhow::Result<()> {
        let condition = constant_int_operand(instruction, 0, "llvm.assume operand-bundle condition")?;
        if condition != 1 {
            bail!("llvm.assume operand-bundle condition must be constant true");
        }
        let env = LoweringEnv::new();
        self.execute_lowering_rule(
            NopIntrinsicKind::Assume.lowering_rule(),
            env,
            Some(HandlerSemantic::Nop),
        )?;
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

        for &index in kind.constant_operand_indices() {
            let _ = constant_int_operand(
                instruction,
                index,
                &format!("identity intrinsic {:?} operand {index}", kind),
            )?;
        }

        if matches!(
            kind,
            IdentityIntrinsicKind::SsaCopyScalar | IdentityIntrinsicKind::ArithmeticFenceScalar
        ) {
            let value = instruction_operand_value(instruction, kind.value_operand_index())?;
            if matches!(value.get_type(), BasicTypeEnum::VectorType(_))
                || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
            {
                let Some(rule) = kind.vector_lowering_rule() else {
                    bail!("identity intrinsic {:?} does not support fixed vector lowering", kind);
                };
                return self.lower_vector_identity_copy_intrinsic(instruction, kind, rule);
            }
        }

        if kind.is_expect_hint() {
            let value = instruction_operand_value(instruction, kind.value_operand_index())?;
            let expected = instruction_operand_value(instruction, 1)?;
            if matches!(value.get_type(), BasicTypeEnum::VectorType(_))
                || matches!(expected.get_type(), BasicTypeEnum::VectorType(_))
                || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
            {
                let Some(rule) = kind.vector_lowering_rule() else {
                    bail!("identity intrinsic {:?} does not support fixed vector lowering", kind);
                };
                return self.lower_vector_expect_intrinsic(instruction, kind, rule);
            }
        }

        let value_operand_index = kind.value_operand_index();
        let src = self.materialize_operand(instruction, value_operand_index)?;
        let width = instruction_result_width(instruction)?.context("identity intrinsic result has no scalar width")?;
        if kind == IdentityIntrinsicKind::ArithmeticFenceScalar {
            let source = instruction_operand_value(instruction, value_operand_index)?;
            ensure_arithmetic_fence_shape(source, instruction.get_type())?;
        }
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
            checked_intrinsic_integer_width(width as u64)?;
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

    fn lower_vector_identity_copy_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: IdentityIntrinsicKind,
        rule: &str,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector identity intrinsic {:?} result must be a fixed vector", kind);
        };
        let value = instruction_operand_value(instruction, kind.value_operand_index())?;
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .with_context(|| format!("vector identity intrinsic {:?} result fields", kind))?;
        let value_fields = vector_fields_from_type(value.get_type())
            .with_context(|| format!("vector identity intrinsic {:?} value fields", kind))?;
        if value_fields.len() != result_fields.len() {
            bail!(
                "vector identity intrinsic {:?} lane count mismatch: result {}, value {}",
                kind,
                result_fields.len(),
                value_fields.len()
            );
        }

        let source = if is_undef_or_poison_value(value) {
            AggregateBinding {
                fields: vec![None; value_fields.len()],
            }
        } else {
            self.vector_operand(instruction, kind.value_operand_index())?
        };
        if source.fields.len() != result_fields.len() {
            bail!(
                "vector identity intrinsic {:?} source field count mismatch: value has {}, result has {}",
                kind,
                source.fields.len(),
                result_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let value_info = value_fields[index];
            if value_info.width != result_info.width || value_info.kind != result_info.kind {
                bail!(
                    "vector identity intrinsic {:?} lane {index} mismatch: result {:?} {}, value {:?} {}",
                    kind,
                    result_info.kind,
                    result_info.width,
                    value_info.kind,
                    value_info.width
                );
            }
            if kind == IdentityIntrinsicKind::ArithmeticFenceScalar && result_info.kind != ScalarKind::Float {
                bail!(
                    "llvm.arithmetic.fence vector lane {index} must be half/float/double, got {:?}",
                    result_info.kind
                );
            }
            let Some(field) = source.fields.get(index).copied().flatten() else {
                fields.push(None);
                continue;
            };
            let src = field.binding;
            if src.width != result_info.width {
                bail!(
                    "vector identity intrinsic {:?} lane {index} binding width mismatch: result {}, value {}",
                    kind,
                    result_info.width,
                    src.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%value_lane", src)
                .binding("%src_lane", src)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::Mov))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector identity lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vector_expect_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: IdentityIntrinsicKind,
        rule: &str,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector identity intrinsic {:?} result must be a fixed vector", kind);
        };
        let value = instruction_operand_value(instruction, kind.value_operand_index())?;
        let expected = instruction_operand_value(instruction, 1)?;
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vector expect result fields")?;
        let value_fields = vector_fields_from_type(value.get_type()).context("vector expect value fields")?;
        let expected_fields = vector_fields_from_type(expected.get_type()).context("vector expect expected fields")?;
        if value_fields.len() != result_fields.len() || expected_fields.len() != result_fields.len() {
            bail!(
                "vector expect lane count mismatch: result {}, value {}, expected {}",
                result_fields.len(),
                value_fields.len(),
                expected_fields.len()
            );
        }

        let source = if is_undef_or_poison_value(value) {
            AggregateBinding {
                fields: vec![None; value_fields.len()],
            }
        } else {
            self.vector_operand(instruction, kind.value_operand_index())?
        };
        if source.fields.len() != result_fields.len() {
            bail!(
                "vector expect source field count mismatch: value has {}, result has {}",
                source.fields.len(),
                result_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let value_info = value_fields[index];
            let expected_info = expected_fields[index];
            if result_info.kind != ScalarKind::Integer
                || value_info.kind != ScalarKind::Integer
                || expected_info.kind != ScalarKind::Integer
            {
                bail!(
                    "vector expect lane {index} requires integer lanes, got result {:?}, value {:?}, expected {:?}",
                    result_info.kind,
                    value_info.kind,
                    expected_info.kind
                );
            }
            if value_info.width != result_info.width || expected_info.width != result_info.width {
                bail!(
                    "vector expect lane {index} width mismatch: result i{}, value i{}, expected i{}",
                    result_info.width,
                    value_info.width,
                    expected_info.width
                );
            }
            checked_intrinsic_integer_width(result_info.width as u64)?;
            let Some(field) = source.fields.get(index).copied().flatten() else {
                fields.push(None);
                continue;
            };
            let src = field.binding;
            if src.width != result_info.width {
                bail!(
                    "vector expect lane {index} binding width mismatch: result i{}, value i{}",
                    result_info.width,
                    src.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%value_lane", src)
                .binding("%src_lane", src)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::Mov))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector expect lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
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
        let thunk_name = translator_private_symbol_name(
            self.emit_markers,
            ".amice.vm.tls_addr",
            "tls",
            function_name,
            self.native_calls.len(),
        );
        let thunk = self
            .module
            .add_function(&thunk_name, thunk_type, Some(Linkage::Private));
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
        if matches!(kind, CompileTimeIntrinsicKind::WidenableCondition) {
            return self.lower_widenable_condition_intrinsic(instruction);
        }
        if matches!(
            kind,
            CompileTimeIntrinsicKind::AllowRuntimeCheck | CompileTimeIntrinsicKind::AllowUbsanCheck
        ) {
            return self.lower_runtime_check_gate_intrinsic(instruction, kind);
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

    fn lower_runtime_check_gate_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: CompileTimeIntrinsicKind,
    ) -> anyhow::Result<()> {
        let (rule, immediate_name) = match kind {
            CompileTimeIntrinsicKind::AllowRuntimeCheck => {
                self.validate_allow_runtime_check_intrinsic(instruction)?;
                ("llvm.allow.runtime.check.integer", "allow_runtime_check()")
            },
            CompileTimeIntrinsicKind::AllowUbsanCheck => {
                self.validate_allow_ubsan_check_intrinsic(instruction)?;
                ("llvm.allow.ubsan.check.integer", "allow_ubsan_check()")
            },
            CompileTimeIntrinsicKind::IsConstant
            | CompileTimeIntrinsicKind::ObjectSize
            | CompileTimeIntrinsicKind::WidenableCondition => bail!("unexpected runtime-check gate kind: {kind:?}"),
        };

        let width = instruction_result_width(instruction)?.context("runtime-check gate result has no scalar width")?;
        if width != 1 {
            bail!("runtime-check gate intrinsic must return i1, got i{width}");
        }

        let dst = self.ensure_result_binding(instruction)?;
        let env = LoweringEnv::new()
            .reg("%vr", dst.reg, width)
            .llvm_value("%r", instruction_key(instruction))
            .imm(immediate_name, 1)
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(rule, env, Some(HandlerSemantic::MovImm))?;
        Ok(())
    }

    fn validate_allow_runtime_check_intrinsic(&self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!("llvm.allow.runtime.check expects exactly 1 metadata argument, got {actual_args}");
        }
        Ok(())
    }

    fn validate_allow_ubsan_check_intrinsic(&self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!("llvm.allow.ubsan.check expects exactly 1 i8 immarg, got {actual_args}");
        }
        let kind = instruction_operand_value(instruction, 0).context("llvm.allow.ubsan.check missing kind operand")?;
        let BasicTypeEnum::IntType(kind_ty) = kind.get_type() else {
            bail!("llvm.allow.ubsan.check kind must be i8");
        };
        if kind_ty.get_bit_width() != 8 {
            bail!(
                "llvm.allow.ubsan.check kind must be i8, got i{}",
                kind_ty.get_bit_width()
            );
        }
        let _ = constant_int_operand(instruction, 0, "llvm.allow.ubsan.check kind")?;
        Ok(())
    }

    fn lower_widenable_condition_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 0 {
            bail!("llvm.experimental.widenable.condition expects exactly 0 arguments, got {actual_args}");
        }
        let width = instruction_result_width(instruction)?
            .context("llvm.experimental.widenable.condition result has no scalar width")?;
        if width != 1 {
            bail!("llvm.experimental.widenable.condition must return i1, got i{width}");
        }

        let dst = self.ensure_result_binding(instruction)?;
        let env = LoweringEnv::new()
            .reg("%vr", dst.reg, width)
            .llvm_value("%r", instruction_key(instruction))
            .imm("widenable_condition()", 1)
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule("llvm.widenable.condition.integer", env, Some(HandlerSemantic::MovImm))?;
        Ok(())
    }

    fn lower_objectsize_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.objectsize expects exactly 4 arguments, got {actual_args}");
        }
        let min = objectsize_i1_immarg(instruction, 1)?;
        let null_is_unknown = objectsize_i1_immarg(instruction, 2)?;
        let dynamic = objectsize_i1_immarg(instruction, 3)?;

        let width = instruction_result_width(instruction)?.context("llvm.objectsize result has no scalar width")?;
        let ptr = instruction_basic_operand(instruction, 0).context("llvm.objectsize missing pointer operand")?;
        if !ptr.is_pointer_value() {
            bail!("llvm.objectsize operand must be a pointer");
        }

        let pointer = ptr.into_pointer_value();
        if dynamic
            && !pointer.is_null()
            && let Some(object) = self.dynamic_alloca_object(ptr)?
        {
            return self.lower_dynamic_alloca_objectsize(instruction, object);
        }
        if dynamic
            && !pointer.is_null()
            && let Some(object) = self.dynamic_alloca_gep_object(ptr)?
        {
            return self.lower_dynamic_alloca_gep_objectsize(instruction, object);
        }
        if dynamic
            && !pointer.is_null()
            && let Some(object) = self.dynamic_static_gep_object(ptr)?
        {
            return self.lower_dynamic_static_gep_objectsize(instruction, object);
        }

        let size = if pointer.is_null() {
            if null_is_unknown {
                objectsize_unknown_value(min, width)?
            } else {
                0
            }
        } else {
            match self.static_object_size(ptr) {
                Ok(size) => size,
                Err(error) if objectsize_can_fold_unknown(&error, dynamic) => objectsize_unknown_value(min, width)?,
                Err(error) => return Err(error),
            }
        };
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

    fn lower_dynamic_alloca_objectsize(
        &mut self,
        instruction: InstructionValue<'ctx>,
        object: DynamicAllocaObject,
    ) -> anyhow::Result<()> {
        let width = instruction_result_width(instruction)?.context("llvm.objectsize result has no scalar width")?;
        let dst = self.ensure_result_binding(instruction)?;
        let env = LoweringEnv::new()
            .binding("object_count(%ptr)", object.count)
            .reg("%vr", dst.reg, width)
            .llvm_value("%r", instruction_key(instruction))
            .imm("object_elem_size(%ptr)", object.elem_size)
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(
            "llvm.objectsize.dynamic_alloca",
            env,
            Some(HandlerSemantic::Bin(BinOp::Mul)),
        )?;
        Ok(())
    }

    fn lower_dynamic_alloca_gep_objectsize(
        &mut self,
        instruction: InstructionValue<'ctx>,
        object: DynamicAllocaGepObject,
    ) -> anyhow::Result<()> {
        let width = instruction_result_width(instruction)?.context("llvm.objectsize result has no scalar width")?;
        let dst = self.ensure_result_binding(instruction)?;
        let env = LoweringEnv::new()
            .binding("object_count(%ptr)", object.object.count)
            .binding("object_offset(%ptr)", object.offset)
            .reg("%vr", dst.reg, width)
            .llvm_value("%r", instruction_key(instruction))
            .imm("object_elem_size(%ptr)", object.object.elem_size)
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule("llvm.objectsize.dynamic_alloca_gep", env, None)?;
        Ok(())
    }

    fn lower_dynamic_static_gep_objectsize(
        &mut self,
        instruction: InstructionValue<'ctx>,
        object: DynamicStaticGepObject,
    ) -> anyhow::Result<()> {
        let width = instruction_result_width(instruction)?.context("llvm.objectsize result has no scalar width")?;
        if width < 64 && u128::from(object.total_size) >= (1_u128 << width) {
            bail!("llvm.objectsize result {} does not fit in i{width}", object.total_size);
        }
        let dst = self.ensure_result_binding(instruction)?;
        let env = LoweringEnv::new()
            .binding("object_offset(%ptr)", object.offset)
            .reg("%vr", dst.reg, width)
            .llvm_value("%r", instruction_key(instruction))
            .imm("object_size(%ptr)", object.total_size)
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(
            "llvm.objectsize.static_gep",
            env,
            Some(HandlerSemantic::Bin(BinOp::Sub)),
        )?;
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
            | FloatIntrinsicKind::RoundEven
            | FloatIntrinsicKind::Sin
            | FloatIntrinsicKind::Cos
            | FloatIntrinsicKind::Exp
            | FloatIntrinsicKind::Exp2
            | FloatIntrinsicKind::Log
            | FloatIntrinsicKind::Log10
            | FloatIntrinsicKind::Log2 => self.lower_float_unary_intrinsic(instruction, kind),
            FloatIntrinsicKind::MinNum
            | FloatIntrinsicKind::MaxNum
            | FloatIntrinsicKind::Minimum
            | FloatIntrinsicKind::Maximum
            | FloatIntrinsicKind::CopySign
            | FloatIntrinsicKind::Pow => self.lower_float_binary_intrinsic(instruction, kind),
            FloatIntrinsicKind::PowI => self.lower_float_int_binary_intrinsic(instruction, kind),
            FloatIntrinsicKind::Fma | FloatIntrinsicKind::FmulAdd => {
                self.lower_float_ternary_intrinsic(instruction, kind)
            },
            FloatIntrinsicKind::IsFpClass => self.lower_is_fpclass_intrinsic(instruction, kind),
            FloatIntrinsicKind::FPToSISat | FloatIntrinsicKind::FPToUISat => {
                self.lower_saturating_float_to_int_intrinsic(instruction, kind)
            },
            FloatIntrinsicKind::LRint
            | FloatIntrinsicKind::LLRint
            | FloatIntrinsicKind::LRound
            | FloatIntrinsicKind::LLRound => self.lower_round_to_int_intrinsic(instruction, kind),
            FloatIntrinsicKind::ConvertToFp16 | FloatIntrinsicKind::ConvertFromFp16 => {
                self.lower_fp16_conversion_intrinsic(instruction, kind)
            },
        }
    }

    fn lower_fp16_conversion_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!(
                "fp16 conversion intrinsic {:?} expects exactly 1 argument, got {actual_args}",
                kind
            );
        }

        let source =
            instruction_operand_value(instruction, 0).context("fp16 conversion intrinsic missing operand 0")?;
        let (source_width, result_width) = match kind {
            FloatIntrinsicKind::ConvertToFp16 => {
                if !matches!(source.get_type(), BasicTypeEnum::FloatType(_)) {
                    bail!("llvm.convert.to.fp16 operand must be a scalar float");
                }
                let source_width =
                    value_width(source).context("llvm.convert.to.fp16 source has unsupported float width")?;
                checked_intrinsic_float_width(source_width as u64)?;
                let result_width =
                    instruction_result_width(instruction)?.context("llvm.convert.to.fp16 result has no width")?;
                if !matches!(instruction.get_type(), AnyTypeEnum::IntType(_)) || result_width != 16 {
                    bail!("llvm.convert.to.fp16 must return i16, got width {result_width}");
                }
                (source_width, 16)
            },
            FloatIntrinsicKind::ConvertFromFp16 => {
                if !matches!(source.get_type(), BasicTypeEnum::IntType(int_type) if int_type.get_bit_width() == 16) {
                    bail!("llvm.convert.from.fp16 operand must be i16");
                }
                let result_width =
                    instruction_result_width(instruction)?.context("llvm.convert.from.fp16 result has no width")?;
                if !matches!(instruction.get_type(), AnyTypeEnum::FloatType(_)) {
                    bail!("llvm.convert.from.fp16 result must be a scalar float");
                }
                checked_intrinsic_float_width(result_width as u64)?;
                (16, result_width)
            },
            FloatIntrinsicKind::FAbs
            | FloatIntrinsicKind::Sqrt
            | FloatIntrinsicKind::Canonicalize
            | FloatIntrinsicKind::Floor
            | FloatIntrinsicKind::Ceil
            | FloatIntrinsicKind::Trunc
            | FloatIntrinsicKind::Rint
            | FloatIntrinsicKind::NearbyInt
            | FloatIntrinsicKind::Round
            | FloatIntrinsicKind::RoundEven
            | FloatIntrinsicKind::Sin
            | FloatIntrinsicKind::Cos
            | FloatIntrinsicKind::Exp
            | FloatIntrinsicKind::Exp2
            | FloatIntrinsicKind::Log
            | FloatIntrinsicKind::Log10
            | FloatIntrinsicKind::Log2
            | FloatIntrinsicKind::Fma
            | FloatIntrinsicKind::FmulAdd
            | FloatIntrinsicKind::MinNum
            | FloatIntrinsicKind::MaxNum
            | FloatIntrinsicKind::Minimum
            | FloatIntrinsicKind::Maximum
            | FloatIntrinsicKind::CopySign
            | FloatIntrinsicKind::Pow
            | FloatIntrinsicKind::PowI
            | FloatIntrinsicKind::IsFpClass
            | FloatIntrinsicKind::FPToSISat
            | FloatIntrinsicKind::FPToUISat
            | FloatIntrinsicKind::LRint
            | FloatIntrinsicKind::LLRint
            | FloatIntrinsicKind::LRound
            | FloatIntrinsicKind::LLRound => {
                bail!("{kind:?} is not an fp16 conversion intrinsic");
            },
        };

        let env = LoweringEnv::new()
            .llvm_source("%a", source)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", source_width as u64)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_constrained_float_unary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let Some(rule) = kind.constrained_unary_lowering_rule() else {
            bail!("{kind:?} is not a supported constrained floating unary intrinsic");
        };

        let expected_args = if kind.constrained_unary_has_rounding_mode() {
            3
        } else {
            2
        };
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != expected_args {
            bail!(
                "constrained floating unary {:?} expects exactly {expected_args} arguments, got {actual_args}",
                kind
            );
        }

        let exception_index = if kind.constrained_unary_has_rounding_mode() {
            let rounding = metadata_string_operand(instruction, 1, "constrained floating unary rounding mode")?;
            if rounding != "round.tonearest" {
                bail!("constrained floating unary rounding mode {rounding} is not supported by vm_virtualize");
            }
            2
        } else {
            1
        };
        let exception = metadata_string_operand(
            instruction,
            exception_index,
            "constrained floating unary exception behavior",
        )?;
        if exception != "fpexcept.ignore" {
            bail!("constrained floating unary exception behavior {exception} is not supported by vm_virtualize");
        }

        let source =
            instruction_operand_value(instruction, 0).context("constrained floating unary missing operand 0")?;
        if matches!(source.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(vector_rule) = kind.constrained_vector_unary_lowering_rule() else {
                bail!(
                    "constrained floating unary {:?} does not support fixed vector lowering",
                    kind
                );
            };
            return self.lower_vector_float_unary(instruction, vector_rule, kind.semantic(), |width| {
                checked_float_intrinsic_width(kind, width as u64).map(|_| ())
            });
        }
        if !matches!(source.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!(
                "constrained floating unary {:?} only supports scalar floating-point operands",
                kind
            );
        }
        if !matches!(instruction.get_type(), AnyTypeEnum::FloatType(_)) {
            bail!("constrained floating unary {:?} result must be a scalar float", kind);
        }

        let source_width =
            value_width(source).context("constrained floating unary source has unsupported float width")?;
        let result_width =
            instruction_result_width(instruction)?.context("constrained floating unary result has no scalar width")?;
        checked_float_intrinsic_width(kind, source_width as u64)?;
        checked_float_intrinsic_width(kind, result_width as u64)?;
        if source_width != result_width {
            bail!(
                "constrained floating unary {:?} width mismatch: source i{}, result i{}",
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
        self.execute_lowering_rule(rule, env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_constrained_float_binary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let Some(rule) = kind.constrained_binary_lowering_rule() else {
            bail!("{kind:?} is not a supported constrained floating binary intrinsic");
        };
        let has_rounding = kind.constrained_binary_has_rounding_mode();
        let expected_args = if has_rounding { 4 } else { 3 };
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != expected_args {
            bail!(
                "constrained floating binary {:?} expects exactly {expected_args} arguments, got {actual_args}",
                kind
            );
        }
        validate_constrained_float_metadata(
            instruction,
            has_rounding.then_some(2),
            if has_rounding { 3 } else { 2 },
            "constrained floating binary",
        )?;

        let lhs = instruction_operand_value(instruction, 0).context("constrained floating binary missing operand 0")?;
        let rhs = instruction_operand_value(instruction, 1).context("constrained floating binary missing operand 1")?;
        if matches!(lhs.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(rhs.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(vector_rule) = kind.constrained_vector_binary_lowering_rule() else {
                bail!(
                    "constrained floating binary {:?} does not support fixed vector lowering",
                    kind
                );
            };
            return self.lower_vector_float_binop(instruction, vector_rule, kind.semantic(), |width| {
                checked_float_intrinsic_width(kind, width as u64).map(|_| ())
            });
        }
        if !matches!(lhs.get_type(), BasicTypeEnum::FloatType(_))
            || !matches!(rhs.get_type(), BasicTypeEnum::FloatType(_))
        {
            bail!(
                "constrained floating binary {:?} only supports scalar floating-point operands",
                kind
            );
        }
        if !matches!(instruction.get_type(), AnyTypeEnum::FloatType(_)) {
            bail!("constrained floating binary {:?} result must be a scalar float", kind);
        }

        let lhs_width = value_width(lhs).context("constrained floating binary lhs has unsupported float width")?;
        let rhs_width = value_width(rhs).context("constrained floating binary rhs has unsupported float width")?;
        let result_width =
            instruction_result_width(instruction)?.context("constrained floating binary result has no scalar width")?;
        checked_float_intrinsic_width(kind, lhs_width as u64)?;
        checked_float_intrinsic_width(kind, rhs_width as u64)?;
        checked_float_intrinsic_width(kind, result_width as u64)?;
        if lhs_width != rhs_width || lhs_width != result_width {
            bail!(
                "constrained floating binary {:?} width mismatch: lhs i{}, rhs i{}, result i{}",
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
        self.execute_lowering_rule(rule, env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_constrained_float_int_binary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let Some(rule) = kind.constrained_float_int_binary_lowering_rule() else {
            bail!("{kind:?} is not a supported constrained floating/integer binary intrinsic");
        };
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!(
                "constrained floating/integer binary {:?} expects exactly 4 arguments, got {actual_args}",
                kind
            );
        }
        validate_constrained_float_metadata(instruction, Some(2), 3, "constrained floating/integer binary")?;

        let lhs = instruction_operand_value(instruction, 0)
            .context("constrained floating/integer binary missing operand 0")?;
        let rhs = instruction_operand_value(instruction, 1)
            .context("constrained floating/integer binary missing operand 1")?;
        if matches!(lhs.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.constrained_vector_float_int_binary_lowering_rule() else {
                bail!(
                    "constrained floating/integer binary {:?} does not support fixed vector lowering",
                    kind
                );
            };
            return self.lower_vector_float_int_binop(instruction, rule, kind.semantic(), |width| {
                checked_float_intrinsic_width(kind, width as u64).map(|_| ())
            });
        }
        if !matches!(lhs.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!(
                "constrained floating/integer binary {:?} first operand must be a scalar float",
                kind
            );
        }
        if !matches!(rhs.get_type(), BasicTypeEnum::IntType(int_type) if int_type.get_bit_width() == 32) {
            bail!(
                "constrained floating/integer binary {:?} second operand must be i32",
                kind
            );
        }
        if !matches!(instruction.get_type(), AnyTypeEnum::FloatType(_)) {
            bail!(
                "constrained floating/integer binary {:?} result must be a scalar float",
                kind
            );
        }

        let lhs_width =
            value_width(lhs).context("constrained floating/integer binary lhs has unsupported float width")?;
        let result_width = instruction_result_width(instruction)?
            .context("constrained floating/integer binary result has no scalar width")?;
        checked_float_intrinsic_width(kind, lhs_width as u64)?;
        checked_float_intrinsic_width(kind, result_width as u64)?;
        if lhs_width != result_width {
            bail!(
                "constrained floating/integer binary {:?} width mismatch: lhs i{}, result i{}",
                kind,
                lhs_width,
                result_width
            );
        }

        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", lhs_width as u64)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule(rule, env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_constrained_float_ternary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let Some(rule) = kind.constrained_ternary_lowering_rule() else {
            bail!("{kind:?} is not a supported constrained floating ternary intrinsic");
        };
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 5 {
            bail!(
                "constrained floating ternary {:?} expects exactly 5 arguments, got {actual_args}",
                kind
            );
        }
        validate_constrained_float_metadata(instruction, Some(3), 4, "constrained floating ternary")?;

        let lhs =
            instruction_operand_value(instruction, 0).context("constrained floating ternary missing operand 0")?;
        let rhs =
            instruction_operand_value(instruction, 1).context("constrained floating ternary missing operand 1")?;
        let third =
            instruction_operand_value(instruction, 2).context("constrained floating ternary missing operand 2")?;
        if matches!(lhs.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(rhs.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(third.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.constrained_vector_ternary_lowering_rule() else {
                bail!(
                    "constrained floating ternary {:?} does not support fixed vector lowering",
                    kind
                );
            };
            return self.lower_vector_float_ternary(instruction, rule, kind.semantic(), |width| {
                checked_float_intrinsic_width(kind, width as u64).map(|_| ())
            });
        }
        if !matches!(lhs.get_type(), BasicTypeEnum::FloatType(_))
            || !matches!(rhs.get_type(), BasicTypeEnum::FloatType(_))
            || !matches!(third.get_type(), BasicTypeEnum::FloatType(_))
        {
            bail!(
                "constrained floating ternary {:?} only supports scalar floating-point operands",
                kind
            );
        }
        if !matches!(instruction.get_type(), AnyTypeEnum::FloatType(_)) {
            bail!("constrained floating ternary {:?} result must be a scalar float", kind);
        }

        let lhs_width = value_width(lhs).context("constrained floating ternary lhs has unsupported float width")?;
        let rhs_width = value_width(rhs).context("constrained floating ternary rhs has unsupported float width")?;
        let third_width =
            value_width(third).context("constrained floating ternary third has unsupported float width")?;
        let result_width = instruction_result_width(instruction)?
            .context("constrained floating ternary result has no scalar width")?;
        checked_float_intrinsic_width(kind, lhs_width as u64)?;
        checked_float_intrinsic_width(kind, rhs_width as u64)?;
        checked_float_intrinsic_width(kind, third_width as u64)?;
        checked_float_intrinsic_width(kind, result_width as u64)?;
        if lhs_width != rhs_width || lhs_width != third_width || lhs_width != result_width {
            bail!(
                "constrained floating ternary {:?} width mismatch: lhs i{}, rhs i{}, third i{}, result i{}",
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
        self.execute_lowering_rule(rule, env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_constrained_round_to_int_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let Some(rule) = kind.constrained_round_to_int_lowering_rule() else {
            bail!("{kind:?} is not a supported constrained round-to-int intrinsic");
        };
        let has_rounding = kind.constrained_round_to_int_has_rounding_mode();
        let expected_args = if has_rounding { 3 } else { 2 };
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != expected_args {
            bail!(
                "constrained round-to-int {:?} expects exactly {expected_args} arguments, got {actual_args}",
                kind
            );
        }
        validate_constrained_float_metadata(
            instruction,
            has_rounding.then_some(1),
            if has_rounding { 2 } else { 1 },
            "constrained round-to-int",
        )?;

        let source = instruction_operand_value(instruction, 0).context("constrained round-to-int missing operand 0")?;
        if !matches!(source.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!(
                "constrained round-to-int {:?} only supports scalar floating-point operands",
                kind
            );
        }
        if !matches!(instruction.get_type(), AnyTypeEnum::IntType(_)) {
            bail!("constrained round-to-int {:?} result must be an integer scalar", kind);
        }

        let source_width =
            value_width(source).context("constrained round-to-int source has unsupported float width")?;
        let result_width =
            instruction_result_width(instruction)?.context("constrained round-to-int result has no scalar width")?;
        checked_float_intrinsic_width(kind, source_width as u64)?;
        checked_round_to_int_result_width(result_width as u64)?;

        let env = LoweringEnv::new()
            .llvm_source("%a", source)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", source_width as u64)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule(rule, env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_constrained_float_binop_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: ConstrainedFloatBinOpKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!(
                "constrained floating binop {:?} expects exactly 4 arguments, got {actual_args}",
                kind
            );
        }

        let rounding = metadata_string_operand(instruction, 2, "constrained floating binop rounding mode")?;
        if rounding != "round.tonearest" {
            bail!("constrained floating binop rounding mode {rounding} is not supported by vm_virtualize");
        }
        let exception = metadata_string_operand(instruction, 3, "constrained floating binop exception behavior")?;
        if exception != "fpexcept.ignore" {
            bail!("constrained floating binop exception behavior {exception} is not supported by vm_virtualize");
        }

        let lhs = instruction_operand_value(instruction, 0)?;
        let rhs = instruction_operand_value(instruction, 1)?;
        let is_scalar_float = matches!(lhs.get_type(), BasicTypeEnum::FloatType(_))
            && matches!(rhs.get_type(), BasicTypeEnum::FloatType(_))
            && matches!(instruction.get_type(), AnyTypeEnum::FloatType(_));
        if !is_scalar_float {
            return self.lower_vector_float_binop(instruction, kind.vector_lowering_rule(), kind.semantic(), |width| {
                checked_intrinsic_float_width(width as u64).map(|_| ())
            });
        }
        let lhs_width = value_width(lhs).context("constrained floating binop lhs width")?;
        let rhs_width = value_width(rhs).context("constrained floating binop rhs width")?;
        let width = instruction_result_width(instruction)?.context("constrained floating binop result width")?;
        if lhs_width != width || rhs_width != width {
            bail!("constrained floating binop width mismatch: result f{width}, lhs f{lhs_width}, rhs f{rhs_width}");
        }
        checked_intrinsic_float_width(width as u64)?;

        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_constrained_float_cmp_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: ConstrainedFloatCmpKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!(
                "constrained floating compare {:?} expects exactly 4 arguments, got {actual_args}",
                kind
            );
        }

        let predicate_name = metadata_string_operand(instruction, 2, "constrained floating compare predicate")?;
        let predicate = float_predicate_from_metadata_name_for(&predicate_name, "llvm.experimental.constrained.fcmp")?;
        let exception = metadata_string_operand(instruction, 3, "constrained floating compare exception behavior")?;
        if exception != "fpexcept.ignore" {
            bail!("constrained floating compare exception behavior {exception} is not supported by vm_virtualize");
        }

        let lhs = instruction_operand_value(instruction, 0)?;
        let rhs = instruction_operand_value(instruction, 1)?;
        if matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
            || matches!(lhs.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(rhs.get_type(), BasicTypeEnum::VectorType(_))
        {
            return self.lower_vector_fcmp(instruction, predicate, kind.vector_lowering_rule());
        }
        if !matches!(lhs.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!("constrained floating compare lhs must be a scalar float");
        }
        if !matches!(rhs.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!("constrained floating compare rhs must be a scalar float");
        }
        if !matches!(instruction.get_type(), AnyTypeEnum::IntType(_)) {
            bail!("constrained floating compare result must be i1");
        }
        let result_width =
            instruction_result_width(instruction)?.context("constrained floating compare result width")?;
        if result_width != 1 {
            bail!("constrained floating compare result must be i1, got i{result_width}");
        }

        let lhs_width = value_width(lhs).context("constrained floating compare lhs width")?;
        let rhs_width = value_width(rhs).context("constrained floating compare rhs width")?;
        if lhs_width != rhs_width {
            bail!("constrained floating compare width mismatch: lhs f{lhs_width}, rhs f{rhs_width}");
        }
        checked_intrinsic_float_width(lhs_width as u64)?;

        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("predicate(%r)", predicate as u64)
            .imm("operand_width(%a,%b)", lhs_width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(HandlerSemantic::Fcmp))?;
        Ok(())
    }

    fn lower_constrained_float_cast_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: ConstrainedFloatCastKind,
    ) -> anyhow::Result<()> {
        let expected_args = if kind.has_rounding_mode() { 3 } else { 2 };
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != expected_args {
            bail!(
                "constrained floating cast {:?} expects exactly {expected_args} arguments, got {actual_args}",
                kind
            );
        }

        let exception_index = if kind.has_rounding_mode() {
            let rounding = metadata_string_operand(instruction, 1, "constrained floating cast rounding mode")?;
            if rounding != "round.tonearest" {
                bail!("constrained floating cast rounding mode {rounding} is not supported by vm_virtualize");
            }
            2
        } else {
            1
        };
        let exception = metadata_string_operand(
            instruction,
            exception_index,
            "constrained floating cast exception behavior",
        )?;
        if exception != "fpexcept.ignore" {
            bail!("constrained floating cast exception behavior {exception} is not supported by vm_virtualize");
        }

        let src = instruction_operand_value(instruction, 0)?;
        if matches!(src.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            return self.lower_vector_float_cast_with_rule(instruction, src, kind.vector_lowering_rule(), kind.op());
        }

        match kind {
            ConstrainedFloatCastKind::SIToFP | ConstrainedFloatCastKind::UIToFP => {
                if !matches!(src.get_type(), BasicTypeEnum::IntType(_)) {
                    bail!("constrained sitofp/uitofp source must be a scalar integer");
                }
                if !matches!(instruction.get_type(), AnyTypeEnum::FloatType(_)) {
                    bail!("constrained sitofp/uitofp result must be a scalar float");
                }
            },
            ConstrainedFloatCastKind::FPToSI | ConstrainedFloatCastKind::FPToUI => {
                if !matches!(src.get_type(), BasicTypeEnum::FloatType(_)) {
                    bail!("constrained fptosi/fptoui source must be a scalar float");
                }
                if !matches!(instruction.get_type(), AnyTypeEnum::IntType(_)) {
                    bail!("constrained fptosi/fptoui result must be a scalar integer");
                }
            },
            ConstrainedFloatCastKind::FPTrunc | ConstrainedFloatCastKind::FPExt => {
                if !matches!(src.get_type(), BasicTypeEnum::FloatType(_)) {
                    bail!("constrained fptrunc/fpext source must be a scalar float");
                }
                if !matches!(instruction.get_type(), AnyTypeEnum::FloatType(_)) {
                    bail!("constrained fptrunc/fpext result must be a scalar float");
                }
            },
        }

        let src_width = value_width(src).context("constrained floating cast source width")?;
        let dst_width = instruction_result_width(instruction)?.context("constrained floating cast result width")?;
        self.validate_float_cast_widths(kind.op(), src_width, dst_width)?;

        let env = LoweringEnv::new()
            .llvm_source("%a", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", src_width as u64)
            .imm("type_width(%r)", dst_width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_saturating_float_to_int_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!(
                "floating saturating conversion intrinsic {:?} expects exactly 1 argument, got {actual_args}",
                kind
            );
        }

        let source = instruction_operand_value(instruction, 0)
            .context("floating saturating conversion intrinsic missing operand 0")?;
        if matches!(source.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!(
                    "floating saturating conversion intrinsic {:?} does not support fixed vector lowering",
                    kind
                );
            };
            return self.lower_vector_saturating_float_to_int_intrinsic(instruction, rule, kind.semantic());
        }
        if !matches!(source.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!("floating saturating conversion only supports scalar floating-point operands");
        }
        let source_width =
            value_width(source).context("floating saturating conversion source has unsupported float width")?;
        checked_float_intrinsic_width(kind, source_width as u64)?;

        let result_width = instruction_result_width(instruction)?
            .context("floating saturating conversion result has no scalar width")?;
        checked_saturating_float_to_int_width(result_width as u64)?;
        if !matches!(instruction.get_type(), AnyTypeEnum::IntType(_)) {
            bail!("floating saturating conversion result must be an integer scalar");
        }

        let env = LoweringEnv::new()
            .llvm_source("%a", source)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", source_width as u64)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_vector_saturating_float_to_int_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        rule: &str,
        semantic: HandlerSemantic,
    ) -> anyhow::Result<()> {
        let HandlerSemantic::FloatCast(op @ (FloatCastOp::FloatToSignedIntSat | FloatCastOp::FloatToUnsignedIntSat)) =
            semantic
        else {
            bail!("vector saturating float-to-int lowering requires a saturating float cast semantic");
        };
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector saturating float-to-int result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("vector saturating float-to-int result fields")?;
        let src = instruction_operand_value(instruction, 0)
            .context("vector saturating float-to-int intrinsic missing operand 0")?;
        let src_fields =
            vector_fields_from_type(src.get_type()).context("vector saturating float-to-int source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "vector saturating float-to-int requires equal lane counts, got source {} and result {}",
                src_fields.len(),
                result_fields.len()
            );
        }

        let src_vector = self.vector_operand(instruction, 0)?;
        if src_vector.fields.len() != src_fields.len() {
            bail!(
                "vector saturating float-to-int source field count mismatch: type has {}, value has {}",
                src_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if src_info.kind != ScalarKind::Float || result_info.kind != ScalarKind::Integer {
                bail!(
                    "vector saturating float-to-int lane {index} requires float -> integer, got source {:?} and result {:?}",
                    src_info.kind,
                    result_info.kind
                );
            }
            self.validate_float_cast_widths(op, src_info.width, result_info.width)?;
            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| {
                    format!("vector saturating float-to-int source lane {index} is undefined or unsupported")
                })?;
            if src_binding.width != src_info.width {
                bail!(
                    "vector saturating float-to-int lane {index} binding width mismatch: source type f{}, binding i{}",
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", src_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::FloatCast(op)))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector saturating float-to-int lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_round_to_int_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 1 {
            bail!(
                "round-to-int intrinsic {:?} expects exactly 1 argument, got {actual_args}",
                kind
            );
        }

        let source = instruction_operand_value(instruction, 0).context("round-to-int intrinsic missing operand 0")?;
        if matches!(source.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!(
                    "round-to-int intrinsic {:?} does not support fixed vector lowering",
                    kind
                );
            };
            return self.lower_vector_round_to_int_intrinsic(instruction, rule, kind.semantic());
        }
        if !matches!(source.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!(
                "round-to-int intrinsic {:?} only supports scalar floating-point operands",
                kind
            );
        }
        let source_width = value_width(source).context("round-to-int intrinsic source has unsupported float width")?;
        checked_float_intrinsic_width(kind, source_width as u64)?;

        let result_width =
            instruction_result_width(instruction)?.context("round-to-int intrinsic result has no scalar width")?;
        if !matches!(instruction.get_type(), AnyTypeEnum::IntType(_)) {
            bail!("round-to-int intrinsic {:?} result must be an integer scalar", kind);
        }
        checked_round_to_int_result_width(result_width as u64)?;

        let env = LoweringEnv::new()
            .llvm_source("%a", source)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", source_width as u64)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_vector_round_to_int_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        rule: &str,
        semantic: HandlerSemantic,
    ) -> anyhow::Result<()> {
        let HandlerSemantic::FloatRoundToInt(op) = semantic else {
            bail!("vector round-to-int lowering requires a round-to-int semantic");
        };
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector round-to-int result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("vector round-to-int result fields")?;
        let src =
            instruction_operand_value(instruction, 0).context("vector round-to-int intrinsic missing operand 0")?;
        let src_fields = vector_fields_from_type(src.get_type()).context("vector round-to-int source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "vector round-to-int requires equal lane counts, got source {} and result {}",
                src_fields.len(),
                result_fields.len()
            );
        }

        let src_vector = self.vector_operand(instruction, 0)?;
        if src_vector.fields.len() != src_fields.len() {
            bail!(
                "vector round-to-int source field count mismatch: type has {}, value has {}",
                src_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if src_info.kind != ScalarKind::Float || result_info.kind != ScalarKind::Integer {
                bail!(
                    "vector round-to-int lane {index} requires float -> integer, got source {:?} and result {:?}",
                    src_info.kind,
                    result_info.kind
                );
            }
            checked_float_width(src_info.width as u64)?;
            checked_round_to_int_result_width(result_info.width as u64)?;
            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector round-to-int source lane {index} is undefined or unsupported"))?;
            if src_binding.width != src_info.width {
                bail!(
                    "vector round-to-int lane {index} binding width mismatch: source type f{}, binding i{}",
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", src_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::FloatRoundToInt(op)))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector round-to-int lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
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
        if matches!(source.get_type(), BasicTypeEnum::VectorType(_)) {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!("floating intrinsic {:?} does not support fixed vector lowering", kind);
            };
            return self.lower_vector_float_unary(instruction, rule, kind.semantic(), |width| {
                checked_float_intrinsic_width(kind, width as u64).map(|_| ())
            });
        }
        if !matches!(source.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!(
                "floating intrinsic {:?} only supports scalar floating-point operands",
                kind
            );
        }
        let source_width = value_width(source).context("floating intrinsic source has unsupported float width")?;
        let result_width =
            instruction_result_width(instruction)?.context("floating intrinsic result has no scalar width")?;
        checked_float_intrinsic_width(kind, source_width as u64)?;
        checked_float_intrinsic_width(kind, result_width as u64)?;
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
        if matches!(lhs.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(rhs.get_type(), BasicTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!("floating intrinsic {:?} does not support fixed vector lowering", kind);
            };
            return self.lower_vector_float_binop(instruction, rule, kind.semantic(), |width| {
                checked_float_intrinsic_width(kind, width as u64).map(|_| ())
            });
        }
        if !matches!(lhs.get_type(), BasicTypeEnum::FloatType(_))
            || !matches!(rhs.get_type(), BasicTypeEnum::FloatType(_))
        {
            bail!(
                "floating intrinsic {:?} only supports scalar floating-point operands",
                kind
            );
        }
        let lhs_width = value_width(lhs).context("floating intrinsic lhs has unsupported float width")?;
        let rhs_width = value_width(rhs).context("floating intrinsic rhs has unsupported float width")?;
        let result_width =
            instruction_result_width(instruction)?.context("floating intrinsic result has no scalar width")?;
        checked_float_intrinsic_width(kind, lhs_width as u64)?;
        checked_float_intrinsic_width(kind, rhs_width as u64)?;
        checked_float_intrinsic_width(kind, result_width as u64)?;
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

    fn lower_float_int_binary_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: FloatIntrinsicKind,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 2 {
            bail!(
                "floating/integer intrinsic {:?} expects exactly 2 arguments, got {actual_args}",
                kind
            );
        }

        let lhs = instruction_operand_value(instruction, 0).context("floating/integer intrinsic missing operand 0")?;
        let rhs = instruction_operand_value(instruction, 1).context("floating/integer intrinsic missing operand 1")?;
        if matches!(lhs.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!(
                    "floating/integer intrinsic {:?} does not support fixed vector lowering",
                    kind
                );
            };
            return self.lower_vector_float_int_binop(instruction, rule, kind.semantic(), |width| {
                checked_float_intrinsic_width(kind, width as u64).map(|_| ())
            });
        }
        if !matches!(lhs.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!(
                "floating/integer intrinsic {:?} first operand must be scalar float/double",
                kind
            );
        }
        if !matches!(rhs.get_type(), BasicTypeEnum::IntType(int_type) if int_type.get_bit_width() == 32) {
            bail!("floating/integer intrinsic {:?} second operand must be i32", kind);
        }
        let lhs_width = value_width(lhs).context("floating/integer intrinsic lhs has unsupported float width")?;
        let result_width =
            instruction_result_width(instruction)?.context("floating/integer intrinsic result has no scalar width")?;
        checked_float_intrinsic_width(kind, lhs_width as u64)?;
        checked_float_intrinsic_width(kind, result_width as u64)?;
        if lhs_width != result_width {
            bail!(
                "floating/integer intrinsic {:?} width mismatch: lhs i{}, result i{}",
                kind,
                lhs_width,
                result_width
            );
        }

        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", lhs_width as u64)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(kind.semantic()))?;
        Ok(())
    }

    fn lower_vector_float_int_binop(
        &mut self,
        instruction: InstructionValue<'ctx>,
        rule: &str,
        semantic: HandlerSemantic,
        validate_width: impl Fn(u8) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector floating/integer binop result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("vector floating/integer binop fields")?;
        let lhs_value =
            instruction_operand_value(instruction, 0).context("vector floating/integer binop missing operand 0")?;
        let lhs_fields =
            vector_fields_from_type(lhs_value.get_type()).context("vector floating/integer binop lhs fields")?;
        let rhs =
            instruction_operand_value(instruction, 1).context("vector floating/integer binop missing operand 1")?;
        if !matches!(rhs.get_type(), BasicTypeEnum::IntType(int_type) if int_type.get_bit_width() == 32) {
            bail!("vector floating/integer binop second operand must be scalar i32");
        }

        let lhs = self.vector_operand(instruction, 0)?;
        if lhs.fields.len() != result_fields.len() || lhs_fields.len() != result_fields.len() {
            bail!(
                "vector floating/integer binop lane count mismatch: result {}, lhs {}/{}",
                result_fields.len(),
                lhs.fields.len(),
                lhs_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            if result_info.kind != ScalarKind::Float || lhs_info.kind != ScalarKind::Float {
                bail!(
                    "vector floating/integer binop lane {index} requires float lanes, got result {:?}, lhs {:?}",
                    result_info.kind,
                    lhs_info.kind
                );
            }
            if lhs_info.width != result_info.width {
                bail!(
                    "vector floating/integer binop lane {index} width mismatch: result f{}, lhs f{}",
                    result_info.width,
                    lhs_info.width
                );
            }
            validate_width(result_info.width)?;
            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| {
                    format!("vector floating/integer binop lhs lane {index} is undefined or unsupported")
                })?;
            if lhs_binding.width != result_info.width {
                bail!(
                    "vector floating/integer binop lane {index} binding width mismatch: result f{}, lhs f{}",
                    result_info.width,
                    lhs_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .llvm_source("%b", rhs)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%b)", 32)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(semantic.clone()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector floating/integer binop lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
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
        if matches!(lhs.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(rhs.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(third.get_type(), BasicTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!("floating intrinsic {:?} does not support fixed vector lowering", kind);
            };
            return self.lower_vector_float_ternary(instruction, rule, kind.semantic(), |width| {
                checked_float_intrinsic_width(kind, width as u64).map(|_| ())
            });
        }
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
        checked_float_intrinsic_width(kind, lhs_width as u64)?;
        checked_float_intrinsic_width(kind, rhs_width as u64)?;
        checked_float_intrinsic_width(kind, third_width as u64)?;
        checked_float_intrinsic_width(kind, result_width as u64)?;
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

    fn lower_vector_float_ternary(
        &mut self,
        instruction: InstructionValue<'ctx>,
        rule: &str,
        semantic: HandlerSemantic,
        validate_width: impl Fn(u8) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector floating ternary result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vector floating ternary fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("vector floating ternary lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("vector floating ternary rhs fields")?;
        let third_fields = vector_fields_from_type(instruction_operand_value(instruction, 2)?.get_type())
            .context("vector floating ternary third fields")?;
        let lhs = self.vector_operand(instruction, 0)?;
        let rhs = self.vector_operand(instruction, 1)?;
        let third = self.vector_operand(instruction, 2)?;
        if lhs.fields.len() != result_fields.len()
            || rhs.fields.len() != result_fields.len()
            || third.fields.len() != result_fields.len()
            || lhs_fields.len() != result_fields.len()
            || rhs_fields.len() != result_fields.len()
            || third_fields.len() != result_fields.len()
        {
            bail!(
                "vector floating ternary lane count mismatch: result {}, lhs {}/{}, rhs {}/{}, third {}/{}",
                result_fields.len(),
                lhs.fields.len(),
                lhs_fields.len(),
                rhs.fields.len(),
                rhs_fields.len(),
                third.fields.len(),
                third_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            let third_info = third_fields[index];
            if result_info.kind != ScalarKind::Float
                || lhs_info.kind != ScalarKind::Float
                || rhs_info.kind != ScalarKind::Float
                || third_info.kind != ScalarKind::Float
            {
                bail!(
                    "vector floating ternary lane {index} requires float lanes, got result {:?}, lhs {:?}, rhs {:?}, third {:?}",
                    result_info.kind,
                    lhs_info.kind,
                    rhs_info.kind,
                    third_info.kind
                );
            }
            if lhs_info.width != result_info.width
                || rhs_info.width != result_info.width
                || third_info.width != result_info.width
            {
                bail!(
                    "vector floating ternary lane {index} width mismatch: result f{}, lhs f{}, rhs f{}, third f{}",
                    result_info.width,
                    lhs_info.width,
                    rhs_info.width,
                    third_info.width
                );
            }
            validate_width(result_info.width)?;
            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector floating ternary lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector floating ternary rhs lane {index} is undefined or unsupported"))?;
            let third_binding = third
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector floating ternary third lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != result_info.width
                || rhs_binding.width != result_info.width
                || third_binding.width != result_info.width
            {
                bail!(
                    "vector floating ternary lane {index} binding width mismatch: result f{}, lhs f{}, rhs f{}, third f{}",
                    result_info.width,
                    lhs_binding.width,
                    rhs_binding.width,
                    third_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .binding("%c_lane", third_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(semantic.clone()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector floating ternary lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
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

        let source = instruction_operand_value(instruction, 0).context("llvm.is.fpclass missing float operand 0")?;
        let mask = checked_fpclass_mask(constant_int_operand(instruction, 1, "llvm.is.fpclass mask")?)?;
        if matches!(source.get_type(), BasicTypeEnum::VectorType(_))
            || matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        {
            let Some(rule) = kind.vector_lowering_rule() else {
                bail!("llvm.is.fpclass does not support fixed vector lowering");
            };
            return self.lower_vector_is_fpclass_intrinsic(instruction, rule, mask);
        }

        let result_width =
            instruction_result_width(instruction)?.context("llvm.is.fpclass result has no scalar width")?;
        if result_width != 1 {
            bail!("llvm.is.fpclass must return i1, got i{result_width}");
        }

        if !matches!(source.get_type(), BasicTypeEnum::FloatType(_)) {
            bail!("llvm.is.fpclass only supports scalar floating-point operands");
        }
        let source_width = value_width(source).context("llvm.is.fpclass source has unsupported float width")?;
        checked_float_width(source_width as u64)?;

        let env = LoweringEnv::new()
            .llvm_source("%a", source)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", source_width as u64)
            .imm("type_width(%r)", result_width as u64)
            .imm("fpclass_mask(%r)", mask as u64);
        self.execute_lowering_rule(kind.lowering_rule(), env, Some(HandlerSemantic::FloatClass))?;
        Ok(())
    }

    fn lower_vector_is_fpclass_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        rule: &str,
        mask: u16,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector llvm.is.fpclass result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("vector llvm.is.fpclass result fields")?;
        let source_value =
            instruction_operand_value(instruction, 0).context("vector llvm.is.fpclass missing float operand 0")?;
        let source_fields =
            vector_fields_from_type(source_value.get_type()).context("vector llvm.is.fpclass source fields")?;
        let source = self.vector_operand(instruction, 0)?;
        if source.fields.len() != result_fields.len() || source_fields.len() != result_fields.len() {
            bail!(
                "vector llvm.is.fpclass lane count mismatch: result {}, source {}/{}",
                result_fields.len(),
                source.fields.len(),
                source_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let source_info = source_fields[index];
            if result_info.kind != ScalarKind::Integer || result_info.width != 1 {
                bail!(
                    "vector llvm.is.fpclass lane {index} result must be i1, got {:?}{}",
                    result_info.kind,
                    result_info.width
                );
            }
            if source_info.kind != ScalarKind::Float {
                bail!(
                    "vector llvm.is.fpclass lane {index} source must be float, got {:?}",
                    source_info.kind
                );
            }
            checked_float_width(source_info.width as u64)?;
            let source_binding = source
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector llvm.is.fpclass source lane {index} is undefined or unsupported"))?;
            if source_binding.width != source_info.width {
                bail!(
                    "vector llvm.is.fpclass lane {index} binding width mismatch: source f{}, binding i{}",
                    source_info.width,
                    source_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", source_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", source_info.width as u64)
                .imm("type_width(%r)", 1)
                .imm("fpclass_mask(%r)", mask as u64);
            let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::FloatClass))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector llvm.is.fpclass lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vp_is_fpclass_intrinsic(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 4 {
            bail!("llvm.vp.is.fpclass expects exactly 4 arguments, got {actual_args}");
        }
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("llvm.vp.is.fpclass result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("llvm.vp.is.fpclass result fields")?;
        let source_value = instruction_operand_value(instruction, 0).context("llvm.vp.is.fpclass missing source")?;
        let source_fields =
            vector_fields_from_type(source_value.get_type()).context("llvm.vp.is.fpclass source fields")?;
        if source_fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.is.fpclass source/result lane count mismatch: source {}, result {}",
                source_fields.len(),
                result_fields.len()
            );
        }
        let fpclass_mask =
            checked_fpclass_mask(constant_int_operand(instruction, 1, "llvm.vp.is.fpclass class mask")?)?;
        let mask_value = instruction_operand_value(instruction, 2).context("llvm.vp.is.fpclass missing VP mask")?;
        let mask = constant_i1_vector_mask(mask_value, result_fields.len(), "llvm.vp.is.fpclass VP mask")?;
        let evl = constant_int_operand(instruction, 3, "llvm.vp.is.fpclass evl")?;
        let source = self
            .vector_operand(instruction, 0)
            .context("llvm.vp.is.fpclass source vector")?;
        if source.fields.len() != result_fields.len() {
            bail!(
                "llvm.vp.is.fpclass binding lane count mismatch: source {}, result {}",
                source.fields.len(),
                result_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let source_info = source_fields[index];
            if result_info.kind != ScalarKind::Integer || result_info.width != 1 {
                bail!(
                    "llvm.vp.is.fpclass lane {index} result must be i1, got {:?}{}",
                    result_info.kind,
                    result_info.width
                );
            }
            if source_info.kind != ScalarKind::Float {
                bail!(
                    "llvm.vp.is.fpclass lane {index} source must be float, got {:?}",
                    source_info.kind
                );
            }
            checked_float_width(source_info.width as u64)?;
            if !mask[index] || index as u64 >= evl {
                fields.push(None);
                continue;
            }

            let source_binding = source
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("llvm.vp.is.fpclass source lane {index} is undefined or unsupported"))?;
            if source_binding.width != source_info.width {
                bail!(
                    "llvm.vp.is.fpclass lane {index} binding width mismatch: source f{}, binding i{}",
                    source_info.width,
                    source_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", source_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", source_info.width as u64)
                .imm("type_width(%r)", 1)
                .imm("fpclass_mask(%r)", fpclass_mask as u64);
            let env = self.execute_lowering_rule(
                "llvm.vp.vector.is.fpclass.float",
                env,
                Some(HandlerSemantic::FloatClass),
            )?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("llvm.vp.is.fpclass lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
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

    fn extend_vp_strided_stride(&mut self, rule: &str, stride: ValueBinding) -> anyhow::Result<ValueBinding> {
        if stride.width == 64 {
            return Ok(stride);
        }

        let action = self.emit_action_for_shape(
            rule,
            &HandlerSemantic::Cast(CastOp::SExt),
            &[
                ("dst", "%vwide"),
                ("src", "%vs"),
                ("from_width", "type_width(%stride)"),
                ("to_width", "64"),
            ],
        )?;
        let widened = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 64,
        };
        let env = LoweringEnv::new()
            .binding("%vs", stride)
            .binding("%vwide", widened)
            .imm("type_width(%stride)", stride.width as u64)
            .imm("64", 64);
        self.emit_profile_action(&action, &env)
            .with_context(|| format!("while lowering {rule} stride sign extension"))?;
        Ok(widened)
    }

    fn emit_vp_strided_lane_address(
        &mut self,
        rule: &str,
        mul: &LoweringAction,
        add: &LoweringAction,
        base: ValueBinding,
        stride: ValueBinding,
        lane: usize,
    ) -> anyhow::Result<ValueBinding> {
        if lane == 0 {
            return Ok(base);
        }

        let lane_reg = self.alloc_temporary_vreg()?;
        self.push_constant(
            lane_reg,
            u64::try_from(lane).context("vp.strided lane index does not fit u64")?,
            64,
        )?;
        let lane_index = ValueBinding {
            reg: lane_reg,
            width: 64,
        };
        let offset = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 64,
        };
        let address = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 64,
        };

        let mul_env = LoweringEnv::new()
            .binding("%vs", stride)
            .binding("%vwide", stride)
            .binding("%vi", lane_index)
            .binding("%vo", offset)
            .imm("64", 64);
        self.emit_profile_action(mul, &mul_env)
            .with_context(|| format!("while lowering {rule} lane {lane} stride multiply"))?;

        let add_env = LoweringEnv::new()
            .binding("%vp", base)
            .binding("%vo", offset)
            .binding("%addr", address)
            .imm("64", 64);
        self.emit_profile_action(add, &add_env)
            .with_context(|| format!("while lowering {rule} lane {lane} address add"))?;
        Ok(address)
    }

    fn native_call_final_returns(
        &mut self,
        instruction: InstructionValue<'ctx>,
        target: &NativeCallTarget<'ctx>,
    ) -> anyhow::Result<Vec<ValueBinding>> {
        if target.returns_void {
            return Ok(Vec::new());
        }

        if target.return_is_aggregate {
            return target
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
                .collect();
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

        bail!(
            "native scalar call return metadata has {} fields",
            target.return_fields.len()
        )
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
        if matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)) {
            return self.lower_vector_icmp(instruction, pred);
        }
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

    fn lower_vector_icmp(&mut self, instruction: InstructionValue<'ctx>, pred: IntPredicate) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector icmp result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vector icmp fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("vector icmp lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("vector icmp rhs fields")?;
        let lhs = self.vector_operand(instruction, 0)?;
        let rhs = self.vector_operand(instruction, 1)?;
        if lhs.fields.len() != result_fields.len()
            || rhs.fields.len() != result_fields.len()
            || lhs_fields.len() != result_fields.len()
            || rhs_fields.len() != result_fields.len()
        {
            bail!(
                "vector icmp lane count mismatch: result {}, lhs {}/{}, rhs {}/{}",
                result_fields.len(),
                lhs.fields.len(),
                lhs_fields.len(),
                rhs.fields.len(),
                rhs_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            if result_info.kind != ScalarKind::Integer || result_info.width != 1 {
                bail!(
                    "vector icmp lane {index} result must be i1, got {:?} i{}",
                    result_info.kind,
                    result_info.width
                );
            }
            let rule = match (lhs_info.kind, rhs_info.kind) {
                (ScalarKind::Integer, ScalarKind::Integer) => "llvm.vector.icmp.integer",
                (ScalarKind::Pointer, ScalarKind::Pointer) => "llvm.vector.icmp.pointer",
                (lhs_kind, rhs_kind) => {
                    bail!(
                        "vector icmp lane {index} requires matching integer or pointer lanes, got lhs {lhs_kind:?}, rhs {rhs_kind:?}"
                    )
                },
            };
            if lhs_info.width != rhs_info.width {
                bail!(
                    "vector icmp lane {index} operand width mismatch: lhs {}{}, rhs {}{}",
                    scalar_kind_prefix(lhs_info.kind),
                    lhs_info.width,
                    scalar_kind_prefix(rhs_info.kind),
                    rhs_info.width
                );
            }
            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector icmp lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector icmp rhs lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != lhs_info.width || rhs_binding.width != rhs_info.width {
                bail!(
                    "vector icmp lane {index} binding width mismatch: lhs type {}{}, lhs binding i{}, rhs type {}{}, rhs binding i{}",
                    scalar_kind_prefix(lhs_info.kind),
                    lhs_info.width,
                    lhs_binding.width,
                    scalar_kind_prefix(rhs_info.kind),
                    rhs_info.width,
                    rhs_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .imm("lane(%r)", index as u64)
                .imm("predicate(%r)", map_predicate(pred) as u64)
                .imm("operand_width(%a,%b)", lhs_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::Icmp))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector icmp lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_fcmp(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let lhs = instruction_operand_value(instruction, 0)?;
        let rhs = instruction_operand_value(instruction, 1)?;
        let pred = instruction
            .get_fcmp_predicate()
            .context("fcmp instruction has no predicate")?;
        let vm_predicate = map_float_predicate(pred);
        if matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)) {
            return self.lower_vector_fcmp(instruction, vm_predicate, "llvm.vector.fcmp.float");
        }
        let lhs_width = value_width(lhs)?;
        let rhs_width = value_width(rhs)?;
        if lhs_width != rhs_width {
            bail!("fcmp operands have mismatched widths: {lhs_width} and {rhs_width}");
        }

        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("predicate(%r)", vm_predicate as u64)
            .imm("operand_width(%a,%b)", lhs_width as u64);
        self.execute_lowering_rule("llvm.fcmp.float", env, Some(HandlerSemantic::Fcmp))?;
        Ok(())
    }

    fn lower_vector_fcmp(
        &mut self,
        instruction: InstructionValue<'ctx>,
        pred: VmFloatPredicate,
        rule: &'static str,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector fcmp result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vector fcmp fields")?;
        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("vector fcmp lhs fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("vector fcmp rhs fields")?;
        let lhs = self.vector_operand(instruction, 0)?;
        let rhs = self.vector_operand(instruction, 1)?;
        if lhs.fields.len() != result_fields.len()
            || rhs.fields.len() != result_fields.len()
            || lhs_fields.len() != result_fields.len()
            || rhs_fields.len() != result_fields.len()
        {
            bail!(
                "vector fcmp lane count mismatch: result {}, lhs {}/{}, rhs {}/{}",
                result_fields.len(),
                lhs.fields.len(),
                lhs_fields.len(),
                rhs.fields.len(),
                rhs_fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let lhs_info = lhs_fields[index];
            let rhs_info = rhs_fields[index];
            if result_info.kind != ScalarKind::Integer || result_info.width != 1 {
                bail!(
                    "vector fcmp lane {index} result must be i1, got {:?} i{}",
                    result_info.kind,
                    result_info.width
                );
            }
            if lhs_info.kind != ScalarKind::Float || rhs_info.kind != ScalarKind::Float {
                bail!(
                    "vector fcmp lane {index} requires floating lanes, got lhs {:?}, rhs {:?}",
                    lhs_info.kind,
                    rhs_info.kind
                );
            }
            if lhs_info.width != rhs_info.width {
                bail!(
                    "vector fcmp lane {index} operand width mismatch: lhs f{}, rhs f{}",
                    lhs_info.width,
                    rhs_info.width
                );
            }
            let lhs_binding = lhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector fcmp lhs lane {index} is undefined or unsupported"))?;
            let rhs_binding = rhs
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector fcmp rhs lane {index} is undefined or unsupported"))?;
            if lhs_binding.width != lhs_info.width || rhs_binding.width != rhs_info.width {
                bail!(
                    "vector fcmp lane {index} binding width mismatch: lhs type f{}, lhs binding i{}, rhs type f{}, rhs binding i{}",
                    lhs_info.width,
                    lhs_binding.width,
                    rhs_info.width,
                    rhs_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", lhs_binding)
                .binding("%b_lane", rhs_binding)
                .imm("lane(%r)", index as u64)
                .imm("predicate(%r)", pred as u64)
                .imm("operand_width(%a,%b)", lhs_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::Fcmp))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector fcmp lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_cast(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let src = instruction_operand_value(instruction, 0)?;
        if matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)) {
            return match instruction.get_opcode() {
                InstructionOpcode::ZExt | InstructionOpcode::SExt | InstructionOpcode::Trunc => {
                    self.lower_vector_integer_cast(instruction, src)
                },
                InstructionOpcode::PtrToInt | InstructionOpcode::IntToPtr | InstructionOpcode::AddrSpaceCast => {
                    self.lower_vector_pointer_cast(instruction, src)
                },
                InstructionOpcode::BitCast => self.lower_vector_bitcast(instruction, src),
                opcode => bail!("unsupported vector cast opcode: {opcode:?}"),
            };
        }
        self.ensure_scalar_pointer_cast_address_spaces(instruction, src)?;
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

    fn ensure_scalar_pointer_cast_address_spaces(
        &self,
        instruction: InstructionValue<'ctx>,
        src: BasicValueEnum<'ctx>,
    ) -> anyhow::Result<()> {
        match instruction.get_opcode() {
            InstructionOpcode::PtrToInt => {
                self.ensure_no_non_integral_pointer_type_ref(src.get_type().as_type_ref(), "ptrtoint source")
            },
            InstructionOpcode::IntToPtr => {
                let AnyTypeEnum::PointerType(result_ty) = instruction.get_type() else {
                    bail!("inttoptr result must be a pointer");
                };
                self.ensure_no_non_integral_pointer_type_ref(result_ty.as_type_ref(), "inttoptr result")
            },
            InstructionOpcode::AddrSpaceCast => {
                self.ensure_no_non_integral_pointer_type_ref(src.get_type().as_type_ref(), "addrspacecast source")?;
                let AnyTypeEnum::PointerType(result_ty) = instruction.get_type() else {
                    bail!("addrspacecast result must be a pointer");
                };
                self.ensure_no_non_integral_pointer_type_ref(result_ty.as_type_ref(), "addrspacecast result")
            },
            _ => Ok(()),
        }
    }

    fn ensure_no_non_integral_pointer_type_ref(&self, ty: LLVMTypeRef, context: &str) -> anyhow::Result<()> {
        ensure_no_non_integral_pointer_type_ref(&self.non_integral_address_spaces, ty, context)
    }

    fn lower_vector_integer_cast(
        &mut self,
        instruction: InstructionValue<'ctx>,
        src: BasicValueEnum<'ctx>,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector integer cast result must be a fixed vector");
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("vector integer cast result fields")?;
        let src_fields = vector_fields_from_type(src.get_type()).context("vector integer cast source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "vector integer cast requires equal lane counts, got source {} and result {}",
                src_fields.len(),
                result_fields.len()
            );
        }

        let (semantic, width_check): (HandlerSemantic, fn(u8, u8) -> bool) = match instruction.get_opcode() {
            InstructionOpcode::ZExt => (HandlerSemantic::Cast(CastOp::ZExt), |from, to| from < to),
            InstructionOpcode::SExt => (HandlerSemantic::Cast(CastOp::SExt), |from, to| from < to),
            InstructionOpcode::Trunc => (HandlerSemantic::Cast(CastOp::Trunc), |from, to| from > to),
            opcode => bail!("unsupported vector integer cast opcode: {opcode:?}"),
        };

        let src_vector = self.vector_operand(instruction, 0)?;
        if src_vector.fields.len() != src_fields.len() {
            bail!(
                "vector integer cast source field count mismatch: type has {}, value has {}",
                src_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if src_info.kind != ScalarKind::Integer || result_info.kind != ScalarKind::Integer {
                bail!(
                    "vector integer cast lane {index} requires integer lanes, got source {:?} and result {:?}",
                    src_info.kind,
                    result_info.kind
                );
            }
            if !width_check(src_info.width, result_info.width) {
                bail!(
                    "vector integer cast lane {index} has invalid width transition i{} -> i{}",
                    src_info.width,
                    result_info.width
                );
            }
            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector integer cast source lane {index} is undefined or unsupported"))?;
            if src_binding.width != src_info.width {
                bail!(
                    "vector integer cast lane {index} binding width mismatch: source type i{}, binding i{}",
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", src_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule("llvm.vector.cast.integer", env, Some(semantic.clone()))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector integer cast lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vector_pointer_cast(
        &mut self,
        instruction: InstructionValue<'ctx>,
        src: BasicValueEnum<'ctx>,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector pointer cast result must be a fixed vector");
        };
        self.ensure_no_non_integral_pointer_type_ref(src.get_type().as_type_ref(), "vector pointer cast source")?;
        self.ensure_no_non_integral_pointer_type_ref(result_ty.as_type_ref(), "vector pointer cast result")?;
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .context("vector pointer cast result fields")?;
        let src_fields = vector_fields_from_type(src.get_type()).context("vector pointer cast source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "vector pointer cast requires equal lane counts, got source {} and result {}",
                src_fields.len(),
                result_fields.len()
            );
        }

        let src_vector = self.vector_operand(instruction, 0)?;
        if src_vector.fields.len() != src_fields.len() {
            bail!(
                "vector pointer cast source field count mismatch: type has {}, value has {}",
                src_fields.len(),
                src_vector.fields.len()
            );
        }

        let opcode = instruction.get_opcode();
        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            let semantic = match opcode {
                InstructionOpcode::PtrToInt => {
                    if src_info.kind != ScalarKind::Pointer || result_info.kind != ScalarKind::Integer {
                        bail!(
                            "vector ptrtoint lane {index} requires pointer -> integer, got {}{} -> {}{}",
                            scalar_kind_prefix(src_info.kind),
                            src_info.width,
                            scalar_kind_prefix(result_info.kind),
                            result_info.width
                        );
                    }
                    if result_info.width < src_info.width {
                        HandlerSemantic::Cast(CastOp::Trunc)
                    } else if result_info.width == src_info.width {
                        HandlerSemantic::Cast(CastOp::Bitcast)
                    } else {
                        bail!(
                            "vector ptrtoint lane {index} cannot widen pointer width {} to integer width {}",
                            src_info.width,
                            result_info.width
                        );
                    }
                },
                InstructionOpcode::IntToPtr => {
                    if src_info.kind != ScalarKind::Integer || result_info.kind != ScalarKind::Pointer {
                        bail!(
                            "vector inttoptr lane {index} requires integer -> pointer, got {}{} -> {}{}",
                            scalar_kind_prefix(src_info.kind),
                            src_info.width,
                            scalar_kind_prefix(result_info.kind),
                            result_info.width
                        );
                    }
                    if src_info.width < result_info.width {
                        HandlerSemantic::Cast(CastOp::ZExt)
                    } else if src_info.width == result_info.width {
                        HandlerSemantic::Cast(CastOp::Bitcast)
                    } else {
                        bail!(
                            "vector inttoptr lane {index} cannot narrow integer width {} to pointer width {}",
                            src_info.width,
                            result_info.width
                        );
                    }
                },
                InstructionOpcode::AddrSpaceCast => {
                    if src_info.kind != ScalarKind::Pointer || result_info.kind != ScalarKind::Pointer {
                        bail!(
                            "vector addrspacecast lane {index} requires pointer -> pointer, got {}{} -> {}{}",
                            scalar_kind_prefix(src_info.kind),
                            src_info.width,
                            scalar_kind_prefix(result_info.kind),
                            result_info.width
                        );
                    }
                    if src_info.width != result_info.width {
                        bail!(
                            "vector addrspacecast lane {index} requires equal pointer widths, got {} and {}",
                            src_info.width,
                            result_info.width
                        );
                    }
                    HandlerSemantic::Cast(CastOp::Bitcast)
                },
                opcode => bail!("unsupported vector pointer cast opcode: {opcode:?}"),
            };
            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector pointer cast source lane {index} is undefined or unsupported"))?;
            if src_binding.width != src_info.width {
                bail!(
                    "vector pointer cast lane {index} binding width mismatch: source type i{}, binding i{}",
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", src_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule("llvm.vector.cast.pointer", env, Some(semantic))?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector pointer cast lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_vector_bitcast(
        &mut self,
        instruction: InstructionValue<'ctx>,
        src: BasicValueEnum<'ctx>,
    ) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector bitcast result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vector bitcast result fields")?;
        let src_fields = vector_fields_from_type(src.get_type()).context("vector bitcast source fields")?;
        if src_fields.len() != result_fields.len() {
            bail!(
                "vector bitcast requires equal lane counts, got source {} and result {}",
                src_fields.len(),
                result_fields.len()
            );
        }

        let src_vector = self.vector_operand(instruction, 0)?;
        if src_vector.fields.len() != src_fields.len() {
            bail!(
                "vector bitcast source field count mismatch: type has {}, value has {}",
                src_fields.len(),
                src_vector.fields.len()
            );
        }

        let mut fields = Vec::with_capacity(result_fields.len());
        for (index, result_info) in result_fields.iter().copied().enumerate() {
            let src_info = src_fields[index];
            if src_info.width != result_info.width {
                bail!(
                    "vector bitcast lane {index} requires equal lane widths, got source i{} and result i{}",
                    src_info.width,
                    result_info.width
                );
            }
            let src_binding = src_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| format!("vector bitcast source lane {index} is undefined or unsupported"))?;
            if src_binding.width != src_info.width {
                bail!(
                    "vector bitcast lane {index} binding width mismatch: source type i{}, binding i{}",
                    src_info.width,
                    src_binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%a_lane", src_binding)
                .imm("lane(%r)", index as u64)
                .imm("type_width(%a)", src_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(
                "llvm.vector.bitcast.element",
                env,
                Some(HandlerSemantic::Cast(CastOp::Bitcast)),
            )?;
            let stable = match env.get("%r")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector bitcast lowering must produce a lane register")
                },
            };
            fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields });
        Ok(())
    }

    fn lower_freeze(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let value = instruction_operand_value(instruction, 0)?;
        match value.get_type() {
            BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_) => {
                return self.lower_aggregate_freeze(instruction, value);
            },
            BasicTypeEnum::VectorType(_) => {
                return self.lower_vector_freeze(instruction, value);
            },
            _ => {},
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
            self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: Vec::new() });
            return Ok(());
        }

        let source = if is_undef_or_poison_value(value) {
            AggregateBinding {
                fields: vec![None; field_infos.len()],
            }
        } else if let Some(binding) = self.aggregates.get(&value_key(value)).cloned() {
            binding
        } else if let Some(binding) = self.constant_aggregate_binding(value, true)? {
            binding
        } else {
            bail!("aggregate freeze operand was not built by supported aggregate lowering");
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

    fn lower_vector_freeze(
        &mut self,
        instruction: InstructionValue<'ctx>,
        value: BasicValueEnum<'ctx>,
    ) -> anyhow::Result<()> {
        let field_infos = vector_fields_from_type(value.get_type()).context("freeze vector fields")?;
        let source = if is_undef_or_poison_value(value) {
            AggregateBinding {
                fields: vec![None; field_infos.len()],
            }
        } else if let Some(binding) = self.aggregates.get(&value_key(value)).cloned() {
            binding
        } else if let Some(binding) = self.constant_vector_binding(value, true, false)? {
            binding
        } else {
            bail!("vector freeze operand was not built by supported vector lowering");
        };
        if source.fields.len() != field_infos.len() {
            bail!(
                "vector freeze lane count mismatch: value has {}, type has {}",
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
                    "vector freeze lane {index} width mismatch: value is {}, type expects {}",
                    src.width,
                    info.width
                );
            }
            let env = LoweringEnv::new()
                .binding("%value", src)
                .imm("type_width(%field)", info.width as u64)
                .imm("type_width(%r)", info.width as u64);
            let env = self.execute_lowering_rule("llvm.vector.freeze", env, Some(HandlerSemantic::Mov))?;
            let stable = match env.get("%vr")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("vector freeze lowering must produce a lane register")
                },
            };
            frozen_fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: frozen_fields });
        Ok(())
    }

    fn lower_select(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        match instruction.get_type() {
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_) => {
                return self.lower_aggregate_select(instruction);
            },
            AnyTypeEnum::VectorType(_) => {
                return self.lower_vector_select(instruction);
            },
            _ => {},
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
            let _ = self.materialize_operand(instruction, 0)?;
            self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: Vec::new() });
            return Ok(());
        }

        let cond = self.materialize_operand(instruction, 0)?;
        let then_aggregate = self
            .aggregate_operand_or_constant(instruction, 1)
            .context("select then aggregate operand")?;
        let else_aggregate = self
            .aggregate_operand_or_constant(instruction, 2)
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

    fn lower_vector_select(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector select result must be a fixed vector");
        };
        let field_infos =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("vector select result fields")?;
        let cond_value = instruction_operand_value(instruction, 0)?;
        if matches!(cond_value.get_type(), BasicTypeEnum::VectorType(_)) {
            return self.lower_vector_condition_select(instruction, cond_value, field_infos);
        }

        let cond = self.materialize_value(cond_value)?;
        if cond.width != 1 {
            bail!("vector select only supports scalar i1 condition, got i{}", cond.width);
        }

        let then_vector = self
            .vector_operand(instruction, 1)
            .context("select then vector operand")?;
        let else_vector = self
            .vector_operand(instruction, 2)
            .context("select else vector operand")?;
        if then_vector.fields.len() != field_infos.len() || else_vector.fields.len() != field_infos.len() {
            bail!(
                "vector select lane count mismatch: type has {}, then has {}, else has {}",
                field_infos.len(),
                then_vector.fields.len(),
                else_vector.fields.len()
            );
        }

        let actions = self.select_lowering_actions("llvm.select.vector", "type_width(%field)")?;
        let mut lane_moves = Vec::with_capacity(field_infos.len());
        let mut result_fields = Vec::with_capacity(field_infos.len());
        for (index, info) in field_infos.iter().copied().enumerate() {
            let then_field = then_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("vector select then lane {index} is undefined or unsupported"))?;
            let else_field = else_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("vector select else lane {index} is undefined or unsupported"))?;
            if then_field.binding.width != info.width || else_field.binding.width != info.width {
                bail!(
                    "vector select lane {index} width mismatch: type i{}, then i{}, else i{}",
                    info.width,
                    then_field.binding.width,
                    else_field.binding.width
                );
            }

            let dst = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: info.width,
            };
            lane_moves.push((info, dst, then_field.binding, else_field.binding));
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
        for (info, dst, then_value, _) in &lane_moves {
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
        for (info, dst, _, else_value) in &lane_moves {
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

    fn lower_vector_condition_select(
        &mut self,
        instruction: InstructionValue<'ctx>,
        cond_value: BasicValueEnum<'ctx>,
        field_infos: Vec<ReturnField>,
    ) -> anyhow::Result<()> {
        let cond_infos = vector_fields_from_type(cond_value.get_type()).context("vector condition select fields")?;
        if cond_infos.len() != field_infos.len() || cond_infos.iter().any(|field| field.width != 1) {
            bail!(
                "vector condition select requires <N x i1> condition matching result lanes: cond has {}, result has {}",
                cond_infos.len(),
                field_infos.len()
            );
        }

        let cond_vector = self
            .vector_operand(instruction, 0)
            .context("select condition vector operand")?;
        let then_vector = self
            .vector_operand(instruction, 1)
            .context("select then vector operand")?;
        let else_vector = self
            .vector_operand(instruction, 2)
            .context("select else vector operand")?;
        if cond_vector.fields.len() != field_infos.len()
            || then_vector.fields.len() != field_infos.len()
            || else_vector.fields.len() != field_infos.len()
        {
            bail!(
                "vector condition select lane count mismatch: type has {}, cond has {}, then has {}, else has {}",
                field_infos.len(),
                cond_vector.fields.len(),
                then_vector.fields.len(),
                else_vector.fields.len()
            );
        }

        let actions = self.select_lowering_actions("llvm.select.vector_condition", "type_width(%field)")?;
        let mut result_fields = Vec::with_capacity(field_infos.len());
        for (index, info) in field_infos.iter().copied().enumerate() {
            let cond_field = cond_vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("vector condition select condition lane {index} is undefined"))?;
            let then_field =
                then_vector.fields.get(index).copied().flatten().with_context(|| {
                    format!("vector condition select then lane {index} is undefined or unsupported")
                })?;
            let else_field =
                else_vector.fields.get(index).copied().flatten().with_context(|| {
                    format!("vector condition select else lane {index} is undefined or unsupported")
                })?;
            if cond_field.binding.width != 1 {
                bail!(
                    "vector condition select condition lane {index} must be i1, got i{}",
                    cond_field.binding.width
                );
            }
            if then_field.binding.width != info.width || else_field.binding.width != info.width {
                bail!(
                    "vector condition select lane {index} width mismatch: type i{}, then i{}, else i{}",
                    info.width,
                    then_field.binding.width,
                    else_field.binding.width
                );
            }

            let dst = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: info.width,
            };
            let then_label = self.builder.new_label();
            let else_label = self.builder.new_label();
            let join_label = self.builder.new_label();
            let branch_env = LoweringEnv::new()
                .binding("%vc", cond_field.binding)
                .label("then_label", then_label)
                .label("else_label", else_label);
            self.emit_profile_action(&actions.br_if, &branch_env)?;

            self.builder.bind_label(then_label);
            let then_env = LoweringEnv::new()
                .binding("%vr", dst)
                .binding("%vt", then_field.binding)
                .imm("type_width(%field)", info.width as u64)
                .label("join_label", join_label);
            self.emit_profile_action(&actions.then_mov, &then_env)?;
            self.emit_profile_action(&actions.br, &then_env)?;

            self.builder.bind_label(else_label);
            let else_env = LoweringEnv::new()
                .binding("%vr", dst)
                .binding("%ve", else_field.binding)
                .imm("type_width(%field)", info.width as u64)
                .label("join_label", join_label);
            self.emit_profile_action(&actions.else_mov, &else_env)?;
            self.emit_profile_action(&actions.br, &else_env)?;

            self.builder.bind_label(join_label);
            result_fields.push(Some(AggregateField::owned(dst)));
        }

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
        let inserted_aggregate = if let Some(binding) = self.aggregates.get(&value_key(inserted)).cloned() {
            binding
        } else if let Some(binding) = self.constant_aggregate_binding(inserted, false)? {
            binding
        } else {
            bail!("insertvalue subaggregate operand was not built by supported aggregate lowering");
        };
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
        let aggregate = self.aggregate_operand_or_constant(instruction, 0)?;
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

    fn lower_insert_element(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let vector_value = instruction_operand_value(instruction, 0)?;
        let inserted = instruction_operand_value(instruction, 1)?;
        let Some(lane_index) = vector_lane_index(instruction, 2)? else {
            return self.lower_dynamic_insert_element(instruction, vector_value, inserted);
        };
        let fields = vector_fields_from_type(vector_value.get_type()).context("insertelement vector fields")?;
        let field = fields
            .get(lane_index)
            .copied()
            .with_context(|| format!("insertelement lane {lane_index} is out of range"))?;
        let inserted_field = return_field_from_type(inserted.get_type()).context("insertelement scalar lane")?;
        if inserted_field != field {
            bail!(
                "insertelement lane type mismatch: inserted {:?} width {}, vector lane {:?} width {}",
                inserted_field.kind,
                inserted_field.width,
                field.kind,
                field.width
            );
        }

        let mut vector = self.vector_seed_from_operand(instruction, 0)?;
        if vector.fields.len() != fields.len() {
            bail!(
                "insertelement vector field count mismatch: type has {}, value has {}",
                fields.len(),
                vector.fields.len()
            );
        }
        let env = LoweringEnv::new()
            .llvm_source("%element", inserted)
            .imm("lane(%r)", lane_index as u64)
            .imm("type_width(%r)", inserted_field.width as u64)
            .imm("type_width(%element)", inserted_field.width as u64);
        let env = self.execute_lowering_rule("llvm.vector.insert.element", env, Some(HandlerSemantic::Mov))?;
        let stable = match env.get("%r")? {
            LoweringValue::Reg(binding) => binding,
            LoweringValue::Imm(_) | LoweringValue::Label(_) => bail!("vector insert bind must produce a register"),
        };
        let slot = vector
            .fields
            .get_mut(lane_index)
            .with_context(|| format!("insertelement lane {lane_index} is out of range"))?;
        *slot = Some(AggregateField::owned(stable));
        self.insert_aggregate_value(instruction_key(instruction), vector);
        Ok(())
    }

    fn lower_dynamic_insert_element(
        &mut self,
        instruction: InstructionValue<'ctx>,
        vector_value: BasicValueEnum<'ctx>,
        inserted: BasicValueEnum<'ctx>,
    ) -> anyhow::Result<()> {
        let fields = vector_fields_from_type(vector_value.get_type()).context("dynamic insertelement vector fields")?;
        let inserted_field =
            return_field_from_type(inserted.get_type()).context("dynamic insertelement scalar lane")?;
        if fields.iter().any(|field| *field != inserted_field) {
            bail!(
                "dynamic insertelement lane type mismatch: inserted {:?} width {}",
                inserted_field.kind,
                inserted_field.width
            );
        }

        let vector = self
            .vector_operand(instruction, 0)
            .context("dynamic insertelement base vector")?;
        if vector.fields.len() != fields.len() {
            bail!(
                "dynamic insertelement vector field count mismatch: type has {}, value has {}",
                fields.len(),
                vector.fields.len()
            );
        }
        let source_lanes = vector
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                (*field)
                    .map(|field| field.binding)
                    .with_context(|| format!("dynamic insertelement base lane {index} is undefined or unavailable"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let inserted = self.materialize_value(inserted)?;
        let index = self.materialize_vector_lane_index(instruction, 2)?;
        let actions = self.dynamic_lane_actions("llvm.vector.insert.dynamic_element")?;
        let inserted_mov = self.emit_action_for_shape(
            "llvm.vector.insert.dynamic_element",
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%ve"), ("width", "type_width(%element)")],
        )?;
        let original_mov = self.emit_action_for_shape(
            "llvm.vector.insert.dynamic_element",
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vo"), ("width", "type_width(%element)")],
        )?;

        let mut result_fields = Vec::with_capacity(fields.len());
        for (lane_index, (field, original)) in fields.iter().copied().zip(source_lanes).enumerate() {
            if original.width != field.width || inserted.width != field.width {
                bail!(
                    "dynamic insertelement lane {lane_index} width mismatch: type i{}, original i{}, inserted i{}",
                    field.width,
                    original.width,
                    inserted.width
                );
            }

            let dst = ValueBinding {
                reg: self.builder.alloc_vreg()?,
                width: field.width,
            };
            let inserted_label = self.builder.new_label();
            let original_label = self.builder.new_label();
            let join_label = self.builder.new_label();
            self.emit_dynamic_lane_test(&actions, index, lane_index, inserted_label, original_label)?;

            self.builder.bind_label(inserted_label);
            let inserted_env = LoweringEnv::new()
                .binding("%vr", dst)
                .binding("%ve", inserted)
                .imm("type_width(%element)", field.width as u64)
                .label("join_label", join_label);
            self.emit_profile_action(&inserted_mov, &inserted_env)?;
            self.emit_profile_action(&actions.br, &inserted_env)?;

            self.builder.bind_label(original_label);
            let original_env = LoweringEnv::new()
                .binding("%vr", dst)
                .binding("%vo", original)
                .imm("type_width(%element)", field.width as u64);
            self.emit_profile_action(&original_mov, &original_env)?;

            self.builder.bind_label(join_label);
            result_fields.push(Some(AggregateField::owned(dst)));
        }

        self.insert_aggregate_value(instruction_key(instruction), AggregateBinding { fields: result_fields });
        Ok(())
    }

    fn lower_extract_element(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let vector_value = instruction_operand_value(instruction, 0)?;
        let Some(lane_index) = vector_lane_index(instruction, 1)? else {
            return self.lower_dynamic_extract_element(instruction, vector_value);
        };
        let fields = vector_fields_from_type(vector_value.get_type()).context("extractelement vector fields")?;
        let field = fields
            .get(lane_index)
            .copied()
            .with_context(|| format!("extractelement lane {lane_index} is out of range"))?;
        let vector = self.vector_operand(instruction, 0)?;
        if vector.fields.len() != fields.len() {
            bail!(
                "extractelement vector field count mismatch: type has {}, value has {}",
                fields.len(),
                vector.fields.len()
            );
        }
        let src = vector
            .fields
            .get(lane_index)
            .copied()
            .flatten()
            .map(|field| field.binding)
            .with_context(|| format!("extractelement lane {lane_index} is undefined or unavailable"))?;
        let result_width =
            instruction_result_width(instruction)?.context("extractelement result has no scalar width")?;
        if result_width != field.width || src.width != field.width {
            bail!(
                "extractelement lane width mismatch: result i{}, value i{}, vector lane i{}",
                result_width,
                src.width,
                field.width
            );
        }

        let env = LoweringEnv::new()
            .binding("%lane", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("lane(%r)", lane_index as u64)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule("llvm.vector.extract.element", env, Some(HandlerSemantic::Mov))?;
        Ok(())
    }

    fn lower_vector_extract_last_active_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
    ) -> anyhow::Result<()> {
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != 3 {
            bail!("llvm.experimental.vector.extract.last.active expects exactly 3 arguments, got {actual_args}");
        }

        let vector_value = instruction_operand_value(instruction, 0)?;
        let fields = vector_fields_from_type(vector_value.get_type())
            .context("llvm.experimental.vector.extract.last.active vector")?;
        let mask_value = instruction_operand_value(instruction, 1)
            .context("llvm.experimental.vector.extract.last.active missing mask")?;
        let mask = constant_i1_vector_mask(
            mask_value,
            fields.len(),
            "llvm.experimental.vector.extract.last.active mask",
        )?;
        let lane_index = mask.iter().rposition(|is_active| *is_active);

        let passthru_value = instruction_operand_value(instruction, 2)
            .context("llvm.experimental.vector.extract.last.active missing passthru")?;
        let passthru_field = return_field_from_type(passthru_value.get_type())
            .context("llvm.experimental.vector.extract.last.active passthru")?;
        let result_field = return_field_from_any_scalar_type(instruction.get_type())
            .context("llvm.experimental.vector.extract.last.active result")?;
        if passthru_field != result_field {
            bail!(
                "llvm.experimental.vector.extract.last.active passthru/result mismatch: passthru {:?}{}, result {:?}{}",
                passthru_field.kind,
                passthru_field.width,
                result_field.kind,
                result_field.width
            );
        }

        let src = if let Some(index) = lane_index {
            let field = fields[index];
            if field != result_field {
                bail!(
                    "llvm.experimental.vector.extract.last.active lane {index} result mismatch: lane {:?}{}, result {:?}{}",
                    field.kind,
                    field.width,
                    result_field.kind,
                    result_field.width
                );
            }
            let vector = self.vector_operand(instruction, 0)?;
            if vector.fields.len() != fields.len() {
                bail!(
                    "llvm.experimental.vector.extract.last.active vector field count mismatch: type has {}, value has {}",
                    fields.len(),
                    vector.fields.len()
                );
            }
            vector
                .fields
                .get(index)
                .copied()
                .flatten()
                .map(|field| field.binding)
                .with_context(|| {
                    format!("llvm.experimental.vector.extract.last.active lane {index} is undefined or unavailable")
                })?
        } else {
            self.materialize_operand(instruction, 2)?
        };

        if src.width != result_field.width {
            bail!(
                "llvm.experimental.vector.extract.last.active selected value width mismatch: result {}, selected {}",
                result_field.width,
                src.width
            );
        }

        let env = LoweringEnv::new()
            .binding("%value", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", u64::from(result_field.width));
        self.execute_lowering_rule(
            "llvm.experimental.vector.extract.last.active",
            env,
            Some(HandlerSemantic::Mov),
        )?;
        Ok(())
    }

    fn lower_dynamic_extract_element(
        &mut self,
        instruction: InstructionValue<'ctx>,
        vector_value: BasicValueEnum<'ctx>,
    ) -> anyhow::Result<()> {
        let fields =
            vector_fields_from_type(vector_value.get_type()).context("dynamic extractelement vector fields")?;
        let vector = self
            .vector_operand(instruction, 0)
            .context("dynamic extractelement vector operand")?;
        if vector.fields.len() != fields.len() {
            bail!(
                "dynamic extractelement vector field count mismatch: type has {}, value has {}",
                fields.len(),
                vector.fields.len()
            );
        }

        let lanes = vector
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                (*field)
                    .map(|field| field.binding)
                    .with_context(|| format!("dynamic extractelement lane {index} is undefined or unavailable"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let result = self.ensure_result_binding(instruction)?;
        if fields
            .iter()
            .zip(&lanes)
            .any(|(field, lane)| field.width != result.width || lane.width != result.width)
        {
            bail!("dynamic extractelement lane/result width mismatch");
        }

        let index = self.materialize_vector_lane_index(instruction, 1)?;
        let actions = self.dynamic_lane_actions("llvm.vector.extract.dynamic_element")?;
        let lane_mov = self.emit_action_for_shape(
            "llvm.vector.extract.dynamic_element",
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vl"), ("width", "type_width(%r)")],
        )?;
        let default_lane = lanes
            .first()
            .copied()
            .context("dynamic extractelement requires at least one lane")?;
        let default_label = self.builder.new_label();
        let join_label = self.builder.new_label();

        for (lane_index, lane) in lanes.iter().copied().enumerate() {
            let case_label = self.builder.new_label();
            let next_label = if lane_index + 1 == lanes.len() {
                default_label
            } else {
                self.builder.new_label()
            };
            self.emit_dynamic_lane_test(&actions, index, lane_index, case_label, next_label)?;

            self.builder.bind_label(case_label);
            let case_env = LoweringEnv::new()
                .binding("%vr", result)
                .binding("%vl", lane)
                .imm("type_width(%r)", result.width as u64)
                .label("join_label", join_label);
            self.emit_profile_action(&lane_mov, &case_env)?;
            self.emit_profile_action(&actions.br, &case_env)?;

            if lane_index + 1 != lanes.len() {
                self.builder.bind_label(next_label);
            }
        }

        self.builder.bind_label(default_label);
        let default_env = LoweringEnv::new()
            .binding("%vr", result)
            .binding("%vl", default_lane)
            .imm("type_width(%r)", result.width as u64);
        self.emit_profile_action(&lane_mov, &default_env)?;
        self.builder.bind_label(join_label);
        Ok(())
    }

    fn materialize_vector_lane_index(
        &mut self,
        instruction: InstructionValue<'ctx>,
        operand_index: u32,
    ) -> anyhow::Result<ValueBinding> {
        let value = instruction_operand_value(instruction, operand_index)?;
        if is_undef_or_poison_value(value) {
            bail!("dynamic vector lane index cannot be undef or poison");
        }
        if !value.is_int_value() {
            bail!("dynamic vector lane index must be an integer");
        }
        self.materialize_value(value)
    }

    fn emit_dynamic_lane_test(
        &mut self,
        actions: &DynamicLaneActions,
        index: ValueBinding,
        lane_index: usize,
        case_label: LabelId,
        next_label: LabelId,
    ) -> anyhow::Result<()> {
        let lane_key = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: index.width,
        };
        let matched = ValueBinding {
            reg: self.alloc_temporary_vreg()?,
            width: 1,
        };
        let env = LoweringEnv::new()
            .binding("%vi", index)
            .binding("%vk", lane_key)
            .binding("%vm", matched)
            .imm("lane(%r)", lane_index as u64)
            .imm("type_width(%index)", index.width as u64)
            .imm("eq", CmpPredicate::Eq as u64)
            .label("case_label", case_label)
            .label("next_case_label", next_label);
        self.emit_profile_action(&actions.const_mov, &env)?;
        self.emit_profile_action(&actions.icmp, &env)?;
        self.emit_profile_action(&actions.br_if, &env)?;
        Ok(())
    }

    fn lower_shuffle_vector(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("shufflevector result must be a fixed vector");
        };
        let result_fields =
            vector_fields_from_type(BasicTypeEnum::VectorType(result_ty)).context("shufflevector result fields")?;
        let mask = shuffle_vector_mask(instruction)?;
        if mask.len() != result_fields.len() {
            bail!(
                "shufflevector mask/result lane count mismatch: mask has {}, result has {}",
                mask.len(),
                result_fields.len()
            );
        }

        let lhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("shufflevector lhs vector fields")?;
        let rhs_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
            .context("shufflevector rhs vector fields")?;
        let sources = vec![
            (self.vector_seed_from_operand(instruction, 0)?, lhs_fields),
            (self.vector_seed_from_operand(instruction, 1)?, rhs_fields),
        ];
        self.lower_vector_lane_permutation(
            instruction_key(instruction),
            result_fields,
            sources,
            mask,
            "llvm.vector.shuffle.element",
            "shufflevector",
        )
    }

    fn lower_vector_permute_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        kind: VectorPermuteIntrinsicKind,
    ) -> anyhow::Result<()> {
        let expected_args = kind.arg_count();
        let actual_args = instruction.get_num_operands().saturating_sub(1);
        if actual_args != expected_args {
            bail!(
                "vector permute intrinsic {:?} expects exactly {expected_args} arguments, got {actual_args}",
                kind
            );
        }
        if let VectorPermuteIntrinsicKind::Deinterleave(factor) = kind {
            return self.lower_vector_deinterleave_intrinsic(instruction, factor);
        }

        let AnyTypeEnum::VectorType(result_ty) = instruction.get_type() else {
            bail!("vector permute intrinsic {:?} result must be a fixed vector", kind);
        };
        let result_fields = vector_fields_from_type(BasicTypeEnum::VectorType(result_ty))
            .with_context(|| format!("vector permute intrinsic {:?} result fields", kind))?;
        let lane_count = result_fields.len();
        if lane_count == 0 {
            bail!("zero-lane vector permute intrinsic is not supported by vm_virtualize");
        }

        let (sources, mask) = match kind {
            VectorPermuteIntrinsicKind::Reverse => {
                let first_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
                    .context("llvm.vector.reverse source fields")?;
                if first_fields.len() != lane_count {
                    bail!(
                        "llvm.vector.reverse source/result lane count mismatch: source {}, result {}",
                        first_fields.len(),
                        lane_count
                    );
                }
                let mask = (0..lane_count).rev().map(Some).collect();
                let source = self.vector_seed_from_operand(instruction, 0)?;
                (vec![(source, first_fields)], mask)
            },
            VectorPermuteIntrinsicKind::Splice => {
                let first_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
                    .context("llvm.vector.splice lhs fields")?;
                if first_fields.len() != lane_count {
                    bail!(
                        "llvm.vector.splice lhs/result lane count mismatch: lhs {}, result {}",
                        first_fields.len(),
                        lane_count
                    );
                }
                let second_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
                    .context("llvm.vector.splice rhs fields")?;
                if second_fields.len() != lane_count {
                    bail!(
                        "llvm.vector.splice operand lane count mismatch: lhs {}, rhs {}, result {}",
                        first_fields.len(),
                        second_fields.len(),
                        lane_count
                    );
                }
                let imm_value = instruction_operand_value(instruction, 2)?;
                if value_width(imm_value)? != 32 {
                    bail!("llvm.vector.splice immarg must be i32");
                }
                let imm = signed_constant_int_operand(instruction, 2, "llvm.vector.splice immarg")?;
                let lane_count_i64 = i64::try_from(lane_count).context("llvm.vector.splice lane count overflow")?;
                if !(-lane_count_i64..lane_count_i64).contains(&imm) {
                    bail!("llvm.vector.splice immarg {imm} is outside -VL..VL-1 for VL {lane_count}");
                }
                let mask = vector_splice_mask(lane_count, imm)?;
                let lhs = self.vector_seed_from_operand(instruction, 0)?;
                let rhs = self.vector_seed_from_operand(instruction, 1)?;
                (vec![(lhs, first_fields), (rhs, second_fields)], mask)
            },
            VectorPermuteIntrinsicKind::InsertSubvector => {
                let base_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
                    .context("llvm.vector.insert base fields")?;
                if base_fields.len() != lane_count {
                    bail!(
                        "llvm.vector.insert base/result lane count mismatch: base {}, result {}",
                        base_fields.len(),
                        lane_count
                    );
                }
                let sub_fields = vector_fields_from_type(instruction_operand_value(instruction, 1)?.get_type())
                    .context("llvm.vector.insert subvector fields")?;
                if sub_fields.is_empty() {
                    bail!("zero-lane llvm.vector.insert subvector is not supported by vm_virtualize");
                }
                let imm_value = instruction_operand_value(instruction, 2)?;
                if value_width(imm_value)? != 64 {
                    bail!("llvm.vector.insert offset immarg must be i64");
                }
                let offset = constant_int_operand(instruction, 2, "llvm.vector.insert offset immarg")?;
                let offset = usize::try_from(offset).context("llvm.vector.insert offset does not fit usize")?;
                if offset % sub_fields.len() != 0 {
                    bail!(
                        "llvm.vector.insert offset {offset} must be a multiple of subvector lane count {}",
                        sub_fields.len()
                    );
                }
                let end = offset
                    .checked_add(sub_fields.len())
                    .context("llvm.vector.insert offset range overflow")?;
                if end > lane_count {
                    bail!("llvm.vector.insert subvector range {offset}..{end} exceeds result lane count {lane_count}");
                }

                let mut mask = (0..lane_count).map(Some).collect::<Vec<_>>();
                let sub_base = base_fields.len();
                for sub_lane in 0..sub_fields.len() {
                    mask[offset + sub_lane] = Some(sub_base + sub_lane);
                }
                let base = self.vector_seed_from_operand(instruction, 0)?;
                let sub = self.vector_seed_from_operand(instruction, 1)?;
                (vec![(base, base_fields), (sub, sub_fields)], mask)
            },
            VectorPermuteIntrinsicKind::ExtractSubvector => {
                let source_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
                    .context("llvm.vector.extract source fields")?;
                let imm_value = instruction_operand_value(instruction, 1)?;
                if value_width(imm_value)? != 64 {
                    bail!("llvm.vector.extract offset immarg must be i64");
                }
                let offset = constant_int_operand(instruction, 1, "llvm.vector.extract offset immarg")?;
                let offset = usize::try_from(offset).context("llvm.vector.extract offset does not fit usize")?;
                if offset % lane_count != 0 {
                    bail!("llvm.vector.extract offset {offset} must be a multiple of result lane count {lane_count}");
                }
                let end = offset
                    .checked_add(lane_count)
                    .context("llvm.vector.extract offset range overflow")?;
                if end > source_fields.len() {
                    bail!(
                        "llvm.vector.extract result range {offset}..{end} exceeds source lane count {}",
                        source_fields.len()
                    );
                }

                let mask = (offset..end).map(Some).collect();
                let source = self.vector_seed_from_operand(instruction, 0)?;
                (vec![(source, source_fields)], mask)
            },
            VectorPermuteIntrinsicKind::Interleave(factor) => {
                let factor = usize::from(factor);
                if lane_count % factor != 0 {
                    bail!("llvm.vector.interleave{factor} result lane count {lane_count} is not divisible by factor");
                }
                let source_lane_count = lane_count / factor;
                let mut sources = Vec::with_capacity(factor);
                for operand_index in 0..factor {
                    let fields = vector_fields_from_type(
                        instruction_operand_value(instruction, operand_index as u32)?.get_type(),
                    )
                    .with_context(|| format!("llvm.vector.interleave{factor} operand {operand_index} fields"))?;
                    if fields.len() != source_lane_count {
                        bail!(
                            "llvm.vector.interleave{factor} operand {operand_index} lane count {}, expected {source_lane_count}",
                            fields.len()
                        );
                    }
                    let value = self.vector_seed_from_operand(instruction, operand_index as u32)?;
                    sources.push((value, fields));
                }

                let mask = (0..lane_count)
                    .map(|result_lane| {
                        let source_index = result_lane % factor;
                        let source_local_lane = result_lane / factor;
                        Some(source_index * source_lane_count + source_local_lane)
                    })
                    .collect();
                (sources, mask)
            },
            VectorPermuteIntrinsicKind::Compress => {
                let source_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
                    .context("llvm.experimental.vector.compress source fields")?;
                if source_fields.len() != lane_count {
                    bail!(
                        "llvm.experimental.vector.compress source/result lane count mismatch: source {}, result {}",
                        source_fields.len(),
                        lane_count
                    );
                }
                let lane_mask = constant_i1_vector_mask(
                    instruction_operand_value(instruction, 1)?,
                    lane_count,
                    "llvm.experimental.vector.compress mask",
                )?;
                let passthru_fields = vector_fields_from_type(instruction_operand_value(instruction, 2)?.get_type())
                    .context("llvm.experimental.vector.compress passthru fields")?;
                if passthru_fields.len() != lane_count {
                    bail!(
                        "llvm.experimental.vector.compress passthru/result lane count mismatch: passthru {}, result {}",
                        passthru_fields.len(),
                        lane_count
                    );
                }

                let active_lanes = lane_mask
                    .iter()
                    .enumerate()
                    .filter_map(|(lane, active)| active.then_some(lane))
                    .collect::<Vec<_>>();
                let mut mask = Vec::with_capacity(lane_count);
                for result_lane in 0..lane_count {
                    if let Some(source_lane) = active_lanes.get(result_lane) {
                        mask.push(Some(*source_lane));
                    } else {
                        mask.push(Some(lane_count + result_lane));
                    }
                }
                let source = self.vector_seed_from_operand(instruction, 0)?;
                let passthru = self.vector_seed_from_operand(instruction, 2)?;
                (vec![(source, source_fields), (passthru, passthru_fields)], mask)
            },
            VectorPermuteIntrinsicKind::Deinterleave(_) => {
                unreachable!("deinterleave returns before vector-result permutation lowering")
            },
        };

        self.lower_vector_lane_permutation(
            instruction_key(instruction),
            result_fields,
            sources,
            mask,
            kind.lowering_rule(),
            match kind {
                VectorPermuteIntrinsicKind::Reverse => "llvm.vector.reverse",
                VectorPermuteIntrinsicKind::Splice => "llvm.vector.splice",
                VectorPermuteIntrinsicKind::InsertSubvector => "llvm.vector.insert",
                VectorPermuteIntrinsicKind::ExtractSubvector => "llvm.vector.extract",
                VectorPermuteIntrinsicKind::Interleave(_) => "llvm.vector.interleave",
                VectorPermuteIntrinsicKind::Deinterleave(_) => unreachable!("deinterleave returns an aggregate"),
                VectorPermuteIntrinsicKind::Compress => "llvm.experimental.vector.compress",
            },
        )
    }

    fn lower_vector_deinterleave_intrinsic(
        &mut self,
        instruction: InstructionValue<'ctx>,
        factor: u8,
    ) -> anyhow::Result<()> {
        let factor = usize::from(factor);
        let AnyTypeEnum::StructType(return_ty) = instruction.get_type() else {
            bail!("llvm.vector.deinterleave{factor} result must be a struct of fixed vectors");
        };
        if return_ty.count_fields() != factor as u32 {
            bail!(
                "llvm.vector.deinterleave{factor} result field count {}, expected {factor}",
                return_ty.count_fields()
            );
        }

        let source_fields = vector_fields_from_type(instruction_operand_value(instruction, 0)?.get_type())
            .context("llvm.vector.deinterleave source fields")?;
        if source_fields.is_empty() {
            bail!("zero-lane llvm.vector.deinterleave source is not supported by vm_virtualize");
        }
        if source_fields.len() % factor != 0 {
            bail!(
                "llvm.vector.deinterleave{factor} source lane count {} is not divisible by factor",
                source_fields.len()
            );
        }
        let result_lane_count = source_fields.len() / factor;

        let mut result_fields = Vec::with_capacity(source_fields.len());
        for output_index in 0..factor {
            let field_ty = return_ty
                .get_field_type_at_index(output_index as u32)
                .with_context(|| format!("llvm.vector.deinterleave{factor} result field {output_index}"))?;
            let fields = vector_fields_from_type(field_ty)
                .with_context(|| format!("llvm.vector.deinterleave{factor} result vector {output_index} fields"))?;
            if fields.len() != result_lane_count {
                bail!(
                    "llvm.vector.deinterleave{factor} result vector {output_index} lane count {}, expected {result_lane_count}",
                    fields.len()
                );
            }
            result_fields.extend(fields);
        }

        let mask = (0..factor)
            .flat_map(|output_index| {
                (0..result_lane_count).map(move |result_lane| Some(result_lane * factor + output_index))
            })
            .collect();
        let source = self.vector_seed_from_operand(instruction, 0)?;
        self.lower_vector_lane_permutation(
            instruction_key(instruction),
            result_fields,
            vec![(source, source_fields)],
            mask,
            "llvm.vector.deinterleave.element",
            "llvm.vector.deinterleave",
        )
    }

    fn lower_vector_lane_permutation(
        &mut self,
        result_key: ValueKey,
        result_fields: Vec<ReturnField>,
        sources: Vec<(AggregateBinding, Vec<ReturnField>)>,
        mask: Vec<Option<usize>>,
        rule: &str,
        context: &str,
    ) -> anyhow::Result<()> {
        if mask.len() != result_fields.len() {
            bail!(
                "{context} mask/result lane count mismatch: mask has {}, result has {}",
                mask.len(),
                result_fields.len()
            );
        }

        let mut source_base = Vec::with_capacity(sources.len());
        let mut next_base = 0usize;
        for (source_index, (binding, fields)) in sources.iter().enumerate() {
            if binding.fields.len() != fields.len() {
                bail!(
                    "{context} operand {source_index} lane count mismatch: value has {}, type has {}",
                    binding.fields.len(),
                    fields.len()
                );
            }
            source_base.push(next_base);
            next_base += fields.len();
        }

        let mut shuffled_fields = Vec::with_capacity(result_fields.len());
        for (result_lane, (selector, result_info)) in mask.into_iter().zip(result_fields).enumerate() {
            let Some(source_lane) = selector else {
                shuffled_fields.push(None);
                continue;
            };
            let Some((source_index, source_base_lane)) = source_base
                .iter()
                .copied()
                .enumerate()
                .rev()
                .find(|(_, base)| source_lane >= *base)
            else {
                bail!("{context} lane {result_lane} source {source_lane} has no source vector");
            };
            let source_local_lane = source_lane - source_base_lane;
            let (source_binding, source_fields) = sources
                .get(source_index)
                .with_context(|| format!("{context} source vector {source_index} is out of range"))?;
            let source = source_binding
                .fields
                .get(source_local_lane)
                .with_context(|| format!("{context} source lane {source_lane} is out of range"))?;
            let source_info = source_fields
                .get(source_local_lane)
                .copied()
                .with_context(|| format!("{context} source lane {source_lane} type is out of range"))?;
            if source_info != result_info {
                bail!(
                    "{context} lane {result_lane} type mismatch: source {:?} width {}, result {:?} width {}",
                    source_info.kind,
                    source_info.width,
                    result_info.kind,
                    result_info.width
                );
            }
            let Some(source_binding) = source.as_ref().map(|field| field.binding) else {
                shuffled_fields.push(None);
                continue;
            };
            let env = LoweringEnv::new()
                .binding("%lane", source_binding)
                .imm("source_lane(%r)", source_lane as u64)
                .imm("lane(%r)", result_lane as u64)
                .imm("type_width(%field)", result_info.width as u64)
                .imm("type_width(%r)", result_info.width as u64);
            let env = self.execute_lowering_rule(rule, env, Some(HandlerSemantic::Mov))?;
            let stable = match env.get("%vr")? {
                LoweringValue::Reg(binding) => binding,
                LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                    bail!("{context} lowering must produce a lane register")
                },
            };
            shuffled_fields.push(Some(AggregateField::owned(stable)));
        }

        self.insert_aggregate_value(
            result_key,
            AggregateBinding {
                fields: shuffled_fields,
            },
        );
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

        if self.aggregate_return {
            if self.aggregate_return_fields.is_empty() {
                let ret = instruction_operand_value(instruction, 0)?;
                let leaf_count = aggregate_leaf_count(ret.get_type()).context("empty aggregate return fields")?;
                if leaf_count != 0 {
                    bail!("aggregate return signature has no fields but return value has {leaf_count}");
                }
                let ret_action =
                    self.emit_action_for_shape("llvm.ret.aggregate", &HandlerSemantic::Ret, &[("src", "ret0")])?;
                let env = LoweringEnv::new().reg("ret0", 0, 64);
                self.emit_profile_action(&ret_action, &env)?;
                return Ok(());
            }

            let mov_action = self.emit_action_for_shape(
                "llvm.ret.aggregate",
                &HandlerSemantic::Mov,
                &[("dst", "ret_slot"), ("src", "%vf"), ("width", "field_width(%field)")],
            )?;
            let ret_action =
                self.emit_action_for_shape("llvm.ret.aggregate", &HandlerSemantic::Ret, &[("src", "ret0")])?;
            let aggregate = self.aggregate_operand_or_constant(instruction, 0)?;
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
            if matches!(phi.get_type(), AnyTypeEnum::VectorType(_)) {
                self.lower_vector_phi_move(phi, from)?;
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
        self.lower_composite_phi_move(phi, from, field_infos, CompositePhiKind::Aggregate)
    }

    fn lower_vector_phi_move(&mut self, phi: InstructionValue<'ctx>, from: BasicBlock<'ctx>) -> anyhow::Result<()> {
        let AnyTypeEnum::VectorType(vector_ty) = phi.get_type() else {
            bail!("vector phi result must be a fixed vector");
        };
        let field_infos =
            vector_fields_from_type(BasicTypeEnum::VectorType(vector_ty)).context("vector phi result fields")?;
        self.lower_composite_phi_move(phi, from, field_infos, CompositePhiKind::Vector)
    }

    fn lower_composite_phi_move(
        &mut self,
        phi: InstructionValue<'ctx>,
        from: BasicBlock<'ctx>,
        field_infos: Vec<ReturnField>,
        kind: CompositePhiKind,
    ) -> anyhow::Result<()> {
        let kind_name = kind.name();
        let dst = self
            .aggregates
            .get(&instruction_key(phi))
            .cloned()
            .with_context(|| format!("missing destination {kind_name} binding for phi"))?;
        let incoming = phi_incoming_value(phi, from)?;
        let src = self.composite_phi_incoming(incoming, kind)?;
        if dst.fields.len() != field_infos.len() || src.fields.len() != field_infos.len() {
            bail!(
                "{kind_name} phi field count mismatch: type has {}, dst has {}, incoming has {}",
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
                .with_context(|| format!("{kind_name} phi destination field {index} is unavailable"))?;
            let src_field = src
                .fields
                .get(index)
                .copied()
                .flatten()
                .with_context(|| format!("{kind_name} phi incoming field {index} is undefined or unsupported"))?;
            if dst_field.binding.width != info.width || src_field.binding.width != info.width {
                bail!(
                    "{kind_name} phi field {index} width mismatch: type i{}, dst i{}, incoming i{}",
                    info.width,
                    dst_field.binding.width,
                    src_field.binding.width
                );
            }

            let env = LoweringEnv::new()
                .binding("%incoming_field", src_field.binding)
                .binding("%vr", dst_field.binding)
                .imm("type_width(%field)", info.width as u64);
            self.execute_lowering_rule(kind.lowering_rule(), env, Some(HandlerSemantic::Mov))?;
        }

        Ok(())
    }

    fn composite_phi_incoming(
        &mut self,
        incoming: BasicValueEnum<'ctx>,
        kind: CompositePhiKind,
    ) -> anyhow::Result<AggregateBinding> {
        if let Some(binding) = self.aggregates.get(&value_key(incoming)).cloned() {
            return Ok(binding);
        }

        let constant = match kind {
            CompositePhiKind::Aggregate => self.constant_aggregate_binding(incoming, false)?,
            CompositePhiKind::Vector => self.constant_vector_binding(incoming, false, false)?,
        };
        if let Some(binding) = constant {
            return Ok(binding);
        }

        let kind_name = kind.name();
        bail!("{kind_name} phi incoming value was not built by supported {kind_name} lowering")
    }

    fn materialize_operand(&mut self, instruction: InstructionValue<'ctx>, index: u32) -> anyhow::Result<ValueBinding> {
        let value =
            instruction_basic_operand(instruction, index).with_context(|| format!("missing value operand {index}"))?;
        self.materialize_value(value)
    }

    fn aggregate_seed_from_operand(
        &mut self,
        instruction: InstructionValue<'ctx>,
        index: u32,
    ) -> anyhow::Result<AggregateBinding> {
        let value = instruction_basic_operand(instruction, index)
            .with_context(|| format!("missing aggregate operand {index}"))?;
        if let Some(binding) = self.aggregates.get(&value_key(value)) {
            return Ok(binding.clone());
        }
        if is_undef_or_poison_value(value) {
            return Ok(AggregateBinding {
                fields: vec![None; aggregate_leaf_count(value.get_type())?],
            });
        }
        if let Some(binding) = self.constant_aggregate_binding(value, true)? {
            return Ok(binding);
        }

        bail!("aggregate seed was not built by supported insertvalue lowering")
    }

    fn aggregate_operand_or_constant(
        &mut self,
        instruction: InstructionValue<'ctx>,
        index: u32,
    ) -> anyhow::Result<AggregateBinding> {
        let value = instruction_basic_operand(instruction, index)
            .with_context(|| format!("missing aggregate operand {index}"))?;
        if let Some(binding) = self.aggregates.get(&value_key(value)).cloned() {
            return Ok(binding);
        }
        if let Some(binding) = self.constant_aggregate_binding(value, false)? {
            return Ok(binding);
        }
        bail!("aggregate value was not built by supported insertvalue lowering")
    }

    fn vector_seed_from_operand(
        &mut self,
        instruction: InstructionValue<'ctx>,
        index: u32,
    ) -> anyhow::Result<AggregateBinding> {
        let value =
            instruction_basic_operand(instruction, index).with_context(|| format!("missing vector operand {index}"))?;
        if let Some(binding) = self.aggregates.get(&value_key(value)) {
            return Ok(binding.clone());
        }
        if is_undef_or_poison_value(value) {
            return Ok(AggregateBinding {
                fields: vec![None; vector_fields_from_type(value.get_type())?.len()],
            });
        }
        if let Some(binding) = self.constant_vector_binding(value, true, true)? {
            return Ok(binding);
        }
        bail!("vector value was not built by supported insertelement lowering")
    }

    fn vector_operand(&mut self, instruction: InstructionValue<'ctx>, index: u32) -> anyhow::Result<AggregateBinding> {
        let value =
            instruction_basic_operand(instruction, index).with_context(|| format!("missing vector operand {index}"))?;
        if let Some(binding) = self.aggregates.get(&value_key(value)).cloned() {
            return Ok(binding);
        }
        if let Some(binding) = self.constant_vector_binding(value, false, false)? {
            return Ok(binding);
        }
        bail!("vector value was not built by supported insertelement lowering")
    }

    fn constant_aggregate_binding(
        &mut self,
        value: BasicValueEnum<'ctx>,
        allow_undef_fields: bool,
    ) -> anyhow::Result<Option<AggregateBinding>> {
        match value.get_type() {
            BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_) => {},
            _ => return Ok(None),
        }
        if unsafe { LLVMIsAConstant(value.as_value_ref()) }.is_null() {
            return Ok(None);
        }

        let mut fields = Vec::new();
        self.collect_constant_aggregate_fields(value, allow_undef_fields, &mut fields)?;
        Ok(Some(AggregateBinding { fields }))
    }

    fn collect_constant_aggregate_fields(
        &mut self,
        value: BasicValueEnum<'ctx>,
        allow_undef_fields: bool,
        fields: &mut Vec<Option<AggregateField>>,
    ) -> anyhow::Result<()> {
        if is_undef_or_poison_value(value) {
            if !allow_undef_fields {
                bail!("constant aggregate field is undef or poison");
            }
            fields.extend((0..return_fields_from_aggregate_type(value.get_type())?.len()).map(|_| None));
            return Ok(());
        }

        let value_ref = value.as_value_ref();
        if !unsafe { LLVMIsAConstantAggregateZero(value_ref) }.is_null() {
            for field in return_fields_from_aggregate_type(value.get_type())? {
                let reg = self.alloc_temporary_vreg()?;
                self.push_constant(reg, 0, field.width)?;
                fields.push(Some(AggregateField::owned(ValueBinding {
                    reg,
                    width: field.width,
                })));
            }
            return Ok(());
        }
        if unsafe { LLVMIsAConstant(value_ref) }.is_null() {
            bail!("constant aggregate field is not a constant");
        }

        match value.get_type() {
            BasicTypeEnum::StructType(ty) => {
                for index in 0..ty.count_fields() {
                    let element_ref = unsafe { LLVMGetAggregateElement(value_ref, index) };
                    if element_ref.is_null() {
                        bail!("constant aggregate struct field {index} is not materialized as a constant");
                    }
                    let element = unsafe { BasicValueEnum::new(element_ref) };
                    self.collect_constant_aggregate_fields(element, allow_undef_fields, fields)
                        .with_context(|| format!("constant aggregate struct field {index}"))?;
                }
            },
            BasicTypeEnum::ArrayType(ty) => {
                for index in 0..ty.len() {
                    let element_ref = unsafe { LLVMGetAggregateElement(value_ref, index) };
                    if element_ref.is_null() {
                        bail!("constant aggregate array element {index} is not materialized as a constant");
                    }
                    let element = unsafe { BasicValueEnum::new(element_ref) };
                    self.collect_constant_aggregate_fields(element, allow_undef_fields, fields)
                        .with_context(|| format!("constant aggregate array element {index}"))?;
                }
            },
            other => {
                let expected = return_field_from_type(other)?;
                let binding = self.materialize_value(value)?;
                if binding.width != expected.width {
                    bail!(
                        "constant aggregate scalar field width mismatch: value is {}, type expects {}",
                        binding.width,
                        expected.width
                    );
                }
                fields.push(Some(AggregateField::owned(binding)));
            },
        }

        Ok(())
    }

    fn constant_vector_binding(
        &mut self,
        value: BasicValueEnum<'ctx>,
        allow_undef_lanes: bool,
        stable_lanes: bool,
    ) -> anyhow::Result<Option<AggregateBinding>> {
        let fields = vector_fields_from_type(value.get_type())?;
        let value_ref = value.as_value_ref();
        if !unsafe { LLVMIsAConstantAggregateZero(value_ref) }.is_null() {
            let fields = fields
                .iter()
                .map(|field| {
                    let reg = if stable_lanes {
                        self.builder.alloc_vreg()?
                    } else {
                        self.alloc_temporary_vreg()?
                    };
                    self.push_constant(reg, 0, field.width)?;
                    Ok(Some(AggregateField::owned(ValueBinding {
                        reg,
                        width: field.width,
                    })))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            return Ok(Some(AggregateBinding { fields }));
        }
        if unsafe { LLVMIsAConstantVector(value_ref) }.is_null()
            && unsafe { LLVMIsAConstantDataVector(value_ref) }.is_null()
        {
            return Ok(None);
        }

        let mut lanes = Vec::with_capacity(fields.len());
        for (index, field) in fields.iter().copied().enumerate() {
            // SAFETY: `value_ref` is a live LLVM constant vector. `index` is bounded by the
            // fixed-vector lane count derived from its LLVM type above.
            let lane_ref = unsafe { LLVMGetAggregateElement(value_ref, index as u32) };
            if lane_ref.is_null() {
                bail!("constant vector lane {index} is not materialized as a constant");
            }
            let lane = unsafe { BasicValueEnum::new(lane_ref) };
            if is_undef_or_poison_value(lane) {
                if allow_undef_lanes {
                    lanes.push(None);
                    continue;
                }
                bail!("constant vector operand lane {index} is undef or poison");
            }

            let lane_field = return_field_from_type(lane.get_type())
                .with_context(|| format!("constant vector lane {index} type"))?;
            if lane_field != field {
                bail!(
                    "constant vector lane {index} type mismatch: value {:?} width {}, vector {:?} width {}",
                    lane_field.kind,
                    lane_field.width,
                    field.kind,
                    field.width
                );
            }
            let binding = self
                .materialize_value(lane)
                .with_context(|| format!("constant vector lane {index} materialization"))?;
            let binding = if stable_lanes {
                self.emit_aggregate_field_mov("llvm.aggregate.extract.subaggregate", binding, field.width)?
            } else {
                binding
            };
            lanes.push(Some(AggregateField::owned(binding)));
        }

        Ok(Some(AggregateBinding { fields: lanes }))
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
                    16 => u64::from(f64_to_f16_bits(constant)),
                    32 => u64::from((constant as f32).to_bits()),
                    64 => constant.to_bits(),
                    _ => unreachable!("float_type_width only returns 16, 32 or 64"),
                };
                let reg = self.alloc_temporary_vreg()?;
                self.push_constant(reg, imm, width)?;
                return Ok(ValueBinding { reg, width });
            }
        }

        if value.is_pointer_value() {
            let pointer_value = value.into_pointer_value();
            self.ensure_no_non_integral_pointer_type_ref(
                pointer_value.get_type().as_type_ref(),
                "pointer value materialization",
            )?;
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

    fn dynamic_alloca_object(&self, value: BasicValueEnum<'ctx>) -> anyhow::Result<Option<DynamicAllocaObject>> {
        if !value.is_pointer_value() {
            return Ok(None);
        }
        self.dynamic_alloca_object_ref(value.as_value_ref())
    }

    fn dynamic_alloca_object_ref(&self, value_ref: LLVMValueRef) -> anyhow::Result<Option<DynamicAllocaObject>> {
        if value_ref.is_null() {
            return Ok(None);
        }
        if let Some(object) = self.dynamic_allocas.get(&(value_ref as usize)).copied() {
            return Ok(Some(object));
        }
        if is_objectsize_pointer_cast_instruction(value_ref) {
            let operand = single_value_operand(value_ref, "llvm.objectsize dynamic alloca pointer cast")?;
            return self.dynamic_alloca_object_ref(operand);
        }
        if !unsafe { LLVMIsAConstantExpr(value_ref) }.is_null() {
            match unsafe { LLVMGetConstOpcode(value_ref) } {
                LLVMOpcode::LLVMBitCast | LLVMOpcode::LLVMAddrSpaceCast => {
                    let operand = single_value_operand(value_ref, "llvm.objectsize dynamic alloca pointer cast")?;
                    return self.dynamic_alloca_object_ref(operand);
                },
                _ => {},
            }
        }
        Ok(None)
    }

    fn dynamic_alloca_gep_object(&self, value: BasicValueEnum<'ctx>) -> anyhow::Result<Option<DynamicAllocaGepObject>> {
        if !value.is_pointer_value() {
            return Ok(None);
        }
        self.dynamic_alloca_gep_object_ref(value.as_value_ref())
    }

    fn dynamic_alloca_gep_object_ref(&self, value_ref: LLVMValueRef) -> anyhow::Result<Option<DynamicAllocaGepObject>> {
        if value_ref.is_null() {
            return Ok(None);
        }
        if let Some(object) = self.dynamic_alloca_geps.get(&(value_ref as usize)).copied() {
            return Ok(Some(object));
        }
        if is_objectsize_pointer_cast_instruction(value_ref) {
            let operand = single_value_operand(value_ref, "llvm.objectsize dynamic alloca GEP pointer cast")?;
            return self.dynamic_alloca_gep_object_ref(operand);
        }
        if !unsafe { LLVMIsAConstantExpr(value_ref) }.is_null() {
            match unsafe { LLVMGetConstOpcode(value_ref) } {
                LLVMOpcode::LLVMBitCast | LLVMOpcode::LLVMAddrSpaceCast => {
                    let operand = single_value_operand(value_ref, "llvm.objectsize dynamic alloca GEP pointer cast")?;
                    return self.dynamic_alloca_gep_object_ref(operand);
                },
                _ => {},
            }
        }
        Ok(None)
    }

    fn static_object_base(&self, value: BasicValueEnum<'ctx>) -> anyhow::Result<Option<StaticObjectBase>> {
        if !value.is_pointer_value() {
            return Ok(None);
        }
        match self.static_object_base_and_offset(value.as_value_ref()) {
            Ok((total_size, base_offset)) if base_offset >= 0 => {
                let base_offset = u64::try_from(base_offset).context("static object base offset overflow")?;
                Ok((base_offset <= total_size).then_some(StaticObjectBase {
                    total_size,
                    base_offset,
                }))
            },
            Ok(_) | Err(_) => Ok(None),
        }
    }

    fn dynamic_static_gep_object(&self, value: BasicValueEnum<'ctx>) -> anyhow::Result<Option<DynamicStaticGepObject>> {
        if !value.is_pointer_value() {
            return Ok(None);
        }
        self.dynamic_static_gep_object_ref(value.as_value_ref())
    }

    fn dynamic_static_gep_object_ref(&self, value_ref: LLVMValueRef) -> anyhow::Result<Option<DynamicStaticGepObject>> {
        if value_ref.is_null() {
            return Ok(None);
        }
        if let Some(object) = self.dynamic_static_geps.get(&(value_ref as usize)).copied() {
            return Ok(Some(object));
        }
        if is_objectsize_pointer_cast_instruction(value_ref) {
            let operand = single_value_operand(value_ref, "llvm.objectsize dynamic static GEP pointer cast")?;
            return self.dynamic_static_gep_object_ref(operand);
        }
        if !unsafe { LLVMIsAConstantExpr(value_ref) }.is_null() {
            match unsafe { LLVMGetConstOpcode(value_ref) } {
                LLVMOpcode::LLVMBitCast | LLVMOpcode::LLVMAddrSpaceCast => {
                    let operand = single_value_operand(value_ref, "llvm.objectsize dynamic static GEP pointer cast")?;
                    return self.dynamic_static_gep_object_ref(operand);
                },
                _ => {},
            }
        }
        Ok(None)
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
        if is_objectsize_pointer_cast_instruction(value_ref) {
            let operand = single_value_operand(value_ref, "llvm.objectsize pointer cast")?;
            return self.static_object_base_and_offset(operand);
        }
        if !unsafe { LLVMIsAConstantExpr(value_ref) }.is_null() {
            match unsafe { LLVMGetConstOpcode(value_ref) } {
                LLVMOpcode::LLVMBitCast | LLVMOpcode::LLVMAddrSpaceCast => {
                    let operand = single_value_operand(value_ref, "llvm.objectsize pointer cast")?;
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
        self.ensure_no_non_integral_pointer_type_ref(
            unsafe { LLVMTypeOf(operand_ref) },
            "ptrtoint constant expression source",
        )?;

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
        self.ensure_no_non_integral_pointer_type_ref(
            unsafe { LLVMTypeOf(expr_ref) },
            "inttoptr constant expression result",
        )?;

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
        self.ensure_no_non_integral_pointer_type_ref(
            unsafe { LLVMTypeOf(operand_ref) },
            "pointer cast constant expression source",
        )?;
        self.ensure_no_non_integral_pointer_type_ref(
            unsafe { LLVMTypeOf(expr_ref) },
            "pointer cast constant expression result",
        )?;
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
        let thunk_name = translator_private_symbol_name(
            self.emit_markers,
            ".amice.vm.global_addr",
            "ga",
            function_name,
            self.native_calls.len(),
        );
        let thunk = self
            .module
            .add_function(&thunk_name, thunk_type, Some(Linkage::Private));
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

fn copy_indirect_call_attributes_to_adapter(adapter: FunctionValue<'_>, source: CallSiteValue<'_>) {
    adapter.set_call_conventions(source.get_call_convention());
    copy_call_site_attributes_to_function_at(adapter, source, AttributeLoc::Function, AttributeLoc::Function);
    copy_call_site_attributes_to_function_at(adapter, source, AttributeLoc::Return, AttributeLoc::Return);

    for index in 0..source.count_arguments() {
        copy_call_site_attributes_to_function_at(
            adapter,
            source,
            AttributeLoc::Param(index),
            AttributeLoc::Param(index + 1),
        );
    }
}

fn copy_call_site_attributes(target: CallSiteValue<'_>, source: CallSiteValue<'_>) {
    target.set_call_convention(source.get_call_convention());
    copy_call_site_attributes_at(target, source, AttributeLoc::Function, AttributeLoc::Function);
    copy_call_site_attributes_at(target, source, AttributeLoc::Return, AttributeLoc::Return);

    for index in 0..source.count_arguments() {
        copy_call_site_attributes_at(target, source, AttributeLoc::Param(index), AttributeLoc::Param(index));
    }
}

fn copy_call_site_attributes_to_function_at(
    target: FunctionValue<'_>,
    source: CallSiteValue<'_>,
    source_loc: AttributeLoc,
    target_loc: AttributeLoc,
) {
    for attr in source.attributes(source_loc) {
        add_function_attribute_if_missing(target, target_loc, attr);
    }
}

fn add_function_attribute_if_missing(function: FunctionValue<'_>, loc: AttributeLoc, attr: Attribute) {
    if !function.attributes(loc).contains(&attr) {
        function.add_attribute(loc, attr);
    }
}

fn copy_call_site_attributes_at(
    target: CallSiteValue<'_>,
    source: CallSiteValue<'_>,
    source_loc: AttributeLoc,
    target_loc: AttributeLoc,
) {
    for attr in source.attributes(source_loc) {
        add_call_site_attribute_if_missing(target, target_loc, attr);
    }
}

fn add_call_site_attribute_if_missing(call_site: CallSiteValue<'_>, loc: AttributeLoc, attr: Attribute) {
    if !call_site.attributes(loc).contains(&attr) {
        call_site.add_attribute(loc, attr);
    }
}

fn native_call_target_with_arg_types<'ctx>(
    function: FunctionValue<'ctx>,
    arg_types: Vec<BasicMetadataTypeEnum<'ctx>>,
) -> anyhow::Result<NativeCallTarget<'ctx>> {
    let fn_type = function.get_type();
    let (returns_void, return_is_aggregate, return_fields) = match fn_type.get_return_type() {
        None => (true, false, Vec::new()),
        Some(BasicTypeEnum::IntType(return_type)) => (
            false,
            false,
            vec![ReturnField {
                width: checked_width(return_type.get_bit_width())?,
                kind: ScalarKind::Integer,
            }],
        ),
        Some(BasicTypeEnum::PointerType(_)) => (
            false,
            false,
            vec![ReturnField {
                width: 64,
                kind: ScalarKind::Pointer,
            }],
        ),
        Some(BasicTypeEnum::FloatType(return_type)) => (
            false,
            false,
            vec![ReturnField {
                width: float_type_width(return_type.as_type_ref())?,
                kind: ScalarKind::Float,
            }],
        ),
        Some(BasicTypeEnum::StructType(return_type)) => {
            let fields = return_fields_from_aggregate_type(BasicTypeEnum::StructType(return_type))
                .context("native return fields")?;
            (false, true, fields)
        },
        Some(BasicTypeEnum::ArrayType(return_type)) => {
            let fields = return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(return_type))
                .context("native return fields")?;
            (false, true, fields)
        },
        Some(BasicTypeEnum::VectorType(return_type)) => {
            let fields = vector_fields_from_type(BasicTypeEnum::VectorType(return_type))
                .context("native vector return fields")?;
            (false, true, fields)
        },
        Some(BasicTypeEnum::ScalableVectorType(_)) => {
            bail!("scalable vector native call returns are not supported by vm_virtualize")
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
                return_fields_from_aggregate_type(BasicTypeEnum::StructType(*struct_ty))
                    .with_context(|| format!("native aggregate parameter {index} fields"))?
            },
            BasicMetadataTypeEnum::ArrayType(array_ty) => {
                return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(*array_ty))
                    .with_context(|| format!("native aggregate parameter {index} fields"))?
            },
            BasicMetadataTypeEnum::VectorType(vector_ty) => {
                vector_fields_from_type(BasicTypeEnum::VectorType(*vector_ty))
                    .with_context(|| format!("native vector parameter {index} fields"))?
            },
            BasicMetadataTypeEnum::ScalableVectorType(_) => {
                bail!("scalable vector native call parameters are not supported by vm_virtualize")
            },
            _ => {
                bail!(
                    "only scalar integer, pointer, half/float/double, direct struct/array, and fixed vector native call parameters are supported"
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
            "only up to {NATIVE_CALL_MAX_ARGS} flattened scalar integer/pointer/floating native call argument slots are supported, got {}",
            param_widths.len()
        );
    }

    Ok(NativeCallTarget {
        function,
        arg_types,
        param_widths,
        params,
        returns_void,
        return_is_aggregate,
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
                fields.len()
            },
            BasicMetadataTypeEnum::ArrayType(array_ty) => {
                let fields = return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(*array_ty))
                    .with_context(|| format!("indirect call aggregate argument {index} fields"))?;
                fields.len()
            },
            BasicMetadataTypeEnum::VectorType(vector_ty) => {
                let fields = vector_fields_from_type(BasicTypeEnum::VectorType(*vector_ty))
                    .with_context(|| format!("indirect call vector argument {index} fields"))?;
                fields.len()
            },
            BasicMetadataTypeEnum::ScalableVectorType(_) => {
                bail!("scalable vector indirect call arguments are not supported by vm_virtualize")
            },
            _ => bail!(
                "only scalar integer, pointer, half/float/double, direct struct/array, and fixed vector indirect call arguments are supported"
            ),
        };
        flattened_param_count += leaf_count;
    }
    if flattened_param_count > NATIVE_CALL_MAX_ARGS {
        bail!(
            "only up to {NATIVE_CALL_MAX_ARGS} flattened scalar integer/pointer/floating indirect call argument slots are supported, got {flattened_param_count}"
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
            fields.len()
        },
        Some(BasicTypeEnum::ArrayType(return_type)) => {
            let fields = return_fields_from_aggregate_type(BasicTypeEnum::ArrayType(return_type))
                .context("indirect call return fields")?;
            fields.len()
        },
        Some(BasicTypeEnum::VectorType(return_type)) => {
            let fields = vector_fields_from_type(BasicTypeEnum::VectorType(return_type))
                .context("indirect call vector return fields")?;
            fields.len()
        },
        Some(BasicTypeEnum::ScalableVectorType(_)) => {
            bail!("scalable vector indirect call returns are not supported by vm_virtualize")
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

fn translator_private_symbol_name(
    emit_markers: bool,
    marker_prefix: &str,
    opaque_kind: &str,
    function_name: &str,
    index: usize,
) -> String {
    if emit_markers {
        format!("{marker_prefix}.{}.{}", translator_symbol_suffix(function_name), index)
    } else {
        let mut hasher = DefaultHasher::new();
        opaque_kind.hash(&mut hasher);
        function_name.hash(&mut hasher);
        index.hash(&mut hasher);
        format!(".L__{:016x}", hasher.finish())
    }
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

fn objectsize_i1_immarg(instruction: InstructionValue<'_>, index: u32) -> anyhow::Result<bool> {
    let flag = constant_int_operand(instruction, index, &format!("llvm.objectsize immarg {index}"))?;
    if flag > 1 {
        bail!("llvm.objectsize immarg {index} must be i1");
    }
    Ok(flag != 0)
}

fn objectsize_unknown_value(min: bool, width: u8) -> anyhow::Result<u64> {
    if width == 0 || width > 64 {
        bail!("llvm.objectsize unknown result width i{width} is not supported");
    }
    if min {
        return Ok(0);
    }
    if width == 64 {
        return Ok(u64::MAX);
    }
    Ok((1_u64 << width) - 1)
}

fn gep_is_inbounds(instruction: InstructionValue<'_>) -> bool {
    instruction.get_opcode() == InstructionOpcode::GetElementPtr
        && unsafe { LLVMIsInBounds(instruction.as_value_ref()) } != 0
}

fn objectsize_can_fold_unknown(error: &anyhow::Error, dynamic: bool) -> bool {
    let message = format!("{error:#}");
    if message.contains("llvm.objectsize only supports static alloca, global, and constant-offset GEP operands") {
        return true;
    }
    !dynamic
        && [
            "llvm.objectsize dynamic alloca count is not statically known",
            "llvm.objectsize getelementptr index must be an integer constant",
            "unsupported llvm.objectsize pointer constant expression opcode",
        ]
        .iter()
        .any(|needle| message.contains(needle))
}

fn single_constant_expr_operand(value: LLVMValueRef, context: &str) -> anyhow::Result<LLVMValueRef> {
    single_value_operand(value, context)
}

fn single_value_operand(value: LLVMValueRef, context: &str) -> anyhow::Result<LLVMValueRef> {
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

fn is_objectsize_pointer_cast_instruction(value: LLVMValueRef) -> bool {
    !unsafe { LLVMIsABitCastInst(value) }.is_null() || !unsafe { LLVMIsAAddrSpaceCastInst(value) }.is_null()
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

fn module_data_layout(module: &Module<'_>) -> String {
    let data_layout = module.get_data_layout();
    let layout = data_layout.as_str().to_string_lossy().into_owned();
    drop(data_layout);

    if layout.trim().is_empty() {
        DEFAULT_X86_64_DATA_LAYOUT.to_owned()
    } else {
        layout
    }
}

fn non_integral_address_spaces_from_layout(layout: &str) -> HashSet<u32> {
    layout
        .split('-')
        .filter_map(|component| component.strip_prefix("ni:"))
        .flat_map(|spaces| spaces.split(':'))
        .filter_map(|space| space.parse::<u32>().ok())
        .collect()
}

fn ensure_no_non_integral_pointer_type_ref(
    non_integral_address_spaces: &HashSet<u32>,
    ty: LLVMTypeRef,
    context: &str,
) -> anyhow::Result<()> {
    if non_integral_address_spaces.is_empty() || ty.is_null() {
        return Ok(());
    }

    match unsafe { LLVMGetTypeKind(ty) } {
        LLVMTypeKind::LLVMPointerTypeKind => {
            let address_space = unsafe { LLVMGetPointerAddressSpace(ty) };
            if non_integral_address_spaces.contains(&address_space) {
                bail!(
                    "{context} uses non-integral pointer address space {address_space}, which is not supported by vm_virtualize"
                );
            }
        },
        LLVMTypeKind::LLVMArrayTypeKind
        | LLVMTypeKind::LLVMVectorTypeKind
        | LLVMTypeKind::LLVMScalableVectorTypeKind => {
            let element = unsafe { LLVMGetElementType(ty) };
            ensure_no_non_integral_pointer_type_ref(non_integral_address_spaces, element, context)?;
        },
        LLVMTypeKind::LLVMStructTypeKind => {
            let field_count = unsafe { LLVMCountStructElementTypes(ty) };
            for index in 0..field_count {
                let field = unsafe { LLVMStructGetTypeAtIndex(ty, index) };
                ensure_no_non_integral_pointer_type_ref(
                    non_integral_address_spaces,
                    field,
                    &format!("{context} field {index}"),
                )?;
            }
        },
        _ => {},
    }

    Ok(())
}

fn store_size(target_data: &TargetData, ty: LLVMTypeRef) -> anyhow::Result<u64> {
    // SAFETY: `target_data` 属于当前 module，`ty` 是从同一 context 取得的 LLVM type。
    // LLVM 在这里只读取 layout 元数据；零大小类型对 GEP 的字节贡献就是 0。
    Ok(unsafe { LLVMStoreSizeOfType(target_data.as_mut_ptr(), ty) })
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
        AnyTypeEnum::ScalableVectorType(_) => bail!("scalable vector values are not supported by vm_virtualize"),
        AnyTypeEnum::VectorType(_)
            if matches!(
                instruction.get_opcode(),
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
                    | InstructionOpcode::AShr
                    | InstructionOpcode::FAdd
                    | InstructionOpcode::FSub
                    | InstructionOpcode::FMul
                    | InstructionOpcode::FDiv
                    | InstructionOpcode::FRem
                    | InstructionOpcode::FNeg
                    | InstructionOpcode::Load
                    | InstructionOpcode::Call
                    | InstructionOpcode::SIToFP
                    | InstructionOpcode::UIToFP
                    | InstructionOpcode::FPToSI
                    | InstructionOpcode::FPToUI
                    | InstructionOpcode::FPTrunc
                    | InstructionOpcode::FPExt
                    | InstructionOpcode::ICmp
                    | InstructionOpcode::FCmp
                    | InstructionOpcode::ZExt
                    | InstructionOpcode::SExt
                    | InstructionOpcode::Trunc
                    | InstructionOpcode::PtrToInt
                    | InstructionOpcode::IntToPtr
                    | InstructionOpcode::AddrSpaceCast
                    | InstructionOpcode::BitCast
                    | InstructionOpcode::InsertElement
                    | InstructionOpcode::ExtractValue
                    | InstructionOpcode::Freeze
                    | InstructionOpcode::ShuffleVector
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
        | InstructionOpcode::AShr => matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)),
        InstructionOpcode::FAdd
        | InstructionOpcode::FSub
        | InstructionOpcode::FMul
        | InstructionOpcode::FDiv
        | InstructionOpcode::FRem => matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)),
        InstructionOpcode::FNeg => matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)),
        InstructionOpcode::SIToFP
        | InstructionOpcode::UIToFP
        | InstructionOpcode::FPToSI
        | InstructionOpcode::FPToUI => {
            matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        },
        InstructionOpcode::FPTrunc | InstructionOpcode::FPExt => {
            matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        },
        InstructionOpcode::ICmp => matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)),
        InstructionOpcode::FCmp => matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)),
        InstructionOpcode::ZExt
        | InstructionOpcode::SExt
        | InstructionOpcode::Trunc
        | InstructionOpcode::PtrToInt
        | InstructionOpcode::IntToPtr
        | InstructionOpcode::AddrSpaceCast => {
            matches!(instruction.get_type(), AnyTypeEnum::VectorType(_))
        },
        InstructionOpcode::BitCast => matches!(instruction.get_type(), AnyTypeEnum::VectorType(_)),
        InstructionOpcode::InsertValue
        | InstructionOpcode::InsertElement
        | InstructionOpcode::ShuffleVector
        | InstructionOpcode::AtomicCmpXchg => true,
        InstructionOpcode::Freeze => matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_) | AnyTypeEnum::VectorType(_)
        ),
        InstructionOpcode::ExtractValue => matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_) | AnyTypeEnum::VectorType(_)
        ),
        InstructionOpcode::Load => matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_) | AnyTypeEnum::VectorType(_)
        ),
        InstructionOpcode::Call => matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_) | AnyTypeEnum::VectorType(_)
        ),
        InstructionOpcode::Select => matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_) | AnyTypeEnum::VectorType(_)
        ),
        InstructionOpcode::Phi => matches!(
            instruction.get_type(),
            AnyTypeEnum::StructType(_) | AnyTypeEnum::ArrayType(_) | AnyTypeEnum::VectorType(_)
        ),
        _ => false,
    }
}

fn memory_is_volatile(instruction: InstructionValue<'_>, kind: &str) -> anyhow::Result<bool> {
    instruction
        .get_volatile()
        .with_context(|| format!("{kind} volatile flag cannot be read"))
}

fn atomic_sync_scope(instruction: InstructionValue<'_>, kind: &str) -> anyhow::Result<u8> {
    // SAFETY: `instruction` 是当前 module 中的 live atomic/fence instruction；C API
    // 只读取 syncscope ID，不访问用户内存。LLVM 21 在 LLVMContext.h 中定义
    // `SyncScope::SingleThread = 0`、`SyncScope::System = 1`。
    checked_supported_atomic_sync_scope(
        unsafe { LLVMGetAtomicSyncScopeID(instruction.as_value_ref()) } as u64,
        kind,
    )
}

fn checked_supported_atomic_sync_scope(scope: u64, kind: &str) -> anyhow::Result<u8> {
    match scope {
        value if value == LLVM_SINGLETHREAD_SYNC_SCOPE_ID as u64 || value == LLVM_SYSTEM_SYNC_SCOPE_ID as u64 => {
            Ok(value as u8)
        },
        other => bail!("{kind} atomic syncscope {other} is not supported by vm_virtualize"),
    }
}

fn memory_ordering(instruction: InstructionValue<'_>, kind: &str) -> anyhow::Result<AtomicOrdering> {
    instruction
        .get_atomic_ordering()
        .with_context(|| format!("{kind} atomic ordering cannot be read"))
}

fn ensure_atomic_load_store_value_type(ty: AnyTypeEnum<'_>, kind: &str) -> anyhow::Result<()> {
    match ty {
        AnyTypeEnum::IntType(_) | AnyTypeEnum::PointerType(_) => Ok(()),
        AnyTypeEnum::FloatType(float_ty) => {
            checked_float_width(float_type_width(float_ty.as_type_ref())? as u64)?;
            Ok(())
        },
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
        (true, other) => bail!("floating atomicrmw operation {op:?} requires half/float/double operand, got {other:?}"),
        (false, BasicTypeEnum::IntType(_) | BasicTypeEnum::PointerType(_)) => Ok(()),
        (false, BasicTypeEnum::FloatType(_)) => {
            bail!("integer atomicrmw operation {op:?} cannot be applied to floating-point memory")
        },
        (false, other) => bail!("atomicrmw memory type is not supported by vm_virtualize: {other:?}"),
    }
}

fn ensure_atomic_load_store_basic_value_type(ty: BasicTypeEnum<'_>, kind: &str) -> anyhow::Result<()> {
    match ty {
        BasicTypeEnum::IntType(_) | BasicTypeEnum::PointerType(_) => Ok(()),
        BasicTypeEnum::FloatType(float_ty) => {
            checked_float_width(float_type_width(float_ty.as_type_ref())? as u64)?;
            Ok(())
        },
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

fn unsupported_stack_introspection_intrinsic_reason(callee: FunctionValue<'_>) -> Option<&'static str> {
    let name = callee.get_name().to_str().unwrap_or("");
    match name {
        "llvm.returnaddress" => Some("llvm.returnaddress is stack introspection and is not supported by vm_virtualize"),
        "llvm.localaddress" => Some("llvm.localaddress is stack introspection and is not supported by vm_virtualize"),
        _ if name.starts_with("llvm.frameaddress") => {
            Some("llvm.frameaddress is stack introspection and is not supported by vm_virtualize")
        },
        _ if name.starts_with("llvm.addressofreturnaddress") => {
            Some("llvm.addressofreturnaddress is stack introspection and is not supported by vm_virtualize")
        },
        _ if name.starts_with("llvm.sponentry") => {
            Some("llvm.sponentry is stack introspection and is not supported by vm_virtualize")
        },
        _ => None,
    }
}

fn unsupported_target_state_intrinsic_reason(callee: FunctionValue<'_>) -> Option<&'static str> {
    let name = callee.get_name().to_str().unwrap_or("");
    match name {
        _ if name.starts_with("llvm.read_register.") || name.starts_with("llvm.write_register.") => {
            Some("LLVM target register intrinsics are not supported by vm_virtualize")
        },
        _ if name == "llvm.gcroot" || name == "llvm.gcread" || name == "llvm.gcwrite" => {
            Some("LLVM GC stack intrinsics are not supported by vm_virtualize")
        },
        _ if name.starts_with("llvm.experimental.stackmap")
            || name.starts_with("llvm.experimental.patchpoint")
            || name.starts_with("llvm.experimental.gc.statepoint") =>
        {
            Some("LLVM stackmap, patchpoint, and statepoint intrinsics are not supported by vm_virtualize")
        },
        _ => None,
    }
}

fn unsupported_target_specific_intrinsic_reason(callee: FunctionValue<'_>) -> Option<&'static str> {
    let name = callee.get_name().to_str().unwrap_or("");
    if target_specific_intrinsic_name(name) {
        Some("LLVM target-specific intrinsics are not supported by vm_virtualize")
    } else {
        None
    }
}

fn target_specific_intrinsic_name(name: &str) -> bool {
    const TARGET_PREFIXES: &[&str] = &[
        "llvm.aarch64.",
        "llvm.amdgcn.",
        "llvm.arm.",
        "llvm.bpf.",
        "llvm.dx.",
        "llvm.hexagon.",
        "llvm.loongarch.",
        "llvm.mips.",
        "llvm.nvvm.",
        "llvm.ppc.",
        "llvm.r600.",
        "llvm.riscv.",
        "llvm.s390.",
        "llvm.spu.",
        "llvm.spv.",
        "llvm.ve.",
        "llvm.wasm.",
        "llvm.x86.",
        "llvm.xcore.",
    ];
    TARGET_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

fn call_uses_inline_asm(instruction: InstructionValue<'_>) -> bool {
    // SAFETY: `instruction` is a call-like instruction on this path. The LLVM C API calls below only
    // inspect operands/value classes, and every returned raw value is null-checked before use.
    let callee = unsafe { LLVMGetCalledValue(instruction.as_value_ref()) };
    if value_is_inline_asm(callee) {
        return true;
    }

    // LLVMGetCalledValue is deprecated and can be conservative for opaque-pointer call forms. The
    // callee is still the final call operand, so keep a second read path for inline asm detection.
    let operand_count = unsafe { LLVMGetNumOperands(instruction.as_value_ref()) };
    if operand_count <= 0 {
        return false;
    }
    let callee_operand = unsafe { LLVMGetOperand(instruction.as_value_ref(), (operand_count - 1) as u32) };
    value_is_inline_asm(callee_operand)
}

fn call_uses_musttail(instruction: InstructionValue<'_>) -> bool {
    match instruction.get_tail_call_kind() {
        Ok(LLVMTailCallKind::LLVMTailCallKindMustTail) => true,
        Ok(LLVMTailCallKind::LLVMTailCallKindNone)
        | Ok(LLVMTailCallKind::LLVMTailCallKindTail)
        | Ok(LLVMTailCallKind::LLVMTailCallKindNoTail)
        | Err(_) => false,
    }
}

fn call_has_operand_bundles(instruction: InstructionValue<'_>) -> bool {
    // SAFETY: `instruction` is a call instruction on this path; LLVMGetNumOperandBundles only reads
    // CallBase metadata and does not take ownership of any bundle object.
    unsafe { LLVMGetNumOperandBundles(instruction.as_value_ref()) != 0 }
}

fn value_is_inline_asm(value: LLVMValueRef) -> bool {
    if value.is_null() {
        return false;
    }

    // SAFETY: LLVMIsAInlineAsm is a pure value class test and accepts any non-null LLVMValueRef.
    if !unsafe { LLVMIsAInlineAsm(value) }.is_null() {
        return true;
    }

    // SAFETY: LLVMGetValueKind only reads the kind tag of the non-null LLVMValueRef checked above.
    unsafe { LLVMGetValueKind(value) == LLVMValueKind::LLVMInlineAsmValueKind }
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

fn return_field_from_any_scalar_type(ty: AnyTypeEnum<'_>) -> anyhow::Result<ReturnField> {
    match ty {
        AnyTypeEnum::IntType(int_ty) => Ok(ReturnField {
            width: checked_width(int_ty.get_bit_width())?,
            kind: ScalarKind::Integer,
        }),
        AnyTypeEnum::PointerType(_) => Ok(ReturnField {
            width: 64,
            kind: ScalarKind::Pointer,
        }),
        AnyTypeEnum::FloatType(float_ty) => Ok(ReturnField {
            width: float_type_width(float_ty.as_type_ref())?,
            kind: ScalarKind::Float,
        }),
        other => bail!("unsupported scalar result type: {other:?}"),
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

fn composite_fields_from_type(ty: BasicTypeEnum<'_>) -> anyhow::Result<Vec<ReturnField>> {
    match ty {
        BasicTypeEnum::VectorType(_) => vector_fields_from_type(ty),
        BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_) => return_fields_from_composite_type(ty),
        other => Ok(vec![return_field_from_type(other)?]),
    }
}

fn return_fields_from_composite_type(ty: BasicTypeEnum<'_>) -> anyhow::Result<Vec<ReturnField>> {
    match ty {
        BasicTypeEnum::StructType(ty) => {
            let mut fields = Vec::new();
            for index in 0..ty.count_fields() {
                let field_ty = ty
                    .get_field_type_at_index(index)
                    .with_context(|| format!("composite struct field {index} is unavailable"))?;
                fields.extend(
                    return_fields_from_composite_type(field_ty)
                        .with_context(|| format!("composite struct field {index}"))?,
                );
            }
            Ok(fields)
        },
        BasicTypeEnum::ArrayType(ty) => {
            let element_ty = ty.get_element_type();
            let element_fields = return_fields_from_composite_type(element_ty).context("composite array element")?;
            let mut fields = Vec::new();
            for _ in 0..ty.len() {
                fields.extend(element_fields.iter().copied());
            }
            Ok(fields)
        },
        BasicTypeEnum::VectorType(_) => vector_fields_from_type(ty),
        other => Ok(vec![return_field_from_type(other)?]),
    }
}

fn aggregate_leaf_count(ty: BasicTypeEnum<'_>) -> anyhow::Result<usize> {
    match ty {
        BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_) => Ok(return_fields_from_aggregate_type(ty)?.len()),
        other => bail!("unsupported aggregate value type: {other:?}"),
    }
}

fn vector_fields_from_type(ty: BasicTypeEnum<'_>) -> anyhow::Result<Vec<ReturnField>> {
    let BasicTypeEnum::VectorType(vector_ty) = ty else {
        bail!("fixed vector lowering requires a fixed vector type, got {ty:?}");
    };
    let lane = return_field_from_type(vector_ty.get_element_type()).context("fixed vector lane type")?;
    let lane_count = usize::try_from(vector_ty.get_size()).context("fixed vector lane count overflow")?;
    if lane_count == 0 {
        bail!("zero-lane fixed vectors are not supported by vm_virtualize");
    }
    Ok(vec![lane; lane_count])
}

fn vector_byte_addressable_fields(ty: BasicTypeEnum<'_>) -> anyhow::Result<Vec<ReturnField>> {
    let fields = vector_fields_from_type(ty)?;
    for (index, field) in fields.iter().enumerate() {
        if field.width % 8 != 0 {
            bail!(
                "fixed vector lane {index} width {} is not byte-addressable",
                field.width
            );
        }
    }
    Ok(fields)
}

fn ensure_pointer_vector_lanes(ty: BasicTypeEnum<'_>, expected_lanes: usize, name: &str) -> anyhow::Result<()> {
    let fields = vector_fields_from_type(ty).with_context(|| format!("{name} type"))?;
    if fields.len() != expected_lanes {
        bail!(
            "{name} lane count mismatch: pointer vector has {}, value has {expected_lanes}",
            fields.len()
        );
    }
    for (index, field) in fields.iter().enumerate() {
        if field.kind != ScalarKind::Pointer || field.width != 64 {
            bail!(
                "{name} lane {index} must be a pointer, got {:?}{}",
                field.kind,
                field.width
            );
        }
    }
    Ok(())
}

fn vector_memory_fields<'ctx>(
    target_data: &TargetData,
    ty: BasicTypeEnum<'ctx>,
) -> anyhow::Result<Vec<AggregateMemoryField>> {
    let BasicTypeEnum::VectorType(vector_ty) = ty else {
        bail!("fixed vector memory lowering requires a fixed vector type, got {ty:?}");
    };
    let lane_ty = vector_ty.get_element_type();
    let lane = return_field_from_type(lane_ty).context("fixed vector memory lane type")?;
    if lane.width % 8 != 0 {
        bail!("fixed vector memory lane width {} is not byte-addressable", lane.width);
    }
    let lane_stride = store_size(target_data, lane_ty.as_type_ref()).context("fixed vector memory lane stride")?;
    if lane_stride != u64::from(lane.width / 8) {
        bail!(
            "fixed vector memory lane width {} does not match data-layout stride {}",
            lane.width,
            lane_stride
        );
    }
    let lane_count = usize::try_from(vector_ty.get_size()).context("fixed vector lane count overflow")?;
    if lane_count == 0 {
        bail!("zero-lane fixed vectors are not supported by vm_virtualize");
    }
    let expected_size = lane_stride
        .checked_mul(u64::try_from(lane_count).context("fixed vector lane count does not fit u64")?)
        .context("fixed vector memory size overflow")?;
    let actual_size = store_size(target_data, vector_ty.as_type_ref()).context("fixed vector memory store size")?;
    if actual_size != expected_size {
        bail!(
            "fixed vector memory layout is not lane-contiguous: vector store size {actual_size}, expected {expected_size}"
        );
    }

    (0..lane_count)
        .map(|index| {
            let offset = lane_stride
                .checked_mul(u64::try_from(index).context("fixed vector lane index does not fit u64")?)
                .context("fixed vector lane offset overflow")?;
            Ok(AggregateMemoryField { offset, info: lane })
        })
        .collect()
}

fn vector_lane_index(instruction: InstructionValue<'_>, operand_index: u32) -> anyhow::Result<Option<usize>> {
    let value = instruction_operand_value(instruction, operand_index)?;
    if is_undef_or_poison_value(value) {
        bail!("vector lane index cannot be undef or poison");
    }
    if !value.is_int_value() {
        bail!("vector lane index must be an integer");
    }
    value
        .into_int_value()
        .get_zero_extended_constant()
        .map(|lane| usize::try_from(lane).context("vector lane index does not fit usize"))
        .transpose()
}

fn shuffle_vector_mask(instruction: InstructionValue<'_>) -> anyhow::Result<Vec<Option<usize>>> {
    // SAFETY: `instruction` 是当前 module 中的 live shufflevector 指令；LLVM 只读取
    // 指令自带的常量 mask 元数据，不访问用户内存。`-1` 是 LLVM C API 表示 undef lane 的约定。
    let mask_len = unsafe { LLVMGetNumMaskElements(instruction.as_value_ref()) };
    if mask_len == 0 {
        bail!("zero-lane shufflevector is not supported by vm_virtualize");
    }

    let mut mask = Vec::with_capacity(usize::try_from(mask_len).context("shufflevector mask length overflow")?);
    for index in 0..mask_len {
        let lane = unsafe { LLVMGetMaskValue(instruction.as_value_ref(), index) };
        if lane < 0 {
            mask.push(None);
        } else {
            mask.push(Some(
                usize::try_from(lane).context("shufflevector mask lane does not fit usize")?,
            ));
        }
    }
    Ok(mask)
}

fn vector_splice_mask(lane_count: usize, imm: i64) -> anyhow::Result<Vec<Option<usize>>> {
    let lane_count_i64 = i64::try_from(lane_count).context("llvm.vector.splice lane count overflow")?;
    let mut mask = Vec::with_capacity(lane_count);
    if imm >= 0 {
        let start = usize::try_from(imm).context("llvm.vector.splice positive immarg overflow")?;
        mask.extend((0..lane_count).map(|lane| Some(start + lane)));
        return Ok(mask);
    }

    let from_lhs = usize::try_from(-imm).context("llvm.vector.splice negative immarg overflow")?;
    let lhs_start = usize::try_from(lane_count_i64 + imm).context("llvm.vector.splice lhs start overflow")?;
    mask.extend((0..from_lhs).map(|lane| Some(lhs_start + lane)));
    mask.extend((0..(lane_count - from_lhs)).map(|lane| Some(lane_count + lane)));
    Ok(mask)
}

fn experimental_vp_splice_mask(
    lane_count: usize,
    imm: i64,
    evl1: usize,
    evl2: usize,
    lane_mask: &[bool],
) -> anyhow::Result<Vec<Option<usize>>> {
    if lane_mask.len() != lane_count {
        bail!(
            "llvm.experimental.vp.splice mask lane count mismatch: mask {}, result {lane_count}",
            lane_mask.len()
        );
    }
    let start = if imm >= 0 {
        usize::try_from(imm).context("llvm.experimental.vp.splice positive immarg overflow")?
    } else {
        let evl1_i64 = i64::try_from(evl1).context("llvm.experimental.vp.splice evl1 overflow")?;
        usize::try_from(evl1_i64 + imm).context("llvm.experimental.vp.splice negative immarg start overflow")?
    };
    let active_len = evl1
        .checked_add(evl2)
        .context("llvm.experimental.vp.splice active vector length overflow")?;

    (0..lane_count)
        .map(|lane| {
            if lane >= evl2 || !lane_mask[lane] {
                return Ok(None);
            }
            let window_index = start
                .checked_add(lane)
                .context("llvm.experimental.vp.splice window index overflow")?;
            if window_index >= active_len {
                return Ok(None);
            }
            if window_index < evl1 {
                Ok(Some(window_index))
            } else {
                Ok(Some(lane_count + (window_index - evl1)))
            }
        })
        .collect()
}

fn constant_i1_vector_mask(value: BasicValueEnum<'_>, expected_lanes: usize, name: &str) -> anyhow::Result<Vec<bool>> {
    if is_undef_or_poison_value(value) {
        bail!("{name} cannot be undef or poison");
    }

    let lanes = vector_fields_from_type(value.get_type()).with_context(|| format!("{name} type"))?;
    if lanes.len() != expected_lanes {
        bail!(
            "{name} lane count mismatch: mask has {}, value has {expected_lanes}",
            lanes.len()
        );
    }
    for (index, lane) in lanes.iter().enumerate() {
        if lane.kind != ScalarKind::Integer || lane.width != 1 {
            bail!("{name} lane {index} must be i1, got {:?}{}", lane.kind, lane.width);
        }
    }

    let value_ref = value.as_value_ref();
    if !unsafe { LLVMIsAConstantAggregateZero(value_ref) }.is_null() {
        return Ok(vec![false; expected_lanes]);
    }
    if unsafe { LLVMIsAConstantVector(value_ref) }.is_null()
        && unsafe { LLVMIsAConstantDataVector(value_ref) }.is_null()
    {
        bail!("{name} must be a constant <N x i1> vector");
    }

    let mut mask = Vec::with_capacity(expected_lanes);
    for index in 0..expected_lanes {
        // SAFETY: `value_ref` is a live LLVM constant vector from this module. The index is bounded
        // by the vector lane count checked above, and LLVM returns null for unsupported constants.
        let lane_ref = unsafe { LLVMGetAggregateElement(value_ref, index as u32) };
        if lane_ref.is_null() {
            bail!("{name} lane {index} is not materialized as a constant");
        }
        if unsafe { LLVMIsAConstantInt(lane_ref) }.is_null() {
            bail!("{name} lane {index} must be an i1 constant");
        }
        let lane = unsafe { BasicValueEnum::new(lane_ref) };
        let bit = lane
            .into_int_value()
            .get_zero_extended_constant()
            .with_context(|| format!("{name} lane {index} is not a scalar constant"))?;
        if bit > 1 {
            bail!("{name} lane {index} has invalid i1 value {bit}");
        }
        mask.push(bit != 0);
    }
    Ok(mask)
}

fn instruction_aggregate_type(instruction: InstructionValue<'_>) -> anyhow::Result<BasicTypeEnum<'_>> {
    match instruction.get_type() {
        AnyTypeEnum::StructType(ty) => Ok(BasicTypeEnum::StructType(ty)),
        AnyTypeEnum::ArrayType(ty) => Ok(BasicTypeEnum::ArrayType(ty)),
        other => bail!("instruction result is not an aggregate: {other:?}"),
    }
}

fn instruction_composite_fields(instruction: InstructionValue<'_>) -> anyhow::Result<Vec<ReturnField>> {
    match instruction.get_type() {
        AnyTypeEnum::StructType(ty) => return_fields_from_composite_type(BasicTypeEnum::StructType(ty)),
        AnyTypeEnum::ArrayType(ty) => return_fields_from_composite_type(BasicTypeEnum::ArrayType(ty)),
        AnyTypeEnum::VectorType(ty) => vector_fields_from_type(BasicTypeEnum::VectorType(ty)),
        other => bail!("instruction result is not a supported composite value: {other:?}"),
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
            fields: composite_fields_from_type(ty)?,
            is_aggregate: matches!(
                ty,
                BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_) | BasicTypeEnum::VectorType(_)
            ),
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
                flattened_index += composite_fields_from_type(field_ty)
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
            let element_leaf_count = composite_fields_from_type(element_ty)
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

fn metadata_string_operand(instruction: InstructionValue<'_>, index: u32, name: &str) -> anyhow::Result<String> {
    // SAFETY: `instruction` is live while lowering. We only inspect operand metadata and copy the
    // MDString text through the amice-llvm shim; non-string metadata is reported as unsupported.
    let value = unsafe { LLVMGetOperand(instruction.as_value_ref(), index) };
    if value.is_null() {
        bail!("{name} operand is missing");
    }
    metadata_string_from_value_ref(value).with_context(|| format!("{name} must be metadata string"))
}

fn validate_constrained_float_metadata(
    instruction: InstructionValue<'_>,
    rounding_index: Option<u32>,
    exception_index: u32,
    name: &str,
) -> anyhow::Result<()> {
    if let Some(index) = rounding_index {
        let rounding_name = format!("{name} rounding mode");
        let rounding = metadata_string_operand(instruction, index, &rounding_name)?;
        if rounding != "round.tonearest" {
            bail!("{name} rounding mode {rounding} is not supported by vm_virtualize");
        }
    }

    let exception_name = format!("{name} exception behavior");
    let exception = metadata_string_operand(instruction, exception_index, &exception_name)?;
    if exception != "fpexcept.ignore" {
        bail!("{name} exception behavior {exception} is not supported by vm_virtualize");
    }
    Ok(())
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

fn masked_memory_intrinsic_kind(function: FunctionValue<'_>) -> Option<MaskedMemoryIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.masked.load.") {
        Some(MaskedMemoryIntrinsicKind::Load)
    } else if name.starts_with("llvm.masked.store.") {
        Some(MaskedMemoryIntrinsicKind::Store)
    } else if name.starts_with("llvm.masked.expandload.") {
        Some(MaskedMemoryIntrinsicKind::ExpandLoad)
    } else if name.starts_with("llvm.masked.compressstore.") {
        Some(MaskedMemoryIntrinsicKind::CompressStore)
    } else if name.starts_with("llvm.masked.gather.") {
        Some(MaskedMemoryIntrinsicKind::Gather)
    } else if name.starts_with("llvm.masked.scatter.") {
        Some(MaskedMemoryIntrinsicKind::Scatter)
    } else if name.starts_with("llvm.experimental.vp.strided.load.") {
        Some(MaskedMemoryIntrinsicKind::VpStridedLoad)
    } else if name.starts_with("llvm.experimental.vp.strided.store.") {
        Some(MaskedMemoryIntrinsicKind::VpStridedStore)
    } else if name.starts_with("llvm.vp.load.") {
        Some(MaskedMemoryIntrinsicKind::VpLoad)
    } else if name.starts_with("llvm.vp.store.") {
        Some(MaskedMemoryIntrinsicKind::VpStore)
    } else if name.starts_with("llvm.vp.gather.") {
        Some(MaskedMemoryIntrinsicKind::VpGather)
    } else if name.starts_with("llvm.vp.scatter.") {
        Some(MaskedMemoryIntrinsicKind::VpScatter)
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

fn vscale_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().starts_with("llvm.vscale.")
}

fn get_rounding_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().as_ref() == "llvm.get.rounding"
}

fn flt_rounds_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().as_ref() == "llvm.flt.rounds"
}

fn fp_state_intrinsic_kind(function: FunctionValue<'_>) -> Option<FpStateIntrinsicKind> {
    match function.get_name().to_string_lossy().as_ref() {
        "llvm.set.rounding" => Some(FpStateIntrinsicKind::SetRounding),
        "llvm.reset.fpenv" => Some(FpStateIntrinsicKind::Reset(FpStateKind::Env)),
        "llvm.reset.fpmode" => Some(FpStateIntrinsicKind::Reset(FpStateKind::Mode)),
        name if name.starts_with("llvm.get.fpenv.") => Some(FpStateIntrinsicKind::Get(FpStateKind::Env)),
        name if name.starts_with("llvm.set.fpenv.") => Some(FpStateIntrinsicKind::Set(FpStateKind::Env)),
        name if name.starts_with("llvm.get.fpmode.") => Some(FpStateIntrinsicKind::Get(FpStateKind::Mode)),
        name if name.starts_with("llvm.set.fpmode.") => Some(FpStateIntrinsicKind::Set(FpStateKind::Mode)),
        _ => None,
    }
}

fn thread_pointer_intrinsic(function: FunctionValue<'_>) -> bool {
    function
        .get_name()
        .to_string_lossy()
        .starts_with("llvm.thread.pointer.")
}

fn sideeffect_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().as_ref() == "llvm.sideeffect"
}

fn clear_cache_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().as_ref() == "llvm.clear_cache"
}

fn pseudoprobe_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().as_ref() == "llvm.pseudoprobe"
}

fn prefetch_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().starts_with("llvm.prefetch.")
}

fn stack_intrinsic_kind(function: FunctionValue<'_>) -> Option<StackIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.stacksave") {
        Some(StackIntrinsicKind::Save)
    } else if name.starts_with("llvm.stackrestore") {
        Some(StackIntrinsicKind::Restore)
    } else {
        None
    }
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
    } else if name == "llvm.experimental.noalias.scope.decl" {
        Some(NopIntrinsicKind::NoAliasScopeDecl)
    } else if name == "llvm.donothing" {
        Some(NopIntrinsicKind::DoNothing)
    } else if name == "llvm.fake.use" {
        Some(NopIntrinsicKind::FakeUse)
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
    } else if name.starts_with("llvm.arithmetic.fence.") {
        Some(IdentityIntrinsicKind::ArithmeticFenceScalar)
    } else if name.starts_with("llvm.launder.invariant.group.") {
        Some(IdentityIntrinsicKind::LaunderInvariantGroup)
    } else if name.starts_with("llvm.strip.invariant.group.") {
        Some(IdentityIntrinsicKind::StripInvariantGroup)
    } else if name.starts_with("llvm.preserve.array.access.index.") {
        Some(IdentityIntrinsicKind::PreserveArrayAccessIndex)
    } else if name.starts_with("llvm.preserve.union.access.index.") {
        Some(IdentityIntrinsicKind::PreserveUnionAccessIndex)
    } else if name.starts_with("llvm.preserve.struct.access.index.") {
        Some(IdentityIntrinsicKind::PreserveStructAccessIndex)
    } else if name == "llvm.preserve.static.offset" {
        Some(IdentityIntrinsicKind::PreserveStaticOffset)
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

fn vector_permute_intrinsic_kind(function: FunctionValue<'_>) -> Option<VectorPermuteIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vector.reverse.") {
        Some(VectorPermuteIntrinsicKind::Reverse)
    } else if name.starts_with("llvm.vector.splice.") {
        Some(VectorPermuteIntrinsicKind::Splice)
    } else if name.starts_with("llvm.vector.insert.") {
        Some(VectorPermuteIntrinsicKind::InsertSubvector)
    } else if name.starts_with("llvm.vector.extract.") {
        Some(VectorPermuteIntrinsicKind::ExtractSubvector)
    } else if let Some(factor) = vector_interleave_factor(&name) {
        Some(VectorPermuteIntrinsicKind::Interleave(factor))
    } else if let Some(factor) = vector_deinterleave_factor(&name) {
        Some(VectorPermuteIntrinsicKind::Deinterleave(factor))
    } else if name.starts_with("llvm.experimental.vector.compress.") {
        Some(VectorPermuteIntrinsicKind::Compress)
    } else {
        None
    }
}

fn experimental_vp_splice_intrinsic(function: FunctionValue<'_>) -> bool {
    function
        .get_name()
        .to_string_lossy()
        .starts_with("llvm.experimental.vp.splice.")
}

fn vector_interleave_factor(name: &str) -> Option<u8> {
    let rest = name.strip_prefix("llvm.vector.interleave")?;
    let (factor, suffix) = rest.split_once('.')?;
    if !suffix.is_empty() {
        let factor = factor.parse::<u8>().ok()?;
        if (2..=8).contains(&factor) {
            return Some(factor);
        }
    }
    None
}

fn vector_deinterleave_factor(name: &str) -> Option<u8> {
    let rest = name.strip_prefix("llvm.vector.deinterleave")?;
    let (factor, suffix) = rest.split_once('.')?;
    if !suffix.is_empty() {
        let factor = factor.parse::<u8>().ok()?;
        if (2..=8).contains(&factor) {
            return Some(factor);
        }
    }
    None
}

fn experimental_vector_extract_last_active_intrinsic(function: FunctionValue<'_>) -> bool {
    function
        .get_name()
        .to_string_lossy()
        .starts_with("llvm.experimental.vector.extract.last.active.")
}

fn get_active_lane_mask_intrinsic(function: FunctionValue<'_>) -> bool {
    function
        .get_name()
        .to_string_lossy()
        .starts_with("llvm.get.active.lane.mask.")
}

fn experimental_get_vector_length_intrinsic(function: FunctionValue<'_>) -> bool {
    function
        .get_name()
        .to_string_lossy()
        .starts_with("llvm.experimental.get.vector.length.")
}

fn experimental_cttz_elts_intrinsic(function: FunctionValue<'_>) -> bool {
    function
        .get_name()
        .to_string_lossy()
        .starts_with("llvm.experimental.cttz.elts.")
}

fn vp_cttz_elts_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().starts_with("llvm.vp.cttz.elts.")
}

fn compile_time_intrinsic_kind(function: FunctionValue<'_>) -> Option<CompileTimeIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name == "llvm.allow.runtime.check" {
        Some(CompileTimeIntrinsicKind::AllowRuntimeCheck)
    } else if name == "llvm.allow.ubsan.check" {
        Some(CompileTimeIntrinsicKind::AllowUbsanCheck)
    } else if name.starts_with("llvm.is.constant.") {
        Some(CompileTimeIntrinsicKind::IsConstant)
    } else if name.starts_with("llvm.objectsize.") {
        Some(CompileTimeIntrinsicKind::ObjectSize)
    } else if name == "llvm.experimental.widenable.condition" {
        Some(CompileTimeIntrinsicKind::WidenableCondition)
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
    } else if name.starts_with("llvm.lrint.") {
        Some(FloatIntrinsicKind::LRint)
    } else if name.starts_with("llvm.llrint.") {
        Some(FloatIntrinsicKind::LLRint)
    } else if name.starts_with("llvm.lround.") {
        Some(FloatIntrinsicKind::LRound)
    } else if name.starts_with("llvm.llround.") {
        Some(FloatIntrinsicKind::LLRound)
    } else if name.starts_with("llvm.sin.") {
        Some(FloatIntrinsicKind::Sin)
    } else if name.starts_with("llvm.cos.") {
        Some(FloatIntrinsicKind::Cos)
    } else if name.starts_with("llvm.exp2.") {
        Some(FloatIntrinsicKind::Exp2)
    } else if name.starts_with("llvm.exp.") {
        Some(FloatIntrinsicKind::Exp)
    } else if name.starts_with("llvm.log10.") {
        Some(FloatIntrinsicKind::Log10)
    } else if name.starts_with("llvm.log2.") {
        Some(FloatIntrinsicKind::Log2)
    } else if name.starts_with("llvm.log.") {
        Some(FloatIntrinsicKind::Log)
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
    } else if name.starts_with("llvm.powi.") {
        Some(FloatIntrinsicKind::PowI)
    } else if name.starts_with("llvm.pow.") {
        Some(FloatIntrinsicKind::Pow)
    } else if name.starts_with("llvm.is.fpclass.") {
        Some(FloatIntrinsicKind::IsFpClass)
    } else if name.starts_with("llvm.fptosi.sat.") {
        Some(FloatIntrinsicKind::FPToSISat)
    } else if name.starts_with("llvm.fptoui.sat.") {
        Some(FloatIntrinsicKind::FPToUISat)
    } else if name.starts_with("llvm.convert.to.fp16.") {
        Some(FloatIntrinsicKind::ConvertToFp16)
    } else if name.starts_with("llvm.convert.from.fp16.") {
        Some(FloatIntrinsicKind::ConvertFromFp16)
    } else {
        None
    }
}

fn constrained_float_binop_intrinsic_kind(function: FunctionValue<'_>) -> Option<ConstrainedFloatBinOpKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.experimental.constrained.fadd.") {
        Some(ConstrainedFloatBinOpKind::Add)
    } else if name.starts_with("llvm.experimental.constrained.fsub.") {
        Some(ConstrainedFloatBinOpKind::Sub)
    } else if name.starts_with("llvm.experimental.constrained.fmul.") {
        Some(ConstrainedFloatBinOpKind::Mul)
    } else if name.starts_with("llvm.experimental.constrained.fdiv.") {
        Some(ConstrainedFloatBinOpKind::Div)
    } else if name.starts_with("llvm.experimental.constrained.frem.") {
        Some(ConstrainedFloatBinOpKind::Rem)
    } else {
        None
    }
}

fn constrained_float_unary_intrinsic_kind(function: FunctionValue<'_>) -> Option<FloatIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.experimental.constrained.fabs.") {
        Some(FloatIntrinsicKind::FAbs)
    } else if name.starts_with("llvm.experimental.constrained.sqrt.") {
        Some(FloatIntrinsicKind::Sqrt)
    } else if name.starts_with("llvm.experimental.constrained.canonicalize.") {
        Some(FloatIntrinsicKind::Canonicalize)
    } else if name.starts_with("llvm.experimental.constrained.floor.") {
        Some(FloatIntrinsicKind::Floor)
    } else if name.starts_with("llvm.experimental.constrained.ceil.") {
        Some(FloatIntrinsicKind::Ceil)
    } else if name.starts_with("llvm.experimental.constrained.trunc.") {
        Some(FloatIntrinsicKind::Trunc)
    } else if name.starts_with("llvm.experimental.constrained.rint.") {
        Some(FloatIntrinsicKind::Rint)
    } else if name.starts_with("llvm.experimental.constrained.nearbyint.") {
        Some(FloatIntrinsicKind::NearbyInt)
    } else if name.starts_with("llvm.experimental.constrained.round.") {
        Some(FloatIntrinsicKind::Round)
    } else if name.starts_with("llvm.experimental.constrained.roundeven.") {
        Some(FloatIntrinsicKind::RoundEven)
    } else if name.starts_with("llvm.experimental.constrained.sin.") {
        Some(FloatIntrinsicKind::Sin)
    } else if name.starts_with("llvm.experimental.constrained.cos.") {
        Some(FloatIntrinsicKind::Cos)
    } else if name.starts_with("llvm.experimental.constrained.exp.") {
        Some(FloatIntrinsicKind::Exp)
    } else if name.starts_with("llvm.experimental.constrained.exp2.") {
        Some(FloatIntrinsicKind::Exp2)
    } else if name.starts_with("llvm.experimental.constrained.log.") {
        Some(FloatIntrinsicKind::Log)
    } else if name.starts_with("llvm.experimental.constrained.log10.") {
        Some(FloatIntrinsicKind::Log10)
    } else if name.starts_with("llvm.experimental.constrained.log2.") {
        Some(FloatIntrinsicKind::Log2)
    } else {
        None
    }
}

fn constrained_float_binary_intrinsic_kind(function: FunctionValue<'_>) -> Option<FloatIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.experimental.constrained.pow.") {
        Some(FloatIntrinsicKind::Pow)
    } else if name.starts_with("llvm.experimental.constrained.minnum.") {
        Some(FloatIntrinsicKind::MinNum)
    } else if name.starts_with("llvm.experimental.constrained.maxnum.") {
        Some(FloatIntrinsicKind::MaxNum)
    } else if name.starts_with("llvm.experimental.constrained.minimum.") {
        Some(FloatIntrinsicKind::Minimum)
    } else if name.starts_with("llvm.experimental.constrained.maximum.") {
        Some(FloatIntrinsicKind::Maximum)
    } else if name.starts_with("llvm.experimental.constrained.copysign.") {
        Some(FloatIntrinsicKind::CopySign)
    } else {
        None
    }
}

fn constrained_float_int_binary_intrinsic_kind(function: FunctionValue<'_>) -> Option<FloatIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.experimental.constrained.powi.") {
        Some(FloatIntrinsicKind::PowI)
    } else {
        None
    }
}

fn constrained_float_ternary_intrinsic_kind(function: FunctionValue<'_>) -> Option<FloatIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.experimental.constrained.fma.") {
        Some(FloatIntrinsicKind::Fma)
    } else if name.starts_with("llvm.experimental.constrained.fmuladd.") {
        Some(FloatIntrinsicKind::FmulAdd)
    } else {
        None
    }
}

fn constrained_round_to_int_intrinsic_kind(function: FunctionValue<'_>) -> Option<FloatIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.experimental.constrained.lrint.") {
        Some(FloatIntrinsicKind::LRint)
    } else if name.starts_with("llvm.experimental.constrained.llrint.") {
        Some(FloatIntrinsicKind::LLRint)
    } else if name.starts_with("llvm.experimental.constrained.lround.") {
        Some(FloatIntrinsicKind::LRound)
    } else if name.starts_with("llvm.experimental.constrained.llround.") {
        Some(FloatIntrinsicKind::LLRound)
    } else {
        None
    }
}

fn constrained_float_cmp_intrinsic_kind(function: FunctionValue<'_>) -> Option<ConstrainedFloatCmpKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.experimental.constrained.fcmp.") {
        Some(ConstrainedFloatCmpKind::Quiet)
    } else if name.starts_with("llvm.experimental.constrained.fcmps.") {
        Some(ConstrainedFloatCmpKind::Signaling)
    } else {
        None
    }
}

fn constrained_float_cast_intrinsic_kind(function: FunctionValue<'_>) -> Option<ConstrainedFloatCastKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.experimental.constrained.sitofp.") {
        Some(ConstrainedFloatCastKind::SIToFP)
    } else if name.starts_with("llvm.experimental.constrained.uitofp.") {
        Some(ConstrainedFloatCastKind::UIToFP)
    } else if name.starts_with("llvm.experimental.constrained.fptosi.") {
        Some(ConstrainedFloatCastKind::FPToSI)
    } else if name.starts_with("llvm.experimental.constrained.fptoui.") {
        Some(ConstrainedFloatCastKind::FPToUI)
    } else if name.starts_with("llvm.experimental.constrained.fptrunc.") {
        Some(ConstrainedFloatCastKind::FPTrunc)
    } else if name.starts_with("llvm.experimental.constrained.fpext.") {
        Some(ConstrainedFloatCastKind::FPExt)
    } else {
        None
    }
}

fn vector_reduce_float_intrinsic_kind(function: FunctionValue<'_>) -> Option<VectorReduceFloatKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vector.reduce.fadd.") {
        Some(VectorReduceFloatKind::Add)
    } else if name.starts_with("llvm.vector.reduce.fmul.") {
        Some(VectorReduceFloatKind::Mul)
    } else if name.starts_with("llvm.vector.reduce.fmin.") {
        Some(VectorReduceFloatKind::Min)
    } else if name.starts_with("llvm.vector.reduce.fmax.") {
        Some(VectorReduceFloatKind::Max)
    } else if name.starts_with("llvm.vector.reduce.fminimum.") {
        Some(VectorReduceFloatKind::Minimum)
    } else if name.starts_with("llvm.vector.reduce.fmaximum.") {
        Some(VectorReduceFloatKind::Maximum)
    } else {
        None
    }
}

fn vp_reduce_float_intrinsic_kind(function: FunctionValue<'_>) -> Option<VectorReduceFloatKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.reduce.fadd.") {
        Some(VectorReduceFloatKind::Add)
    } else if name.starts_with("llvm.vp.reduce.fmul.") {
        Some(VectorReduceFloatKind::Mul)
    } else if name.starts_with("llvm.vp.reduce.fmin.") {
        Some(VectorReduceFloatKind::Min)
    } else if name.starts_with("llvm.vp.reduce.fmax.") {
        Some(VectorReduceFloatKind::Max)
    } else if name.starts_with("llvm.vp.reduce.fminimum.") {
        Some(VectorReduceFloatKind::Minimum)
    } else if name.starts_with("llvm.vp.reduce.fmaximum.") {
        Some(VectorReduceFloatKind::Maximum)
    } else {
        None
    }
}

fn stepvector_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().starts_with("llvm.stepvector.")
}

fn vp_select_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().starts_with("llvm.vp.select.")
}

fn vp_merge_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().starts_with("llvm.vp.merge.")
}

fn experimental_vp_reverse_intrinsic(function: FunctionValue<'_>) -> bool {
    function
        .get_name()
        .to_string_lossy()
        .starts_with("llvm.experimental.vp.reverse.")
}

fn experimental_vp_splat_intrinsic(function: FunctionValue<'_>) -> bool {
    function
        .get_name()
        .to_string_lossy()
        .starts_with("llvm.experimental.vp.splat.")
}

fn vp_is_fpclass_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().starts_with("llvm.vp.is.fpclass.")
}

fn vp_pointer_cast_intrinsic_kind(function: FunctionValue<'_>) -> Option<VpPointerCastKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.ptrtoint.") {
        Some(VpPointerCastKind::PtrToInt)
    } else if name.starts_with("llvm.vp.inttoptr.") {
        Some(VpPointerCastKind::IntToPtr)
    } else {
        None
    }
}

fn vp_integer_cast_intrinsic_kind(function: FunctionValue<'_>) -> Option<VpIntegerCastKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.zext.") {
        Some(VpIntegerCastKind::ZExt)
    } else if name.starts_with("llvm.vp.sext.") {
        Some(VpIntegerCastKind::SExt)
    } else if name.starts_with("llvm.vp.trunc.") {
        Some(VpIntegerCastKind::Trunc)
    } else {
        None
    }
}

fn vp_float_cast_intrinsic_kind(function: FunctionValue<'_>) -> Option<VpFloatCastKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.sitofp.") {
        Some(VpFloatCastKind::SIToFP)
    } else if name.starts_with("llvm.vp.uitofp.") {
        Some(VpFloatCastKind::UIToFP)
    } else if name.starts_with("llvm.vp.fptosi.") {
        Some(VpFloatCastKind::FPToSI)
    } else if name.starts_with("llvm.vp.fptoui.") {
        Some(VpFloatCastKind::FPToUI)
    } else if name.starts_with("llvm.vp.fptrunc.") {
        Some(VpFloatCastKind::FPTrunc)
    } else if name.starts_with("llvm.vp.fpext.") {
        Some(VpFloatCastKind::FPExt)
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
    } else if name.starts_with("llvm.loop.decrement.reg.") {
        Some(IntegerIntrinsicKind::LoopDecrementReg)
    } else if name.starts_with("llvm.fshl.") {
        Some(IntegerIntrinsicKind::FShl)
    } else if name.starts_with("llvm.fshr.") {
        Some(IntegerIntrinsicKind::FShr)
    } else {
        None
    }
}

fn hardware_loop_intrinsic_kind(function: FunctionValue<'_>) -> Option<HardwareLoopIntrinsicKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.set.loop.iterations.") {
        Some(HardwareLoopIntrinsicKind::SetIterations)
    } else if name.starts_with("llvm.start.loop.iterations.") {
        Some(HardwareLoopIntrinsicKind::StartIterations)
    } else if name.starts_with("llvm.test.set.loop.iterations.") {
        Some(HardwareLoopIntrinsicKind::TestSetIterations)
    } else if name.starts_with("llvm.test.start.loop.iterations.") {
        Some(HardwareLoopIntrinsicKind::TestStartIterations)
    } else {
        None
    }
}

fn loop_decrement_intrinsic(function: FunctionValue<'_>) -> bool {
    let name = function.get_name().to_string_lossy();
    name.starts_with("llvm.loop.decrement.") && !name.starts_with("llvm.loop.decrement.reg.")
}

fn vp_integer_unary_intrinsic_kind(function: FunctionValue<'_>) -> Option<VpIntegerUnaryKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.ctpop.") {
        Some(VpIntegerUnaryKind::CtPop)
    } else if name.starts_with("llvm.vp.ctlz.") {
        Some(VpIntegerUnaryKind::CtLz)
    } else if name.starts_with("llvm.vp.cttz.") {
        Some(VpIntegerUnaryKind::CtTz)
    } else if name.starts_with("llvm.vp.abs.") {
        Some(VpIntegerUnaryKind::Abs)
    } else if name.starts_with("llvm.vp.bswap.") {
        Some(VpIntegerUnaryKind::BSwap)
    } else if name.starts_with("llvm.vp.bitreverse.") {
        Some(VpIntegerUnaryKind::BitReverse)
    } else {
        None
    }
}

fn vp_float_unary_intrinsic_kind(function: FunctionValue<'_>) -> Option<VpFloatUnaryKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.fneg.") {
        Some(VpFloatUnaryKind::Neg)
    } else if name.starts_with("llvm.vp.fabs.") {
        Some(VpFloatUnaryKind::Abs)
    } else if name.starts_with("llvm.vp.sqrt.") {
        Some(VpFloatUnaryKind::Sqrt)
    } else if name.starts_with("llvm.vp.canonicalize.") {
        Some(VpFloatUnaryKind::Canonicalize)
    } else if name.starts_with("llvm.vp.floor.") {
        Some(VpFloatUnaryKind::Floor)
    } else if name.starts_with("llvm.vp.ceil.") {
        Some(VpFloatUnaryKind::Ceil)
    } else if name.starts_with("llvm.vp.roundtozero.") {
        Some(VpFloatUnaryKind::RoundToZero)
    } else if name.starts_with("llvm.vp.rint.") {
        Some(VpFloatUnaryKind::Rint)
    } else if name.starts_with("llvm.vp.nearbyint.") {
        Some(VpFloatUnaryKind::NearbyInt)
    } else if name.starts_with("llvm.vp.round.") {
        Some(VpFloatUnaryKind::Round)
    } else if name.starts_with("llvm.vp.roundeven.") {
        Some(VpFloatUnaryKind::RoundEven)
    } else if name.starts_with("llvm.vp.sin.") {
        Some(VpFloatUnaryKind::Sin)
    } else if name.starts_with("llvm.vp.cos.") {
        Some(VpFloatUnaryKind::Cos)
    } else if name.starts_with("llvm.vp.exp.") {
        Some(VpFloatUnaryKind::Exp)
    } else if name.starts_with("llvm.vp.exp2.") {
        Some(VpFloatUnaryKind::Exp2)
    } else if name.starts_with("llvm.vp.log.") {
        Some(VpFloatUnaryKind::Log)
    } else if name.starts_with("llvm.vp.log10.") {
        Some(VpFloatUnaryKind::Log10)
    } else if name.starts_with("llvm.vp.log2.") {
        Some(VpFloatUnaryKind::Log2)
    } else {
        None
    }
}

fn vp_round_to_int_intrinsic_kind(function: FunctionValue<'_>) -> Option<VpRoundToIntKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.lrint.") {
        Some(VpRoundToIntKind::LRint)
    } else if name.starts_with("llvm.vp.llrint.") {
        Some(VpRoundToIntKind::LLRint)
    } else {
        None
    }
}

fn vp_float_ternary_intrinsic_kind(function: FunctionValue<'_>) -> Option<VpFloatTernaryKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.fma.") {
        Some(VpFloatTernaryKind::Fma)
    } else if name.starts_with("llvm.vp.fmuladd.") {
        Some(VpFloatTernaryKind::MulAdd)
    } else {
        None
    }
}

fn vp_icmp_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().starts_with("llvm.vp.icmp.")
}

fn vp_fcmp_intrinsic(function: FunctionValue<'_>) -> bool {
    function.get_name().to_string_lossy().starts_with("llvm.vp.fcmp.")
}

fn vp_float_binop_intrinsic_kind(function: FunctionValue<'_>) -> Option<VpFloatBinopKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.fadd.") {
        Some(VpFloatBinopKind::Add)
    } else if name.starts_with("llvm.vp.fsub.") {
        Some(VpFloatBinopKind::Sub)
    } else if name.starts_with("llvm.vp.fmul.") {
        Some(VpFloatBinopKind::Mul)
    } else if name.starts_with("llvm.vp.fdiv.") {
        Some(VpFloatBinopKind::Div)
    } else if name.starts_with("llvm.vp.frem.") {
        Some(VpFloatBinopKind::Rem)
    } else if name.starts_with("llvm.vp.minnum.") {
        Some(VpFloatBinopKind::MinNum)
    } else if name.starts_with("llvm.vp.maxnum.") {
        Some(VpFloatBinopKind::MaxNum)
    } else if name.starts_with("llvm.vp.minimum.") {
        Some(VpFloatBinopKind::Minimum)
    } else if name.starts_with("llvm.vp.maximum.") {
        Some(VpFloatBinopKind::Maximum)
    } else if name.starts_with("llvm.vp.copysign.") {
        Some(VpFloatBinopKind::CopySign)
    } else {
        None
    }
}

fn vp_integer_binop_intrinsic_kind(function: FunctionValue<'_>) -> Option<VpIntegerBinopKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.add.") {
        Some(VpIntegerBinopKind::Add)
    } else if name.starts_with("llvm.vp.sub.") {
        Some(VpIntegerBinopKind::Sub)
    } else if name.starts_with("llvm.vp.mul.") {
        Some(VpIntegerBinopKind::Mul)
    } else if name.starts_with("llvm.vp.udiv.") {
        Some(VpIntegerBinopKind::UDiv)
    } else if name.starts_with("llvm.vp.sdiv.") {
        Some(VpIntegerBinopKind::SDiv)
    } else if name.starts_with("llvm.vp.urem.") {
        Some(VpIntegerBinopKind::URem)
    } else if name.starts_with("llvm.vp.srem.") {
        Some(VpIntegerBinopKind::SRem)
    } else if name.starts_with("llvm.vp.smax.") {
        Some(VpIntegerBinopKind::SMax)
    } else if name.starts_with("llvm.vp.smin.") {
        Some(VpIntegerBinopKind::SMin)
    } else if name.starts_with("llvm.vp.umax.") {
        Some(VpIntegerBinopKind::UMax)
    } else if name.starts_with("llvm.vp.umin.") {
        Some(VpIntegerBinopKind::UMin)
    } else if name.starts_with("llvm.vp.uadd.sat.") {
        Some(VpIntegerBinopKind::UAddSat)
    } else if name.starts_with("llvm.vp.usub.sat.") {
        Some(VpIntegerBinopKind::USubSat)
    } else if name.starts_with("llvm.vp.sadd.sat.") {
        Some(VpIntegerBinopKind::SAddSat)
    } else if name.starts_with("llvm.vp.ssub.sat.") {
        Some(VpIntegerBinopKind::SSubSat)
    } else if name.starts_with("llvm.vp.xor.") {
        Some(VpIntegerBinopKind::Xor)
    } else if name.starts_with("llvm.vp.and.") {
        Some(VpIntegerBinopKind::And)
    } else if name.starts_with("llvm.vp.or.") {
        Some(VpIntegerBinopKind::Or)
    } else if name.starts_with("llvm.vp.shl.") {
        Some(VpIntegerBinopKind::Shl)
    } else if name.starts_with("llvm.vp.lshr.") {
        Some(VpIntegerBinopKind::LShr)
    } else if name.starts_with("llvm.vp.ashr.") {
        Some(VpIntegerBinopKind::AShr)
    } else {
        None
    }
}

fn vp_integer_ternary_intrinsic_kind(function: FunctionValue<'_>) -> Option<VpIntegerTernaryKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.fshl.") {
        Some(VpIntegerTernaryKind::FShl)
    } else if name.starts_with("llvm.vp.fshr.") {
        Some(VpIntegerTernaryKind::FShr)
    } else {
        None
    }
}

fn vector_reduce_integer_intrinsic_kind(function: FunctionValue<'_>) -> Option<VectorReduceIntegerKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vector.reduce.add.") {
        Some(VectorReduceIntegerKind::Add)
    } else if name.starts_with("llvm.vector.reduce.mul.") {
        Some(VectorReduceIntegerKind::Mul)
    } else if name.starts_with("llvm.vector.reduce.and.") {
        Some(VectorReduceIntegerKind::And)
    } else if name.starts_with("llvm.vector.reduce.or.") {
        Some(VectorReduceIntegerKind::Or)
    } else if name.starts_with("llvm.vector.reduce.xor.") {
        Some(VectorReduceIntegerKind::Xor)
    } else if name.starts_with("llvm.vector.reduce.smax.") {
        Some(VectorReduceIntegerKind::SMax)
    } else if name.starts_with("llvm.vector.reduce.smin.") {
        Some(VectorReduceIntegerKind::SMin)
    } else if name.starts_with("llvm.vector.reduce.umax.") {
        Some(VectorReduceIntegerKind::UMax)
    } else if name.starts_with("llvm.vector.reduce.umin.") {
        Some(VectorReduceIntegerKind::UMin)
    } else {
        None
    }
}

fn vp_reduce_integer_intrinsic_kind(function: FunctionValue<'_>) -> Option<VectorReduceIntegerKind> {
    let name = function.get_name().to_string_lossy();
    if name.starts_with("llvm.vp.reduce.add.") {
        Some(VectorReduceIntegerKind::Add)
    } else if name.starts_with("llvm.vp.reduce.mul.") {
        Some(VectorReduceIntegerKind::Mul)
    } else if name.starts_with("llvm.vp.reduce.and.") {
        Some(VectorReduceIntegerKind::And)
    } else if name.starts_with("llvm.vp.reduce.or.") {
        Some(VectorReduceIntegerKind::Or)
    } else if name.starts_with("llvm.vp.reduce.xor.") {
        Some(VectorReduceIntegerKind::Xor)
    } else if name.starts_with("llvm.vp.reduce.smax.") {
        Some(VectorReduceIntegerKind::SMax)
    } else if name.starts_with("llvm.vp.reduce.smin.") {
        Some(VectorReduceIntegerKind::SMin)
    } else if name.starts_with("llvm.vp.reduce.umax.") {
        Some(VectorReduceIntegerKind::UMax)
    } else if name.starts_with("llvm.vp.reduce.umin.") {
        Some(VectorReduceIntegerKind::UMin)
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

fn signed_constant_int_operand(instruction: InstructionValue<'_>, index: u32, name: &str) -> anyhow::Result<i64> {
    let value = instruction_operand_value(instruction, index)?;
    if is_undef_or_poison_value(value) {
        bail!("{name} cannot be undef or poison");
    }
    if !value.is_int_value() {
        bail!("{name} must be an integer constant");
    }
    value
        .into_int_value()
        .get_sign_extended_constant()
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

fn ensure_is_constant_query_operand(value: BasicValueEnum<'_>) -> anyhow::Result<()> {
    if is_undef_or_poison_value(value) {
        bail!("llvm.is.constant operand cannot be undef or poison");
    }

    match value.get_type() {
        BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_) | BasicTypeEnum::PointerType(_) => {
            value_width(value).context(
                "llvm.is.constant only supports integer, pointer, half, float, double scalar, or fixed vector operands",
            )?;
        },
        BasicTypeEnum::VectorType(_) => {
            vector_fields_from_type(value.get_type()).context(
                "llvm.is.constant only supports fixed vectors with integer, pointer, half, float, or double lanes",
            )?;
        },
        other => bail!("unsupported llvm.is.constant operand type: {other:?}"),
    }
    Ok(())
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
                "identity intrinsic {:?} only supports integer, pointer, half, float, and double scalar copies; got source {:?}, result {:?}",
                kind,
                source_type,
                result_type
            );
        },
    }
    Ok(())
}

fn ensure_arithmetic_fence_shape(source: BasicValueEnum<'_>, result_type: AnyTypeEnum<'_>) -> anyhow::Result<()> {
    match (source.get_type(), result_type) {
        (BasicTypeEnum::FloatType(source_type), AnyTypeEnum::FloatType(result_type)) => {
            let source_width = float_type_width(source_type.as_type_ref())?;
            let result_width = float_type_width(result_type.as_type_ref())?;
            if source_width != result_width {
                bail!(
                    "llvm.arithmetic.fence scalar width mismatch: result f{}, value f{}",
                    result_width,
                    source_width
                );
            }
        },
        (source_type, result_type) => {
            bail!(
                "llvm.arithmetic.fence only supports half, float, and double scalar operands; got source {:?}, result {:?}",
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
/// 当 value 不是 `half`、`float` 或 `double` 时返回错误；更宽或特殊浮点类型暂不进入 x-register lowering。
pub fn float_value_width(value: amice_plugin::inkwell::values::FloatValue<'_>) -> anyhow::Result<u8> {
    float_type_width(value.get_type().as_type_ref())
}

/// 返回当前 VMP 标量浮点路径支持的 LLVM float type 位宽。
///
/// # Errors
/// 仅接受 LLVM `half`、`float` 和 `double`，其它浮点类型会作为 safe-skip 边界返回错误。
pub fn float_type_width(type_ref: LLVMTypeRef) -> anyhow::Result<u8> {
    // SAFETY: caller passes an LLVM type reference from the current module/context. This only
    // inspects the type kind and does not dereference user memory.
    match unsafe { LLVMGetTypeKind(type_ref) } {
        LLVMTypeKind::LLVMHalfTypeKind => Ok(16),
        LLVMTypeKind::LLVMFloatTypeKind => Ok(32),
        LLVMTypeKind::LLVMDoubleTypeKind => Ok(64),
        other => bail!("unsupported floating point type kind: {other:?}"),
    }
}

fn f64_to_f16_bits(value: f64) -> u16 {
    let value = value as f32;
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let frac = bits & 0x7f_ffff;

    if exp == 0xff {
        if frac == 0 {
            return sign | 0x7c00;
        }
        let payload = (frac >> 13) as u16;
        return sign | 0x7c00 | payload | 1;
    }

    let half_exp = exp - 127 + 15;
    if half_exp >= 0x1f {
        return sign | 0x7c00;
    }
    if half_exp <= 0 {
        if half_exp < -10 {
            return sign;
        }
        let mant = frac | 0x80_0000;
        let shift = (14 - half_exp) as u32;
        let mut half_frac = (mant >> shift) as u16;
        if ((mant >> (shift - 1)) & 1) != 0 {
            half_frac = half_frac.wrapping_add(1);
        }
        return sign | half_frac;
    }

    let mut half = sign | ((half_exp as u16) << 10) | ((frac >> 13) as u16);
    if (frac & 0x0000_1000) != 0 {
        half = half.wrapping_add(1);
    }
    half
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

fn mask_integer_to_width(value: u64, width: u8) -> u64 {
    if width >= 64 {
        value
    } else {
        value & ((1_u64 << width) - 1)
    }
}

fn checked_prefetch_rw(value: u64) -> anyhow::Result<u8> {
    if value <= 1 {
        Ok(value as u8)
    } else {
        bail!("llvm.prefetch rw immarg must be 0 or 1, got {value}")
    }
}

fn checked_prefetch_locality(value: u64) -> anyhow::Result<u8> {
    if value <= 3 {
        Ok(value as u8)
    } else {
        bail!("llvm.prefetch locality immarg must be in 0..=3, got {value}")
    }
}

fn checked_prefetch_cache(value: u64) -> anyhow::Result<u8> {
    if value <= 1 {
        Ok(value as u8)
    } else {
        bail!("llvm.prefetch cache immarg must be 0 or 1, got {value}")
    }
}

fn checked_intrinsic_integer_width(width: u64) -> anyhow::Result<u8> {
    if matches!(width, 1 | 8 | 16 | 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("integer intrinsic width i{width} is not supported by vm_virtualize")
    }
}

fn checked_fp_state_width(width: u64) -> anyhow::Result<u8> {
    if matches!(width, 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("floating-point state width i{width} is not supported by vm_virtualize")
    }
}

fn checked_int_unary_width(op: IntUnaryOp, width: u64) -> anyhow::Result<u8> {
    match op {
        IntUnaryOp::CtPop | IntUnaryOp::CtLz | IntUnaryOp::CtTz | IntUnaryOp::Abs
            if matches!(width, 1 | 8 | 16 | 32 | 64) =>
        {
            Ok(width as u8)
        },
        IntUnaryOp::BSwap => checked_bswap_intrinsic_width(width),
        _ => checked_intrinsic_integer_width(width),
    }
}

fn checked_integer_intrinsic_kind_width(kind: IntegerIntrinsicKind, width: u64) -> anyhow::Result<u8> {
    match kind {
        IntegerIntrinsicKind::CtPop
        | IntegerIntrinsicKind::CtLz
        | IntegerIntrinsicKind::CtTz
        | IntegerIntrinsicKind::Abs
            if matches!(width, 1 | 8 | 16 | 32 | 64) =>
        {
            Ok(width as u8)
        },
        IntegerIntrinsicKind::BSwap => checked_bswap_intrinsic_width(width),
        _ => checked_intrinsic_integer_width(width),
    }
}

fn checked_bswap_intrinsic_width(width: u64) -> anyhow::Result<u8> {
    if matches!(width, 16 | 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("llvm.bswap width i{width} is not supported by vm_virtualize")
    }
}

fn checked_saturating_float_to_int_width(width: u64) -> anyhow::Result<u8> {
    if matches!(width, 1 | 8 | 16 | 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("saturating float-to-int result width i{width} is not supported by vm_virtualize")
    }
}

fn checked_round_to_int_result_width(width: u64) -> anyhow::Result<u8> {
    if matches!(width, 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("round-to-int intrinsic result width i{width} is not supported by vm_virtualize")
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
    if matches!(width, 16 | 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("unsupported floating point width: {width}")
    }
}

fn checked_intrinsic_float_width(width: u64) -> anyhow::Result<u8> {
    if matches!(width, 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("floating intrinsic width f{width} is not supported by vm_virtualize")
    }
}

fn checked_float_intrinsic_width(kind: FloatIntrinsicKind, width: u64) -> anyhow::Result<u8> {
    if kind.accepts_half() {
        checked_float_width(width)
    } else {
        checked_intrinsic_float_width(width)
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

fn predicate_from_metadata_name(name: &str) -> anyhow::Result<CmpPredicate> {
    match name {
        "eq" => Ok(CmpPredicate::Eq),
        "ne" => Ok(CmpPredicate::Ne),
        "ugt" => Ok(CmpPredicate::Ugt),
        "uge" => Ok(CmpPredicate::Uge),
        "ult" => Ok(CmpPredicate::Ult),
        "ule" => Ok(CmpPredicate::Ule),
        "sgt" => Ok(CmpPredicate::Sgt),
        "sge" => Ok(CmpPredicate::Sge),
        "slt" => Ok(CmpPredicate::Slt),
        "sle" => Ok(CmpPredicate::Sle),
        other => bail!("unsupported llvm.vp.icmp predicate metadata {other:?}"),
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

fn float_predicate_from_metadata_name(name: &str) -> anyhow::Result<VmFloatPredicate> {
    float_predicate_from_metadata_name_for(name, "llvm.vp.fcmp")
}

fn float_predicate_from_metadata_name_for(name: &str, intrinsic: &str) -> anyhow::Result<VmFloatPredicate> {
    match name {
        "false" => Ok(VmFloatPredicate::False),
        "oeq" => Ok(VmFloatPredicate::Oeq),
        "ogt" => Ok(VmFloatPredicate::Ogt),
        "oge" => Ok(VmFloatPredicate::Oge),
        "olt" => Ok(VmFloatPredicate::Olt),
        "ole" => Ok(VmFloatPredicate::Ole),
        "one" => Ok(VmFloatPredicate::One),
        "ord" => Ok(VmFloatPredicate::Ord),
        "uno" => Ok(VmFloatPredicate::Uno),
        "ueq" => Ok(VmFloatPredicate::Ueq),
        "ugt" => Ok(VmFloatPredicate::Ugt),
        "uge" => Ok(VmFloatPredicate::Uge),
        "ult" => Ok(VmFloatPredicate::Ult),
        "ule" => Ok(VmFloatPredicate::Ule),
        "une" => Ok(VmFloatPredicate::Une),
        "true" => Ok(VmFloatPredicate::True),
        other => bail!("unsupported {intrinsic} predicate metadata {other:?}"),
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

    #[test]
    fn target_specific_intrinsic_name_matches_target_prefixes_only() {
        for name in [
            "llvm.x86.rdtsc",
            "llvm.aarch64.ldxr",
            "llvm.riscv.ntl.p1",
            "llvm.amdgcn.workitem.id.x",
            "llvm.r600.read.tidig.x",
            "llvm.wasm.memory.size",
            "llvm.spv.load",
            "llvm.dx.resource.load",
        ] {
            assert!(target_specific_intrinsic_name(name), "{name} should be target-specific");
        }

        for name in [
            "llvm.readcyclecounter",
            "llvm.vscale.i64",
            "llvm.memcpy.p0.p0.i64",
            "llvm.returnaddress",
            "llvm.experimental.stackmap",
            "not.llvm.x86.rdtsc",
        ] {
            assert!(
                !target_specific_intrinsic_name(name),
                "{name} should not be classified as target-specific"
            );
        }
    }
}
