//! profile package 加载器与 DSL parser。
//!
//! # 契约
//! profile package 是一个目录，包含 `manifest.toml` 以及 manifest 引用的六个 DSL 文件。
//! 解析过程刻意保持声明式：源码会先去掉注释和空白，只保留语义行，再转换为类型化 profile
//! 结构，并在任何 LLVM IR 被改写前接受 verifier 校验。
//!
//! # 错误
//! 加载和解析 API 会在 I/O 失败、TOML 格式错误、package 版本不支持、scope 非法、
//! dispatch 不支持，或 DSL 语句无法被 AMICE 校验时返回 `ProfileError`。
//!
//! # 坑点
//! 只完成解析并不代表 profile 可以安全使用。lowering 或生成 runtime 前必须调用
//! `verify::verify_profile` 函数。

use crate::abi::{AbiProfile, NativeCallPolicy, VmRegister};
use crate::isa::{
    AtomicRmwOp, BinOp, CastOp, CounterKind, FloatBinOp, FloatCastOp, FloatTernaryOp, FloatUnaryOp, HandlerEffect,
    HandlerSemantic, InstructionDesc, IntOverflowOp, IntTernaryOp, IntUnaryOp, IsaProfile, Opcode, OperandDesc,
    OperandKind, PcEffect, PcExpr, SUPPORTED_DECODED_WIDTHS, SemanticAtomicRmwOp, SemanticBinOp, SemanticExpr,
    SemanticFloatBinOp, SemanticFloatCastOp, SemanticFloatTernaryOp, SemanticFloatUnaryOp, SemanticIntOverflowOp,
    SemanticIntTernaryOp, SemanticIntUnaryOp, SemanticProgram, SemanticStmt, SuperOp,
};
use crate::runtime::{
    ControlStateSlot, DispatchStrategy, HandlerClonePolicy, RegisterBank, RuntimeProfile, WideRegisterPolicy,
};
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// 合法的 profile scope 取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeScope {
    /// 为每个受保护函数独立生成 artifact 或派生 key。
    Func,
    /// 在当前 LLVM module 内共享生成的 artifact。
    Module,
}

impl Display for RuntimeScope {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Func => f.write_str("func"),
            Self::Module => f.write_str("module"),
        }
    }
}

impl FromStr for RuntimeScope {
    type Err = ProfileError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "func" => Ok(Self::Func),
            "module" => Ok(Self::Module),
            other => Err(ProfileError::InvalidScope(other.to_owned())),
        }
    }
}

/// profile package 解析与校验错误。
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("failed to read profile file {path}: {source}")]
    /// manifest 或 DSL 文件无法从磁盘读取。
    ReadFile {
        /// 读取失败的路径。
        path: PathBuf,
        /// 底层文件系统错误。
        source: std::io::Error,
    },
    #[error("failed to parse manifest: {0}")]
    /// `manifest.toml` 不是合法 TOML，或不匹配 manifest schema。
    Manifest(#[from] toml::de::Error),
    #[error("profile version {0} is not supported")]
    /// manifest 声明了当前 crate 不支持加载的 profile 格式版本。
    UnsupportedVersion(u32),
    #[error("invalid runtime scope: {0}")]
    /// scope 字符串不是 `func` 或 `module`。
    InvalidScope(String),
    #[error("invalid runtime dispatch: {0}")]
    /// dispatch 策略不被 LLVM runtime emitter 支持。
    InvalidDispatch(String),
    #[error("profile package is invalid: {0}")]
    /// 解析后的 DSL 违反 VMP 契约或 verifier 不变量。
    Invalid(String),
}

/// profile package 的 manifest 清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// profile package 格式版本。
    pub version: u32,
    /// 用于诊断和 key 派生的人类可读 profile 名称。
    pub name: String,
    /// 必须匹配 LLVM module 的 target 约束。
    pub target: TargetManifest,
    /// 指向六个 profile DSL 文件的相对路径。
    pub profile: ProfileFiles,
}

/// profile package 声明的 target 约束。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetManifest {
    /// 要求的 target 指针位宽。
    pub pointer_bits: u16,
    /// 要求的 target endian 字符串。
    pub endian: String,
}

/// package DSL 文件路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileFiles {
    /// `abi.vm` 的相对路径。
    pub abi: String,
    /// `isa.vm` 的相对路径。
    pub isa: String,
    /// `lowering.vm` 的相对路径。
    pub lowering: String,
    /// `bytecode.vm` 的相对路径。
    pub bytecode: String,
    /// `decoder.vm` 的相对路径。
    pub decoder: String,
    /// `runtime.vm` 的相对路径。
    pub runtime: String,
}

/// 来自 `bytecode.vm` 的 bytecode 容器和 record 设置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BytecodeProfile {
    /// bytecode package scope；只允许 `func` 和 `module`。
    pub scope: RuntimeScope,
    /// `code` segment 声明的模式。
    pub code_segment: SegmentMode,
    /// 按源码顺序声明的 bytecode segment。
    pub segments: Vec<BytecodeSegment>,
    /// 指令 record layout。
    pub instruction_record: InstructionRecordProfile,
    /// package 支持的 relocation record。
    pub relocations: Vec<RelocProfile>,
    /// constant-pool 保护策略。
    pub const_pool: ConstPoolProfile,
    /// fake instruction 插入策略。
    pub fake_instruction: FakeInstructionProfile,
    /// dead bytecode 插入策略。
    pub dead_bytecode: DeadBytecodeProfile,
}

impl BytecodeProfile {
    /// 返回 profile 中指定名称的 segment。
    pub fn segment(&self, name: &str) -> Option<&BytecodeSegment> {
        self.segments.iter().find(|segment| segment.name == name)
    }

    /// 返回 profile 中指定名称的 relocation。
    pub fn relocation(&self, name: &str) -> Option<&RelocProfile> {
        self.relocations.iter().find(|reloc| reloc.name == name)
    }
}

/// `bytecode.vm` 声明的一个 bytecode package segment。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BytecodeSegment {
    /// segment 名称，例如 `header`、`const_pool`、`code` 或 `reloc`。
    pub name: String,
    /// segment 编码模式。
    pub mode: SegmentMode,
}

/// 支持的 bytecode segment 模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentMode {
    /// segment 不经过 code-stream 压缩，直接生成。
    Fixed,
    /// segment 由 decoder/encoder pipeline 保护。
    Compressed,
}

impl FromStr for SegmentMode {
    type Err = ProfileError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "fixed" => Ok(Self::Fixed),
            "compressed" => Ok(Self::Compressed),
            other => Err(ProfileError::Invalid(format!(
                "unsupported bytecode segment mode {other}"
            ))),
        }
    }
}

/// `bytecode.vm` 声明的指令 record layout。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionRecordProfile {
    /// opcode 字段使用的编码。
    pub opcode: OpcodeEncoding,
    /// operand 字段使用的编码。
    pub operands: OperandEncoding,
    /// profile 允许的 decoded record 宽度集合。
    pub decoded_widths: Vec<u8>,
    /// 未在 `isa.vm` 单条指令上覆盖时使用的 decoded record 宽度。
    pub default_decoded_width: u8,
}

/// `bytecode.vm` 声明的 opcode 字段编码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpcodeEncoding {
    /// 由配置的 decoder 逆变换保护的 varint opcode。
    VarintEncrypted,
}

/// `bytecode.vm` 声明的 operand 字段编码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperandEncoding {
    /// 使用命名 schema 的 bit-packed operand。
    Bitpack {
        /// schema 名称，当前期望为 `operand_stream` 或 `instr`。
        schema: String,
    },
}

/// `bytecode.vm` 声明的 relocation record。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocProfile {
    /// relocation 类型名称。
    pub name: String,
    /// relocation payload 的编码宽度。
    pub width: RelocWidth,
    /// 解释 relocation payload 时使用的 base。
    pub base: RelocBase,
}

/// `bytecode.vm` 声明的 relocation 字段宽度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocWidth {
    /// relocation payload 编码为 varint。
    Varint,
}

/// `bytecode.vm` 声明的 relocation base。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocBase {
    /// relocation payload 相对于 code segment 起点解释。
    CodeStart,
}

/// `bytecode.vm` 声明的 const-pool 策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstPoolProfile {
    /// const-pool 字节使用的加密变换。
    pub encryption: ConstPoolEncryption,
}

/// `bytecode.vm` 声明的 const-pool 加密变换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstPoolEncryption {
    /// 用 per-function key stream XOR 每个 const-pool 字节。
    XorStreamFunctionKey,
}

/// `bytecode.vm` 声明的 fake instruction 插入策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeInstructionProfile {
    /// 是否在真实指令后插入 fake instruction。
    pub enabled: bool,
    /// 每个插入点插入的 fake instruction 数量。
    pub count: u8,
}

/// `bytecode.vm` 声明的 dead bytecode 插入策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadBytecodeProfile {
    /// 是否追加不可达 bytecode record。
    pub enabled: bool,
    /// 追加到每个函数 package 的 dead record 数量。
    pub count: u8,
}

/// decoder pipeline 步骤。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderStep {
    /// 使用 per-function key stream XOR 字节流。
    XorStream,
    /// 减去 per-function additive stream。
    AddStream,
    /// 将字节左旋 `amount` 位。
    Rol {
        /// 旋转位数。
        amount: u8,
    },
    /// 将字节右旋 `amount` 位。
    Ror {
        /// 旋转位数。
        amount: u8,
    },
    /// 从字节流解码 unsigned varint。
    VarintDecode,
    /// 使用 instruction record schema 解释整数 token。
    BitUnpack,
}

/// runtime decoder 的配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoderProfile {
    /// 按 runtime 执行顺序排列的 decoder step。
    pub steps: Vec<DecoderStep>,
}

/// `lowering.vm` 声明的 lowering 覆盖元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweringProfile {
    /// 按 profile 顺序解析出的 lowering rule。
    pub rules: Vec<LoweringRule>,
    /// 按 profile 顺序解析出的 VM IR 超级指令融合规则。
    pub fusions: Vec<LoweringFusion>,
    /// 解析 lowering rule 时发现的文本形式 q-register 引用。
    pub q_register_references: Vec<String>,
}

/// 必需的 LLVM lowering 契约及其声明式 matcher pattern。
pub const REQUIRED_LOWERING_MATCHES: &[(&str, &str)] = &[
    ("llvm.add.integer", "%r = llvm.add integer %a, %b"),
    ("llvm.sub.integer", "%r = llvm.sub integer %a, %b"),
    ("llvm.mul.integer", "%r = llvm.mul integer %a, %b"),
    ("llvm.udiv.integer", "%r = llvm.udiv integer %a, %b"),
    ("llvm.sdiv.integer", "%r = llvm.sdiv integer %a, %b"),
    ("llvm.urem.integer", "%r = llvm.urem integer %a, %b"),
    ("llvm.srem.integer", "%r = llvm.srem integer %a, %b"),
    ("llvm.smax.integer", "%r = llvm.smax integer %a, %b"),
    ("llvm.smin.integer", "%r = llvm.smin integer %a, %b"),
    ("llvm.umax.integer", "%r = llvm.umax integer %a, %b"),
    ("llvm.umin.integer", "%r = llvm.umin integer %a, %b"),
    ("llvm.uadd.sat.integer", "%r = llvm.uadd.sat integer %a, %b"),
    ("llvm.usub.sat.integer", "%r = llvm.usub.sat integer %a, %b"),
    ("llvm.sadd.sat.integer", "%r = llvm.sadd.sat integer %a, %b"),
    ("llvm.ssub.sat.integer", "%r = llvm.ssub.sat integer %a, %b"),
    ("llvm.ushl.sat.integer", "%r = llvm.ushl.sat integer %a, %b"),
    ("llvm.sshl.sat.integer", "%r = llvm.sshl.sat integer %a, %b"),
    (
        "llvm.uadd.with.overflow.integer",
        "%r = llvm.uadd.with.overflow integer %a, %b",
    ),
    (
        "llvm.sadd.with.overflow.integer",
        "%r = llvm.sadd.with.overflow integer %a, %b",
    ),
    (
        "llvm.usub.with.overflow.integer",
        "%r = llvm.usub.with.overflow integer %a, %b",
    ),
    (
        "llvm.ssub.with.overflow.integer",
        "%r = llvm.ssub.with.overflow integer %a, %b",
    ),
    (
        "llvm.umul.with.overflow.integer",
        "%r = llvm.umul.with.overflow integer %a, %b",
    ),
    (
        "llvm.smul.with.overflow.integer",
        "%r = llvm.smul.with.overflow integer %a, %b",
    ),
    ("llvm.ctpop.integer", "%r = llvm.ctpop integer %value"),
    ("llvm.ctlz.integer", "%r = llvm.ctlz integer %value"),
    ("llvm.cttz.integer", "%r = llvm.cttz integer %value"),
    ("llvm.abs.integer", "%r = llvm.abs integer %value"),
    ("llvm.bswap.integer", "%r = llvm.bswap integer %value"),
    ("llvm.bitreverse.integer", "%r = llvm.bitreverse integer %value"),
    ("llvm.fshl.integer", "%r = llvm.fshl integer %a, %b, %shift"),
    ("llvm.fshr.integer", "%r = llvm.fshr integer %a, %b, %shift"),
    ("llvm.objectsize.integer", "%r = llvm.objectsize pointer %ptr"),
    ("llvm.readcyclecounter.integer", "%r = llvm.readcyclecounter"),
    ("llvm.readsteadycounter.integer", "%r = llvm.readsteadycounter"),
    ("llvm.fadd.float", "%r = llvm.fadd float %a, %b"),
    ("llvm.fsub.float", "%r = llvm.fsub float %a, %b"),
    ("llvm.fmul.float", "%r = llvm.fmul float %a, %b"),
    ("llvm.fdiv.float", "%r = llvm.fdiv float %a, %b"),
    ("llvm.frem.float", "%r = llvm.frem float %a, %b"),
    ("llvm.minnum.float", "%r = llvm.minnum float %a, %b"),
    ("llvm.maxnum.float", "%r = llvm.maxnum float %a, %b"),
    ("llvm.minimum.float", "%r = llvm.minimum float %a, %b"),
    ("llvm.maximum.float", "%r = llvm.maximum float %a, %b"),
    ("llvm.copysign.float", "%r = llvm.copysign float %a, %b"),
    ("llvm.sqrt.float", "%r = llvm.sqrt float %a"),
    ("llvm.canonicalize.float", "%r = llvm.canonicalize float %a"),
    ("llvm.floor.float", "%r = llvm.floor float %a"),
    ("llvm.ceil.float", "%r = llvm.ceil float %a"),
    ("llvm.trunc.float", "%r = llvm.trunc float %a"),
    ("llvm.rint.float", "%r = llvm.rint float %a"),
    ("llvm.nearbyint.float", "%r = llvm.nearbyint float %a"),
    ("llvm.round.float", "%r = llvm.round float %a"),
    ("llvm.roundeven.float", "%r = llvm.roundeven float %a"),
    ("llvm.fma.float", "%r = llvm.fma float %a, %b, %c"),
    ("llvm.fmuladd.float", "%r = llvm.fmuladd float %a, %b, %c"),
    ("llvm.fneg.float", "%r = llvm.fneg float %a"),
    ("llvm.fabs.float", "%r = llvm.fabs float %a"),
    ("llvm.sitofp.float", "%r = llvm.sitofp scalar %a"),
    ("llvm.uitofp.float", "%r = llvm.uitofp scalar %a"),
    ("llvm.fptosi.float", "%r = llvm.fptosi scalar %a"),
    ("llvm.fptoui.float", "%r = llvm.fptoui scalar %a"),
    ("llvm.fptrunc.float", "%r = llvm.fptrunc scalar %a"),
    ("llvm.fpext.float", "%r = llvm.fpext scalar %a"),
    ("llvm.is.fpclass.float", "%r = llvm.is.fpclass float %a, mask"),
    ("llvm.bitops.integer", "%r = llvm.bitop integer %a, %b"),
    ("llvm.shift.integer", "%r = llvm.shift integer %a, %b"),
    ("llvm.icmp.scalar", "%r = llvm.icmp scalar %a, %b"),
    ("llvm.fcmp.float", "%r = llvm.fcmp float %a, %b"),
    ("llvm.cast.integer", "%r = llvm.cast integer %a"),
    ("llvm.cast.pointer", "%r = llvm.cast pointer %a"),
    ("llvm.cast.bitcast.scalar", "%r = llvm.bitcast scalar %a"),
    (
        "llvm.constexpr.integer.binop",
        "%r = llvm.constexpr.binop integer %a, %b",
    ),
    ("llvm.constexpr.integer.cast", "%r = llvm.constexpr.cast integer %value"),
    ("llvm.constexpr.ptrtoint", "%r = llvm.constexpr.ptrtoint pointer %value"),
    ("llvm.constexpr.inttoptr", "%r = llvm.constexpr.inttoptr integer %value"),
    ("llvm.freeze.scalar", "%r = llvm.freeze scalar %value"),
    ("llvm.aggregate.freeze", "%r = llvm.freeze aggregate %value"),
    ("llvm.const_pool.materialize", "%v = llvm.constant integer"),
    ("llvm.expect.integer", "%r = llvm.expect integer %value, %expected"),
    (
        "llvm.expect.with_probability.integer",
        "%r = llvm.expect.with_probability integer %value, %expected, %probability",
    ),
    ("llvm.ssa.copy.scalar", "%r = llvm.ssa.copy scalar %value"),
    (
        "llvm.launder.invariant.group.pointer",
        "%r = llvm.launder.invariant.group pointer %value",
    ),
    (
        "llvm.strip.invariant.group.pointer",
        "%r = llvm.strip.invariant.group pointer %value",
    ),
    (
        "llvm.invariant.start.pointer",
        "%r = llvm.invariant.start pointer %value",
    ),
    ("llvm.invariant.end", "llvm.invariant.end %descriptor, %ptr"),
    ("llvm.prefetch", "llvm.prefetch %ptr"),
    (
        "llvm.experimental.noalias.scope.decl",
        "llvm.experimental.noalias.scope.decl metadata",
    ),
    ("llvm.sideeffect", "llvm.sideeffect"),
    ("llvm.donothing", "llvm.donothing"),
    ("llvm.annotation.integer", "%r = llvm.annotation integer %value"),
    ("llvm.ptr.annotation.pointer", "%r = llvm.ptr.annotation pointer %value"),
    ("llvm.ptrmask.pointer", "%r = llvm.ptrmask pointer %ptr, %mask"),
    (
        "llvm.threadlocal.address.pointer",
        "%r = llvm.threadlocal.address pointer %value",
    ),
    ("llvm.global.address.pointer", "%r = llvm.global.address pointer %value"),
    ("llvm.var.annotation", "llvm.var.annotation %ptr"),
    ("llvm.codeview.annotation", "llvm.codeview.annotation metadata"),
    ("llvm.lifetime.start", "llvm.lifetime.start %ptr"),
    ("llvm.lifetime.end", "llvm.lifetime.end %ptr"),
    ("llvm.assume", "llvm.assume %cond"),
    ("llvm.dbg.nop", "llvm.dbg intrinsic"),
    ("llvm.alloca.stack", "%r = llvm.alloca fixed %ty"),
    ("llvm.alloca.dynamic", "%r = llvm.alloca dynamic %count, %ty"),
    ("llvm.memory.scalar", "llvm.memory scalar %ptr"),
    ("llvm.memory.volatile.scalar", "llvm.memory volatile scalar %ptr"),
    ("llvm.memory.aggregate.load", "%r = llvm.load aggregate %ptr"),
    ("llvm.memory.aggregate.store", "llvm.store aggregate %ptr, %value"),
    (
        "llvm.memory.volatile.aggregate.load",
        "%r = llvm.load volatile aggregate %ptr",
    ),
    (
        "llvm.memory.volatile.aggregate.store",
        "llvm.store volatile aggregate %ptr, %value",
    ),
    ("llvm.atomic.load.scalar", "%r = llvm.atomic.load scalar %ptr"),
    ("llvm.atomic.store.scalar", "llvm.atomic.store scalar %ptr"),
    (
        "llvm.atomic.volatile.load.scalar",
        "%r = llvm.atomic.volatile.load scalar %ptr",
    ),
    (
        "llvm.atomic.volatile.store.scalar",
        "llvm.atomic.volatile.store scalar %ptr",
    ),
    ("llvm.atomic.rmw.scalar", "%r = llvm.atomic.rmw scalar %ptr, %value"),
    (
        "llvm.atomic.volatile.rmw.scalar",
        "%r = llvm.atomic.volatile.rmw scalar %ptr, %value",
    ),
    ("llvm.cmpxchg.scalar", "llvm.cmpxchg scalar %ptr, %cmp, %new"),
    (
        "llvm.volatile.cmpxchg.scalar",
        "llvm.volatile.cmpxchg scalar %ptr, %cmp, %new",
    ),
    ("llvm.fence", "llvm.fence ordering"),
    ("llvm.memcpy.fixed", "llvm.memcpy fixed %dst, %src, %len"),
    ("llvm.memmove.fixed", "llvm.memmove fixed %dst, %src, %len"),
    ("llvm.memcpy.dynamic", "llvm.memcpy dynamic %dst, %src, %len"),
    ("llvm.memmove.dynamic", "llvm.memmove dynamic %dst, %src, %len"),
    ("llvm.memset.fixed", "llvm.memset fixed %dst, %value, %len"),
    ("llvm.memset.dynamic", "llvm.memset dynamic %dst, %value, %len"),
    (
        "llvm.volatile.memcpy.dynamic",
        "llvm.volatile.memcpy dynamic %dst, %src, %len",
    ),
    (
        "llvm.volatile.memmove.dynamic",
        "llvm.volatile.memmove dynamic %dst, %src, %len",
    ),
    (
        "llvm.volatile.memset.dynamic",
        "llvm.volatile.memset dynamic %dst, %value, %len",
    ),
    ("llvm.gep.constant", "%r = llvm.gep constant %base"),
    ("llvm.gep.dynamic", "%r = llvm.gep dynamic %base, %index"),
    ("llvm.call.direct", "%r = llvm.call direct %callee"),
    ("llvm.call.indirect", "%r = llvm.call indirect %callee"),
    ("llvm.select.scalar", "%r = llvm.select scalar %cond, %then, %else"),
    (
        "llvm.select.aggregate",
        "%r = llvm.select aggregate %cond, %then, %else",
    ),
    ("llvm.aggregate.insert", "%r = llvm.insertvalue aggregate %agg, %field"),
    (
        "llvm.aggregate.insert.subaggregate",
        "%r = llvm.insertvalue aggregate %agg, subaggregate %field",
    ),
    ("llvm.aggregate.extract", "%r = llvm.extractvalue aggregate %agg"),
    (
        "llvm.aggregate.extract.subaggregate",
        "%r = llvm.extractvalue subaggregate %agg",
    ),
    ("llvm.br.control", "llvm.br terminator"),
    ("llvm.switch.control", "llvm.switch terminator"),
    ("llvm.unreachable", "llvm.unreachable terminator"),
    ("llvm.trap", "llvm.trap intrinsic"),
    ("llvm.ret.scalar", "llvm.ret scalar %value"),
    ("llvm.ret.void", "llvm.ret void"),
    ("llvm.ret.aggregate", "llvm.ret aggregate %value"),
    ("llvm.ret.sret", "llvm.ret sret %ptr"),
    ("llvm.phi.edge_move", "%r = llvm.phi edge %incoming"),
    ("llvm.aggregate.phi.edge_move", "%r = llvm.phi aggregate edge %incoming"),
];

/// 返回指定契约名对应的必需 lowering matcher pattern。
pub fn lowering_match_pattern(contract: &str) -> Option<&'static str> {
    REQUIRED_LOWERING_MATCHES
        .iter()
        .find_map(|(name, pattern)| (*name == contract).then_some(*pattern))
}

impl LoweringProfile {
    /// 返回指定 profile 名称的 lowering rule。
    pub fn rule(&self, name: &str) -> Option<&LoweringRule> {
        self.rules.iter().find(|rule| rule.name == name)
    }

    /// 返回指定声明式 matcher 对应的 lowering rule。
    pub fn rule_by_match(&self, pattern: &str) -> Option<&LoweringRule> {
        self.rules
            .iter()
            .find(|rule| rule.matcher.as_ref().is_some_and(|matcher| matcher.pattern == pattern))
    }

    /// 返回指定目标 ISA 指令名对应的 VM IR fusion 声明。
    pub fn fusion_for_target(&self, target: &str) -> Option<&LoweringFusion> {
        self.fusions.iter().find(|fusion| fusion.target == target)
    }

    /// 当 rule 存在且会 emit 所有请求的 VM 指令时返回 true。
    pub fn covers(&self, name: &str, required_emits: &[&str]) -> bool {
        self.rule(name).is_some_and(|rule| {
            required_emits
                .iter()
                .all(|required| rule.emitted_instructions.iter().any(|emitted| emitted == required))
        })
    }
}

/// 一条结构化 LLVM-to-VM lowering rule。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweringRule {
    /// rule 名称，通常是 `REQUIRED_LOWERING_MATCHES` 中的一项。
    pub name: String,
    /// rule body 中的声明式 match pattern。
    pub matcher: Option<LoweringMatch>,
    /// translator 执行的有序 action plan。
    pub actions: Vec<LoweringAction>,
    /// 此 rule emit 的指令名；缓存后供 verifier 检查。
    pub emitted_instructions: Vec<String>,
}

/// `lowering.vm` 中声明的保守 VM IR 超级指令融合规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweringFusion {
    /// fusion 名称，通常形如 `super.iadd_xor`。
    pub name: String,
    /// 融合后发射的 profile ISA 指令名。
    pub target: String,
    /// 线性相邻的源 VM 指令名序列。
    pub sequence: Vec<String>,
    /// translator 必须满足的保守条件名称。
    pub requirements: Vec<String>,
}

/// lowering rule 内的声明式 `match` 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweringMatch {
    /// 用于选择此 rule 的声明式 pattern 字符串。
    pub pattern: String,
}

/// `lower {}` block 内解析出的 action。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoweringAction {
    /// 将 LLVM 源值物化为 VM 值。
    Materialize {
        /// 此 action 产生的 VM 侧临时值名称。
        target: String,
        /// LLVM 源 placeholder 或 host-context 值名。
        source: String,
        /// 可选的期望值类型。
        value_type: Option<String>,
    },
    /// 分配由 VM 寄存器承载的值。
    VReg {
        /// 此 action 产生的 VM 侧临时值名称。
        target: String,
        /// 请求的 VM 值类型。
        value_type: String,
    },
    /// emit 一条 profile ISA 指令。
    Emit {
        /// profile 声明的 ISA 指令名。
        instruction: String,
        /// 以 ISA operand 名称为 key 的 operand 表达式。
        operands: Vec<(String, String)>,
    },
    /// 把 LLVM 结果 placeholder 绑定到 VM 值。
    Bind {
        /// LLVM placeholder，例如 `%r`。
        llvm_value: String,
        /// 同一 rule 中较早定义的 VM 值名。
        vm_value: String,
    },
}

/// 完整解析后的 VM profile package。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePackage {
    /// 解析后的 manifest。
    pub manifest: Manifest,
    /// 解析后的 ABI profile。
    pub abi: AbiProfile,
    /// 解析后的 ISA profile。
    pub isa: IsaProfile,
    /// 解析后的 lowering profile。
    pub lowering: LoweringProfile,
    /// 解析后的 bytecode profile。
    pub bytecode: BytecodeProfile,
    /// 解析后的 decoder profile。
    pub decoder: DecoderProfile,
    /// 解析后的 runtime profile。
    pub runtime: RuntimeProfile,
}

impl ProfilePackage {
    /// 从目录加载 profile package。
    ///
    /// # 参数
    /// - `path`: 包含 `manifest.toml` 以及所有 manifest 引用 DSL 文件的目录。
    ///
    /// # 错误
    /// 当任一文件无法读取、manifest 不是合法 TOML、版本不受支持，或某个 DSL 文件包含
    /// 未知/非法语句时返回 `ProfileError`。
    ///
    /// # 契约
    /// 此函数只解析 package。将 package 用于 lowering 或 runtime 生成前必须调用 `verify_profile`。
    pub fn load_from_path(path: &Path) -> Result<Self, ProfileError> {
        let manifest_path = path.join("manifest.toml");
        let manifest_text = read_to_string(&manifest_path)?;
        let manifest: Manifest = toml::from_str(&manifest_text)?;
        let abi_source = read_profile_file(path, &manifest.profile.abi)?;
        let isa_source = read_profile_file(path, &manifest.profile.isa)?;
        let lowering_source = read_profile_file(path, &manifest.profile.lowering)?;
        let bytecode_source = read_profile_file(path, &manifest.profile.bytecode)?;
        let decoder_source = read_profile_file(path, &manifest.profile.decoder)?;
        let runtime_source = read_profile_file(path, &manifest.profile.runtime)?;

        Self::from_sources(
            manifest,
            &abi_source,
            &isa_source,
            &lowering_source,
            &bytecode_source,
            &decoder_source,
            &runtime_source,
        )
    }

    /// 返回内置 simple VMP profile package。
    ///
    /// # 错误
    /// 如果嵌入式 package 变得内部不一致，则返回 `ProfileError`。测试会确保它持续可加载。
    ///
    /// # 契约
    /// 内置 profile 刻意保持小型，但对当前标量整数/指针 lowering 边界是完整的。
    pub fn builtin_test() -> Result<Self, ProfileError> {
        let manifest: Manifest = toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml"))?;

        Self::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
    }

    /// 从已加载的 package 源码构建 profile。
    ///
    /// # 参数
    /// - `manifest`: 已解析的 manifest，提供版本、target 和源码 identity。
    /// - `*_source`: 每个 profile section 的原始 DSL 文本。
    ///
    /// # 错误
    /// manifest 版本不受支持或 DSL 语法/取值非法时返回 `ProfileError`。
    ///
    /// # 契约
    /// parser 会剥离注释，而且永远不会执行 profile 文本。语义有效性、lowering 覆盖、
    /// 寄存器模型和 q-disabled 策略会在构建后由 verifier 强制执行。
    pub fn from_sources(
        manifest: Manifest,
        abi_source: &str,
        isa_source: &str,
        lowering_source: &str,
        bytecode_source: &str,
        decoder_source: &str,
        runtime_source: &str,
    ) -> Result<Self, ProfileError> {
        if manifest.version != 1 {
            return Err(ProfileError::UnsupportedVersion(manifest.version));
        }

        let bytecode = parse_bytecode(bytecode_source)?;
        let default_decoded_width = bytecode.instruction_record.default_decoded_width;

        Ok(Self {
            manifest,
            abi: parse_abi(abi_source)?,
            isa: parse_isa(isa_source, default_decoded_width)?,
            lowering: parse_lowering(lowering_source)?,
            bytecode,
            decoder: parse_decoder(decoder_source)?,
            runtime: parse_runtime(runtime_source)?,
        })
    }
}

fn read_to_string(path: &Path) -> Result<String, ProfileError> {
    std::fs::read_to_string(path).map_err(|source| ProfileError::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}

fn read_profile_file(package_path: &Path, manifest_entry: &str) -> Result<String, ProfileError> {
    read_to_string(&package_path.join(manifest_entry))
}

fn semantic_lines(source: &str) -> impl Iterator<Item = String> + '_ {
    // Profile DSL 被刻意设计为不可执行：这里剥离注释，下游 parser 只会看到声明式语句，
    // 并且这些语句都能在 LLVM IR 被改写前完成检查。
    source.lines().filter_map(|line| {
        let without_comment = line.split('#').next().unwrap_or_default().trim();
        (!without_comment.is_empty()).then(|| without_comment.to_owned())
    })
}

fn parse_abi(source: &str) -> Result<AbiProfile, ProfileError> {
    let mut abi = AbiProfile::default();

    for line in semantic_lines(source) {
        if let Some((left, right)) = line.split_once("->") {
            let left = left.trim();
            let right = right.trim();
            if let Some(index) = left.strip_prefix("arg").and_then(|v| v.parse::<usize>().ok()) {
                if let Some(reg) = parse_x_register(right) {
                    if index >= abi.integer_args.len() {
                        abi.integer_args.resize(index + 1, 0);
                    }
                    abi.integer_args[index] = reg;
                } else {
                    return Err(ProfileError::Invalid(format!(
                        "abi.vm scalar argument {left} must map to an x register"
                    )));
                }
            } else if let Some(index) = left.strip_prefix("vec").and_then(|v| v.parse::<usize>().ok()) {
                if let Some(reg) = parse_q_register(right) {
                    if index >= abi.vector_args.len() {
                        abi.vector_args.resize(index + 1, 0);
                    }
                    abi.vector_args[index] = reg;
                } else {
                    return Err(ProfileError::Invalid(format!(
                        "abi.vm vector argument {left} must map to a q register"
                    )));
                }
            }
        } else if let Some((left, right)) = line.split_once("<-") {
            let left = left.trim();
            if left == "ret_pc" {
                abi.ret_pc_alias = right.trim().to_owned();
                abi.ret_pc_declared = true;
            } else if let Some(index) = left.strip_prefix("ret").and_then(|v| v.parse::<usize>().ok()) {
                if let Some(reg) = parse_x_register(right.trim()) {
                    if index >= abi.integer_returns.len() {
                        abi.integer_returns.resize(index + 1, 0);
                    }
                    abi.integer_returns[index] = reg;
                    if index == 0 {
                        abi.integer_return = reg;
                    }
                } else {
                    return Err(ProfileError::Invalid(format!(
                        "abi.vm scalar return {} must map to an x register",
                        left.trim()
                    )));
                }
            } else if let Some(index) = left.strip_prefix("vret").and_then(|v| v.parse::<usize>().ok()) {
                if let Some(reg) = parse_q_register(right.trim()) {
                    if index >= abi.vector_returns.len() {
                        abi.vector_returns.resize(index + 1, 0);
                    }
                    abi.vector_returns[index] = reg;
                } else {
                    return Err(ProfileError::Invalid(format!(
                        "abi.vm vector return {} must map to a q register",
                        left.trim()
                    )));
                }
            }
        } else if let Some(value) = line.strip_prefix("call_link =") {
            abi.lr_alias = value.trim().to_owned();
            abi.call_link_declared = true;
        } else if let Some(value) = line.strip_prefix("call_args =") {
            abi.vm_call_args = parse_register_list(value.trim())?;
        } else if let Some(value) = line.strip_prefix("ret_values =") {
            abi.vm_call_returns = parse_register_list(value.trim())?;
        } else if let Some(value) = line.strip_prefix("args =") {
            abi.native_args = parse_register_list(value.trim())?;
        } else if let Some(value) = line.strip_prefix("returns =") {
            abi.native_returns = parse_register_list(value.trim())?;
        } else if let Some(value) = line.strip_prefix("clobbers =") {
            abi.native_clobbers = parse_register_list(value.trim())?;
        } else if let Some(value) = line.strip_prefix("policy =") {
            abi.native_policy = parse_native_call_policy(value.trim())?;
        } else if let Some(value) = line.strip_prefix("max_returns =") {
            abi.max_returns = value
                .trim()
                .parse::<u8>()
                .map_err(|_| ProfileError::Invalid(format!("abi.vm max_returns must be u8, got {}", value.trim())))?;
        } else if is_abi_block_line(&line) {
            continue;
        } else {
            return Err(ProfileError::Invalid(format!(
                "abi.vm has unsupported statement: {line}"
            )));
        }
    }

    Ok(abi)
}

fn is_abi_block_line(line: &str) -> bool {
    matches!(
        line,
        "}" | "abi host_to_vm {" | "abi vm_call {" | "native_call default {"
    )
}

fn parse_native_call_policy(value: &str) -> Result<NativeCallPolicy, ProfileError> {
    match value {
        "direct" => Ok(NativeCallPolicy::Direct),
        other => Err(ProfileError::Invalid(format!("unsupported native_call policy {other}"))),
    }
}

fn parse_x_register(input: &str) -> Option<u8> {
    input
        .split_whitespace()
        .next()
        .and_then(|reg| reg.strip_prefix('x'))
        .and_then(|index| index.parse::<u8>().ok())
}

fn parse_q_register(input: &str) -> Option<u8> {
    input
        .split_whitespace()
        .next()
        .and_then(|reg| reg.strip_prefix('q'))
        .and_then(|index| index.parse::<u8>().ok())
}

fn parse_register_list(input: &str) -> Result<Vec<VmRegister>, ProfileError> {
    let trimmed = input.trim();
    let Some(body) = trimmed.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) else {
        return Err(ProfileError::Invalid(format!("invalid ABI register list {trimmed}")));
    };

    let mut registers = Vec::new();
    for item in body.split(',').map(str::trim).filter(|item| !item.is_empty()) {
        if let Some((first, last)) = item.split_once("..") {
            let first = parse_vm_register(first)?;
            let last = parse_vm_register(last)?;
            registers.extend(expand_register_range(first, last)?);
        } else {
            registers.push(parse_vm_register(item)?);
        }
    }

    Ok(registers)
}

fn parse_vm_register(input: &str) -> Result<VmRegister, ProfileError> {
    let input = input.trim();
    if let Some(index) = input.strip_prefix('x').and_then(|value| value.parse::<u8>().ok()) {
        Ok(VmRegister::X(index))
    } else if let Some(index) = input.strip_prefix('q').and_then(|value| value.parse::<u8>().ok()) {
        Ok(VmRegister::Q(index))
    } else {
        Err(ProfileError::Invalid(format!("invalid VM register {input}")))
    }
}

fn expand_register_range(first: VmRegister, last: VmRegister) -> Result<Vec<VmRegister>, ProfileError> {
    match (first, last) {
        (VmRegister::X(first), VmRegister::X(last)) if first <= last => Ok((first..=last).map(VmRegister::X).collect()),
        (VmRegister::Q(first), VmRegister::Q(last)) if first <= last => Ok((first..=last).map(VmRegister::Q).collect()),
        _ => Err(ProfileError::Invalid(format!(
            "invalid mixed or descending VM register range {first:?}..{last:?}"
        ))),
    }
}

fn parse_isa(source: &str, default_decoded_width: u8) -> Result<IsaProfile, ProfileError> {
    let mut instructions = Vec::new();
    let mut current = None;

    for line in semantic_lines(source) {
        if current.is_none() {
            if let Some(header) = line.strip_prefix("instr ") {
                let (name, operand_descs) = parse_instruction_header(header)?;
                current = Some(ParsedInstruction {
                    name,
                    operand_descs,
                    opcodes: Vec::new(),
                    decoded_width: None,
                    semantic: Vec::new(),
                    depth: brace_delta(&line),
                });

                if current.as_ref().is_some_and(|instruction| instruction.depth == 0) {
                    push_instruction(
                        &mut instructions,
                        current.take().expect("current instruction exists"),
                        default_decoded_width,
                    )?;
                }
            } else if !line.is_empty() {
                return Err(ProfileError::Invalid(format!(
                    "isa.vm has unsupported top-level statement: {line}"
                )));
            }
            continue;
        }

        if let Some(opcodes) = parse_opcode_aliases(&line)? {
            current
                .as_mut()
                .expect("current instruction exists")
                .opcodes
                .extend(opcodes);
        } else if let Some(width) = parse_decoded_width_override(&line)? {
            let instruction = current.as_mut().expect("current instruction exists");
            if instruction.decoded_width.replace(width).is_some() {
                return Err(ProfileError::Invalid(format!(
                    "ISA instruction {} declares decoded_width more than once",
                    instruction.name
                )));
            }
        } else if let Some(instruction) = current.as_mut() {
            instruction.semantic.push(line.clone());
        }

        if let Some(instruction) = current.as_mut() {
            instruction.depth += brace_delta(&line);
            if instruction.depth == 0 {
                push_instruction(
                    &mut instructions,
                    current.take().expect("current instruction exists"),
                    default_decoded_width,
                )?;
            }
        }
    }

    if let Some(instruction) = current {
        return Err(ProfileError::Invalid(format!(
            "unterminated ISA instruction {}",
            instruction.name
        )));
    }

    if instructions.is_empty() {
        return Err(ProfileError::Invalid(
            "isa.vm did not declare any instructions".to_owned(),
        ));
    }

    Ok(IsaProfile { instructions })
}

fn parse_lowering(source: &str) -> Result<LoweringProfile, ProfileError> {
    let mut rules = Vec::new();
    let mut fusions = Vec::new();
    let mut q_register_references = Vec::new();
    let mut current: Option<LoweringBlockBuilder> = None;

    for line in semantic_lines(source) {
        collect_q_register_refs(&line, &mut q_register_references);

        if current.is_none() {
            if let Some(header) = line.strip_prefix("rule ") {
                current = Some(LoweringBlockBuilder::Rule(LoweringRuleBuilder::new(header)?));
            } else if let Some(header) = line.strip_prefix("fusion ") {
                current = Some(LoweringBlockBuilder::Fusion(LoweringFusionBuilder::new(header)?));
            } else {
                return Err(ProfileError::Invalid(format!(
                    "lowering.vm has unsupported top-level statement: {line}"
                )));
            }
            if current.as_ref().is_some_and(LoweringBlockBuilder::is_done) {
                current
                    .take()
                    .expect("current lowering block exists")
                    .finish_into(&mut rules, &mut fusions)?;
            }
            continue;
        }

        let block = current.as_mut().expect("current lowering block exists");
        block.apply_line(&line)?;
        block.add_depth(brace_delta(&line));
        if block.is_done() {
            current
                .take()
                .expect("current lowering block exists")
                .finish_into(&mut rules, &mut fusions)?;
        }
    }

    if let Some(block) = current {
        return Err(ProfileError::Invalid(format!(
            "unterminated lowering block {}",
            block.name()
        )));
    }

    Ok(LoweringProfile {
        rules,
        fusions,
        q_register_references,
    })
}

#[derive(Debug)]
enum LoweringBlockBuilder {
    Rule(LoweringRuleBuilder),
    Fusion(LoweringFusionBuilder),
}

impl LoweringBlockBuilder {
    fn name(&self) -> &str {
        match self {
            Self::Rule(rule) => &rule.name,
            Self::Fusion(fusion) => &fusion.name,
        }
    }

    fn is_done(&self) -> bool {
        match self {
            Self::Rule(rule) => rule.depth == 0,
            Self::Fusion(fusion) => fusion.depth == 0,
        }
    }

    fn add_depth(&mut self, delta: i32) {
        match self {
            Self::Rule(rule) => rule.depth += delta,
            Self::Fusion(fusion) => fusion.depth += delta,
        }
    }

    fn apply_line(&mut self, line: &str) -> Result<(), ProfileError> {
        match self {
            Self::Rule(rule) => rule.apply_line(line),
            Self::Fusion(fusion) => fusion.apply_line(line),
        }
    }

    fn finish_into(self, rules: &mut Vec<LoweringRule>, fusions: &mut Vec<LoweringFusion>) -> Result<(), ProfileError> {
        match self {
            Self::Rule(rule) => rules.push(rule.finish()?),
            Self::Fusion(fusion) => fusions.push(fusion.finish()?),
        }
        Ok(())
    }
}

#[derive(Debug)]
struct LoweringRuleBuilder {
    name: String,
    matcher: Option<LoweringMatch>,
    actions: Vec<LoweringAction>,
    emitted_instructions: Vec<String>,
    depth: i32,
}

impl LoweringRuleBuilder {
    fn new(header: &str) -> Result<Self, ProfileError> {
        let name = header
            .split([' ', '{'])
            .next()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ProfileError::Invalid(format!("invalid lowering rule header: rule {header}")))?;
        Ok(Self {
            name: name.to_owned(),
            matcher: None,
            actions: Vec::new(),
            emitted_instructions: Vec::new(),
            depth: brace_delta(&format!("rule {header}")),
        })
    }

    fn apply_line(&mut self, line: &str) -> Result<(), ProfileError> {
        if let Some(pattern) = line.strip_prefix("match ") {
            if self
                .matcher
                .replace(LoweringMatch {
                    pattern: pattern.to_owned(),
                })
                .is_some()
            {
                return Err(ProfileError::Invalid(format!(
                    "lowering rule {} declares match more than once",
                    self.name
                )));
            }
        } else if line == "lower {" || line == "}" {
            return Ok(());
        } else if let Some(action) = parse_lowering_action(line)? {
            if let LoweringAction::Emit { instruction, .. } = &action {
                self.emitted_instructions.push(instruction.clone());
            }
            self.actions.push(action);
        } else {
            return Err(ProfileError::Invalid(format!(
                "lowering rule {} has unsupported statement: {line}",
                self.name
            )));
        }
        Ok(())
    }

    fn finish(self) -> Result<LoweringRule, ProfileError> {
        if self.matcher.is_none() {
            return Err(ProfileError::Invalid(format!(
                "lowering rule {} must declare a match line",
                self.name
            )));
        }
        if self.actions.is_empty() {
            return Err(ProfileError::Invalid(format!(
                "lowering rule {} must declare at least one lower action",
                self.name
            )));
        }

        Ok(LoweringRule {
            name: self.name,
            matcher: self.matcher,
            actions: self.actions,
            emitted_instructions: self.emitted_instructions,
        })
    }
}

#[derive(Debug)]
struct LoweringFusionBuilder {
    name: String,
    target: Option<String>,
    sequence: Vec<String>,
    requirements: Vec<String>,
    depth: i32,
}

impl LoweringFusionBuilder {
    fn new(header: &str) -> Result<Self, ProfileError> {
        let name = header
            .split([' ', '{'])
            .next()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ProfileError::Invalid(format!("invalid lowering fusion header: fusion {header}")))?;
        Ok(Self {
            name: name.to_owned(),
            target: None,
            sequence: Vec::new(),
            requirements: Vec::new(),
            depth: brace_delta(&format!("fusion {header}")),
        })
    }

    fn apply_line(&mut self, line: &str) -> Result<(), ProfileError> {
        if line == "}" {
            return Ok(());
        }
        if let Some(target) = line.strip_prefix("target ") {
            if self.target.replace(target.trim().to_owned()).is_some() {
                return Err(ProfileError::Invalid(format!(
                    "lowering fusion {} declares target more than once",
                    self.name
                )));
            }
        } else if let Some(sequence) = line.strip_prefix("sequence ") {
            if !self.sequence.is_empty() {
                return Err(ProfileError::Invalid(format!(
                    "lowering fusion {} declares sequence more than once",
                    self.name
                )));
            }
            self.sequence = split_call_args(sequence)?
                .into_iter()
                .map(|item| item.trim().to_owned())
                .filter(|item| !item.is_empty())
                .collect();
        } else if let Some(requirement) = line.strip_prefix("require ") {
            self.requirements.push(requirement.trim().to_owned());
        } else {
            return Err(ProfileError::Invalid(format!(
                "lowering fusion {} has unsupported statement: {line}",
                self.name
            )));
        }
        Ok(())
    }

    fn finish(self) -> Result<LoweringFusion, ProfileError> {
        let target = self
            .target
            .ok_or_else(|| ProfileError::Invalid(format!("lowering fusion {} must declare target", self.name)))?;
        if self.sequence.is_empty() {
            return Err(ProfileError::Invalid(format!(
                "lowering fusion {} must declare sequence",
                self.name
            )));
        }
        if self.requirements.is_empty() {
            return Err(ProfileError::Invalid(format!(
                "lowering fusion {} must declare at least one require line",
                self.name
            )));
        }

        Ok(LoweringFusion {
            name: self.name,
            target,
            sequence: self.sequence,
            requirements: self.requirements,
        })
    }
}

fn parse_lowering_action(line: &str) -> Result<Option<LoweringAction>, ProfileError> {
    if let Some(rest) = line.strip_prefix("emit ") {
        let (instruction, operands) = rest
            .split_once(' ')
            .map_or((rest, ""), |(instruction, operands)| (instruction, operands));
        return Ok(Some(LoweringAction::Emit {
            instruction: instruction.to_owned(),
            operands: parse_lowering_emit_operands(operands)?,
        }));
    }

    if let Some(rest) = line.strip_prefix("bind ") {
        let (llvm_value, vm_value) = rest
            .split_once('=')
            .ok_or_else(|| ProfileError::Invalid(format!("invalid lowering bind action: {line}")))?;
        return Ok(Some(LoweringAction::Bind {
            llvm_value: llvm_value.trim().to_owned(),
            vm_value: vm_value.trim().to_owned(),
        }));
    }

    if let Some((target, rest)) = line.split_once(" = materialize ") {
        let (source, value_type) = rest
            .split_once(" as ")
            .map_or((rest.trim().to_owned(), None), |(source, value_type)| {
                (source.trim().to_owned(), Some(value_type.trim().to_owned()))
            });
        return Ok(Some(LoweringAction::Materialize {
            target: target.trim().to_owned(),
            source,
            value_type,
        }));
    }

    if let Some((target, value_type)) = line.split_once(" = vreg ") {
        return Ok(Some(LoweringAction::VReg {
            target: target.trim().to_owned(),
            value_type: value_type.trim().to_owned(),
        }));
    }

    Ok(None)
}

fn parse_lowering_emit_operands(operands: &str) -> Result<Vec<(String, String)>, ProfileError> {
    split_call_args(operands)?
        .into_iter()
        .filter(|operand| !operand.is_empty())
        .map(|operand| {
            let (name, value) = operand
                .split_once('=')
                .ok_or_else(|| ProfileError::Invalid(format!("invalid lowering emit operand: {operand}")))?;
            Ok((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

fn parse_bytecode(source: &str) -> Result<BytecodeProfile, ProfileError> {
    let mut scope = RuntimeScope::Func;
    let mut segments = Vec::new();
    let mut opcode = None;
    let mut operands = None;
    let mut decoded_widths = None;
    let mut default_decoded_width = None;
    let mut relocations = Vec::new();
    let mut current_reloc: Option<RelocBuilder> = None;
    let mut const_pool_encryption = None;
    let mut fake_instruction = FakeInstructionProfile {
        enabled: false,
        count: 0,
    };
    let mut dead_bytecode = DeadBytecodeProfile {
        enabled: false,
        count: 0,
    };

    for line in semantic_lines(source) {
        if current_reloc.is_some() {
            if line == "}" {
                let reloc = current_reloc.take().expect("current relocation exists");
                relocations.push(reloc.finish()?);
                continue;
            }
            current_reloc
                .as_mut()
                .expect("current relocation exists")
                .apply_line(&line)?;
            continue;
        }

        if let Some(value) = line.strip_prefix("bytecode.scope =") {
            scope = value.trim().parse()?;
        } else if let Some(segment) = parse_bytecode_segment(&line)? {
            if segments.iter().any(|seen: &BytecodeSegment| seen.name == segment.name) {
                return Err(ProfileError::Invalid(format!(
                    "bytecode.vm declares duplicate segment {}",
                    segment.name
                )));
            }
            segments.push(segment);
        } else if let Some(value) = line.strip_prefix("opcode:") {
            opcode = Some(parse_opcode_encoding(value.trim())?);
        } else if let Some(value) = line.strip_prefix("operands:") {
            operands = Some(parse_operand_encoding(value.trim())?);
        } else if let Some(value) = line.strip_prefix("decoded_width:") {
            let parsed = parse_record_decoded_widths(value.trim())?;
            decoded_widths = Some(parsed.allowed);
            default_decoded_width = Some(parsed.default);
        } else if let Some(rest) = line.strip_prefix("reloc ") {
            current_reloc = Some(RelocBuilder::new(rest)?);
        } else if let Some(value) = line.strip_prefix("const_pool encryption") {
            const_pool_encryption = Some(parse_const_pool_encryption(value.trim())?);
        } else if let Some(value) = line.strip_prefix("fake_instruction") {
            fake_instruction = parse_fake_instruction_profile(value.trim())?;
        } else if let Some(value) = line.strip_prefix("dead_bytecode") {
            dead_bytecode = parse_dead_bytecode_profile(value.trim())?;
        } else if is_bytecode_block_line(&line) {
            continue;
        } else {
            return Err(ProfileError::Invalid(format!(
                "bytecode.vm has unsupported statement: {line}"
            )));
        }
    }

    if let Some(reloc) = current_reloc {
        return Err(ProfileError::Invalid(format!(
            "unterminated bytecode relocation {}",
            reloc.name
        )));
    }

    let instruction_record = InstructionRecordProfile {
        opcode: opcode.ok_or_else(|| ProfileError::Invalid("bytecode.vm record instr missing opcode".to_owned()))?,
        operands: operands
            .ok_or_else(|| ProfileError::Invalid("bytecode.vm record instr missing operands".to_owned()))?,
        decoded_widths: decoded_widths
            .ok_or_else(|| ProfileError::Invalid("bytecode.vm record instr missing decoded_width".to_owned()))?,
        default_decoded_width: default_decoded_width.ok_or_else(|| {
            ProfileError::Invalid("bytecode.vm record instr missing decoded_width default".to_owned())
        })?,
    };
    let code_segment = segments
        .iter()
        .find(|segment| segment.name == "code")
        .map(|segment| segment.mode)
        .ok_or_else(|| ProfileError::Invalid("bytecode.vm must declare segment code".to_owned()))?;

    Ok(BytecodeProfile {
        scope,
        code_segment,
        segments,
        instruction_record,
        relocations,
        const_pool: ConstPoolProfile {
            encryption: const_pool_encryption
                .ok_or_else(|| ProfileError::Invalid("bytecode.vm must declare const_pool encryption".to_owned()))?,
        },
        fake_instruction,
        dead_bytecode,
    })
}

fn is_bytecode_block_line(line: &str) -> bool {
    matches!(line, "}" | "bytecode {" | "record instr {")
}

fn parse_bytecode_segment(line: &str) -> Result<Option<BytecodeSegment>, ProfileError> {
    let Some(rest) = line.strip_prefix("segment ") else {
        return Ok(None);
    };
    let mut parts = rest.split_whitespace();
    let name = parts
        .next()
        .ok_or_else(|| ProfileError::Invalid(format!("invalid bytecode segment line: {line}")))?;
    let mode = parts
        .next()
        .ok_or_else(|| ProfileError::Invalid(format!("invalid bytecode segment line: {line}")))?
        .parse()?;

    Ok(Some(BytecodeSegment {
        name: name.to_owned(),
        mode,
    }))
}

fn parse_opcode_encoding(value: &str) -> Result<OpcodeEncoding, ProfileError> {
    match value {
        "varint encrypted" => Ok(OpcodeEncoding::VarintEncrypted),
        other => Err(ProfileError::Invalid(format!(
            "unsupported bytecode opcode encoding {other}"
        ))),
    }
}

fn parse_operand_encoding(value: &str) -> Result<OperandEncoding, ProfileError> {
    let mut parts = value.split_whitespace();
    let kind = parts
        .next()
        .ok_or_else(|| ProfileError::Invalid("empty bytecode operand encoding".to_owned()))?;
    if kind != "bitpack" {
        return Err(ProfileError::Invalid(format!(
            "unsupported bytecode operand encoding {value}"
        )));
    }

    let schema = parts
        .find_map(|part| part.strip_prefix("schema="))
        .ok_or_else(|| ProfileError::Invalid(format!("bitpack operand encoding missing schema in {value}")))?;

    Ok(OperandEncoding::Bitpack {
        schema: schema.to_owned(),
    })
}

#[derive(Debug)]
struct ParsedRecordWidths {
    allowed: Vec<u8>,
    default: u8,
}

fn parse_record_decoded_widths(value: &str) -> Result<ParsedRecordWidths, ProfileError> {
    let allowed = value
        .split_whitespace()
        .find_map(|part| part.strip_prefix("one_of="))
        .ok_or_else(|| ProfileError::Invalid(format!("decoded_width missing one_of list in {value}")))
        .and_then(parse_decoded_width_list)?;
    let default = value
        .split_whitespace()
        .find_map(|part| part.strip_prefix("default="))
        .ok_or_else(|| ProfileError::Invalid(format!("decoded_width missing default in {value}")))
        .and_then(parse_decoded_width_literal)?;

    if allowed.is_empty() {
        return Err(ProfileError::Invalid("decoded_width one_of list is empty".to_owned()));
    }
    if !allowed.contains(&default) {
        return Err(ProfileError::Invalid(format!(
            "decoded_width default {default} is not listed in {:?}",
            allowed
        )));
    }

    Ok(ParsedRecordWidths { allowed, default })
}

fn parse_decoded_width_list(value: &str) -> Result<Vec<u8>, ProfileError> {
    let Some(body) = value.strip_prefix('[').and_then(|value| value.strip_suffix(']')) else {
        return Err(ProfileError::Invalid(format!(
            "decoded_width one_of must use [..], got {value}"
        )));
    };

    let mut widths = Vec::new();
    for item in body.split(',').map(str::trim).filter(|item| !item.is_empty()) {
        let width = parse_decoded_width_literal(item)?;
        if widths.contains(&width) {
            return Err(ProfileError::Invalid(format!(
                "decoded_width one_of declares duplicate width {width}"
            )));
        }
        widths.push(width);
    }

    Ok(widths)
}

#[derive(Debug)]
struct RelocBuilder {
    name: String,
    width: Option<RelocWidth>,
    base: Option<RelocBase>,
}

impl RelocBuilder {
    fn new(header: &str) -> Result<Self, ProfileError> {
        let name = header
            .split([' ', '{'])
            .next()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ProfileError::Invalid(format!("invalid bytecode relocation header: reloc {header}")))?;

        Ok(Self {
            name: name.to_owned(),
            width: None,
            base: None,
        })
    }

    fn apply_line(&mut self, line: &str) -> Result<(), ProfileError> {
        if let Some(value) = line.strip_prefix("width =") {
            self.width = Some(parse_reloc_width(value.trim())?);
        } else if let Some(value) = line.strip_prefix("base =") {
            self.base = Some(parse_reloc_base(value.trim())?);
        }
        Ok(())
    }

    fn finish(self) -> Result<RelocProfile, ProfileError> {
        Ok(RelocProfile {
            name: self.name,
            width: self
                .width
                .ok_or_else(|| ProfileError::Invalid("bytecode relocation missing width".to_owned()))?,
            base: self
                .base
                .ok_or_else(|| ProfileError::Invalid("bytecode relocation missing base".to_owned()))?,
        })
    }
}

fn parse_reloc_width(value: &str) -> Result<RelocWidth, ProfileError> {
    match value {
        "varint" => Ok(RelocWidth::Varint),
        other => Err(ProfileError::Invalid(format!(
            "unsupported bytecode relocation width {other}"
        ))),
    }
}

fn parse_reloc_base(value: &str) -> Result<RelocBase, ProfileError> {
    match value {
        "code_start" => Ok(RelocBase::CodeStart),
        other => Err(ProfileError::Invalid(format!(
            "unsupported bytecode relocation base {other}"
        ))),
    }
}

fn parse_const_pool_encryption(value: &str) -> Result<ConstPoolEncryption, ProfileError> {
    match value {
        "xor_stream key=function_key" => Ok(ConstPoolEncryption::XorStreamFunctionKey),
        other => Err(ProfileError::Invalid(format!(
            "unsupported const_pool encryption {other}"
        ))),
    }
}

fn parse_fake_instruction_profile(value: &str) -> Result<FakeInstructionProfile, ProfileError> {
    let enabled = parse_enabled_prefix(value, "fake_instruction")?;
    let count = parse_count_field(value, "fake_instruction")?;
    Ok(FakeInstructionProfile { enabled, count })
}

fn parse_dead_bytecode_profile(value: &str) -> Result<DeadBytecodeProfile, ProfileError> {
    let enabled = parse_enabled_prefix(value, "dead_bytecode")?;
    let count = parse_count_field(value, "dead_bytecode")?;
    Ok(DeadBytecodeProfile { enabled, count })
}

fn parse_enabled_prefix(value: &str, name: &str) -> Result<bool, ProfileError> {
    value
        .split_whitespace()
        .next()
        .map(|state| match state {
            "enabled" => Ok(true),
            "disabled" => Ok(false),
            other => Err(ProfileError::Invalid(format!("{name} has invalid state {other}"))),
        })
        .ok_or_else(|| ProfileError::Invalid(format!("{name} missing state")))?
}

fn parse_count_field(value: &str, name: &str) -> Result<u8, ProfileError> {
    let Some(count) = value.split_whitespace().find_map(|part| part.strip_prefix("count=")) else {
        return Ok(0);
    };

    count
        .parse::<u8>()
        .map_err(|_| ProfileError::Invalid(format!("{name} has invalid count {count}")))
}

fn parse_decoder(source: &str) -> Result<DecoderProfile, ProfileError> {
    let mut steps = Vec::new();
    for line in semantic_lines(source) {
        let Some(step) = line.strip_prefix("step ") else {
            if is_decoder_block_line(&line) {
                continue;
            }
            return Err(ProfileError::Invalid(format!(
                "decoder.vm has unsupported statement: {line}"
            )));
        };
        let parsed = if step.starts_with("xor_stream") {
            DecoderStep::XorStream
        } else if step.starts_with("add_stream") {
            DecoderStep::AddStream
        } else if let Some(amount) = step.strip_prefix("rol amount=") {
            DecoderStep::Rol {
                amount: parse_rotate_amount(amount, &line)?,
            }
        } else if let Some(amount) = step.strip_prefix("ror amount=") {
            DecoderStep::Ror {
                amount: parse_rotate_amount(amount, &line)?,
            }
        } else if step.starts_with("varint_decode") {
            DecoderStep::VarintDecode
        } else if step.starts_with("bit_unpack") {
            DecoderStep::BitUnpack
        } else {
            return Err(ProfileError::Invalid(format!("unsupported decoder step: {step}")));
        };
        steps.push(parsed);
    }

    Ok(DecoderProfile { steps })
}

fn is_decoder_block_line(line: &str) -> bool {
    line == "}" || line == "decoder code {" || line.starts_with("input segment ")
}

fn parse_runtime(source: &str) -> Result<RuntimeProfile, ProfileError> {
    let mut runtime = RuntimeProfile::default();
    runtime.banks.clear();
    runtime.aliases.clear();
    runtime.control_state.clear();

    for line in semantic_lines(source) {
        if let Some(value) = line.strip_prefix("runtime.scope =") {
            runtime.scope = value.trim().parse()?;
        } else if let Some(value) = line.strip_prefix("polymorph.scope =") {
            runtime.polymorph_scope = value.trim().parse()?;
        } else if let Some(value) = line.strip_prefix("dispatch =") {
            runtime.dispatch = match value.trim() {
                "switch" => DispatchStrategy::Switch,
                other => return Err(ProfileError::InvalidDispatch(other.to_owned())),
            };
        } else if let Some(value) = line.strip_prefix("q.lowering =") {
            runtime.q_lowering = match value.trim() {
                "disabled" => WideRegisterPolicy::Disabled,
                other => {
                    return Err(ProfileError::Invalid(format!("unsupported q.lowering policy: {other}")));
                },
            };
        } else if let Some(rest) = line.strip_prefix("alias ") {
            if let Some((name, reg)) = rest.split_once('=') {
                runtime.aliases.insert(name.trim().to_owned(), reg.trim().to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("enhance ") {
            parse_runtime_enhancement(&mut runtime, rest.trim())?;
        } else if let Some(bank) = parse_register_bank(&line)? {
            runtime.banks.push(bank);
        } else if let Some(slot) = parse_control_state_slot(&line) {
            runtime.control_state.push(slot);
        } else if is_runtime_block_line(&line) {
            continue;
        } else {
            return Err(ProfileError::Invalid(format!(
                "runtime.vm has unsupported statement: {line}"
            )));
        }
    }

    Ok(runtime)
}

fn is_runtime_block_line(line: &str) -> bool {
    matches!(line, "}" | "registers {" | "control_state {" | "runtime {")
}

#[derive(Debug)]
struct ParsedInstruction {
    name: String,
    operand_descs: Vec<OperandDesc>,
    opcodes: Vec<Opcode>,
    decoded_width: Option<u8>,
    semantic: Vec<String>,
    depth: i32,
}

fn parse_instruction_header(header: &str) -> Result<(String, Vec<OperandDesc>), ProfileError> {
    let name = header
        .split(['(', ' ', '{'])
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ProfileError::Invalid(format!("invalid ISA instruction header: instr {header}")))?;
    let operands = header
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(operands, _)| parse_operand_decls(name, operands))
        .ok_or_else(|| ProfileError::Invalid(format!("ISA instruction {name} is missing operand list")))??;

    Ok((name.to_owned(), operands))
}

fn parse_operand_decls(instruction: &str, operands: &str) -> Result<Vec<OperandDesc>, ProfileError> {
    if operands.trim().is_empty() {
        return Ok(Vec::new());
    }

    operands
        .split(',')
        .map(|operand| {
            let (name, ty) = operand.trim().split_once(':').ok_or_else(|| {
                ProfileError::Invalid(format!("ISA instruction {instruction} has invalid operand {operand}"))
            })?;
            let name = name.trim();
            let ty = ty.trim();
            let (kind, value_type) = parse_operand_type(ty)?;
            Ok(OperandDesc {
                name: name.to_owned(),
                kind,
                value_type,
            })
        })
        .collect()
}

fn parse_operand_type(ty: &str) -> Result<(OperandKind, String), ProfileError> {
    if let Some(value_type) = ty.strip_prefix("vreg<").and_then(|rest| rest.strip_suffix('>')) {
        Ok((OperandKind::VReg, value_type.to_owned()))
    } else if let Some(value_type) = ty.strip_prefix("imm<").and_then(|rest| rest.strip_suffix('>')) {
        Ok((OperandKind::Imm, value_type.to_owned()))
    } else if ty == "const_pool_index" {
        Ok((OperandKind::ConstPoolIndex, "const_pool_index".to_owned()))
    } else if ty == "label" {
        Ok((OperandKind::Label, "label".to_owned()))
    } else {
        Err(ProfileError::Invalid(format!("unsupported ISA operand type {ty}")))
    }
}

fn parse_opcode_aliases(line: &str) -> Result<Option<Vec<Opcode>>, ProfileError> {
    let Some(rest) = line.strip_prefix("opcode alias") else {
        return Ok(None);
    };
    let Some((_, aliases)) = rest.split_once('[') else {
        return Err(ProfileError::Invalid(format!("invalid opcode alias line: {line}")));
    };
    let Some((aliases, _)) = aliases.split_once(']') else {
        return Err(ProfileError::Invalid(format!("invalid opcode alias line: {line}")));
    };

    let mut parsed = Vec::new();
    for alias in aliases.split(',') {
        let alias = alias.trim();
        let opcode = parse_opcode_literal(alias)
            .ok_or_else(|| ProfileError::Invalid(format!("invalid opcode alias {alias}")))?;
        parsed.push(opcode);
    }

    if parsed.is_empty() {
        return Err(ProfileError::Invalid(format!("empty opcode alias line: {line}")));
    }

    Ok(Some(parsed))
}

fn parse_decoded_width_override(line: &str) -> Result<Option<u8>, ProfileError> {
    let Some(value) = line.strip_prefix("decoded_width =") else {
        return Ok(None);
    };

    Ok(Some(parse_decoded_width_literal(value.trim())?))
}

fn parse_decoded_width_literal(value: &str) -> Result<u8, ProfileError> {
    let width = value
        .parse::<u8>()
        .map_err(|_| ProfileError::Invalid(format!("invalid decoded_width {value}")))?;
    if !SUPPORTED_DECODED_WIDTHS.contains(&width) {
        return Err(ProfileError::Invalid(format!(
            "decoded_width must be one of {:?}, got {width}",
            SUPPORTED_DECODED_WIDTHS
        )));
    }
    Ok(width)
}

fn parse_opcode_literal(value: &str) -> Option<Opcode> {
    let parsed = if let Some(hex) = value.strip_prefix("0x") {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        value.parse::<u32>().ok()?
    };
    Opcode::try_from(parsed).ok()
}

fn brace_delta(line: &str) -> i32 {
    line.matches('{').count() as i32 - line.matches('}').count() as i32
}

fn push_instruction(
    instructions: &mut Vec<InstructionDesc>,
    instruction: ParsedInstruction,
    default_decoded_width: u8,
) -> Result<(), ProfileError> {
    if instruction.opcodes.is_empty() {
        return Err(ProfileError::Invalid(format!(
            "ISA instruction {} has no opcode alias",
            instruction.name
        )));
    }

    let semantic_program = parse_semantic_program(&instruction.semantic)?;
    let semantic = template_for_program(&semantic_program).ok_or_else(|| {
        ProfileError::Invalid(format!(
            "ISA instruction {} has no supported AMICE handler semantic in semantic block: {:?}",
            instruction.name, semantic_program.statements
        ))
    })?;
    let effect = semantic_program.effect.clone();

    instructions.push(InstructionDesc::new_with_semantic_program(
        instruction.name,
        instruction.opcodes,
        instruction.operand_descs.len() as u8,
        instruction.decoded_width.unwrap_or(default_decoded_width),
        instruction.operand_descs,
        semantic,
        semantic_program,
        effect,
    ));
    Ok(())
}

fn parse_semantic_program(lines: &[String]) -> Result<SemanticProgram, ProfileError> {
    let semantic_lines = extract_semantic_block(lines)?;
    let mut statements = Vec::new();
    let mut q_register_references = Vec::new();

    for line in semantic_lines {
        collect_q_register_refs(&line, &mut q_register_references);
        statements.push(parse_semantic_statement(&line)?);
    }

    let effect = analyze_semantic_effect(&statements)?;
    Ok(SemanticProgram {
        statements,
        effect,
        q_register_references,
    })
}

fn extract_semantic_block(lines: &[String]) -> Result<Vec<String>, ProfileError> {
    let mut in_block = false;
    let mut depth = 0_i32;
    let mut out = Vec::new();

    for line in lines {
        if !in_block {
            if line == "semantic {" {
                in_block = true;
                depth = 1;
            }
            continue;
        }

        depth += brace_delta(line);
        if depth == 0 {
            return Ok(out);
        }
        out.push(line.clone());
    }

    Err(ProfileError::Invalid(
        "ISA instruction semantic block is missing or unterminated".to_owned(),
    ))
}

fn parse_semantic_statement(line: &str) -> Result<SemanticStmt, ProfileError> {
    if line == "state = unchanged" {
        return Ok(SemanticStmt::StateUnchanged);
    }
    if line == "unreachable" {
        return Ok(SemanticStmt::Unreachable);
    }
    if line == "trap" {
        return Ok(SemanticStmt::Trap);
    }
    if line == "side_effect()" {
        return Ok(SemanticStmt::SideEffect);
    }
    if let Some(value) = line.strip_prefix("pc =") {
        return Ok(SemanticStmt::AssignPc {
            value: parse_pc_expr(value.trim())?,
        });
    }
    if let Some(args) = line
        .strip_prefix("store_width(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_call_args(args)?;
        if args.len() != 3 {
            return Err(ProfileError::Invalid(format!(
                "store_width expects 3 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::StoreWidth {
            ptr: parse_semantic_expr(&args[0])?,
            value: parse_semantic_expr(&args[1])?,
            width: parse_semantic_expr(&args[2])?,
        });
    }
    if let Some(args) = line
        .strip_prefix("volatile_store_width(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_call_args(args)?;
        if args.len() != 3 {
            return Err(ProfileError::Invalid(format!(
                "volatile_store_width expects 3 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::VolatileStoreWidth {
            ptr: parse_semantic_expr(&args[0])?,
            value: parse_semantic_expr(&args[1])?,
            width: parse_semantic_expr(&args[2])?,
        });
    }
    if let Some(args) = line
        .strip_prefix("atomic_store_width(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_call_args(args)?;
        if args.len() != 4 {
            return Err(ProfileError::Invalid(format!(
                "atomic_store_width expects 4 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::AtomicStoreWidth {
            ptr: parse_semantic_expr(&args[0])?,
            value: parse_semantic_expr(&args[1])?,
            width: parse_semantic_expr(&args[2])?,
            ordering: parse_semantic_expr(&args[3])?,
        });
    }
    if let Some(args) = line
        .strip_prefix("volatile_atomic_store_width(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_call_args(args)?;
        if args.len() != 4 {
            return Err(ProfileError::Invalid(format!(
                "volatile_atomic_store_width expects 4 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::VolatileAtomicStoreWidth {
            ptr: parse_semantic_expr(&args[0])?,
            value: parse_semantic_expr(&args[1])?,
            width: parse_semantic_expr(&args[2])?,
            ordering: parse_semantic_expr(&args[3])?,
        });
    }
    if let Some(args) = line
        .strip_prefix("memcpy_dynamic(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_call_args(args)?;
        if args.len() != 3 {
            return Err(ProfileError::Invalid(format!(
                "memcpy_dynamic expects 3 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::MemcpyDynamic {
            dst: parse_semantic_expr(&args[0])?,
            src: parse_semantic_expr(&args[1])?,
            len: parse_semantic_expr(&args[2])?,
        });
    }
    if let Some(args) = line
        .strip_prefix("memmove_dynamic(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_call_args(args)?;
        if args.len() != 3 {
            return Err(ProfileError::Invalid(format!(
                "memmove_dynamic expects 3 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::MemmoveDynamic {
            dst: parse_semantic_expr(&args[0])?,
            src: parse_semantic_expr(&args[1])?,
            len: parse_semantic_expr(&args[2])?,
        });
    }
    if let Some(args) = line
        .strip_prefix("memset_dynamic(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_call_args(args)?;
        if args.len() != 3 {
            return Err(ProfileError::Invalid(format!(
                "memset_dynamic expects 3 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::MemsetDynamic {
            dst: parse_semantic_expr(&args[0])?,
            value: parse_semantic_expr(&args[1])?,
            len: parse_semantic_expr(&args[2])?,
        });
    }
    if let Some(args) = line
        .strip_prefix("volatile_memcpy_dynamic(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_call_args(args)?;
        if args.len() != 3 {
            return Err(ProfileError::Invalid(format!(
                "volatile_memcpy_dynamic expects 3 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::VolatileMemcpyDynamic {
            dst: parse_semantic_expr(&args[0])?,
            src: parse_semantic_expr(&args[1])?,
            len: parse_semantic_expr(&args[2])?,
        });
    }
    if let Some(args) = line
        .strip_prefix("volatile_memmove_dynamic(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_call_args(args)?;
        if args.len() != 3 {
            return Err(ProfileError::Invalid(format!(
                "volatile_memmove_dynamic expects 3 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::VolatileMemmoveDynamic {
            dst: parse_semantic_expr(&args[0])?,
            src: parse_semantic_expr(&args[1])?,
            len: parse_semantic_expr(&args[2])?,
        });
    }
    if let Some(args) = line
        .strip_prefix("volatile_memset_dynamic(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_call_args(args)?;
        if args.len() != 3 {
            return Err(ProfileError::Invalid(format!(
                "volatile_memset_dynamic expects 3 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::VolatileMemsetDynamic {
            dst: parse_semantic_expr(&args[0])?,
            value: parse_semantic_expr(&args[1])?,
            len: parse_semantic_expr(&args[2])?,
        });
    }
    if let Some(args) = line.strip_prefix("cmpxchg(").and_then(|rest| rest.strip_suffix(')')) {
        let args = split_call_args(args)?;
        if args.len() != 8 {
            return Err(ProfileError::Invalid(format!(
                "cmpxchg expects 8 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::CmpXchg {
            old: parse_register_lvalue(&args[0]).ok_or_else(|| {
                ProfileError::Invalid(format!("cmpxchg old result must be a register lvalue in {line}"))
            })?,
            success: parse_register_lvalue(&args[1]).ok_or_else(|| {
                ProfileError::Invalid(format!("cmpxchg success result must be a register lvalue in {line}"))
            })?,
            ptr: parse_semantic_expr(&args[2])?,
            compare: parse_semantic_expr(&args[3])?,
            new: parse_semantic_expr(&args[4])?,
            width: parse_semantic_expr(&args[5])?,
            success_ordering: parse_semantic_expr(&args[6])?,
            failure_ordering: parse_semantic_expr(&args[7])?,
        });
    }
    if let Some(args) = line
        .strip_prefix("volatile_cmpxchg(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_call_args(args)?;
        if args.len() != 8 {
            return Err(ProfileError::Invalid(format!(
                "volatile_cmpxchg expects 8 arguments, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::VolatileCmpXchg {
            old: parse_register_lvalue(&args[0]).ok_or_else(|| {
                ProfileError::Invalid(format!(
                    "volatile_cmpxchg old result must be a register lvalue in {line}"
                ))
            })?,
            success: parse_register_lvalue(&args[1]).ok_or_else(|| {
                ProfileError::Invalid(format!(
                    "volatile_cmpxchg success result must be a register lvalue in {line}"
                ))
            })?,
            ptr: parse_semantic_expr(&args[2])?,
            compare: parse_semantic_expr(&args[3])?,
            new: parse_semantic_expr(&args[4])?,
            width: parse_semantic_expr(&args[5])?,
            success_ordering: parse_semantic_expr(&args[6])?,
            failure_ordering: parse_semantic_expr(&args[7])?,
        });
    }
    if let Some(args) = line.strip_prefix("fence(").and_then(|rest| rest.strip_suffix(')')) {
        let args = split_call_args(args)?;
        if args.len() != 1 {
            return Err(ProfileError::Invalid(format!(
                "fence expects 1 argument, got {} in {line}",
                args.len()
            )));
        }
        return Ok(SemanticStmt::Fence {
            ordering: parse_semantic_expr(&args[0])?,
        });
    }
    if let Some((left, right)) = line.split_once('=') {
        if let Some(dst) = parse_register_lvalue(left.trim()) {
            return Ok(SemanticStmt::AssignReg {
                dst,
                value: parse_semantic_expr(right.trim())?,
            });
        }
    }

    Err(ProfileError::Invalid(format!("unsupported semantic statement: {line}")))
}

fn parse_pc_expr(value: &str) -> Result<PcExpr, ProfileError> {
    if value == "next" {
        Ok(PcExpr::Next)
    } else if value == "return" {
        Ok(PcExpr::Return)
    } else if let Some(register) = parse_reg_ref(value) {
        Ok(PcExpr::Register(register))
    } else if let Some(args) = value.strip_prefix("select(").and_then(|rest| rest.strip_suffix(')')) {
        let args = split_call_args(args)?;
        if args.len() != 3 {
            return Err(ProfileError::Invalid(format!(
                "pc select expects 3 arguments, got {} in {value}",
                args.len()
            )));
        }
        Ok(PcExpr::Select {
            cond: Box::new(parse_semantic_expr(&args[0])?),
            then_pc: args[1].clone(),
            else_pc: args[2].clone(),
        })
    } else {
        Ok(PcExpr::Label(value.to_owned()))
    }
}

fn parse_semantic_expr(value: &str) -> Result<SemanticExpr, ProfileError> {
    let value = value.trim();
    if value == "next" {
        return Ok(SemanticExpr::NextPc);
    }
    if let Some(register) = parse_reg_ref(value) {
        return Ok(SemanticExpr::Register(register));
    }
    if let Some(index) = value
        .strip_prefix("const_pool[")
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return Ok(SemanticExpr::ConstPool(index.to_owned()));
    }
    if let Some(call) = parse_call_table_return(value)? {
        return Ok(call);
    }
    if let Some(expr) = parse_function_expr(value)? {
        return Ok(expr);
    }
    if let Some(expr) = parse_binary_expr(value)? {
        return Ok(expr);
    }

    Ok(SemanticExpr::Operand(value.to_owned()))
}

fn parse_function_expr(value: &str) -> Result<Option<SemanticExpr>, ProfileError> {
    for name in [
        "trunc_width",
        "zero_extend",
        "sign_extend",
        "bitcast_width",
        "int_bin",
        "int_unary",
        "int_ternary",
        "int_overflow",
        "compare",
        "float_bin",
        "float_unary",
        "float_ternary",
        "float_cast",
        "float_compare",
        "float_class",
        "stack_alloc",
        "stack_alloc_dynamic",
        "load_width",
        "volatile_load_width",
        "atomic_load_width",
        "volatile_atomic_load_width",
        "atomic_rmw",
        "volatile_atomic_rmw",
        "read_counter",
    ] {
        let Some(args) = parse_named_call(value, name)? else {
            continue;
        };
        let valid_arity = match name {
            "sign_extend" => matches!(args.len(), 2 | 3),
            "trunc_width" | "stack_alloc" | "load_width" | "volatile_load_width" => args.len() == 2,
            "stack_alloc_dynamic" => args.len() == 3,
            "atomic_load_width" | "volatile_atomic_load_width" => args.len() == 3,
            "atomic_rmw" | "volatile_atomic_rmw" => args.len() == 5,
            "read_counter" => args.len() == 1,
            "zero_extend" | "bitcast_width" => args.len() == 3,
            "int_bin" => args.len() == 3,
            "int_unary" => args.len() == 3,
            "int_ternary" => args.len() == 5,
            "int_overflow" => args.len() == 4,
            "compare" => args.len() == 4,
            "float_bin" => args.len() == 4,
            "float_unary" => args.len() == 3,
            "float_ternary" => args.len() == 5,
            "float_cast" => args.len() == 4,
            "float_compare" => args.len() == 4,
            "float_class" => args.len() == 3,
            _ => false,
        };
        if !valid_arity {
            return Err(ProfileError::Invalid(format!(
                "{name} has invalid argument count {} in {value}",
                args.len()
            )));
        }
        return Ok(Some(match name {
            "trunc_width" => SemanticExpr::TruncWidth {
                value: Box::new(parse_semantic_expr(&args[0])?),
                width: Box::new(parse_semantic_expr(&args[1])?),
            },
            "zero_extend" => SemanticExpr::ZeroExtend {
                value: Box::new(parse_semantic_expr(&args[0])?),
                from_width: Box::new(parse_semantic_expr(&args[1])?),
                to_width: Box::new(parse_semantic_expr(&args[2])?),
            },
            "sign_extend" => SemanticExpr::SignExtend {
                value: Box::new(parse_semantic_expr(&args[0])?),
                from_width: Box::new(parse_semantic_expr(&args[1])?),
                to_width: args
                    .get(2)
                    .map(|arg| parse_semantic_expr(arg).map(Box::new))
                    .transpose()?,
            },
            "bitcast_width" => SemanticExpr::BitcastWidth {
                value: Box::new(parse_semantic_expr(&args[0])?),
                from_width: Box::new(parse_semantic_expr(&args[1])?),
                to_width: Box::new(parse_semantic_expr(&args[2])?),
            },
            "int_bin" => SemanticExpr::Binary {
                op: parse_int_bin_op(&args[0])?,
                lhs: Box::new(parse_semantic_expr(&args[1])?),
                rhs: Box::new(parse_semantic_expr(&args[2])?),
            },
            "int_unary" => SemanticExpr::IntUnary {
                op: parse_int_unary_op(&args[0])?,
                value: Box::new(parse_semantic_expr(&args[1])?),
                width: Box::new(parse_semantic_expr(&args[2])?),
            },
            "int_ternary" => SemanticExpr::IntTernary {
                op: parse_int_ternary_op(&args[0])?,
                lhs: Box::new(parse_semantic_expr(&args[1])?),
                rhs: Box::new(parse_semantic_expr(&args[2])?),
                third: Box::new(parse_semantic_expr(&args[3])?),
                width: Box::new(parse_semantic_expr(&args[4])?),
            },
            "int_overflow" => SemanticExpr::IntOverflow {
                op: parse_int_overflow_op(&args[0])?,
                lhs: Box::new(parse_semantic_expr(&args[1])?),
                rhs: Box::new(parse_semantic_expr(&args[2])?),
                width: Box::new(parse_semantic_expr(&args[3])?),
            },
            "compare" => SemanticExpr::Compare {
                pred: Box::new(parse_semantic_expr(&args[0])?),
                lhs: Box::new(parse_semantic_expr(&args[1])?),
                rhs: Box::new(parse_semantic_expr(&args[2])?),
                width: Box::new(parse_semantic_expr(&args[3])?),
            },
            "float_bin" => SemanticExpr::FloatBinary {
                op: parse_float_bin_op(&args[0])?,
                lhs: Box::new(parse_semantic_expr(&args[1])?),
                rhs: Box::new(parse_semantic_expr(&args[2])?),
                width: Box::new(parse_semantic_expr(&args[3])?),
            },
            "float_unary" => SemanticExpr::FloatUnary {
                op: parse_float_unary_op(&args[0])?,
                value: Box::new(parse_semantic_expr(&args[1])?),
                width: Box::new(parse_semantic_expr(&args[2])?),
            },
            "float_ternary" => SemanticExpr::FloatTernary {
                op: parse_float_ternary_op(&args[0])?,
                lhs: Box::new(parse_semantic_expr(&args[1])?),
                rhs: Box::new(parse_semantic_expr(&args[2])?),
                third: Box::new(parse_semantic_expr(&args[3])?),
                width: Box::new(parse_semantic_expr(&args[4])?),
            },
            "float_cast" => SemanticExpr::FloatCast {
                op: parse_float_cast_op(&args[0])?,
                value: Box::new(parse_semantic_expr(&args[1])?),
                from_width: Box::new(parse_semantic_expr(&args[2])?),
                to_width: Box::new(parse_semantic_expr(&args[3])?),
            },
            "float_compare" => SemanticExpr::FloatCompare {
                pred: Box::new(parse_semantic_expr(&args[0])?),
                lhs: Box::new(parse_semantic_expr(&args[1])?),
                rhs: Box::new(parse_semantic_expr(&args[2])?),
                width: Box::new(parse_semantic_expr(&args[3])?),
            },
            "float_class" => SemanticExpr::FloatClass {
                value: Box::new(parse_semantic_expr(&args[0])?),
                mask: Box::new(parse_semantic_expr(&args[1])?),
                width: Box::new(parse_semantic_expr(&args[2])?),
            },
            "stack_alloc" => SemanticExpr::StackAlloc {
                bytes: Box::new(parse_semantic_expr(&args[0])?),
                align: Box::new(parse_semantic_expr(&args[1])?),
            },
            "stack_alloc_dynamic" => SemanticExpr::StackAllocDynamic {
                count: Box::new(parse_semantic_expr(&args[0])?),
                elem_size: Box::new(parse_semantic_expr(&args[1])?),
                align: Box::new(parse_semantic_expr(&args[2])?),
            },
            "load_width" => SemanticExpr::LoadWidth {
                ptr: Box::new(parse_semantic_expr(&args[0])?),
                width: Box::new(parse_semantic_expr(&args[1])?),
            },
            "volatile_load_width" => SemanticExpr::VolatileLoadWidth {
                ptr: Box::new(parse_semantic_expr(&args[0])?),
                width: Box::new(parse_semantic_expr(&args[1])?),
            },
            "atomic_load_width" => SemanticExpr::AtomicLoadWidth {
                ptr: Box::new(parse_semantic_expr(&args[0])?),
                width: Box::new(parse_semantic_expr(&args[1])?),
                ordering: Box::new(parse_semantic_expr(&args[2])?),
            },
            "volatile_atomic_load_width" => SemanticExpr::VolatileAtomicLoadWidth {
                ptr: Box::new(parse_semantic_expr(&args[0])?),
                width: Box::new(parse_semantic_expr(&args[1])?),
                ordering: Box::new(parse_semantic_expr(&args[2])?),
            },
            "atomic_rmw" => SemanticExpr::AtomicRmw {
                op: parse_atomic_rmw_op(&args[0])?,
                ptr: Box::new(parse_semantic_expr(&args[1])?),
                value: Box::new(parse_semantic_expr(&args[2])?),
                width: Box::new(parse_semantic_expr(&args[3])?),
                ordering: Box::new(parse_semantic_expr(&args[4])?),
            },
            "volatile_atomic_rmw" => SemanticExpr::VolatileAtomicRmw {
                op: parse_atomic_rmw_op(&args[0])?,
                ptr: Box::new(parse_semantic_expr(&args[1])?),
                value: Box::new(parse_semantic_expr(&args[2])?),
                width: Box::new(parse_semantic_expr(&args[3])?),
                ordering: Box::new(parse_semantic_expr(&args[4])?),
            },
            "read_counter" => SemanticExpr::ReadCounter {
                kind: parse_counter_kind(&args[0])?,
            },
            _ => unreachable!("function name is selected from fixed table"),
        }));
    }

    Ok(None)
}

fn parse_counter_kind(value: &str) -> Result<CounterKind, ProfileError> {
    match value.trim() {
        "cycle" | "readcyclecounter" => Ok(CounterKind::Cycle),
        "steady" | "readsteadycounter" => Ok(CounterKind::Steady),
        other => Err(ProfileError::Invalid(format!("unsupported counter kind {other}"))),
    }
}

fn parse_float_bin_op(value: &str) -> Result<SemanticFloatBinOp, ProfileError> {
    match value.trim() {
        "fadd" => Ok(SemanticFloatBinOp::Add),
        "fsub" => Ok(SemanticFloatBinOp::Sub),
        "fmul" => Ok(SemanticFloatBinOp::Mul),
        "fdiv" => Ok(SemanticFloatBinOp::Div),
        "frem" => Ok(SemanticFloatBinOp::Rem),
        "fminnum" | "minnum" => Ok(SemanticFloatBinOp::MinNum),
        "fmaxnum" | "maxnum" => Ok(SemanticFloatBinOp::MaxNum),
        "fminimum" | "minimum" => Ok(SemanticFloatBinOp::Minimum),
        "fmaximum" | "maximum" => Ok(SemanticFloatBinOp::Maximum),
        "fcopysign" | "copysign" => Ok(SemanticFloatBinOp::CopySign),
        other => Err(ProfileError::Invalid(format!(
            "unsupported float_bin operation {other}"
        ))),
    }
}

fn parse_int_unary_op(value: &str) -> Result<SemanticIntUnaryOp, ProfileError> {
    match value.trim() {
        "ctpop" => Ok(SemanticIntUnaryOp::CtPop),
        "ctlz" => Ok(SemanticIntUnaryOp::CtLz),
        "cttz" => Ok(SemanticIntUnaryOp::CtTz),
        "abs" => Ok(SemanticIntUnaryOp::Abs),
        "bswap" => Ok(SemanticIntUnaryOp::BSwap),
        "bitreverse" => Ok(SemanticIntUnaryOp::BitReverse),
        other => Err(ProfileError::Invalid(format!(
            "unsupported int_unary operation {other}"
        ))),
    }
}

fn parse_int_bin_op(value: &str) -> Result<SemanticBinOp, ProfileError> {
    match value.trim() {
        "smax" => Ok(SemanticBinOp::SMax),
        "smin" => Ok(SemanticBinOp::SMin),
        "umax" => Ok(SemanticBinOp::UMax),
        "umin" => Ok(SemanticBinOp::UMin),
        "uadd_sat" => Ok(SemanticBinOp::UAddSat),
        "usub_sat" => Ok(SemanticBinOp::USubSat),
        "sadd_sat" => Ok(SemanticBinOp::SAddSat),
        "ssub_sat" => Ok(SemanticBinOp::SSubSat),
        "ushl_sat" => Ok(SemanticBinOp::UShlSat),
        "sshl_sat" => Ok(SemanticBinOp::SShlSat),
        other => Err(ProfileError::Invalid(format!("unsupported int_bin operation {other}"))),
    }
}

fn parse_int_ternary_op(value: &str) -> Result<SemanticIntTernaryOp, ProfileError> {
    match value.trim() {
        "fshl" => Ok(SemanticIntTernaryOp::FShl),
        "fshr" => Ok(SemanticIntTernaryOp::FShr),
        other => Err(ProfileError::Invalid(format!(
            "unsupported int_ternary operation {other}"
        ))),
    }
}

fn parse_int_overflow_op(value: &str) -> Result<SemanticIntOverflowOp, ProfileError> {
    match value.trim() {
        "uadd" => Ok(SemanticIntOverflowOp::UAdd),
        "sadd" => Ok(SemanticIntOverflowOp::SAdd),
        "usub" => Ok(SemanticIntOverflowOp::USub),
        "ssub" => Ok(SemanticIntOverflowOp::SSub),
        "umul" => Ok(SemanticIntOverflowOp::UMul),
        "smul" => Ok(SemanticIntOverflowOp::SMul),
        other => Err(ProfileError::Invalid(format!(
            "unsupported int_overflow operation {other}"
        ))),
    }
}

fn parse_float_unary_op(value: &str) -> Result<SemanticFloatUnaryOp, ProfileError> {
    match value.trim() {
        "fneg" => Ok(SemanticFloatUnaryOp::Neg),
        "fabs" => Ok(SemanticFloatUnaryOp::Abs),
        "fsqrt" | "sqrt" => Ok(SemanticFloatUnaryOp::Sqrt),
        "fcanonicalize" | "canonicalize" => Ok(SemanticFloatUnaryOp::Canonicalize),
        "ffloor" | "floor" => Ok(SemanticFloatUnaryOp::Floor),
        "fceil" | "ceil" => Ok(SemanticFloatUnaryOp::Ceil),
        "ftrunc" | "trunc" => Ok(SemanticFloatUnaryOp::Trunc),
        "frint" | "rint" => Ok(SemanticFloatUnaryOp::Rint),
        "fnearbyint" | "nearbyint" => Ok(SemanticFloatUnaryOp::NearbyInt),
        "fround" | "round" => Ok(SemanticFloatUnaryOp::Round),
        "froundeven" | "roundeven" => Ok(SemanticFloatUnaryOp::RoundEven),
        other => Err(ProfileError::Invalid(format!(
            "unsupported float_unary operation {other}"
        ))),
    }
}

fn parse_float_ternary_op(value: &str) -> Result<SemanticFloatTernaryOp, ProfileError> {
    match value.trim() {
        "fma" | "ffma" => Ok(SemanticFloatTernaryOp::Fma),
        "fmuladd" | "ffmuladd" => Ok(SemanticFloatTernaryOp::MulAdd),
        other => Err(ProfileError::Invalid(format!(
            "unsupported float_ternary operation {other}"
        ))),
    }
}

fn parse_float_cast_op(value: &str) -> Result<SemanticFloatCastOp, ProfileError> {
    match value.trim() {
        "sitofp" => Ok(SemanticFloatCastOp::SignedIntToFloat),
        "uitofp" => Ok(SemanticFloatCastOp::UnsignedIntToFloat),
        "fptosi" => Ok(SemanticFloatCastOp::FloatToSignedInt),
        "fptoui" => Ok(SemanticFloatCastOp::FloatToUnsignedInt),
        "fptrunc" => Ok(SemanticFloatCastOp::FloatTrunc),
        "fpext" => Ok(SemanticFloatCastOp::FloatExt),
        other => Err(ProfileError::Invalid(format!(
            "unsupported float_cast operation {other}"
        ))),
    }
}

fn parse_atomic_rmw_op(value: &str) -> Result<SemanticAtomicRmwOp, ProfileError> {
    match value.trim() {
        "xchg" => Ok(SemanticAtomicRmwOp::Xchg),
        "add" => Ok(SemanticAtomicRmwOp::Add),
        "sub" => Ok(SemanticAtomicRmwOp::Sub),
        "and" => Ok(SemanticAtomicRmwOp::And),
        "or" => Ok(SemanticAtomicRmwOp::Or),
        "xor" => Ok(SemanticAtomicRmwOp::Xor),
        "nand" => Ok(SemanticAtomicRmwOp::Nand),
        "max" => Ok(SemanticAtomicRmwOp::Max),
        "min" => Ok(SemanticAtomicRmwOp::Min),
        "umax" => Ok(SemanticAtomicRmwOp::UMax),
        "umin" => Ok(SemanticAtomicRmwOp::UMin),
        "uinc_wrap" => Ok(SemanticAtomicRmwOp::UIncWrap),
        "udec_wrap" => Ok(SemanticAtomicRmwOp::UDecWrap),
        "usub_cond" => Ok(SemanticAtomicRmwOp::USubCond),
        "usub_sat" => Ok(SemanticAtomicRmwOp::USubSat),
        "fadd" => Ok(SemanticAtomicRmwOp::FAdd),
        "fsub" => Ok(SemanticAtomicRmwOp::FSub),
        "fmax" => Ok(SemanticAtomicRmwOp::FMax),
        "fmin" => Ok(SemanticAtomicRmwOp::FMin),
        "fmaximum" => Ok(SemanticAtomicRmwOp::FMaximum),
        "fminimum" => Ok(SemanticAtomicRmwOp::FMinimum),
        other => Err(ProfileError::Invalid(format!(
            "unsupported atomic_rmw operation {other}"
        ))),
    }
}

fn parse_binary_expr(value: &str) -> Result<Option<SemanticExpr>, ProfileError> {
    for (token, op) in [
        (" >>u ", SemanticBinOp::LShr),
        (" >>s ", SemanticBinOp::AShr),
        (" /u ", SemanticBinOp::UDiv),
        (" /s ", SemanticBinOp::SDiv),
        (" %u ", SemanticBinOp::URem),
        (" %s ", SemanticBinOp::SRem),
        (" xor ", SemanticBinOp::Xor),
        (" and ", SemanticBinOp::And),
        (" or ", SemanticBinOp::Or),
        (" << ", SemanticBinOp::Shl),
        (" + ", SemanticBinOp::Add),
        (" - ", SemanticBinOp::Sub),
        (" * ", SemanticBinOp::Mul),
    ] {
        if let Some((lhs, rhs)) = split_top_level_once(value, token) {
            return Ok(Some(SemanticExpr::Binary {
                op,
                lhs: Box::new(parse_semantic_expr(lhs)?),
                rhs: Box::new(parse_semantic_expr(rhs)?),
            }));
        }
    }

    Ok(None)
}

fn parse_call_table_return(value: &str) -> Result<Option<SemanticExpr>, ProfileError> {
    let Some(rest) = value.strip_prefix("call_table[") else {
        return Ok(None);
    };
    let Some((callee, rest)) = rest.split_once("].ret") else {
        return Err(ProfileError::Invalid(format!(
            "invalid call_table return expression: {value}"
        )));
    };
    let Some((index, args)) = rest.split_once('(') else {
        return Err(ProfileError::Invalid(format!(
            "invalid call_table return expression: {value}"
        )));
    };
    let index = index
        .parse::<u8>()
        .map_err(|_| ProfileError::Invalid(format!("invalid call_table return index in {value}")))?;
    let args = args
        .strip_suffix(')')
        .ok_or_else(|| ProfileError::Invalid(format!("invalid call_table return expression: {value}")))?;
    Ok(Some(SemanticExpr::CallTableReturn {
        callee: callee.to_owned(),
        index,
        args: split_call_args(args)?
            .into_iter()
            .map(|arg| parse_semantic_expr(&arg))
            .collect::<Result<Vec<_>, _>>()?,
    }))
}

fn parse_named_call(value: &str, name: &str) -> Result<Option<Vec<String>>, ProfileError> {
    let Some(args) = value
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return Ok(None);
    };
    split_call_args(args).map(Some)
}

fn split_call_args(args: &str) -> Result<Vec<String>, ProfileError> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0;
    for (index, ch) in args.char_indices() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(args[start..index].trim().to_owned());
                start = index + 1;
            },
            _ => {},
        }
        if depth < 0 {
            return Err(ProfileError::Invalid(format!(
                "unbalanced expression arguments: {args}"
            )));
        }
    }
    if depth != 0 {
        return Err(ProfileError::Invalid(format!(
            "unbalanced expression arguments: {args}"
        )));
    }
    let tail = args[start..].trim();
    if !tail.is_empty() {
        parts.push(tail.to_owned());
    }
    Ok(parts)
}

fn split_top_level_once<'a>(input: &'a str, token: &str) -> Option<(&'a str, &'a str)> {
    let mut depth = 0_i32;
    let bytes = input.as_bytes();
    let token_bytes = token.as_bytes();
    let mut index = 0;
    while index + token_bytes.len() <= bytes.len() {
        match bytes[index] as char {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {},
        }
        if depth == 0 && bytes[index..].starts_with(token_bytes) {
            return Some((&input[..index], &input[index + token.len()..]));
        }
        index += 1;
    }
    None
}

fn parse_register_lvalue(left: &str) -> Option<String> {
    parse_reg_ref(left)
}

fn parse_reg_ref(value: &str) -> Option<String> {
    let rest = value.strip_prefix("reg[")?;
    let (register, tail) = rest.split_once(']')?;
    tail.is_empty().then(|| register.to_owned())
}

fn template_for_program(program: &SemanticProgram) -> Option<HandlerSemantic> {
    use BinOp::*;
    use CastOp::*;
    use HandlerSemantic::*;
    use IntOverflowOp::*;

    let statements = &program.statements;
    if matches_assign_reg_expr(statements, "dst", &trunc_width(operand("imm"), operand("width"))) && pc_next(statements)
    {
        Some(MovImm)
    } else if matches_assign_reg_expr(statements, "dst", &SemanticExpr::ConstPool("index".to_owned()))
        && pc_next(statements)
    {
        Some(ConstLoad)
    } else if matches_assign_reg_expr(statements, "dst", &trunc_width(reg("src"), operand("width")))
        && pc_next(statements)
    {
        Some(Mov)
    } else if add_xor_template(statements) {
        Some(Super(SuperOp::AddXor))
    } else if icmp_br_if_template(statements) {
        Some(Super(SuperOp::IcmpBrIf))
    } else if gep_load_template(statements) {
        Some(Super(SuperOp::GepLoad))
    } else if load_add_template(statements) {
        Some(Super(SuperOp::LoadAdd))
    } else if matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::ReadCounter {
            kind: CounterKind::Cycle,
        },
    ) && pc_next(statements)
    {
        Some(ReadCounter(CounterKind::Cycle))
    } else if matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::ReadCounter {
            kind: CounterKind::Steady,
        },
    ) && pc_next(statements)
    {
        Some(ReadCounter(CounterKind::Steady))
    } else if int_overflow_template(statements, SemanticIntOverflowOp::UAdd) {
        Some(IntOverflow(UAdd))
    } else if int_overflow_template(statements, SemanticIntOverflowOp::SAdd) {
        Some(IntOverflow(SAdd))
    } else if int_overflow_template(statements, SemanticIntOverflowOp::USub) {
        Some(IntOverflow(USub))
    } else if int_overflow_template(statements, SemanticIntOverflowOp::SSub) {
        Some(IntOverflow(SSub))
    } else if int_overflow_template(statements, SemanticIntOverflowOp::UMul) {
        Some(IntOverflow(UMul))
    } else if int_overflow_template(statements, SemanticIntOverflowOp::SMul) {
        Some(IntOverflow(SMul))
    } else if bin_template(statements, SemanticBinOp::Add) {
        Some(Bin(Add))
    } else if bin_template(statements, SemanticBinOp::Sub) {
        Some(Bin(Sub))
    } else if bin_template(statements, SemanticBinOp::Mul) {
        Some(Bin(Mul))
    } else if bin_template(statements, SemanticBinOp::UDiv) {
        Some(Bin(UDiv))
    } else if bin_template(statements, SemanticBinOp::SDiv) {
        Some(Bin(SDiv))
    } else if bin_template(statements, SemanticBinOp::URem) {
        Some(Bin(URem))
    } else if bin_template(statements, SemanticBinOp::SRem) {
        Some(Bin(SRem))
    } else if bin_template(statements, SemanticBinOp::Xor) {
        Some(Bin(Xor))
    } else if bin_template(statements, SemanticBinOp::And) {
        Some(Bin(And))
    } else if bin_template(statements, SemanticBinOp::Or) {
        Some(Bin(Or))
    } else if bin_template(statements, SemanticBinOp::Shl) {
        Some(Bin(Shl))
    } else if bin_template(statements, SemanticBinOp::LShr) {
        Some(Bin(LShr))
    } else if ashr_template(statements) {
        Some(Bin(AShr))
    } else if bin_template(statements, SemanticBinOp::SMax) {
        Some(Bin(SMax))
    } else if bin_template(statements, SemanticBinOp::SMin) {
        Some(Bin(SMin))
    } else if bin_template(statements, SemanticBinOp::UMax) {
        Some(Bin(UMax))
    } else if bin_template(statements, SemanticBinOp::UMin) {
        Some(Bin(UMin))
    } else if bin_template(statements, SemanticBinOp::UAddSat) {
        Some(Bin(UAddSat))
    } else if bin_template(statements, SemanticBinOp::USubSat) {
        Some(Bin(USubSat))
    } else if bin_template(statements, SemanticBinOp::SAddSat) {
        Some(Bin(SAddSat))
    } else if bin_template(statements, SemanticBinOp::SSubSat) {
        Some(Bin(SSubSat))
    } else if bin_template(statements, SemanticBinOp::UShlSat) {
        Some(Bin(UShlSat))
    } else if bin_template(statements, SemanticBinOp::SShlSat) {
        Some(Bin(SShlSat))
    } else if float_bin_template(statements, SemanticFloatBinOp::Add) {
        Some(FloatBin(FloatBinOp::Add))
    } else if float_bin_template(statements, SemanticFloatBinOp::Sub) {
        Some(FloatBin(FloatBinOp::Sub))
    } else if float_bin_template(statements, SemanticFloatBinOp::Mul) {
        Some(FloatBin(FloatBinOp::Mul))
    } else if float_bin_template(statements, SemanticFloatBinOp::Div) {
        Some(FloatBin(FloatBinOp::Div))
    } else if float_bin_template(statements, SemanticFloatBinOp::Rem) {
        Some(FloatBin(FloatBinOp::Rem))
    } else if float_bin_template(statements, SemanticFloatBinOp::MinNum) {
        Some(FloatBin(FloatBinOp::MinNum))
    } else if float_bin_template(statements, SemanticFloatBinOp::MaxNum) {
        Some(FloatBin(FloatBinOp::MaxNum))
    } else if float_bin_template(statements, SemanticFloatBinOp::Minimum) {
        Some(FloatBin(FloatBinOp::Minimum))
    } else if float_bin_template(statements, SemanticFloatBinOp::Maximum) {
        Some(FloatBin(FloatBinOp::Maximum))
    } else if float_bin_template(statements, SemanticFloatBinOp::CopySign) {
        Some(FloatBin(FloatBinOp::CopySign))
    } else if float_unary_template(statements, SemanticFloatUnaryOp::Neg) {
        Some(FloatUnary(FloatUnaryOp::Neg))
    } else if float_unary_template(statements, SemanticFloatUnaryOp::Abs) {
        Some(FloatUnary(FloatUnaryOp::Abs))
    } else if float_unary_template(statements, SemanticFloatUnaryOp::Sqrt) {
        Some(FloatUnary(FloatUnaryOp::Sqrt))
    } else if float_unary_template(statements, SemanticFloatUnaryOp::Canonicalize) {
        Some(FloatUnary(FloatUnaryOp::Canonicalize))
    } else if float_unary_template(statements, SemanticFloatUnaryOp::Floor) {
        Some(FloatUnary(FloatUnaryOp::Floor))
    } else if float_unary_template(statements, SemanticFloatUnaryOp::Ceil) {
        Some(FloatUnary(FloatUnaryOp::Ceil))
    } else if float_unary_template(statements, SemanticFloatUnaryOp::Trunc) {
        Some(FloatUnary(FloatUnaryOp::Trunc))
    } else if float_unary_template(statements, SemanticFloatUnaryOp::Rint) {
        Some(FloatUnary(FloatUnaryOp::Rint))
    } else if float_unary_template(statements, SemanticFloatUnaryOp::NearbyInt) {
        Some(FloatUnary(FloatUnaryOp::NearbyInt))
    } else if float_unary_template(statements, SemanticFloatUnaryOp::Round) {
        Some(FloatUnary(FloatUnaryOp::Round))
    } else if float_unary_template(statements, SemanticFloatUnaryOp::RoundEven) {
        Some(FloatUnary(FloatUnaryOp::RoundEven))
    } else if float_ternary_template(statements, SemanticFloatTernaryOp::Fma) {
        Some(FloatTernary(FloatTernaryOp::Fma))
    } else if float_ternary_template(statements, SemanticFloatTernaryOp::MulAdd) {
        Some(FloatTernary(FloatTernaryOp::MulAdd))
    } else if float_cast_template(statements, SemanticFloatCastOp::SignedIntToFloat) {
        Some(FloatCast(FloatCastOp::SignedIntToFloat))
    } else if float_cast_template(statements, SemanticFloatCastOp::UnsignedIntToFloat) {
        Some(FloatCast(FloatCastOp::UnsignedIntToFloat))
    } else if float_cast_template(statements, SemanticFloatCastOp::FloatToSignedInt) {
        Some(FloatCast(FloatCastOp::FloatToSignedInt))
    } else if float_cast_template(statements, SemanticFloatCastOp::FloatToUnsignedInt) {
        Some(FloatCast(FloatCastOp::FloatToUnsignedInt))
    } else if float_cast_template(statements, SemanticFloatCastOp::FloatTrunc) {
        Some(FloatCast(FloatCastOp::FloatTrunc))
    } else if float_cast_template(statements, SemanticFloatCastOp::FloatExt) {
        Some(FloatCast(FloatCastOp::FloatExt))
    } else if float_class_template(statements) {
        Some(FloatClass)
    } else if matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::Compare {
            pred: Box::new(operand("pred")),
            lhs: Box::new(reg("lhs")),
            rhs: Box::new(reg("rhs")),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
    {
        Some(Icmp)
    } else if int_unary_template(statements, SemanticIntUnaryOp::CtPop) {
        Some(IntUnary(IntUnaryOp::CtPop))
    } else if int_unary_template(statements, SemanticIntUnaryOp::CtLz) {
        Some(IntUnary(IntUnaryOp::CtLz))
    } else if int_unary_template(statements, SemanticIntUnaryOp::CtTz) {
        Some(IntUnary(IntUnaryOp::CtTz))
    } else if int_unary_template(statements, SemanticIntUnaryOp::Abs) {
        Some(IntUnary(IntUnaryOp::Abs))
    } else if int_unary_template(statements, SemanticIntUnaryOp::BSwap) {
        Some(IntUnary(IntUnaryOp::BSwap))
    } else if int_unary_template(statements, SemanticIntUnaryOp::BitReverse) {
        Some(IntUnary(IntUnaryOp::BitReverse))
    } else if int_ternary_template(statements, SemanticIntTernaryOp::FShl) {
        Some(IntTernary(IntTernaryOp::FShl))
    } else if int_ternary_template(statements, SemanticIntTernaryOp::FShr) {
        Some(IntTernary(IntTernaryOp::FShr))
    } else if matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::FloatCompare {
            pred: Box::new(operand("pred")),
            lhs: Box::new(reg("lhs")),
            rhs: Box::new(reg("rhs")),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
    {
        Some(Fcmp)
    } else if matches_assign_reg_expr(statements, "dst", &zero_extend()) && pc_next(statements) {
        Some(Cast(ZExt))
    } else if matches_assign_reg_expr(statements, "dst", &sign_extend_three_arg()) && pc_next(statements) {
        Some(Cast(SExt))
    } else if matches_assign_reg_expr(statements, "dst", &trunc_width(reg("src"), operand("to_width")))
        && pc_next(statements)
    {
        Some(Cast(Trunc))
    } else if matches_assign_reg_expr(statements, "dst", &bitcast_width()) && pc_next(statements) {
        Some(Cast(Bitcast))
    } else if matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::StackAlloc {
            bytes: Box::new(operand("bytes")),
            align: Box::new(operand("align")),
        },
    ) && pc_next(statements)
    {
        Some(Alloca)
    } else if matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::StackAllocDynamic {
            count: Box::new(reg("count")),
            elem_size: Box::new(operand("elem_size")),
            align: Box::new(operand("align")),
        },
    ) && pc_next(statements)
    {
        Some(DynamicAlloca)
    } else if matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::LoadWidth {
            ptr: Box::new(reg("ptr")),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
    {
        Some(Load)
    } else if matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::VolatileLoadWidth {
            ptr: Box::new(reg("ptr")),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
    {
        Some(VolatileLoad)
    } else if matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::AtomicLoadWidth {
            ptr: Box::new(reg("ptr")),
            width: Box::new(operand("width")),
            ordering: Box::new(operand("ordering")),
        },
    ) && pc_next(statements)
    {
        Some(AtomicLoad)
    } else if matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::VolatileAtomicLoadWidth {
            ptr: Box::new(reg("ptr")),
            width: Box::new(operand("width")),
            ordering: Box::new(operand("ordering")),
        },
    ) && pc_next(statements)
    {
        Some(VolatileAtomicLoad)
    } else if store_template(statements) {
        Some(Store)
    } else if volatile_store_template(statements) {
        Some(VolatileStore)
    } else if atomic_store_template(statements) {
        Some(AtomicStore)
    } else if volatile_atomic_store_template(statements) {
        Some(VolatileAtomicStore)
    } else if memcpy_dynamic_template(statements) {
        Some(MemcpyDynamic)
    } else if memmove_dynamic_template(statements) {
        Some(MemmoveDynamic)
    } else if memset_dynamic_template(statements) {
        Some(MemsetDynamic)
    } else if volatile_memcpy_dynamic_template(statements) {
        Some(VolatileMemcpyDynamic)
    } else if volatile_memmove_dynamic_template(statements) {
        Some(VolatileMemmoveDynamic)
    } else if volatile_memset_dynamic_template(statements) {
        Some(VolatileMemsetDynamic)
    } else if let Some(op) = atomic_rmw_template(statements, false) {
        Some(AtomicRmw(op))
    } else if let Some(op) = atomic_rmw_template(statements, true) {
        Some(VolatileAtomicRmw(op))
    } else if cmpxchg_template(statements) {
        Some(CmpXchg)
    } else if volatile_cmpxchg_template(statements) {
        Some(VolatileCmpXchg)
    } else if fence_template(statements) {
        Some(Fence)
    } else if matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::Binary {
            op: SemanticBinOp::Add,
            lhs: Box::new(reg("base")),
            rhs: Box::new(operand("offset")),
        },
    ) && pc_next(statements)
    {
        Some(Gep)
    } else if call_native_template(statements) {
        Some(CallNative)
    } else if unreachable_template(statements) {
        Some(Unreachable)
    } else if trap_template(statements) {
        Some(Trap)
    } else if side_effect_template(statements) {
        Some(SideEffect)
    } else if statements
        .iter()
        .any(|stmt| matches!(stmt, SemanticStmt::StateUnchanged))
        && pc_next(statements)
    {
        Some(Nop)
    } else if matches_assign_reg_expr(statements, "lr", &SemanticExpr::NextPc) && pc_label(statements, "target") {
        Some(VmCall)
    } else if pc_register(statements, "lr") {
        Some(VmRet)
    } else if pc_label(statements, "target") {
        Some(Br)
    } else if pc_select_template(statements) {
        Some(BrCond)
    } else if matches_assign_reg_expr(statements, "ret0", &reg("src")) && pc_return(statements) {
        Some(Ret)
    } else {
        None
    }
}

fn float_bin_template(statements: &[SemanticStmt], op: SemanticFloatBinOp) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::FloatBinary {
            op,
            lhs: Box::new(reg("lhs")),
            rhs: Box::new(reg("rhs")),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
}

fn int_ternary_template(statements: &[SemanticStmt], op: SemanticIntTernaryOp) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::IntTernary {
            op,
            lhs: Box::new(reg("lhs")),
            rhs: Box::new(reg("rhs")),
            third: Box::new(reg("third")),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
}

fn float_unary_template(statements: &[SemanticStmt], op: SemanticFloatUnaryOp) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::FloatUnary {
            op,
            value: Box::new(reg("src")),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
}

fn float_ternary_template(statements: &[SemanticStmt], op: SemanticFloatTernaryOp) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::FloatTernary {
            op,
            lhs: Box::new(reg("lhs")),
            rhs: Box::new(reg("rhs")),
            third: Box::new(reg("third")),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
}

fn float_cast_template(statements: &[SemanticStmt], op: SemanticFloatCastOp) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::FloatCast {
            op,
            value: Box::new(reg("src")),
            from_width: Box::new(operand("from_width")),
            to_width: Box::new(operand("to_width")),
        },
    ) && pc_next(statements)
}

fn float_class_template(statements: &[SemanticStmt]) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::FloatClass {
            value: Box::new(reg("src")),
            mask: Box::new(operand("mask")),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
}

fn analyze_semantic_effect(statements: &[SemanticStmt]) -> Result<HandlerEffect, ProfileError> {
    let mut pc = None;
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    let mut memory_read = false;
    let mut memory_write = false;
    let mut native_call = false;

    for statement in statements {
        match statement {
            SemanticStmt::AssignReg { dst, value } => {
                push_unique(&mut writes, dst.clone());
                if matches!(
                    value,
                    SemanticExpr::AtomicRmw { .. } | SemanticExpr::VolatileAtomicRmw { .. }
                ) {
                    memory_write = true;
                }
                collect_expr_effects(value, &mut reads, &mut memory_read, &mut native_call);
            },
            SemanticStmt::AssignPc { value } => {
                let effect = match value {
                    PcExpr::Next => PcEffect::Next,
                    PcExpr::Return => PcEffect::Return,
                    PcExpr::Label(_) | PcExpr::Register(_) | PcExpr::Select { .. } => PcEffect::Branch,
                };
                collect_pc_expr_reads(value, &mut reads, &mut memory_read, &mut native_call);
                if pc.replace(effect).is_some() {
                    return Err(ProfileError::Invalid(
                        "handler semantic has multiple pc effects".to_owned(),
                    ));
                }
            },
            SemanticStmt::StoreWidth { ptr, value, width } => {
                memory_write = true;
                collect_expr_effects(ptr, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(value, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(width, &mut reads, &mut memory_read, &mut native_call);
            },
            SemanticStmt::VolatileStoreWidth { ptr, value, width } => {
                memory_write = true;
                collect_expr_effects(ptr, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(value, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(width, &mut reads, &mut memory_read, &mut native_call);
            },
            SemanticStmt::AtomicStoreWidth {
                ptr,
                value,
                width,
                ordering,
            } => {
                memory_write = true;
                collect_expr_effects(ptr, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(value, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(width, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(ordering, &mut reads, &mut memory_read, &mut native_call);
            },
            SemanticStmt::VolatileAtomicStoreWidth {
                ptr,
                value,
                width,
                ordering,
            } => {
                memory_write = true;
                collect_expr_effects(ptr, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(value, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(width, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(ordering, &mut reads, &mut memory_read, &mut native_call);
            },
            SemanticStmt::MemcpyDynamic { dst, src, len }
            | SemanticStmt::MemmoveDynamic { dst, src, len }
            | SemanticStmt::VolatileMemcpyDynamic { dst, src, len }
            | SemanticStmt::VolatileMemmoveDynamic { dst, src, len } => {
                memory_read = true;
                memory_write = true;
                collect_expr_effects(dst, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(src, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(len, &mut reads, &mut memory_read, &mut native_call);
            },
            SemanticStmt::MemsetDynamic { dst, value, len }
            | SemanticStmt::VolatileMemsetDynamic { dst, value, len } => {
                memory_write = true;
                collect_expr_effects(dst, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(value, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(len, &mut reads, &mut memory_read, &mut native_call);
            },
            SemanticStmt::CmpXchg {
                old,
                success,
                ptr,
                compare,
                new,
                width,
                success_ordering,
                failure_ordering,
            }
            | SemanticStmt::VolatileCmpXchg {
                old,
                success,
                ptr,
                compare,
                new,
                width,
                success_ordering,
                failure_ordering,
            } => {
                push_unique(&mut writes, old.clone());
                push_unique(&mut writes, success.clone());
                collect_expr_effects(ptr, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(compare, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(new, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(width, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(success_ordering, &mut reads, &mut memory_read, &mut native_call);
                collect_expr_effects(failure_ordering, &mut reads, &mut memory_read, &mut native_call);
                memory_read = true;
                memory_write = true;
            },
            SemanticStmt::Fence { ordering } => {
                collect_expr_effects(ordering, &mut reads, &mut memory_read, &mut native_call);
                memory_read = true;
                memory_write = true;
            },
            SemanticStmt::Unreachable => {
                if pc.replace(PcEffect::Return).is_some() {
                    return Err(ProfileError::Invalid(
                        "handler semantic has multiple pc effects".to_owned(),
                    ));
                }
            },
            SemanticStmt::Trap => {
                if pc.replace(PcEffect::Return).is_some() {
                    return Err(ProfileError::Invalid(
                        "handler semantic has multiple pc effects".to_owned(),
                    ));
                }
                native_call = true;
            },
            SemanticStmt::SideEffect => {
                native_call = true;
            },
            SemanticStmt::StateUnchanged => {},
        }
    }

    Ok(HandlerEffect {
        pc: pc.ok_or_else(|| ProfileError::Invalid("handler semantic must declare pc effect".to_owned()))?,
        register_reads: reads,
        register_writes: writes,
        memory_read,
        memory_write,
        native_call,
    })
}

fn matches_assign_reg_expr(statements: &[SemanticStmt], dst: &str, expected: &SemanticExpr) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignReg { dst: actual, value } if actual == dst && value == expected
        )
    })
}

fn add_xor_template(statements: &[SemanticStmt]) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &trunc_width(
            SemanticExpr::Binary {
                op: SemanticBinOp::Xor,
                lhs: Box::new(SemanticExpr::Binary {
                    op: SemanticBinOp::Add,
                    lhs: Box::new(reg("lhs")),
                    rhs: Box::new(reg("rhs")),
                }),
                rhs: Box::new(reg("xor_rhs")),
            },
            operand("width"),
        ),
    ) && pc_next(statements)
}

fn icmp_br_if_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignPc {
                value: PcExpr::Select {
                    cond,
                    then_pc,
                    else_pc,
                }
            } if cond.as_ref()
                == &SemanticExpr::Compare {
                    pred: Box::new(operand("pred")),
                    lhs: Box::new(reg("lhs")),
                    rhs: Box::new(reg("rhs")),
                    width: Box::new(operand("width")),
                }
                && then_pc == "then_pc"
                && else_pc == "else_pc"
        )
    })
}

fn gep_load_template(statements: &[SemanticStmt]) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::LoadWidth {
            ptr: Box::new(SemanticExpr::Binary {
                op: SemanticBinOp::Add,
                lhs: Box::new(reg("base")),
                rhs: Box::new(operand("offset")),
            }),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
}

fn load_add_template(statements: &[SemanticStmt]) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &trunc_width(
            SemanticExpr::Binary {
                op: SemanticBinOp::Add,
                lhs: Box::new(SemanticExpr::LoadWidth {
                    ptr: Box::new(reg("ptr")),
                    width: Box::new(operand("width")),
                }),
                rhs: Box::new(reg("addend")),
            },
            operand("width"),
        ),
    ) && pc_next(statements)
}

fn unreachable_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| matches!(stmt, SemanticStmt::Unreachable))
}

fn trap_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| matches!(stmt, SemanticStmt::Trap))
}

fn side_effect_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| matches!(stmt, SemanticStmt::SideEffect)) && pc_next(statements)
}

fn bin_template(statements: &[SemanticStmt], op: SemanticBinOp) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &trunc_width(
            SemanticExpr::Binary {
                op,
                lhs: Box::new(reg("lhs")),
                rhs: Box::new(reg("rhs")),
            },
            operand("width"),
        ),
    ) && pc_next(statements)
}

fn int_overflow_template(statements: &[SemanticStmt], op: SemanticIntOverflowOp) -> bool {
    let value_op = match op {
        SemanticIntOverflowOp::UAdd | SemanticIntOverflowOp::SAdd => SemanticBinOp::Add,
        SemanticIntOverflowOp::USub | SemanticIntOverflowOp::SSub => SemanticBinOp::Sub,
        SemanticIntOverflowOp::UMul | SemanticIntOverflowOp::SMul => SemanticBinOp::Mul,
    };
    matches_assign_reg_expr(
        statements,
        "dst",
        &trunc_width(
            SemanticExpr::Binary {
                op: value_op,
                lhs: Box::new(reg("lhs")),
                rhs: Box::new(reg("rhs")),
            },
            operand("width"),
        ),
    ) && matches_assign_reg_expr(
        statements,
        "overflow",
        &SemanticExpr::IntOverflow {
            op,
            lhs: Box::new(reg("lhs")),
            rhs: Box::new(reg("rhs")),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
}

fn ashr_template(statements: &[SemanticStmt]) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &trunc_width(
            SemanticExpr::Binary {
                op: SemanticBinOp::AShr,
                lhs: Box::new(SemanticExpr::SignExtend {
                    value: Box::new(reg("lhs")),
                    from_width: Box::new(operand("width")),
                    to_width: None,
                }),
                rhs: Box::new(reg("rhs")),
            },
            operand("width"),
        ),
    ) && pc_next(statements)
}

fn store_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::StoreWidth { ptr, value, width }
                if ptr == &reg("ptr") && value == &reg("src") && width == &operand("width")
        )
    }) && pc_next(statements)
}

fn volatile_store_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::VolatileStoreWidth { ptr, value, width }
                if ptr == &reg("ptr") && value == &reg("src") && width == &operand("width")
        )
    }) && pc_next(statements)
}

fn atomic_store_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AtomicStoreWidth {
                ptr,
                value,
                width,
                ordering,
            } if ptr == &reg("ptr")
                && value == &reg("src")
                && width == &operand("width")
                && ordering == &operand("ordering")
        )
    }) && pc_next(statements)
}

fn volatile_atomic_store_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::VolatileAtomicStoreWidth {
                ptr,
                value,
                width,
                ordering,
            } if ptr == &reg("ptr")
                && value == &reg("src")
                && width == &operand("width")
                && ordering == &operand("ordering")
        )
    }) && pc_next(statements)
}

fn memset_dynamic_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::MemsetDynamic { dst, value, len }
                if dst == &reg("dst") && value == &reg("value") && len == &reg("len")
        )
    }) && pc_next(statements)
}

fn memcpy_dynamic_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::MemcpyDynamic { dst, src, len }
                if dst == &reg("dst") && src == &reg("src") && len == &reg("len")
        )
    }) && pc_next(statements)
}

fn memmove_dynamic_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::MemmoveDynamic { dst, src, len }
                if dst == &reg("dst") && src == &reg("src") && len == &reg("len")
        )
    }) && pc_next(statements)
}

fn volatile_memset_dynamic_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::VolatileMemsetDynamic { dst, value, len }
                if dst == &reg("dst") && value == &reg("value") && len == &reg("len")
        )
    }) && pc_next(statements)
}

fn volatile_memcpy_dynamic_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::VolatileMemcpyDynamic { dst, src, len }
                if dst == &reg("dst") && src == &reg("src") && len == &reg("len")
        )
    }) && pc_next(statements)
}

fn volatile_memmove_dynamic_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::VolatileMemmoveDynamic { dst, src, len }
                if dst == &reg("dst") && src == &reg("src") && len == &reg("len")
        )
    }) && pc_next(statements)
}

fn atomic_rmw_template(statements: &[SemanticStmt], volatile: bool) -> Option<AtomicRmwOp> {
    atomic_rmw_ops().into_iter().find_map(|(semantic_op, runtime_op)| {
        let expected = if volatile {
            SemanticExpr::VolatileAtomicRmw {
                op: semantic_op,
                ptr: Box::new(reg("ptr")),
                value: Box::new(reg("src")),
                width: Box::new(operand("width")),
                ordering: Box::new(operand("ordering")),
            }
        } else {
            SemanticExpr::AtomicRmw {
                op: semantic_op,
                ptr: Box::new(reg("ptr")),
                value: Box::new(reg("src")),
                width: Box::new(operand("width")),
                ordering: Box::new(operand("ordering")),
            }
        };
        (matches_assign_reg_expr(statements, "dst", &expected) && pc_next(statements)).then_some(runtime_op)
    })
}

fn atomic_rmw_ops() -> [(SemanticAtomicRmwOp, AtomicRmwOp); 21] {
    [
        (SemanticAtomicRmwOp::Xchg, AtomicRmwOp::Xchg),
        (SemanticAtomicRmwOp::Add, AtomicRmwOp::Add),
        (SemanticAtomicRmwOp::Sub, AtomicRmwOp::Sub),
        (SemanticAtomicRmwOp::And, AtomicRmwOp::And),
        (SemanticAtomicRmwOp::Or, AtomicRmwOp::Or),
        (SemanticAtomicRmwOp::Xor, AtomicRmwOp::Xor),
        (SemanticAtomicRmwOp::Nand, AtomicRmwOp::Nand),
        (SemanticAtomicRmwOp::Max, AtomicRmwOp::Max),
        (SemanticAtomicRmwOp::Min, AtomicRmwOp::Min),
        (SemanticAtomicRmwOp::UMax, AtomicRmwOp::UMax),
        (SemanticAtomicRmwOp::UMin, AtomicRmwOp::UMin),
        (SemanticAtomicRmwOp::UIncWrap, AtomicRmwOp::UIncWrap),
        (SemanticAtomicRmwOp::UDecWrap, AtomicRmwOp::UDecWrap),
        (SemanticAtomicRmwOp::USubCond, AtomicRmwOp::USubCond),
        (SemanticAtomicRmwOp::USubSat, AtomicRmwOp::USubSat),
        (SemanticAtomicRmwOp::FAdd, AtomicRmwOp::FAdd),
        (SemanticAtomicRmwOp::FSub, AtomicRmwOp::FSub),
        (SemanticAtomicRmwOp::FMax, AtomicRmwOp::FMax),
        (SemanticAtomicRmwOp::FMin, AtomicRmwOp::FMin),
        (SemanticAtomicRmwOp::FMaximum, AtomicRmwOp::FMaximum),
        (SemanticAtomicRmwOp::FMinimum, AtomicRmwOp::FMinimum),
    ]
}

fn int_unary_template(statements: &[SemanticStmt], op: SemanticIntUnaryOp) -> bool {
    matches_assign_reg_expr(
        statements,
        "dst",
        &SemanticExpr::IntUnary {
            op,
            value: Box::new(reg("src")),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
}

fn cmpxchg_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::CmpXchg {
                old,
                success,
                ptr,
                compare,
                new,
                width,
                success_ordering,
                failure_ordering,
            } if old == "old"
                && success == "success"
                && ptr == &reg("ptr")
                && compare == &reg("cmp")
                && new == &reg("new")
                && width == &operand("width")
                && success_ordering == &operand("success_ordering")
                && failure_ordering == &operand("failure_ordering")
        )
    }) && pc_next(statements)
}

fn volatile_cmpxchg_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::VolatileCmpXchg {
                old,
                success,
                ptr,
                compare,
                new,
                width,
                success_ordering,
                failure_ordering,
            } if old == "old"
                && success == "success"
                && ptr == &reg("ptr")
                && compare == &reg("cmp")
                && new == &reg("new")
                && width == &operand("width")
                && success_ordering == &operand("success_ordering")
                && failure_ordering == &operand("failure_ordering")
        )
    }) && pc_next(statements)
}

fn fence_template(statements: &[SemanticStmt]) -> bool {
    statements
        .iter()
        .any(|stmt| matches!(stmt, SemanticStmt::Fence { ordering } if ordering == &operand("ordering")))
        && pc_next(statements)
}

fn call_native_template(statements: &[SemanticStmt]) -> bool {
    let has_call_table_return = statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignReg {
                value: SemanticExpr::CallTableReturn { callee, .. },
                ..
            } if callee == "callee"
        )
    });
    has_call_table_return && pc_next(statements)
}

fn pc_next(statements: &[SemanticStmt]) -> bool {
    statements
        .iter()
        .any(|stmt| matches!(stmt, SemanticStmt::AssignPc { value: PcExpr::Next }))
}

fn pc_return(statements: &[SemanticStmt]) -> bool {
    statements
        .iter()
        .any(|stmt| matches!(stmt, SemanticStmt::AssignPc { value: PcExpr::Return }))
}

fn pc_label(statements: &[SemanticStmt], label: &str) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignPc {
                value: PcExpr::Label(actual)
            } if actual == label
        )
    })
}

fn pc_register(statements: &[SemanticStmt], register: &str) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignPc {
                value: PcExpr::Register(actual)
            } if actual == register
        )
    })
}

fn pc_select_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignPc {
                value: PcExpr::Select {
                    cond,
                    then_pc,
                    else_pc,
                }
            } if cond.as_ref() == &reg("cond") && then_pc == "then_pc" && else_pc == "else_pc"
        )
    })
}

fn operand(name: &str) -> SemanticExpr {
    SemanticExpr::Operand(name.to_owned())
}

fn reg(name: &str) -> SemanticExpr {
    SemanticExpr::Register(name.to_owned())
}

fn trunc_width(value: SemanticExpr, width: SemanticExpr) -> SemanticExpr {
    SemanticExpr::TruncWidth {
        value: Box::new(value),
        width: Box::new(width),
    }
}

fn zero_extend() -> SemanticExpr {
    SemanticExpr::ZeroExtend {
        value: Box::new(reg("src")),
        from_width: Box::new(operand("from_width")),
        to_width: Box::new(operand("to_width")),
    }
}

fn sign_extend_three_arg() -> SemanticExpr {
    SemanticExpr::SignExtend {
        value: Box::new(reg("src")),
        from_width: Box::new(operand("from_width")),
        to_width: Some(Box::new(operand("to_width"))),
    }
}

fn bitcast_width() -> SemanticExpr {
    SemanticExpr::BitcastWidth {
        value: Box::new(reg("src")),
        from_width: Box::new(operand("from_width")),
        to_width: Box::new(operand("to_width")),
    }
}

fn collect_pc_expr_reads(value: &PcExpr, reads: &mut Vec<String>, memory_read: &mut bool, native_call: &mut bool) {
    match value {
        PcExpr::Register(register) => push_unique(reads, register.clone()),
        PcExpr::Select { cond, .. } => collect_expr_effects(cond, reads, memory_read, native_call),
        PcExpr::Next | PcExpr::Return | PcExpr::Label(_) => {},
    }
}

fn collect_expr_effects(expr: &SemanticExpr, reads: &mut Vec<String>, memory_read: &mut bool, native_call: &mut bool) {
    match expr {
        SemanticExpr::Register(register) => push_unique(reads, register.clone()),
        SemanticExpr::TruncWidth { value, width } => {
            collect_expr_effects(value, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::ZeroExtend {
            value,
            from_width,
            to_width,
        }
        | SemanticExpr::BitcastWidth {
            value,
            from_width,
            to_width,
        } => {
            collect_expr_effects(value, reads, memory_read, native_call);
            collect_expr_effects(from_width, reads, memory_read, native_call);
            collect_expr_effects(to_width, reads, memory_read, native_call);
        },
        SemanticExpr::SignExtend {
            value,
            from_width,
            to_width,
        } => {
            collect_expr_effects(value, reads, memory_read, native_call);
            collect_expr_effects(from_width, reads, memory_read, native_call);
            if let Some(to_width) = to_width {
                collect_expr_effects(to_width, reads, memory_read, native_call);
            }
        },
        SemanticExpr::Binary { lhs, rhs, .. } => {
            collect_expr_effects(lhs, reads, memory_read, native_call);
            collect_expr_effects(rhs, reads, memory_read, native_call);
        },
        SemanticExpr::IntUnary { value, width, .. } => {
            collect_expr_effects(value, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::IntTernary {
            lhs, rhs, third, width, ..
        } => {
            collect_expr_effects(lhs, reads, memory_read, native_call);
            collect_expr_effects(rhs, reads, memory_read, native_call);
            collect_expr_effects(third, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::IntOverflow { lhs, rhs, width, .. } => {
            collect_expr_effects(lhs, reads, memory_read, native_call);
            collect_expr_effects(rhs, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::Compare { pred, lhs, rhs, width } => {
            collect_expr_effects(pred, reads, memory_read, native_call);
            collect_expr_effects(lhs, reads, memory_read, native_call);
            collect_expr_effects(rhs, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::FloatBinary { lhs, rhs, width, .. } => {
            collect_expr_effects(lhs, reads, memory_read, native_call);
            collect_expr_effects(rhs, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::FloatUnary { value, width, .. } => {
            collect_expr_effects(value, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::FloatTernary {
            lhs, rhs, third, width, ..
        } => {
            collect_expr_effects(lhs, reads, memory_read, native_call);
            collect_expr_effects(rhs, reads, memory_read, native_call);
            collect_expr_effects(third, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::FloatCast {
            value,
            from_width,
            to_width,
            ..
        } => {
            collect_expr_effects(value, reads, memory_read, native_call);
            collect_expr_effects(from_width, reads, memory_read, native_call);
            collect_expr_effects(to_width, reads, memory_read, native_call);
        },
        SemanticExpr::FloatCompare { pred, lhs, rhs, width } => {
            collect_expr_effects(pred, reads, memory_read, native_call);
            collect_expr_effects(lhs, reads, memory_read, native_call);
            collect_expr_effects(rhs, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::FloatClass { value, mask, width } => {
            collect_expr_effects(value, reads, memory_read, native_call);
            collect_expr_effects(mask, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::Select {
            cond,
            then_value,
            else_value,
        } => {
            collect_expr_effects(cond, reads, memory_read, native_call);
            collect_expr_effects(then_value, reads, memory_read, native_call);
            collect_expr_effects(else_value, reads, memory_read, native_call);
        },
        SemanticExpr::ReadCounter { .. } => {
            *native_call = true;
        },
        SemanticExpr::StackAlloc { bytes, align } => {
            collect_expr_effects(bytes, reads, memory_read, native_call);
            collect_expr_effects(align, reads, memory_read, native_call);
        },
        SemanticExpr::StackAllocDynamic {
            count,
            elem_size,
            align,
        } => {
            collect_expr_effects(count, reads, memory_read, native_call);
            collect_expr_effects(elem_size, reads, memory_read, native_call);
            collect_expr_effects(align, reads, memory_read, native_call);
        },
        SemanticExpr::LoadWidth { ptr, width } => {
            *memory_read = true;
            collect_expr_effects(ptr, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::VolatileLoadWidth { ptr, width } => {
            *memory_read = true;
            collect_expr_effects(ptr, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
        },
        SemanticExpr::AtomicLoadWidth { ptr, width, ordering } => {
            *memory_read = true;
            collect_expr_effects(ptr, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
            collect_expr_effects(ordering, reads, memory_read, native_call);
        },
        SemanticExpr::VolatileAtomicLoadWidth { ptr, width, ordering } => {
            *memory_read = true;
            collect_expr_effects(ptr, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
            collect_expr_effects(ordering, reads, memory_read, native_call);
        },
        SemanticExpr::AtomicRmw {
            ptr,
            value,
            width,
            ordering,
            ..
        }
        | SemanticExpr::VolatileAtomicRmw {
            ptr,
            value,
            width,
            ordering,
            ..
        } => {
            *memory_read = true;
            collect_expr_effects(ptr, reads, memory_read, native_call);
            collect_expr_effects(value, reads, memory_read, native_call);
            collect_expr_effects(width, reads, memory_read, native_call);
            collect_expr_effects(ordering, reads, memory_read, native_call);
        },
        SemanticExpr::CallTableReturn { args, .. } => {
            *native_call = true;
            for arg in args {
                collect_expr_effects(arg, reads, memory_read, native_call);
            }
        },
        SemanticExpr::Operand(_) | SemanticExpr::ConstPool(_) | SemanticExpr::NextPc => {},
    }
}

fn collect_q_register_refs(input: &str, out: &mut Vec<String>) {
    for token in input.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')) {
        let Some(index) = token.strip_prefix('q').and_then(|value| value.parse::<u16>().ok()) else {
            continue;
        };
        push_unique(out, format!("q{index}"));
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|seen| seen == &value) {
        values.push(value);
    }
}

fn parse_rotate_amount(amount: &str, line: &str) -> Result<u8, ProfileError> {
    amount
        .trim()
        .parse::<u8>()
        .map_err(|_| ProfileError::Invalid(format!("invalid decoder rotate amount in line: {line}")))
}

fn parse_register_bank(line: &str) -> Result<Option<RegisterBank>, ProfileError> {
    let Some(rest) = line.strip_prefix("bank ") else {
        return Ok(None);
    };
    let mut parts = rest.split_whitespace();
    let name = parts
        .next()
        .ok_or_else(|| ProfileError::Invalid(format!("invalid runtime bank line: {line}")))?;
    let range_keyword = parts
        .next()
        .ok_or_else(|| ProfileError::Invalid(format!("invalid runtime bank line: {line}")))?;
    let range = parts
        .next()
        .ok_or_else(|| ProfileError::Invalid(format!("invalid runtime bank line: {line}")))?;
    let type_keyword = parts
        .next()
        .ok_or_else(|| ProfileError::Invalid(format!("invalid runtime bank line: {line}")))?;
    let value_type = parts
        .next()
        .ok_or_else(|| ProfileError::Invalid(format!("invalid runtime bank line: {line}")))?;

    if range_keyword != "range" || type_keyword != "type" {
        return Err(ProfileError::Invalid(format!("invalid runtime bank line: {line}")));
    }

    let (first, last) = parse_register_range(name, range)?;
    Ok(Some(RegisterBank {
        name: name.to_owned(),
        first,
        last,
        value_type: value_type.to_owned(),
    }))
}

fn parse_runtime_enhancement(runtime: &mut RuntimeProfile, line: &str) -> Result<(), ProfileError> {
    let Some((name, value)) = line.split_once('=') else {
        return Err(ProfileError::Invalid(format!(
            "invalid runtime enhancement line: {line}"
        )));
    };
    let name = name.trim();
    let value = value.trim();

    match name {
        "threaded_dispatch" => runtime.enhancements.threaded_dispatch = parse_enabled_value(value, name)?,
        "indirect_branch_dispatch" => {
            runtime.enhancements.indirect_branch_dispatch = parse_enabled_value(value, name)?;
        },
        "handler_splitting" => runtime.enhancements.handler_splitting = parse_enabled_value(value, name)?,
        "handler_order_shuffle" => runtime.enhancements.handler_order_shuffle = parse_enabled_value(value, name)?,
        "opcode_alias" => runtime.enhancements.opcode_alias = parse_enabled_value(value, name)?,
        "handler_clone" => {
            runtime.enhancements.handler_clone = match value {
                "disabled" => HandlerClonePolicy::Disabled,
                "func" => HandlerClonePolicy::PerFunction,
                other => {
                    return Err(ProfileError::Invalid(format!(
                        "unsupported handler_clone policy {other}"
                    )));
                },
            };
        },
        other => {
            return Err(ProfileError::Invalid(format!(
                "unsupported runtime enhancement {other}"
            )));
        },
    }

    Ok(())
}

fn parse_enabled_value(value: &str, name: &str) -> Result<bool, ProfileError> {
    match value {
        "enabled" => Ok(true),
        "disabled" => Ok(false),
        other => Err(ProfileError::Invalid(format!("{name} has invalid value {other}"))),
    }
}

fn parse_register_range(bank: &str, range: &str) -> Result<(u8, u8), ProfileError> {
    let Some((first, last)) = range.split_once("..") else {
        return Err(ProfileError::Invalid(format!("invalid register range {range}")));
    };
    let first = parse_bank_register(bank, first)?;
    let last = parse_bank_register(bank, last)?;
    Ok((first, last))
}

fn parse_bank_register(bank: &str, register: &str) -> Result<u8, ProfileError> {
    register
        .strip_prefix(bank)
        .and_then(|index| index.parse::<u8>().ok())
        .ok_or_else(|| ProfileError::Invalid(format!("invalid {bank} register {register}")))
}

fn parse_control_state_slot(line: &str) -> Option<ControlStateSlot> {
    let (name, value_type) = line.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || name.contains(' ') {
        return None;
    }
    Some(ControlStateSlot {
        name: name.to_owned(),
        value_type: value_type.trim().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::VmRegister;
    use crate::verify::verify_profile;

    #[test]
    fn builtin_profile_loads() {
        let profile = ProfilePackage::builtin_test().expect("built-in profile should parse");

        assert_eq!(profile.runtime.scope, RuntimeScope::Module);
        assert_eq!(profile.bytecode.scope, RuntimeScope::Func);
        assert_eq!(profile.runtime.polymorph_scope, RuntimeScope::Func);
        assert!(profile.runtime.aliases.contains_key("lr"));
        assert_eq!(profile.runtime.q_lowering, WideRegisterPolicy::Disabled);
        assert_eq!(profile.runtime.banks.len(), 2);
        assert!(
            profile
                .runtime
                .control_state
                .iter()
                .any(|slot| slot.name == "pc" && slot.value_type == "label")
        );
        assert!(profile.isa.has_unique_opcodes());
        assert_eq!(profile.abi.vm_call_args, (0..8).map(VmRegister::X).collect::<Vec<_>>());
        assert_eq!(
            profile.abi.vm_call_returns,
            [VmRegister::X(0), VmRegister::X(1), VmRegister::X(2)]
        );
        assert_eq!(profile.abi.lr_alias, "lr");
        assert_eq!(profile.abi.ret_pc_alias, "lr");
        assert!(profile.abi.call_link_declared);
        assert!(profile.abi.ret_pc_declared);
        assert_eq!(profile.abi.native_args, (0..8).map(VmRegister::X).collect::<Vec<_>>());
        assert_eq!(
            profile.abi.native_returns,
            [VmRegister::X(0), VmRegister::X(1), VmRegister::X(2)]
        );
        assert_eq!(
            profile.abi.native_clobbers,
            (0..16).map(VmRegister::X).collect::<Vec<_>>()
        );
        assert_eq!(profile.abi.native_policy, NativeCallPolicy::Direct);
        assert_eq!(profile.bytecode.segment("header").unwrap().mode, SegmentMode::Fixed);
        assert_eq!(profile.bytecode.segment("const_pool").unwrap().mode, SegmentMode::Fixed);
        assert_eq!(profile.bytecode.segment("code").unwrap().mode, SegmentMode::Compressed);
        assert_eq!(profile.bytecode.segment("reloc").unwrap().mode, SegmentMode::Fixed);
        assert_eq!(
            profile.bytecode.instruction_record.opcode,
            OpcodeEncoding::VarintEncrypted
        );
        assert_eq!(
            profile.bytecode.instruction_record.operands,
            OperandEncoding::Bitpack {
                schema: "operand_stream".to_owned()
            }
        );
        assert_eq!(
            profile.bytecode.instruction_record.decoded_widths,
            [4, 8, 16, 32, 48, 64]
        );
        assert_eq!(profile.bytecode.instruction_record.default_decoded_width, 32);
        assert_eq!(
            profile.bytecode.relocation("label_pc").unwrap().width,
            RelocWidth::Varint
        );
        assert_eq!(
            profile.bytecode.relocation("label_pc").unwrap().base,
            RelocBase::CodeStart
        );
        assert_eq!(
            profile.bytecode.const_pool.encryption,
            ConstPoolEncryption::XorStreamFunctionKey
        );
        assert_eq!(
            profile.bytecode.fake_instruction,
            FakeInstructionProfile {
                enabled: true,
                count: 1
            }
        );
        assert_eq!(
            profile.bytecode.dead_bytecode,
            DeadBytecodeProfile {
                enabled: true,
                count: 2
            }
        );
        assert!(!profile.runtime.enhancements.threaded_dispatch);
        assert!(!profile.runtime.enhancements.indirect_branch_dispatch);
        assert!(profile.runtime.enhancements.handler_splitting);
        assert!(profile.runtime.enhancements.handler_order_shuffle);
        assert!(profile.runtime.enhancements.opcode_alias);
        assert_eq!(profile.runtime.enhancements.handler_clone, HandlerClonePolicy::Disabled);
        assert!(
            profile
                .lowering
                .rule_by_match(lowering_match_pattern("llvm.ssa.copy.scalar").unwrap())
                .is_some()
        );
        assert_eq!(
            profile
                .lowering
                .rule("llvm.objectsize.integer")
                .expect("built-in profile should declare objectsize lowering")
                .emitted_instructions,
            ["mov_imm"]
        );
        assert_eq!(
            profile
                .isa
                .instructions
                .iter()
                .map(|instruction| instruction.opcodes().len())
                .sum::<usize>(),
            439
        );
        let read_cycle = profile
            .isa
            .by_semantic(&HandlerSemantic::ReadCounter(CounterKind::Cycle))
            .unwrap();
        assert_eq!(read_cycle.name, "read_cycle");
        let read_steady = profile
            .isa
            .by_semantic(&HandlerSemantic::ReadCounter(CounterKind::Steady))
            .unwrap();
        assert_eq!(read_steady.name, "read_steady");
        let sideeffect = profile.isa.by_semantic(&HandlerSemantic::SideEffect).unwrap();
        assert_eq!(sideeffect.name, "sideeffect");
        assert!(sideeffect.effect.native_call);
        let ctpop = profile
            .isa
            .by_semantic(&HandlerSemantic::IntUnary(IntUnaryOp::CtPop))
            .unwrap();
        assert_eq!(ctpop.opcodes().len(), 2);
        assert_eq!(ctpop.effect.register_reads, ["src"]);
        assert_eq!(ctpop.effect.register_writes, ["dst"]);
        let ctlz = profile
            .isa
            .by_semantic(&HandlerSemantic::IntUnary(IntUnaryOp::CtLz))
            .unwrap();
        assert_eq!(ctlz.opcodes().len(), 2);
        assert_eq!(ctlz.effect.register_reads, ["src"]);
        assert_eq!(ctlz.effect.register_writes, ["dst"]);
        let cttz = profile
            .isa
            .by_semantic(&HandlerSemantic::IntUnary(IntUnaryOp::CtTz))
            .unwrap();
        assert_eq!(cttz.opcodes().len(), 2);
        assert_eq!(cttz.effect.register_reads, ["src"]);
        assert_eq!(cttz.effect.register_writes, ["dst"]);
        let iabs = profile
            .isa
            .by_semantic(&HandlerSemantic::IntUnary(IntUnaryOp::Abs))
            .unwrap();
        assert_eq!(iabs.opcodes().len(), 2);
        assert_eq!(iabs.effect.register_reads, ["src"]);
        assert_eq!(iabs.effect.register_writes, ["dst"]);
        let ismax = profile.isa.by_semantic(&HandlerSemantic::Bin(BinOp::SMax)).unwrap();
        assert_eq!(ismax.opcodes().len(), 2);
        assert_eq!(ismax.effect.register_reads, ["lhs", "rhs"]);
        assert_eq!(ismax.effect.register_writes, ["dst"]);
        let iuadd_sat = profile.isa.by_semantic(&HandlerSemantic::Bin(BinOp::UAddSat)).unwrap();
        assert_eq!(iuadd_sat.opcodes().len(), 2);
        assert_eq!(iuadd_sat.effect.register_reads, ["lhs", "rhs"]);
        assert_eq!(iuadd_sat.effect.register_writes, ["dst"]);
        let iushl_sat = profile.isa.by_semantic(&HandlerSemantic::Bin(BinOp::UShlSat)).unwrap();
        assert_eq!(iushl_sat.opcodes().len(), 2);
        assert_eq!(iushl_sat.effect.register_reads, ["lhs", "rhs"]);
        assert_eq!(iushl_sat.effect.register_writes, ["dst"]);
        let iumul_overflow = profile
            .isa
            .by_semantic(&HandlerSemantic::IntOverflow(IntOverflowOp::UMul))
            .unwrap();
        assert_eq!(iumul_overflow.opcodes().len(), 2);
        assert_eq!(iumul_overflow.effect.register_reads, ["lhs", "rhs"]);
        assert_eq!(iumul_overflow.effect.register_writes, ["dst", "overflow"]);
        let ismul_overflow = profile
            .isa
            .by_semantic(&HandlerSemantic::IntOverflow(IntOverflowOp::SMul))
            .unwrap();
        assert_eq!(ismul_overflow.opcodes().len(), 2);
        assert_eq!(ismul_overflow.effect.register_reads, ["lhs", "rhs"]);
        assert_eq!(ismul_overflow.effect.register_writes, ["dst", "overflow"]);
        let fshl = profile
            .isa
            .by_semantic(&HandlerSemantic::IntTernary(IntTernaryOp::FShl))
            .unwrap();
        assert_eq!(fshl.opcodes().len(), 2);
        assert_eq!(fshl.decoded_width, 48);
        assert_eq!(fshl.effect.register_reads, ["lhs", "rhs", "third"]);
        assert_eq!(fshl.effect.register_writes, ["dst"]);
        let fcopysign = profile
            .isa
            .by_semantic(&HandlerSemantic::FloatBin(FloatBinOp::CopySign))
            .unwrap();
        assert_eq!(fcopysign.opcodes().len(), 2);
        assert_eq!(fcopysign.effect.register_reads, ["lhs", "rhs"]);
        assert_eq!(fcopysign.effect.register_writes, ["dst"]);
        let fminnum = profile
            .isa
            .by_semantic(&HandlerSemantic::FloatBin(FloatBinOp::MinNum))
            .unwrap();
        assert_eq!(fminnum.opcodes().len(), 2);
        assert_eq!(fminnum.effect.register_reads, ["lhs", "rhs"]);
        assert_eq!(fminnum.effect.register_writes, ["dst"]);
        let fmaxnum = profile
            .isa
            .by_semantic(&HandlerSemantic::FloatBin(FloatBinOp::MaxNum))
            .unwrap();
        assert_eq!(fmaxnum.opcodes().len(), 2);
        assert_eq!(fmaxnum.effect.register_reads, ["lhs", "rhs"]);
        assert_eq!(fmaxnum.effect.register_writes, ["dst"]);
        let fminimum = profile
            .isa
            .by_semantic(&HandlerSemantic::FloatBin(FloatBinOp::Minimum))
            .unwrap();
        assert_eq!(fminimum.opcodes().len(), 2);
        assert_eq!(fminimum.effect.register_reads, ["lhs", "rhs"]);
        assert_eq!(fminimum.effect.register_writes, ["dst"]);
        let fmaximum = profile
            .isa
            .by_semantic(&HandlerSemantic::FloatBin(FloatBinOp::Maximum))
            .unwrap();
        assert_eq!(fmaximum.opcodes().len(), 2);
        assert_eq!(fmaximum.effect.register_reads, ["lhs", "rhs"]);
        assert_eq!(fmaximum.effect.register_writes, ["dst"]);
        let fsqrt = profile
            .isa
            .by_semantic(&HandlerSemantic::FloatUnary(FloatUnaryOp::Sqrt))
            .unwrap();
        assert_eq!(fsqrt.opcodes().len(), 2);
        assert_eq!(fsqrt.effect.register_reads, ["src"]);
        assert_eq!(fsqrt.effect.register_writes, ["dst"]);
        let fcanonicalize = profile
            .isa
            .by_semantic(&HandlerSemantic::FloatUnary(FloatUnaryOp::Canonicalize))
            .unwrap();
        assert_eq!(fcanonicalize.opcodes().len(), 2);
        assert_eq!(fcanonicalize.effect.register_reads, ["src"]);
        assert_eq!(fcanonicalize.effect.register_writes, ["dst"]);
        for semantic in [
            FloatUnaryOp::Floor,
            FloatUnaryOp::Ceil,
            FloatUnaryOp::Trunc,
            FloatUnaryOp::Rint,
            FloatUnaryOp::NearbyInt,
            FloatUnaryOp::Round,
            FloatUnaryOp::RoundEven,
        ] {
            let instruction = profile.isa.by_semantic(&HandlerSemantic::FloatUnary(semantic)).unwrap();
            assert_eq!(instruction.opcodes().len(), 2);
            assert_eq!(instruction.effect.register_reads, ["src"]);
            assert_eq!(instruction.effect.register_writes, ["dst"]);
        }
        let ffma = profile
            .isa
            .by_semantic(&HandlerSemantic::FloatTernary(FloatTernaryOp::Fma))
            .unwrap();
        assert_eq!(ffma.opcodes().len(), 2);
        assert_eq!(ffma.decoded_width, 48);
        assert_eq!(ffma.effect.register_reads, ["lhs", "rhs", "third"]);
        assert_eq!(ffma.effect.register_writes, ["dst"]);
        let ffmuladd = profile
            .isa
            .by_semantic(&HandlerSemantic::FloatTernary(FloatTernaryOp::MulAdd))
            .unwrap();
        assert_eq!(ffmuladd.opcodes().len(), 2);
        assert_eq!(ffmuladd.decoded_width, 48);
        assert_eq!(ffmuladd.effect.register_reads, ["lhs", "rhs", "third"]);
        assert_eq!(ffmuladd.effect.register_writes, ["dst"]);
        let iadd_xor = profile
            .isa
            .by_semantic(&HandlerSemantic::Super(SuperOp::AddXor))
            .unwrap();
        assert_eq!(iadd_xor.opcodes().len(), 2);
        assert_eq!(iadd_xor.decoded_width, 48);
        assert_eq!(iadd_xor.effect.register_reads, ["lhs", "rhs", "xor_rhs"]);
        assert_eq!(iadd_xor.effect.register_writes, ["dst"]);
        let icmp_br_if = profile
            .isa
            .by_semantic(&HandlerSemantic::Super(SuperOp::IcmpBrIf))
            .unwrap();
        assert_eq!(icmp_br_if.opcodes().len(), 2);
        assert_eq!(icmp_br_if.decoded_width, 32);
        assert_eq!(icmp_br_if.effect.register_reads, ["lhs", "rhs"]);
        assert!(icmp_br_if.effect.register_writes.is_empty());
        let gep_load = profile
            .isa
            .by_semantic(&HandlerSemantic::Super(SuperOp::GepLoad))
            .unwrap();
        assert_eq!(gep_load.opcodes().len(), 2);
        assert_eq!(gep_load.decoded_width, 32);
        assert_eq!(gep_load.effect.register_reads, ["base"]);
        assert_eq!(gep_load.effect.register_writes, ["dst"]);
        assert!(gep_load.effect.memory_read);
        let load_iadd = profile
            .isa
            .by_semantic(&HandlerSemantic::Super(SuperOp::LoadAdd))
            .unwrap();
        assert_eq!(load_iadd.opcodes().len(), 2);
        assert_eq!(load_iadd.decoded_width, 32);
        assert_eq!(load_iadd.effect.register_reads, ["ptr", "addend"]);
        assert_eq!(load_iadd.effect.register_writes, ["dst"]);
        assert!(load_iadd.effect.memory_read);
        let add_xor_fusion = profile
            .lowering
            .fusion_for_target("iadd_xor")
            .expect("built-in profile should declare iadd_xor fusion");
        assert_eq!(add_xor_fusion.sequence, ["iadd", "ixor"]);
        assert_eq!(
            add_xor_fusion.requirements,
            ["adjacent", "no_label_between", "temp_single_use", "same_width"]
        );
        let icmp_fusion = profile
            .lowering
            .fusion_for_target("icmp_br_if")
            .expect("built-in profile should declare icmp_br_if fusion");
        assert_eq!(icmp_fusion.sequence, ["icmp", "br_if"]);
        let gep_fusion = profile
            .lowering
            .fusion_for_target("gep_load")
            .expect("built-in profile should declare gep_load fusion");
        assert_eq!(gep_fusion.sequence, ["gep", "load"]);
        let load_add_fusion = profile
            .lowering
            .fusion_for_target("load_iadd")
            .expect("built-in profile should declare load_iadd fusion");
        assert_eq!(load_add_fusion.sequence, ["load", "iadd"]);
        let atomic_umax = profile
            .isa
            .by_semantic(&HandlerSemantic::AtomicRmw(AtomicRmwOp::UMax))
            .unwrap();
        assert_eq!(atomic_umax.opcodes().len(), 2);
        assert!(atomic_umax.effect.memory_read);
        assert!(atomic_umax.effect.memory_write);
        let volatile_atomic_load = profile.isa.by_semantic(&HandlerSemantic::VolatileAtomicLoad).unwrap();
        assert_eq!(volatile_atomic_load.opcodes().len(), 2);
        assert_eq!(volatile_atomic_load.effect.register_reads, ["ptr"]);
        assert_eq!(volatile_atomic_load.effect.register_writes, ["dst"]);
        assert!(volatile_atomic_load.effect.memory_read);
        let volatile_atomic_store = profile.isa.by_semantic(&HandlerSemantic::VolatileAtomicStore).unwrap();
        assert_eq!(volatile_atomic_store.opcodes().len(), 2);
        assert_eq!(volatile_atomic_store.effect.register_reads, ["ptr", "src"]);
        assert!(volatile_atomic_store.effect.register_writes.is_empty());
        assert!(volatile_atomic_store.effect.memory_write);
        for semantic in [
            HandlerSemantic::VolatileMemcpyDynamic,
            HandlerSemantic::VolatileMemmoveDynamic,
        ] {
            let instruction = profile.isa.by_semantic(&semantic).unwrap();
            assert_eq!(instruction.opcodes().len(), 2);
            assert_eq!(instruction.effect.register_reads, ["dst", "src", "len"]);
            assert!(instruction.effect.register_writes.is_empty());
            assert!(instruction.effect.memory_read);
            assert!(instruction.effect.memory_write);
        }
        let volatile_memset = profile
            .isa
            .by_semantic(&HandlerSemantic::VolatileMemsetDynamic)
            .unwrap();
        assert_eq!(volatile_memset.opcodes().len(), 2);
        assert_eq!(volatile_memset.effect.register_reads, ["dst", "value", "len"]);
        assert!(volatile_memset.effect.register_writes.is_empty());
        assert!(!volatile_memset.effect.memory_read);
        assert!(volatile_memset.effect.memory_write);
        for semantic in [
            AtomicRmwOp::UIncWrap,
            AtomicRmwOp::UDecWrap,
            AtomicRmwOp::USubCond,
            AtomicRmwOp::USubSat,
        ] {
            let instruction = profile.isa.by_semantic(&HandlerSemantic::AtomicRmw(semantic)).unwrap();
            assert_eq!(instruction.opcodes().len(), 2);
            assert_eq!(instruction.effect.register_reads, ["ptr", "src"]);
            assert_eq!(instruction.effect.register_writes, ["dst"]);
            assert!(instruction.effect.memory_read);
            assert!(instruction.effect.memory_write);
        }
        let volatile_atomic_add = profile
            .isa
            .by_semantic(&HandlerSemantic::VolatileAtomicRmw(AtomicRmwOp::Add))
            .unwrap();
        assert_eq!(volatile_atomic_add.opcodes().len(), 2);
        assert_eq!(volatile_atomic_add.effect.register_reads, ["ptr", "src"]);
        assert_eq!(volatile_atomic_add.effect.register_writes, ["dst"]);
        assert!(volatile_atomic_add.effect.memory_read);
        assert!(volatile_atomic_add.effect.memory_write);
        let cmpxchg = profile.isa.by_semantic(&HandlerSemantic::CmpXchg).unwrap();
        assert_eq!(cmpxchg.opcodes().len(), 2);
        assert!(cmpxchg.effect.memory_read);
        assert!(cmpxchg.effect.memory_write);
        let volatile_cmpxchg = profile.isa.by_semantic(&HandlerSemantic::VolatileCmpXchg).unwrap();
        assert_eq!(volatile_cmpxchg.opcodes().len(), 2);
        assert_eq!(volatile_cmpxchg.effect.register_reads, ["ptr", "cmp", "new"]);
        assert_eq!(volatile_cmpxchg.effect.register_writes, ["old", "success"]);
        assert!(volatile_cmpxchg.effect.memory_read);
        assert!(volatile_cmpxchg.effect.memory_write);
        let fence = profile.isa.by_semantic(&HandlerSemantic::Fence).unwrap();
        assert_eq!(fence.opcodes().len(), 2);
        assert!(fence.effect.memory_read);
        assert!(fence.effect.memory_write);
        let unreachable = profile
            .isa
            .by_semantic(&HandlerSemantic::Unreachable)
            .expect("built-in profile should declare unreachable instruction");
        assert_eq!(unreachable.operands, 0);
        assert_eq!(unreachable.effect.pc, PcEffect::Return);
        assert!(unreachable.effect.register_reads.is_empty());
        assert!(unreachable.effect.register_writes.is_empty());
        assert_eq!(
            profile
                .lowering
                .rule("llvm.unreachable")
                .expect("built-in profile should declare unreachable lowering")
                .emitted_instructions,
            ["unreachable"]
        );
        let trap = profile
            .isa
            .by_semantic(&HandlerSemantic::Trap)
            .expect("built-in profile should declare trap instruction");
        assert_eq!(trap.operands, 0);
        assert_eq!(trap.effect.pc, PcEffect::Return);
        assert!(trap.effect.register_reads.is_empty());
        assert!(trap.effect.register_writes.is_empty());
        assert!(trap.effect.native_call);
        assert_eq!(
            profile
                .lowering
                .rule("llvm.trap")
                .expect("built-in profile should declare trap lowering")
                .emitted_instructions,
            ["trap"]
        );
        assert_eq!(profile.isa.by_semantic(&HandlerSemantic::MovImm).unwrap().opcode, 0x01);
        let mov_imm = profile.isa.by_semantic(&HandlerSemantic::MovImm).unwrap();
        assert_eq!(mov_imm.effect.pc, PcEffect::Next);
        assert_eq!(mov_imm.effect.register_reads, Vec::<String>::new());
        assert_eq!(mov_imm.effect.register_writes, ["dst"]);
        let freeze_rule = profile
            .lowering
            .rule("llvm.freeze.scalar")
            .expect("built-in profile should declare freeze lowering");
        assert_eq!(freeze_rule.emitted_instructions, ["mov"]);
        let aggregate_freeze_rule = profile
            .lowering
            .rule("llvm.aggregate.freeze")
            .expect("built-in profile should declare aggregate freeze lowering");
        assert_eq!(aggregate_freeze_rule.emitted_instructions, ["mov"]);
        assert_eq!(
            profile
                .lowering
                .rule("llvm.select.aggregate")
                .expect("built-in profile should declare aggregate select lowering")
                .emitted_instructions,
            ["br_if", "mov", "br", "mov"]
        );
        for rule_name in [
            "llvm.aggregate.insert.subaggregate",
            "llvm.aggregate.extract.subaggregate",
            "llvm.aggregate.phi.edge_move",
        ] {
            assert_eq!(
                profile
                    .lowering
                    .rule(rule_name)
                    .expect("built-in profile should declare subaggregate lowering")
                    .emitted_instructions,
                ["mov"]
            );
        }
        assert_eq!(
            profile
                .lowering
                .rule("llvm.memory.aggregate.load")
                .expect("built-in profile should declare aggregate load lowering")
                .emitted_instructions,
            ["gep", "load", "load", "mov"]
        );
        assert_eq!(
            profile
                .lowering
                .rule("llvm.memory.aggregate.store")
                .expect("built-in profile should declare aggregate store lowering")
                .emitted_instructions,
            ["gep", "store", "store"]
        );
        assert_eq!(
            profile
                .lowering
                .rule("llvm.memory.volatile.aggregate.load")
                .expect("built-in profile should declare volatile aggregate load lowering")
                .emitted_instructions,
            ["gep", "volatile_load", "volatile_load", "mov"]
        );
        assert_eq!(
            profile
                .lowering
                .rule("llvm.memory.volatile.aggregate.store")
                .expect("built-in profile should declare volatile aggregate store lowering")
                .emitted_instructions,
            ["gep", "volatile_store", "volatile_store"]
        );
        let ptrmask = profile
            .isa
            .by_name("ptrmask")
            .expect("built-in profile should declare ptrmask instruction");
        assert_eq!(ptrmask.semantic, HandlerSemantic::Bin(BinOp::And));
        assert_eq!(ptrmask.decoded_width, 16);
        assert_eq!(
            profile
                .lowering
                .rule("llvm.ptrmask.pointer")
                .expect("built-in profile should declare ptrmask lowering")
                .emitted_instructions,
            ["ptrmask"]
        );
        let tls_addr = profile
            .isa
            .by_name("tls_addr")
            .expect("built-in profile should declare tls_addr instruction");
        assert_eq!(tls_addr.semantic, HandlerSemantic::Mov);
        assert_eq!(tls_addr.decoded_width, 8);
        assert_eq!(
            profile
                .lowering
                .rule("llvm.threadlocal.address.pointer")
                .expect("built-in profile should declare threadlocal.address lowering")
                .emitted_instructions,
            ["tls_addr"]
        );
        let global_addr = profile
            .isa
            .by_name("global_addr")
            .expect("built-in profile should declare global_addr instruction");
        assert_eq!(global_addr.semantic, HandlerSemantic::Mov);
        assert_eq!(global_addr.decoded_width, 8);
        assert_eq!(
            profile
                .lowering
                .rule("llvm.global.address.pointer")
                .expect("built-in profile should declare global.address lowering")
                .emitted_instructions,
            ["global_addr"]
        );
        for rule_name in ["llvm.constexpr.ptrtoint", "llvm.constexpr.inttoptr"] {
            let rule = profile
                .lowering
                .rule(rule_name)
                .expect("built-in profile should declare pointer constant expression lowering");
            assert_eq!(rule.emitted_instructions, ["zext", "trunc", "bitcast"]);
        }
        assert_eq!(
            profile
                .lowering
                .rule("llvm.constexpr.integer.binop")
                .expect("built-in profile should declare integer binop constant expression lowering")
                .emitted_instructions,
            [
                "iadd", "isub", "imul", "iudiv", "isdiv", "iurem", "isrem", "ixor", "iand", "ior", "ishl", "ilshr",
                "iashr",
            ]
        );
        assert_eq!(
            profile
                .lowering
                .rule("llvm.cast.bitcast.scalar")
                .expect("built-in profile should declare scalar bitcast lowering")
                .emitted_instructions,
            ["bitcast"]
        );
        assert_eq!(
            profile
                .lowering
                .rule("llvm.constexpr.integer.cast")
                .expect("built-in profile should declare integer cast constant expression lowering")
                .emitted_instructions,
            ["zext", "sext", "trunc", "bitcast"]
        );
        for rule_name in [
            "llvm.launder.invariant.group.pointer",
            "llvm.strip.invariant.group.pointer",
            "llvm.invariant.start.pointer",
            "llvm.annotation.integer",
            "llvm.ptr.annotation.pointer",
        ] {
            let rule = profile
                .lowering
                .rule(rule_name)
                .expect("built-in profile should declare identity intrinsic lowering");
            assert_eq!(rule.emitted_instructions, ["mov"]);
        }
        for rule_name in [
            "llvm.invariant.end",
            "llvm.prefetch",
            "llvm.experimental.noalias.scope.decl",
            "llvm.donothing",
            "llvm.var.annotation",
            "llvm.codeview.annotation",
        ] {
            let rule = profile
                .lowering
                .rule(rule_name)
                .expect("built-in profile should declare annotation nop lowering");
            assert_eq!(rule.emitted_instructions, ["fake_nop"]);
        }
        for rule_name in ["llvm.memcpy.fixed", "llvm.memmove.fixed"] {
            let rule = profile
                .lowering
                .rule(rule_name)
                .expect("built-in profile should declare fixed memory-copy intrinsic lowering");
            assert!(rule.emitted_instructions.contains(&"load".to_owned()));
            assert!(rule.emitted_instructions.contains(&"store".to_owned()));
            assert!(rule.emitted_instructions.contains(&"gep".to_owned()));
        }
        let memset_rule = profile
            .lowering
            .rule("llvm.memset.fixed")
            .expect("built-in profile should declare fixed memset intrinsic lowering");
        assert!(memset_rule.emitted_instructions.contains(&"store".to_owned()));
        assert!(memset_rule.emitted_instructions.contains(&"gep".to_owned()));
        verify_profile(&profile).expect("built-in profile should verify");
    }

    #[test]
    fn verifier_requires_lowering_fusion_for_super_instruction() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let lowering = include_str!("../profiles/amice-simple-vmp/lowering.vm").replace(
            "fusion super.gep_load { # 声明 gep 后紧跟 load 的超级内存读取融合模板\n  target gep_load # 融合后发射 isa.vm 中的 gep_load 指令\n  sequence gep, load # 只允许把连续的 gep 与 load 两条 VM 指令融合\n  require adjacent # 两条源指令必须在线性 VM IR 中相邻\n  require no_label_between # 第二条源指令位置不能是任何 VM label 的目标\n  require temp_single_use # gep 产生的临时指针寄存器只能被紧邻的 load 使用\n} # 结束 gep_load 融合模板\n\n",
            "",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            &lowering,
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse before verifier checks fusion coverage");

        let err = verify_profile(&profile).expect_err("missing super fusion must fail verification");
        assert!(err.to_string().contains("fusion super.gep_load"));
    }

    #[test]
    fn ruoke_profile_loads() {
        let profile_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("profiles").join("ruoke");
        let profile = ProfilePackage::load_from_path(&profile_dir).expect("ruoke profile should parse");

        assert_eq!(profile.manifest.name, "ruoke");
        assert_eq!(profile.runtime.scope, RuntimeScope::Func);
        assert_eq!(profile.runtime.polymorph_scope, RuntimeScope::Func);
        assert_eq!(
            profile.runtime.enhancements.handler_clone,
            HandlerClonePolicy::PerFunction
        );
        assert_eq!(
            profile.bytecode.fake_instruction,
            FakeInstructionProfile {
                enabled: true,
                count: 2
            }
        );
        assert_eq!(
            profile.bytecode.dead_bytecode,
            DeadBytecodeProfile {
                enabled: true,
                count: 4
            }
        );
        assert_eq!(
            profile.decoder.steps,
            [
                DecoderStep::AddStream,
                DecoderStep::XorStream,
                DecoderStep::Rol { amount: 2 },
                DecoderStep::Ror { amount: 5 },
                DecoderStep::VarintDecode,
                DecoderStep::BitUnpack,
            ]
        );
        assert!(profile.isa.has_unique_opcodes());
        assert_eq!(
            profile
                .lowering
                .rule("llvm.freeze.scalar")
                .expect("ruoke profile should declare freeze lowering")
                .emitted_instructions,
            ["mov"]
        );
        assert_eq!(
            profile
                .lowering
                .rule("llvm.aggregate.freeze")
                .expect("ruoke profile should declare aggregate freeze lowering")
                .emitted_instructions,
            ["mov"]
        );
        assert_eq!(
            profile
                .lowering
                .rule("llvm.objectsize.integer")
                .expect("ruoke profile should declare objectsize lowering")
                .emitted_instructions,
            ["mov_imm"]
        );
        assert_eq!(
            profile
                .lowering
                .rule("llvm.select.aggregate")
                .expect("ruoke profile should declare aggregate select lowering")
                .emitted_instructions,
            ["br_if", "mov", "br", "mov"]
        );
        for rule_name in [
            "llvm.aggregate.insert.subaggregate",
            "llvm.aggregate.extract.subaggregate",
            "llvm.aggregate.phi.edge_move",
        ] {
            assert_eq!(
                profile
                    .lowering
                    .rule(rule_name)
                    .expect("ruoke profile should declare subaggregate lowering")
                    .emitted_instructions,
                ["mov"]
            );
        }
        let ptrmask = profile
            .isa
            .by_name("ptrmask")
            .expect("ruoke profile should declare ptrmask instruction");
        assert_eq!(ptrmask.semantic, HandlerSemantic::Bin(BinOp::And));
        assert_eq!(ptrmask.decoded_width, 16);
        assert_eq!(
            profile
                .lowering
                .rule("llvm.ptrmask.pointer")
                .expect("ruoke profile should declare ptrmask lowering")
                .emitted_instructions,
            ["ptrmask"]
        );
        let tls_addr = profile
            .isa
            .by_name("tls_addr")
            .expect("ruoke profile should declare tls_addr instruction");
        assert_eq!(tls_addr.semantic, HandlerSemantic::Mov);
        assert_eq!(tls_addr.decoded_width, 8);
        assert_eq!(
            profile
                .lowering
                .rule("llvm.threadlocal.address.pointer")
                .expect("ruoke profile should declare threadlocal.address lowering")
                .emitted_instructions,
            ["tls_addr"]
        );
        let global_addr = profile
            .isa
            .by_name("global_addr")
            .expect("ruoke profile should declare global_addr instruction");
        assert_eq!(global_addr.semantic, HandlerSemantic::Mov);
        assert_eq!(global_addr.decoded_width, 8);
        assert_eq!(
            profile
                .lowering
                .rule("llvm.global.address.pointer")
                .expect("ruoke profile should declare global.address lowering")
                .emitted_instructions,
            ["global_addr"]
        );
        for rule_name in ["llvm.constexpr.ptrtoint", "llvm.constexpr.inttoptr"] {
            assert_eq!(
                profile
                    .lowering
                    .rule(rule_name)
                    .expect("ruoke profile should declare pointer constant expression lowering")
                    .emitted_instructions,
                ["zext", "trunc", "bitcast"]
            );
        }
        assert_eq!(
            profile
                .lowering
                .rule("llvm.constexpr.integer.binop")
                .expect("ruoke profile should declare integer binop constant expression lowering")
                .emitted_instructions,
            [
                "iadd", "isub", "imul", "iudiv", "isdiv", "iurem", "isrem", "ixor", "iand", "ior", "ishl", "ilshr",
                "iashr",
            ]
        );
        assert_eq!(
            profile
                .lowering
                .rule("llvm.cast.bitcast.scalar")
                .expect("ruoke profile should declare scalar bitcast lowering")
                .emitted_instructions,
            ["bitcast"]
        );
        assert_eq!(
            profile
                .lowering
                .rule("llvm.constexpr.integer.cast")
                .expect("ruoke profile should declare integer cast constant expression lowering")
                .emitted_instructions,
            ["zext", "sext", "trunc", "bitcast"]
        );
        for rule_name in [
            "llvm.launder.invariant.group.pointer",
            "llvm.strip.invariant.group.pointer",
            "llvm.invariant.start.pointer",
            "llvm.annotation.integer",
            "llvm.ptr.annotation.pointer",
        ] {
            assert_eq!(
                profile
                    .lowering
                    .rule(rule_name)
                    .expect("ruoke profile should declare identity intrinsic lowering")
                    .emitted_instructions,
                ["mov"]
            );
        }
        for rule_name in [
            "llvm.invariant.end",
            "llvm.prefetch",
            "llvm.experimental.noalias.scope.decl",
            "llvm.donothing",
            "llvm.var.annotation",
            "llvm.codeview.annotation",
        ] {
            assert_eq!(
                profile
                    .lowering
                    .rule(rule_name)
                    .expect("ruoke profile should declare annotation nop lowering")
                    .emitted_instructions,
                ["fake_nop"]
            );
        }
        for rule_name in ["llvm.memcpy.fixed", "llvm.memmove.fixed", "llvm.memset.fixed"] {
            assert!(
                profile.lowering.rule(rule_name).is_some(),
                "ruoke profile should declare {rule_name}"
            );
        }
        assert_eq!(
            profile
                .isa
                .instructions
                .iter()
                .flat_map(|instruction| instruction.opcodes())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            1000
        );
        assert!(profile.isa.by_opcode(0x3e8).is_some());
        verify_profile(&profile).expect("ruoke profile should verify");
    }

    #[test]
    fn unsupported_runtime_enhancement_enabled_is_rejected() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let runtime = include_str!("../profiles/amice-simple-vmp/runtime.vm").replace(
            "enhance threaded_dispatch = disabled # 当前内置配置包不启用 threaded dispatch",
            "enhance threaded_dispatch = enabled # 故意声明当前 LLVM emitter 尚未实现的 threaded dispatch",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            &runtime,
        )
        .expect("profile should parse enabled threaded dispatch");

        let err = verify_profile(&profile).expect_err("unimplemented dispatch enhancement must be rejected");

        assert!(err.to_string().contains("threaded_dispatch"));
    }

    #[test]
    fn handler_splitting_enhancement_is_supported() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let runtime = include_str!("../profiles/amice-simple-vmp/runtime.vm");
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            runtime,
        )
        .expect("profile should parse enabled handler splitting");

        assert!(profile.runtime.enhancements.handler_splitting);
        verify_profile(&profile).expect("handler splitting is implemented by the LLVM runtime emitter");
    }

    #[test]
    fn handler_clone_per_function_enhancement_is_supported() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let runtime = include_str!("../profiles/amice-simple-vmp/runtime.vm").replace(
            "enhance handler_clone = disabled # 默认模块级 runtime 共享一套分派器，按需测试时再启用函数级克隆",
            "enhance handler_clone = func # 测试函数级 handler clone 语义",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            &runtime,
        )
        .expect("profile should parse handler clone policy");

        assert_eq!(
            profile.runtime.enhancements.handler_clone,
            HandlerClonePolicy::PerFunction
        );
        verify_profile(&profile).expect("handler clone is implemented by function-suffixed runtime emission");
    }

    #[test]
    fn bytecode_module_scope_is_accepted_for_shared_module_blob() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let bytecode = include_str!("../profiles/amice-simple-vmp/bytecode.vm").replace(
            "bytecode.scope = func # 每个被保护函数拥有独立的字节码包和重定位表",
            "bytecode.scope = module # 同一 LLVM Module 内的被保护函数共享一个字节码全局容器",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            &bytecode,
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse module bytecode scope");

        assert_eq!(profile.bytecode.scope, RuntimeScope::Module);
        verify_profile(&profile).expect("module bytecode scope should verify");
    }

    #[test]
    fn polymorph_module_scope_is_accepted_for_profile_keying() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let runtime = include_str!("../profiles/amice-simple-vmp/runtime.vm").replace(
            "polymorph.scope = func # 每个被保护函数独立派生 key、opcode 选择和 handler 克隆后缀",
            "polymorph.scope = module # 模块内所有被保护函数共享 profile 派生的多态密钥",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            &runtime,
        )
        .expect("profile should parse module polymorph scope");

        assert_eq!(profile.runtime.polymorph_scope, RuntimeScope::Module);
        verify_profile(&profile).expect("module polymorph scope should verify");
    }

    #[test]
    fn profile_parsers_reject_unknown_core_dsl_statements() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let runtime = include_str!("../profiles/amice-simple-vmp/runtime.vm").replace(
            "dispatch = switch # 生成的运行时通过 LLVM switch 分派处理器",
            "dispatch = switch # 生成的运行时通过 LLVM switch 分派处理器\nruntime.typo = ignored # 该拼写错误必须被解析器拒绝",
        );

        let err = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            &runtime,
        )
        .expect_err("unknown runtime DSL statements must fail parsing");

        assert!(err.to_string().contains("runtime.typo"));
    }

    #[test]
    fn abi_max_returns_must_be_numeric() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let abi = include_str!("../profiles/amice-simple-vmp/abi.vm").replace(
            "max_returns = 3 # 该简单配置包支持最多三个返回槽",
            "max_returns = many # 故意写错，解析器必须拒绝而不是沿用默认值",
        );

        let err = ProfilePackage::from_sources(
            manifest,
            &abi,
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect_err("invalid ABI numeric fields must fail parsing");

        assert!(err.to_string().contains("max_returns"));
    }

    #[test]
    fn decoded_width_override_must_use_supported_width() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let isa = include_str!("../profiles/amice-simple-vmp/isa.vm").replace(
            "decoded_width = 8 # mov 的 opcode 和三个短操作数固定放入 8 字节 decoded record",
            "decoded_width = 12 # 故意声明非法宽度，解析器必须拒绝",
        );

        let err = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            &isa,
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect_err("unsupported decoded_width must fail parsing");

        assert!(err.to_string().contains("decoded_width"));
    }

    #[test]
    fn verifier_rejects_record_width_too_small_for_schema() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let isa = include_str!("../profiles/amice-simple-vmp/isa.vm").replace(
            "decoded_width = 8 # mov 的 opcode 和三个短操作数固定放入 8 字节 decoded record",
            "decoded_width = 4 # 故意压缩到放不下三个操作数，校验器必须拒绝",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            &isa,
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse before verifier checks capacity");

        let err = verify_profile(&profile).expect_err("narrow decoded_width must fail verification");
        assert!(err.to_string().contains("cannot hold opcode/operands"));
    }

    #[test]
    fn example_profile_effective_lines_have_chinese_comments() {
        for (path, source) in [
            (
                "amice-simple-vmp/manifest.toml",
                include_str!("../profiles/amice-simple-vmp/manifest.toml"),
            ),
            (
                "amice-simple-vmp/abi.vm",
                include_str!("../profiles/amice-simple-vmp/abi.vm"),
            ),
            (
                "amice-simple-vmp/isa.vm",
                include_str!("../profiles/amice-simple-vmp/isa.vm"),
            ),
            (
                "amice-simple-vmp/lowering.vm",
                include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            ),
            (
                "amice-simple-vmp/bytecode.vm",
                include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            ),
            (
                "amice-simple-vmp/decoder.vm",
                include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            ),
            (
                "amice-simple-vmp/runtime.vm",
                include_str!("../profiles/amice-simple-vmp/runtime.vm"),
            ),
            ("ruoke/manifest.toml", include_str!("../profiles/ruoke/manifest.toml")),
            ("ruoke/abi.vm", include_str!("../profiles/ruoke/abi.vm")),
            ("ruoke/isa.vm", include_str!("../profiles/ruoke/isa.vm")),
            ("ruoke/lowering.vm", include_str!("../profiles/ruoke/lowering.vm")),
            ("ruoke/bytecode.vm", include_str!("../profiles/ruoke/bytecode.vm")),
            ("ruoke/decoder.vm", include_str!("../profiles/ruoke/decoder.vm")),
            ("ruoke/runtime.vm", include_str!("../profiles/ruoke/runtime.vm")),
        ] {
            for (line_index, line) in source.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }

                let Some((_, comment)) = line.split_once('#') else {
                    panic!("{path}:{} effective profile line has no comment", line_index + 1);
                };
                assert!(
                    contains_cjk(comment),
                    "{path}:{} profile comment must be Chinese: {comment}",
                    line_index + 1
                );
            }
        }
    }

    #[test]
    fn isa_source_drives_opcode_aliases() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let isa = include_str!("../profiles/amice-simple-vmp/isa.vm").replacen(
            "opcode alias [0x01, 0x0e, 0x4f, 0x60, 0x65]",
            "opcode alias [0x1f2, 0x1f3]",
            1,
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            &isa,
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let mov_imm = profile.isa.by_semantic(&HandlerSemantic::MovImm).unwrap();

        assert_eq!(mov_imm.opcode, 0x1f2);
        assert_eq!(mov_imm.opcodes(), &[0x1f2, 0x1f3]);
        verify_profile(&profile).expect("profile with opcode aliases should verify");
    }

    #[test]
    fn isa_semantic_block_drives_instruction_semantics() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let isa = include_str!("../profiles/amice-simple-vmp/isa.vm").replacen("instr iadd", "instr add_alias", 1);
        let lowering = include_str!("../profiles/amice-simple-vmp/lowering.vm")
            .replace(
                "emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令",
                "emit add_alias dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射改名后的加法指令",
            )
            .replace(
                "emit iadd dst=%vr, lhs=%vb, rhs=%vs, width=64 # 缩放偏移与基址相加",
                "emit add_alias dst=%vr, lhs=%vb, rhs=%vs, width=64 # 缩放偏移与基址相加",
            )
            .replace(
                "emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # add 常量表达式发射 iadd 指令",
                "emit add_alias dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # add 常量表达式发射改名后的加法指令",
            )
            .replace(
                "sequence iadd, ixor # 只允许把连续的 iadd 与 ixor 两条 VM 指令融合",
                "sequence add_alias, ixor # 只允许把连续的改名加法与 ixor 两条 VM 指令融合",
            )
            .replace(
                "sequence load, iadd # 只允许把连续的 load 与 iadd 两条 VM 指令融合",
                "sequence load, add_alias # 只允许把连续的 load 与改名加法两条 VM 指令融合",
            );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            &isa,
            &lowering,
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let add = profile
            .isa
            .by_semantic(&HandlerSemantic::Bin(BinOp::Add))
            .expect("semantic block should identify add");

        assert_eq!(add.name, "add_alias");
        verify_profile(&profile).expect("renamed semantic-driven instruction should verify");
    }

    #[test]
    fn lowering_emit_before_materialize_is_rejected() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let valid_add_rule = r#"rule llvm.add.integer { # 将 LLVM 整数 add 降低为 iadd VM 指令
  match %r = llvm.add integer %a, %b # 匹配任意受支持整数宽度的 LLVM add
  lower { # 开始声明 add 的 lowering 动作
    %va = materialize %a as integer # 将左操作数物化为 VM 整数值
    %vb = materialize %b as integer # 将右操作数物化为 VM 整数值
    %vr = vreg integer # 为 add 结果分配一个 VM x 寄存器
    emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令
    bind %r = %vr # 记录 LLVM 结果到 VM 寄存器的绑定
  } # 结束 add lowering 动作
} # 结束 add 规则"#;
        let invalid_add_rule = r#"rule llvm.add.integer { # 将 LLVM 整数 add 降低为 iadd VM 指令
match %r = llvm.add integer %a, %b # 匹配任意受支持整数宽度的 LLVM add
lower { # 开始声明 add 的 lowering 动作
emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 故意在 materialize 和 vreg 前发射，verifier 必须拒绝
%va = materialize %a as integer # 将左操作数物化为 VM 整数值
%vb = materialize %b as integer # 将右操作数物化为 VM 整数值
%vr = vreg integer # 为 add 结果分配一个 VM x 寄存器
bind %r = %vr # 记录 LLVM 结果到 VM 寄存器的绑定
} # 结束 add lowering 动作
} # 结束 add 规则"#;
        let lowering =
            include_str!("../profiles/amice-simple-vmp/lowering.vm").replace(valid_add_rule, invalid_add_rule);
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            &lowering,
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let err = verify_profile(&profile).expect_err("bad lowering action order should be rejected");

        assert!(err.to_string().contains("emits undefined VM value"));
    }

    #[test]
    fn abi_max_returns_is_profile_driven() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let abi = include_str!("../profiles/amice-simple-vmp/abi.vm")
            .replacen("ret2 <- x2 as i64", "ret2 <- x2 as i64\nret3 <- x3 as i64", 1)
            .replace("ret_values = [x0, x1, x2]", "ret_values = [x0, x1, x2, x3]")
            .replace("max_returns = 3", "max_returns = 4");
        let profile = ProfilePackage::from_sources(
            manifest,
            &abi,
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        assert_eq!(profile.abi.max_returns, 4);
        assert_eq!(profile.abi.integer_returns, &[0, 1, 2, 3]);
        verify_profile(&profile).expect("max_returns should not be hard-coded to one");
    }

    #[test]
    fn vm_call_ret_pc_mapping_must_be_declared() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let abi = include_str!("../profiles/amice-simple-vmp/abi.vm")
            .replace("ret_pc <- lr # VM 返回从 lr 恢复执行位置\n", "");
        let profile = ProfilePackage::from_sources(
            manifest,
            &abi,
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse before verifier checks vm_call ABI");

        let err = verify_profile(&profile).expect_err("ret_pc mapping must be explicit");

        assert!(err.to_string().contains("ret_pc"));
    }

    #[test]
    fn runtime_alias_target_is_profile_driven() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let runtime = include_str!("../profiles/amice-simple-vmp/runtime.vm")
            .replace("alias lr = x30", "alias lr = x29")
            .replace("alias sp = x31", "alias sp = x28");
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            &runtime,
        )
        .expect("profile should parse with custom alias targets");

        verify_profile(&profile).expect("alias targets may move within x0..x31");
    }

    #[test]
    fn duplicate_host_argument_registers_are_rejected() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let abi = include_str!("../profiles/amice-simple-vmp/abi.vm").replace("arg7 -> x7 as i64", "arg7 -> x0 as i64");
        let profile = ProfilePackage::from_sources(
            manifest,
            &abi,
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let err = verify_profile(&profile).expect_err("duplicate host argument registers must be rejected");

        assert!(err.to_string().contains("maps x0 more than once"));
    }

    #[test]
    fn unsupported_native_call_policy_is_rejected() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let abi = include_str!("../profiles/amice-simple-vmp/abi.vm").replace(
            "policy = direct # 原生调用重新生成为直接 LLVM 调用",
            "policy = indirect # 故意声明不支持的原生调用策略",
        );
        let err = ProfilePackage::from_sources(
            manifest,
            &abi,
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect_err("unsupported native call policy must be rejected during profile parsing");

        assert!(err.to_string().contains("native_call policy"));
    }

    #[test]
    fn isa_handler_pc_effect_must_match_semantic_contract() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let isa = include_str!("../profiles/amice-simple-vmp/isa.vm").replacen(
            "pc = next # 执行继续到下一条字节码指令",
            "pc = return # 故意声明错误的 PC 行为",
            1,
        );
        let err = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            &isa,
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect_err("wrong handler pc effect must be rejected while parsing semantic AST");

        assert!(err.to_string().contains("semantic block"));
    }

    #[test]
    fn isa_operand_kind_must_match_semantic_contract() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let isa = include_str!("../profiles/amice-simple-vmp/isa.vm").replace(
            "instr iadd(dst: vreg<i64>, lhs: vreg<i64>, rhs: vreg<i64>, width: imm<u8>)",
            "instr iadd(dst: imm<u8>, lhs: vreg<i64>, rhs: vreg<i64>, width: imm<u8>)",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            &isa,
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse before verifier rejects operand kind mismatch");

        let err = verify_profile(&profile).expect_err("wrong operand kind must be rejected");

        assert!(err.to_string().contains("operand dst"));
        assert!(err.to_string().contains("iadd"));
    }

    #[test]
    fn q_disabled_rejects_abi_q_references() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let abi = include_str!("../profiles/amice-simple-vmp/abi.vm")
            .replace("call_args = [x0..x7]", "call_args = [x0..x7, q0]");
        let profile = ProfilePackage::from_sources(
            manifest,
            &abi,
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let err = verify_profile(&profile).expect_err("disabled q lowering must reject q ABI references");

        assert!(err.to_string().contains("q0"));
        assert!(err.to_string().contains("q.lowering is disabled"));
    }

    #[test]
    fn q_disabled_rejects_host_vector_abi_mapping() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let abi = include_str!("../profiles/amice-simple-vmp/abi.vm").replace(
            "ret1 <- x1 as i64 # 第 1 个返回槽从 x1 读回，用于简单结构体直接返回",
            "ret1 <- x1 as i64 # 第 1 个返回槽从 x1 读回，用于简单结构体直接返回\nvec0 -> q0 as v128 # 故意声明被禁用的向量参数\nvret0 <- q0 as v128 # 故意声明被禁用的向量返回",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            &abi,
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let err = verify_profile(&profile).expect_err("disabled q lowering must reject host vector ABI mappings");

        assert!(err.to_string().contains("host_to_vm vector"));
        assert!(err.to_string().contains("q0"));
    }

    #[test]
    fn q_disabled_rejects_isa_semantic_q_references() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let isa = include_str!("../profiles/amice-simple-vmp/isa.vm").replace(
            "reg[dst] = trunc_width(reg[src], width) # 源寄存器低位复制到目标寄存器并按 width 掩码",
            "reg[dst] = trunc_width(reg[src], width) # 源寄存器低位复制到目标寄存器并按 width 掩码\nreg[q0] = reg[src] # 故意让禁用的 q 寄存器进入 handler 语义",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            &isa,
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse q semantic reference before verifier rejects it");

        let err = verify_profile(&profile).expect_err("disabled q lowering must reject ISA q references");

        assert!(err.to_string().contains("isa.vm instruction"));
        assert!(err.to_string().contains("q0"));
    }

    #[test]
    fn q_disabled_rejects_lowering_q_references() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let lowering = include_str!("../profiles/amice-simple-vmp/lowering.vm").replace(
            "emit mov dst=%vr, src=%vi, width=type_width(%r) # 在前驱边发射 mov 物化 phi 结果",
            "emit mov dst=q0, src=%vi, width=type_width(%r) # 故意让禁用的 q 寄存器进入 lowering 规则",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            &lowering,
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let err = verify_profile(&profile).expect_err("disabled q lowering must reject q lowering references");

        assert!(err.to_string().contains("lowering.vm"));
        assert!(err.to_string().contains("q0"));
    }

    #[test]
    fn lowering_emit_must_reference_profile_isa_instruction() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let lowering = include_str!("../profiles/amice-simple-vmp/lowering.vm").replace(
            "emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令",
            "emit ghost_add dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 故意引用不存在的 ISA 指令",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            &lowering,
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let err = verify_profile(&profile).expect_err("lowering emits must exist in ISA");

        assert!(err.to_string().contains("ghost_add"));
        assert!(err.to_string().contains("isa.vm"));
    }

    #[test]
    fn lowering_bind_must_reference_defined_vm_value() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let lowering = include_str!("../profiles/amice-simple-vmp/lowering.vm").replace(
            "bind %r = %vr # 记录 LLVM 结果到 VM 寄存器的绑定",
            "bind %r = %missing # 故意绑定未定义的 VM 值",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            &lowering,
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let err = verify_profile(&profile).expect_err("bind must reference a defined VM value");

        assert!(err.to_string().contains("%missing"));
        assert!(err.to_string().contains("undefined VM value"));
    }

    #[test]
    fn lowering_result_rule_must_bind_result_value() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let lowering = include_str!("../profiles/amice-simple-vmp/lowering.vm")
            .replace("bind %r = %vr # 记录 LLVM 结果到 VM 寄存器的绑定\n", "");
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            &lowering,
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let err = verify_profile(&profile).expect_err("result-producing lowering rules must bind their result");

        assert!(err.to_string().contains("must bind %r"));
    }

    #[test]
    fn lowering_emit_operand_must_exist_in_profile_instruction() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let lowering = include_str!("../profiles/amice-simple-vmp/lowering.vm").replace(
            "emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令",
            "emit iadd dst=%vr, left=%va, rhs=%vb, width=type_width(%r) # 故意使用 ISA 未声明的 operand 名称",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            &lowering,
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let err = verify_profile(&profile).expect_err("emit operands must follow the ISA instruction contract");

        assert!(err.to_string().contains("operand left"));
        assert!(err.to_string().contains("iadd"));
    }

    #[test]
    fn lowering_emit_must_reference_defined_vm_value() {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        let lowering = include_str!("../profiles/amice-simple-vmp/lowering.vm").replace(
            "emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令",
            "emit iadd dst=%vr, lhs=%missing, rhs=%vb, width=type_width(%r) # 故意引用未定义的 VM 值",
        );
        let profile = ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            &lowering,
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile should parse");

        let err = verify_profile(&profile).expect_err("emit operands must reference defined VM values");

        assert!(err.to_string().contains("%missing"));
        assert!(err.to_string().contains("undefined VM value"));
    }

    #[test]
    fn manifest_entries_drive_package_loading() {
        let unique = format!(
            "amice-vm-profile-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after epoch")
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir(&dir).expect("temp profile dir should be creatable");

        let manifest = include_str!("../profiles/amice-simple-vmp/manifest.toml")
            .replace("abi = \"abi.vm\"", "abi = \"custom-abi.vm\"")
            .replace("isa = \"isa.vm\"", "isa = \"custom-isa.vm\"")
            .replace("lowering = \"lowering.vm\"", "lowering = \"custom-lowering.vm\"")
            .replace("bytecode = \"bytecode.vm\"", "bytecode = \"custom-bytecode.vm\"")
            .replace("decoder = \"decoder.vm\"", "decoder = \"custom-decoder.vm\"")
            .replace("runtime = \"runtime.vm\"", "runtime = \"custom-runtime.vm\"");
        std::fs::write(dir.join("manifest.toml"), manifest).expect("manifest should be writable");
        std::fs::write(
            dir.join("custom-abi.vm"),
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
        )
        .expect("abi should be writable");
        std::fs::write(
            dir.join("custom-isa.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
        )
        .expect("isa should be writable");
        std::fs::write(
            dir.join("custom-lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
        )
        .expect("lowering should be writable");
        std::fs::write(
            dir.join("custom-bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
        )
        .expect("bytecode should be writable");
        std::fs::write(
            dir.join("custom-decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
        )
        .expect("decoder should be writable");
        std::fs::write(
            dir.join("custom-runtime.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("runtime should be writable");

        let profile = ProfilePackage::load_from_path(&dir).expect("profile should load via manifest entries");

        verify_profile(&profile).expect("manifest-loaded profile should verify");
        std::fs::remove_dir_all(dir).expect("temp profile dir should be removable");
    }

    fn contains_cjk(text: &str) -> bool {
        text.chars().any(|ch| {
            matches!(
                ch,
                '\u{3400}'..='\u{4dbf}'
                    | '\u{4e00}'..='\u{9fff}'
                    | '\u{f900}'..='\u{faff}'
                    | '\u{20000}'..='\u{2a6df}'
                    | '\u{2a700}'..='\u{2b73f}'
                    | '\u{2b740}'..='\u{2b81f}'
                    | '\u{2b820}'..='\u{2ceaf}'
            )
        })
    }
}
