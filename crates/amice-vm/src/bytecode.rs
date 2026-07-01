//! profile 驱动 VM 函数的 bytecode encoder。
//!
//! # 契约
//! encoder 消费 `VmFunction` 以及每条指令对应的 profile identity，
//! 并写出 `bytecode.vm` 与 `decoder.vm` 描述的 bytecode package。
//! encoder 变换必须是 runtime decoder pipeline 的逆变换。
//!
//! # 不变量
//! - code segment 是 `u8` 流，不是固定整数数组。
//! - opcode 和 operand 顺序必须从 VM IR 指令旁保存的 profile 指令名解析。
//! - 生成 relocation record 前，label PC 必须先按 fake instruction 展开。
//!
//! # 坑点
//! `debug_dump` 只用于诊断文本。runtime 生成必须消费结构化 offset 和 `bytes` 字段。

use crate::isa::{
    AtomicRmwOp, BinOp, CastOp, FloatBinOp, FloatCastOp, FloatUnaryOp, HandlerSemantic, IntTernaryOp, IntUnaryOp,
    MemoryOrdering, SuperOp,
};
use crate::lowering::{
    LabelId, NATIVE_CALL_MAX_ARGS, NATIVE_CALL_MAX_RETURNS, NativeReturn, VmFunction, VmInstruction,
};
use crate::profile::{DecoderStep, ProfilePackage, RuntimeScope, SegmentMode};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const BYTECODE_MAGIC: &[u8; 8] = b"AMICEVMP";
const BYTECODE_HEADER_LEN: usize = 64;
const BYTECODE_PACKAGE_VERSION: u32 = 1;

/// 单个函数编码后的 VM bytecode。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BytecodeImage {
    /// 完整 bytecode package，包含 header、const pool、code 和 reloc。
    pub bytes: Vec<u8>,
    /// 编码后 code segment 在 `bytes` 中的字节偏移。
    pub code_offset: usize,
    /// 编码后 code segment 的字节长度。
    pub code_len: usize,
    /// const-pool segment 在 `bytes` 中的字节偏移。
    pub const_pool_offset: usize,
    /// const-pool segment 的字节长度。
    pub const_pool_len: usize,
    /// relocation segment 在 `bytes` 中的字节偏移。
    pub reloc_offset: usize,
    /// relocation segment 的字节长度。
    pub reloc_len: usize,
    /// decoder 逆变换使用的 per-function bytecode key。
    pub key: u64,
    /// 插入 fake/dead 指令后的编码指令记录数。
    pub instruction_count: u32,
    /// 测试和 debug dump 使用的人类可读编码轨迹。
    pub debug_dump: String,
}

impl BytecodeImage {
    /// 返回生成的 VM runtime 消费的编码 code segment。
    pub fn code_bytes(&self) -> &[u8] {
        &self.bytes[self.code_offset..self.code_offset + self.code_len]
    }
}

/// 由 profile ISA 和 decoder pipeline 驱动的 bytecode encoder。
#[derive(Debug)]
pub struct BytecodeEncoder<'a> {
    profile: &'a ProfilePackage,
}

impl<'a> BytecodeEncoder<'a> {
    /// 为已校验的 profile package 创建 encoder。
    ///
    /// # 契约
    /// 调用方必须传入已经通过 `verify_profile` 的 package；encoder 会假设指令描述、
    /// decoder step 和 bytecode layout 已经内部一致。
    pub fn new(profile: &'a ProfilePackage) -> Self {
        Self { profile }
    }

    /// 把 VM IR 编码为受保护的 `u8` 字节流。
    ///
    /// # 错误
    /// 当 bytecode profile 不受支持、VM IR 缺少匹配的 profile 指令 identity、
    /// label 未绑定、operand 不满足所选指令描述，或 segment 大小无法写入 package header
    /// 时返回错误。
    ///
    /// # 契约
    /// 选中的 profile 指令决定 opcode alias 选择和 operand 序列化顺序。
    /// 这保证两个同语义 ISA 指令在 lowering 后仍能保持不同身份。
    pub fn encode(&self, function: &VmFunction) -> anyhow::Result<BytecodeImage> {
        if self.profile.bytecode.code_segment != SegmentMode::Compressed {
            anyhow::bail!("vm_virtualize requires a compressed code segment");
        }

        let key = bytecode_key(self.profile, function);
        let const_pool = ConstPool::from_function(function);
        let const_pool_bytes = encode_const_pool_values(&const_pool.values, key);
        let fake_count = if self.profile.bytecode.fake_instruction.enabled {
            self.profile.bytecode.fake_instruction.count as usize
        } else {
            0
        };
        let dead_count = if self.profile.bytecode.dead_bytecode.enabled {
            self.profile.bytecode.dead_bytecode.count as usize
        } else {
            0
        };
        let layout = expanded_layout(self.profile, function, fake_count, dead_count)?;
        let expanded_instruction_count = function.instructions.len() * (1 + fake_count) + dead_count;
        let mut records = Vec::with_capacity(expanded_instruction_count);
        let mut debug_lines = Vec::with_capacity(expanded_instruction_count);
        let mut record_index = 0;

        for (pc, instruction) in function.instructions.iter().enumerate() {
            let expanded_pc = layout.record_offsets[record_index];
            let profile_instruction = function
                .profile_instructions
                .get(pc)
                .ok_or_else(|| anyhow::anyhow!("missing profile instruction name for VM instruction at pc {pc}"))?;
            let tokens = self.instruction_tokens(
                &layout.label_pcs,
                &const_pool,
                key,
                expanded_pc,
                profile_instruction,
                instruction,
            )?;
            debug_lines.push(format!(
                "{expanded_pc:04}: {profile_instruction} width={} opcode={} operands={:?} {instruction:?}",
                tokens.decoded_width,
                tokens.opcode(),
                tokens.operands()
            ));
            records.push(tokens);
            record_index += 1;
            for _ in 0..fake_count {
                let fake_pc = layout.record_offsets[record_index];
                let tokens = self.fake_instruction_tokens(key, fake_pc)?;
                debug_lines.push(format!(
                    "{fake_pc:04}: fake_nop width={} opcode={} operands={:?}",
                    tokens.decoded_width,
                    tokens.opcode(),
                    tokens.operands()
                ));
                records.push(tokens);
                record_index += 1;
            }
        }

        for _ in 0..dead_count {
            let dead_pc = layout.record_offsets[record_index];
            let tokens = self.fake_instruction_tokens(key, dead_pc)?;
            debug_lines.push(format!(
                "{dead_pc:04}: dead_fake_nop width={} opcode={} operands={:?}",
                tokens.decoded_width,
                tokens.opcode(),
                tokens.operands()
            ));
            records.push(tokens);
            record_index += 1;
        }

        let code_bytes = self.apply_encoder_pipeline(records, key)?;
        let reloc_bytes = encode_label_pc_relocations(&layout.label_pcs, code_bytes.len())?;
        let package = build_bytecode_package(
            key,
            expanded_instruction_count as u32,
            const_pool_bytes,
            code_bytes,
            reloc_bytes,
        )?;

        Ok(BytecodeImage {
            bytes: package.bytes,
            code_offset: package.code_offset,
            code_len: package.code_len,
            const_pool_offset: package.const_pool_offset,
            const_pool_len: package.const_pool_len,
            reloc_offset: package.reloc_offset,
            reloc_len: package.reloc_len,
            key,
            instruction_count: expanded_instruction_count as u32,
            debug_dump: debug_lines.join("\n"),
        })
    }

    fn instruction_tokens(
        &self,
        label_pcs: &HashMap<LabelId, usize>,
        const_pool: &ConstPool,
        key: u64,
        pc: usize,
        profile_instruction: &str,
        instruction: &VmInstruction,
    ) -> anyhow::Result<InstructionRecord> {
        match instruction {
            VmInstruction::MovImm { dst, imm, width } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::MovImm,
                key,
                pc,
                operands([("dst", *dst as u64), ("imm", *imm), ("width", *width as u64)]),
            ),
            VmInstruction::ConstLoad { dst, value, width } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::ConstLoad,
                key,
                pc,
                operands([
                    ("dst", *dst as u64),
                    ("index", const_pool.index_of(*value)?),
                    ("width", *width as u64),
                ]),
            ),
            VmInstruction::SuperAddXor {
                dst,
                lhs,
                rhs,
                xor_rhs,
                width,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Super(SuperOp::AddXor),
                key,
                pc,
                operands([
                    ("dst", *dst as u64),
                    ("lhs", *lhs as u64),
                    ("rhs", *rhs as u64),
                    ("xor_rhs", *xor_rhs as u64),
                    ("width", *width as u64),
                ]),
            ),
            VmInstruction::SuperIcmpBrIf {
                pred,
                lhs,
                rhs,
                width,
                then_label,
                else_label,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Super(SuperOp::IcmpBrIf),
                key,
                pc,
                operands([
                    ("pred", *pred as u64),
                    ("lhs", *lhs as u64),
                    ("rhs", *rhs as u64),
                    ("width", *width as u64),
                    ("then_pc", label_pc(label_pcs, *then_label)? as u64),
                    ("else_pc", label_pc(label_pcs, *else_label)? as u64),
                ]),
            ),
            VmInstruction::SuperGepLoad {
                dst,
                base,
                offset,
                width,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Super(SuperOp::GepLoad),
                key,
                pc,
                operands([
                    ("dst", *dst as u64),
                    ("base", *base as u64),
                    ("offset", *offset),
                    ("width", *width as u64),
                ]),
            ),
            VmInstruction::SuperLoadAdd {
                dst,
                ptr,
                addend,
                width,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Super(SuperOp::LoadAdd),
                key,
                pc,
                operands([
                    ("dst", *dst as u64),
                    ("ptr", *ptr as u64),
                    ("addend", *addend as u64),
                    ("width", *width as u64),
                ]),
            ),
            VmInstruction::Mov { dst, src, width } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Mov,
                key,
                pc,
                operands([("dst", *dst as u64), ("src", *src as u64), ("width", *width as u64)]),
            ),
            VmInstruction::Bin {
                op,
                dst,
                lhs,
                rhs,
                width,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Bin(*op),
                key,
                pc,
                operands([
                    ("dst", *dst as u64),
                    ("lhs", *lhs as u64),
                    ("rhs", *rhs as u64),
                    ("width", *width as u64),
                ]),
            ),
            VmInstruction::Icmp {
                pred,
                dst,
                lhs,
                rhs,
                width,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Icmp,
                key,
                pc,
                operands([
                    ("pred", *pred as u64),
                    ("dst", *dst as u64),
                    ("lhs", *lhs as u64),
                    ("rhs", *rhs as u64),
                    ("width", *width as u64),
                ]),
            ),
            VmInstruction::IntUnary { op, dst, src, width } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::IntUnary(*op),
                key,
                pc,
                operands([("dst", *dst as u64), ("src", *src as u64), ("width", *width as u64)]),
            ),
            VmInstruction::IntTernary {
                op,
                dst,
                lhs,
                rhs,
                third,
                width,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::IntTernary(*op),
                key,
                pc,
                operands([
                    ("dst", *dst as u64),
                    ("lhs", *lhs as u64),
                    ("rhs", *rhs as u64),
                    ("third", *third as u64),
                    ("width", *width as u64),
                ]),
            ),
            VmInstruction::FloatBin {
                op,
                dst,
                lhs,
                rhs,
                width,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::FloatBin(*op),
                key,
                pc,
                operands([
                    ("dst", *dst as u64),
                    ("lhs", *lhs as u64),
                    ("rhs", *rhs as u64),
                    ("width", *width as u64),
                ]),
            ),
            VmInstruction::FloatUnary { op, dst, src, width } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::FloatUnary(*op),
                key,
                pc,
                operands([("dst", *dst as u64), ("src", *src as u64), ("width", *width as u64)]),
            ),
            VmInstruction::FloatCast {
                op,
                dst,
                src,
                from_width,
                to_width,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::FloatCast(*op),
                key,
                pc,
                operands([
                    ("dst", *dst as u64),
                    ("src", *src as u64),
                    ("from_width", *from_width as u64),
                    ("to_width", *to_width as u64),
                ]),
            ),
            VmInstruction::Fcmp {
                pred,
                dst,
                lhs,
                rhs,
                width,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Fcmp,
                key,
                pc,
                operands([
                    ("pred", *pred as u64),
                    ("dst", *dst as u64),
                    ("lhs", *lhs as u64),
                    ("rhs", *rhs as u64),
                    ("width", *width as u64),
                ]),
            ),
            VmInstruction::Cast {
                op,
                dst,
                src,
                from_width,
                to_width,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Cast(*op),
                key,
                pc,
                operands([
                    ("dst", *dst as u64),
                    ("src", *src as u64),
                    ("from_width", *from_width as u64),
                    ("to_width", *to_width as u64),
                ]),
            ),
            VmInstruction::Alloca { dst, bytes, align } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Alloca,
                key,
                pc,
                operands([("dst", *dst as u64), ("bytes", *bytes), ("align", *align as u64)]),
            ),
            VmInstruction::Load { dst, ptr, width } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Load,
                key,
                pc,
                operands([("dst", *dst as u64), ("ptr", *ptr as u64), ("width", *width as u64)]),
            ),
            VmInstruction::Store { src, ptr, width } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Store,
                key,
                pc,
                operands([("src", *src as u64), ("ptr", *ptr as u64), ("width", *width as u64)]),
            ),
            VmInstruction::AtomicLoad {
                dst,
                ptr,
                width,
                ordering,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::AtomicLoad,
                key,
                pc,
                operands([
                    ("dst", *dst as u64),
                    ("ptr", *ptr as u64),
                    ("width", *width as u64),
                    ("ordering", memory_ordering_tag(*ordering) as u64),
                ]),
            ),
            VmInstruction::AtomicStore {
                src,
                ptr,
                width,
                ordering,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::AtomicStore,
                key,
                pc,
                operands([
                    ("src", *src as u64),
                    ("ptr", *ptr as u64),
                    ("width", *width as u64),
                    ("ordering", memory_ordering_tag(*ordering) as u64),
                ]),
            ),
            VmInstruction::AtomicRmw {
                op,
                dst,
                ptr,
                src,
                width,
                ordering,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::AtomicRmw(*op),
                key,
                pc,
                operands([
                    ("dst", *dst as u64),
                    ("ptr", *ptr as u64),
                    ("src", *src as u64),
                    ("width", *width as u64),
                    ("ordering", memory_ordering_tag(*ordering) as u64),
                ]),
            ),
            VmInstruction::CmpXchg {
                old,
                success,
                ptr,
                cmp,
                new,
                width,
                success_ordering,
                failure_ordering,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::CmpXchg,
                key,
                pc,
                operands([
                    ("old", *old as u64),
                    ("success", *success as u64),
                    ("ptr", *ptr as u64),
                    ("cmp", *cmp as u64),
                    ("new", *new as u64),
                    ("width", *width as u64),
                    ("success_ordering", memory_ordering_tag(*success_ordering) as u64),
                    ("failure_ordering", memory_ordering_tag(*failure_ordering) as u64),
                ]),
            ),
            VmInstruction::Fence { ordering } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Fence,
                key,
                pc,
                operands([("ordering", memory_ordering_tag(*ordering) as u64)]),
            ),
            VmInstruction::Gep { dst, base, offset } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Gep,
                key,
                pc,
                operands([("dst", *dst as u64), ("base", *base as u64), ("offset", *offset)]),
            ),
            VmInstruction::CallNative { call_id, args, returns } => {
                if args.len() > NATIVE_CALL_MAX_ARGS {
                    anyhow::bail!("call_native supports at most 8 arguments, got {}", args.len());
                }
                if returns.len() > NATIVE_CALL_MAX_RETURNS {
                    anyhow::bail!(
                        "call_native supports at most {} returns, got {}",
                        NATIVE_CALL_MAX_RETURNS,
                        returns.len()
                    );
                }

                let mut operands = Vec::with_capacity(2 + NATIVE_CALL_MAX_ARGS + 1 + NATIVE_CALL_MAX_RETURNS * 2);
                operands.push(operand("callee", *call_id as u64));
                operands.push(operand("argc", args.len() as u64));
                for index in 0..NATIVE_CALL_MAX_ARGS {
                    operands.push(operand(
                        &format!("arg{index}"),
                        args.get(index).copied().unwrap_or(0) as u64,
                    ));
                }
                operands.push(operand("ret_count", returns.len() as u64));
                for index in 0..NATIVE_CALL_MAX_RETURNS {
                    let ret = returns.get(index).copied().unwrap_or(NativeReturn { dst: 0, width: 0 });
                    operands.push(operand(&format!("ret{index}"), ret.dst as u64));
                    operands.push(operand(&format!("ret{index}_width"), ret.width as u64));
                }

                record_tokens(
                    self.profile,
                    profile_instruction,
                    &HandlerSemantic::CallNative,
                    key,
                    pc,
                    operands,
                )
            },
            VmInstruction::Br { target } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Br,
                key,
                pc,
                operands([("target", label_pc(label_pcs, *target)? as u64)]),
            ),
            VmInstruction::BrCond {
                cond,
                then_label,
                else_label,
            } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::BrCond,
                key,
                pc,
                operands([
                    ("cond", *cond as u64),
                    ("then_pc", label_pc(label_pcs, *then_label)? as u64),
                    ("else_pc", label_pc(label_pcs, *else_label)? as u64),
                ]),
            ),
            VmInstruction::VmCall { target } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::VmCall,
                key,
                pc,
                operands([("target", label_pc(label_pcs, *target)? as u64)]),
            ),
            VmInstruction::VmRet => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::VmRet,
                key,
                pc,
                Vec::new(),
            ),
            VmInstruction::Ret { src } => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Ret,
                key,
                pc,
                operands([("src", *src as u64)]),
            ),
            VmInstruction::RetVoid => record_tokens(
                self.profile,
                profile_instruction,
                &HandlerSemantic::Ret,
                key,
                pc,
                operands([("src", 0)]),
            ),
        }
    }

    fn fake_instruction_tokens(&self, key: u64, pc: usize) -> anyhow::Result<InstructionRecord> {
        let desc = self
            .profile
            .isa
            .by_semantic(&HandlerSemantic::Nop)
            .ok_or_else(|| anyhow::anyhow!("profile has no opcode for fake nop"))?;
        record_tokens(self.profile, &desc.name, &HandlerSemantic::Nop, key, pc, Vec::new())
    }

    fn apply_encoder_pipeline(&self, records: Vec<InstructionRecord>, key: u64) -> anyhow::Result<Vec<u8>> {
        let mut state = EncoderState::Records(records);

        // `decoder.vm` 描述的是 runtime 解码顺序。编译器必须反向遍历并应用每一步的逆变换；
        // 否则生成的 runtime 和 bytecode image 会悄悄不一致。
        for step in self.profile.decoder.steps.iter().rev() {
            state = match (step, state) {
                (DecoderStep::BitUnpack, EncoderState::Records(records)) => {
                    EncoderState::Tokens(pack_instruction_records(records)?)
                },
                (DecoderStep::VarintDecode, EncoderState::Tokens(records)) => {
                    EncoderState::Bytes(encode_varint_records(records)?)
                },
                (DecoderStep::Rol { amount }, EncoderState::Bytes(mut bytes)) => {
                    bytes
                        .iter_mut()
                        .for_each(|byte| *byte = byte.rotate_right(*amount as u32));
                    EncoderState::Bytes(bytes)
                },
                (DecoderStep::Ror { amount }, EncoderState::Bytes(mut bytes)) => {
                    bytes
                        .iter_mut()
                        .for_each(|byte| *byte = byte.rotate_left(*amount as u32));
                    EncoderState::Bytes(bytes)
                },
                (DecoderStep::AddStream, EncoderState::Bytes(mut bytes)) => {
                    bytes
                        .iter_mut()
                        .enumerate()
                        .for_each(|(index, byte)| *byte = byte.wrapping_add(key_byte(key, index)));
                    EncoderState::Bytes(bytes)
                },
                (DecoderStep::XorStream, EncoderState::Bytes(mut bytes)) => {
                    bytes
                        .iter_mut()
                        .enumerate()
                        .for_each(|(index, byte)| *byte ^= key_byte(key, index));
                    EncoderState::Bytes(bytes)
                },
                (unexpected, _) => anyhow::bail!("decoder step {unexpected:?} is not valid for encoder state"),
            };
        }

        match state {
            EncoderState::Bytes(bytes) => Ok(bytes),
            _ => anyhow::bail!("decoder pipeline did not produce bytecode bytes"),
        }
    }
}

#[derive(Debug)]
enum EncoderState {
    Records(Vec<InstructionRecord>),
    Tokens(Vec<InstructionRecord>),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone)]
struct InstructionRecord {
    decoded_width: u8,
    tokens: Vec<u64>,
}

impl InstructionRecord {
    fn opcode(&self) -> u64 {
        self.tokens[0]
    }

    fn operands(&self) -> &[u64] {
        &self.tokens[1..]
    }
}

#[derive(Debug)]
struct BytecodePackage {
    bytes: Vec<u8>,
    code_offset: usize,
    code_len: usize,
    const_pool_offset: usize,
    const_pool_len: usize,
    reloc_offset: usize,
    reloc_len: usize,
}

fn build_bytecode_package(
    key: u64,
    instruction_count: u32,
    const_pool: Vec<u8>,
    code_bytes: Vec<u8>,
    reloc_bytes: Vec<u8>,
) -> anyhow::Result<BytecodePackage> {
    let const_pool_offset = BYTECODE_HEADER_LEN;
    let const_pool_len = const_pool.len();
    let code_offset = const_pool_offset + const_pool_len;
    let code_len = code_bytes.len();
    let reloc_offset = code_offset + code_len;
    let reloc_len = reloc_bytes.len();

    let mut header = Vec::with_capacity(BYTECODE_HEADER_LEN);
    header.extend_from_slice(BYTECODE_MAGIC);
    write_u32_le(BYTECODE_PACKAGE_VERSION, &mut header);
    write_u32_le(0, &mut header);
    write_u32_le(instruction_count, &mut header);
    write_u32_le(0, &mut header);
    write_u64_le(key, &mut header);
    write_usize_as_u32_le(const_pool_offset, &mut header, "const_pool offset")?;
    write_usize_as_u32_le(const_pool_len, &mut header, "const_pool length")?;
    write_usize_as_u32_le(code_offset, &mut header, "code offset")?;
    write_usize_as_u32_le(code_len, &mut header, "code length")?;
    write_usize_as_u32_le(reloc_offset, &mut header, "reloc offset")?;
    write_usize_as_u32_le(reloc_len, &mut header, "reloc length")?;
    write_u64_le(0, &mut header);
    debug_assert_eq!(header.len(), BYTECODE_HEADER_LEN);

    let mut bytes = Vec::with_capacity(BYTECODE_HEADER_LEN + const_pool_len + code_len + reloc_len);
    bytes.extend(header);
    bytes.extend(const_pool);
    bytes.extend(code_bytes);
    bytes.extend(reloc_bytes);

    Ok(BytecodePackage {
        bytes,
        code_offset,
        code_len,
        const_pool_offset,
        const_pool_len,
        reloc_offset,
        reloc_len,
    })
}

#[derive(Debug)]
struct ExpandedLayout {
    record_offsets: Vec<usize>,
    label_pcs: HashMap<LabelId, usize>,
}

fn expanded_layout(
    profile: &ProfilePackage,
    function: &VmFunction,
    fake_count: usize,
    dead_count: usize,
) -> anyhow::Result<ExpandedLayout> {
    let fake_desc = profile
        .isa
        .by_semantic(&HandlerSemantic::Nop)
        .ok_or_else(|| anyhow::anyhow!("profile has no opcode for fake nop"))?;
    let record_count = function.instructions.len() * (1 + fake_count) + dead_count;
    let mut widths = Vec::with_capacity(record_count);

    for (pc, _) in function.instructions.iter().enumerate() {
        let profile_instruction = function
            .profile_instructions
            .get(pc)
            .ok_or_else(|| anyhow::anyhow!("missing profile instruction name for VM instruction at pc {pc}"))?;
        let desc = profile
            .isa
            .by_name(profile_instruction)
            .ok_or_else(|| anyhow::anyhow!("profile has no instruction named {profile_instruction}"))?;
        widths.push(desc.decoded_width as usize);
        widths.extend(std::iter::repeat_n(fake_desc.decoded_width as usize, fake_count));
    }
    widths.extend(std::iter::repeat_n(fake_desc.decoded_width as usize, dead_count));

    let mut record_offsets = Vec::with_capacity(widths.len());
    let mut prefix = Vec::with_capacity(widths.len() + 1);
    let mut offset = 0usize;
    prefix.push(offset);
    for width in widths {
        record_offsets.push(offset);
        offset = offset
            .checked_add(width)
            .ok_or_else(|| anyhow::anyhow!("expanded bytecode layout overflowed usize"))?;
        prefix.push(offset);
    }

    let stride = 1 + fake_count;
    let label_pcs = function
        .label_pcs
        .iter()
        .map(|(label, pc)| {
            if *pc > function.instructions.len() {
                anyhow::bail!("label {:?} points past instruction stream", label);
            }
            let expanded_index = pc * stride;
            let byte_pc = prefix
                .get(expanded_index)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("label {:?} points past expanded bytecode", label))?;
            Ok((*label, byte_pc))
        })
        .collect::<anyhow::Result<HashMap<_, _>>>()?;

    Ok(ExpandedLayout {
        record_offsets,
        label_pcs,
    })
}

fn encode_label_pc_relocations(label_pcs: &HashMap<LabelId, usize>, code_len: usize) -> anyhow::Result<Vec<u8>> {
    let mut labels = label_pcs.iter().map(|(label, pc)| (*label, *pc)).collect::<Vec<_>>();
    labels.sort_by_key(|(label, _)| label.0);

    let mut bytes = Vec::new();
    encode_varint(labels.len() as u64, &mut bytes);
    for (label, pc) in labels {
        if pc > code_len {
            anyhow::bail!("label {:?} points past instruction stream", label);
        }
        encode_varint(label.0 as u64, &mut bytes);
        encode_varint(pc as u64, &mut bytes);
    }
    Ok(bytes)
}

fn encode_const_pool_values(constants: &[u64], key: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode_varint(constants.len() as u64, &mut bytes);
    for value in constants {
        encode_varint(*value, &mut bytes);
    }
    bytes
        .iter_mut()
        .enumerate()
        .for_each(|(index, byte)| *byte ^= key_byte(key, index));
    bytes
}

#[derive(Debug)]
struct ConstPool {
    values: Vec<u64>,
    indexes: HashMap<u64, u64>,
}

impl ConstPool {
    fn from_function(function: &VmFunction) -> Self {
        let mut values = collect_const_pool_values(function);
        values.sort_unstable();
        values.dedup();
        let indexes = values
            .iter()
            .enumerate()
            .map(|(index, value)| (*value, index as u64))
            .collect();

        Self { values, indexes }
    }

    fn index_of(&self, value: u64) -> anyhow::Result<u64> {
        self.indexes
            .get(&value)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("constant 0x{value:x} was not assigned a const_pool index"))
    }
}

fn collect_const_pool_values(function: &VmFunction) -> Vec<u64> {
    let mut constants = Vec::new();
    for instruction in &function.instructions {
        match instruction {
            VmInstruction::ConstLoad { value, .. } => constants.push(*value),
            VmInstruction::MovImm { .. }
            | VmInstruction::Mov { .. }
            | VmInstruction::SuperAddXor { .. }
            | VmInstruction::SuperIcmpBrIf { .. }
            | VmInstruction::SuperGepLoad { .. }
            | VmInstruction::SuperLoadAdd { .. }
            | VmInstruction::Bin { .. }
            | VmInstruction::IntUnary { .. }
            | VmInstruction::IntTernary { .. }
            | VmInstruction::Icmp { .. }
            | VmInstruction::FloatBin { .. }
            | VmInstruction::FloatUnary { .. }
            | VmInstruction::FloatCast { .. }
            | VmInstruction::Fcmp { .. }
            | VmInstruction::Cast { .. }
            | VmInstruction::Alloca { .. }
            | VmInstruction::Load { .. }
            | VmInstruction::Store { .. }
            | VmInstruction::AtomicLoad { .. }
            | VmInstruction::AtomicStore { .. }
            | VmInstruction::AtomicRmw { .. }
            | VmInstruction::CmpXchg { .. }
            | VmInstruction::Fence { .. }
            | VmInstruction::Gep { .. }
            | VmInstruction::CallNative { .. }
            | VmInstruction::Br { .. }
            | VmInstruction::BrCond { .. }
            | VmInstruction::VmCall { .. }
            | VmInstruction::VmRet
            | VmInstruction::Ret { .. }
            | VmInstruction::RetVoid => {},
        }
    }
    constants
}

fn write_usize_as_u32_le(value: usize, out: &mut Vec<u8>, name: &str) -> anyhow::Result<()> {
    let value = u32::try_from(value).map_err(|_| anyhow::anyhow!("bytecode {name} does not fit in u32"))?;
    write_u32_le(value, out);
    Ok(())
}

fn write_u32_le(value: u32, out: &mut Vec<u8>) {
    out.extend(value.to_le_bytes());
}

fn write_u64_le(value: u64, out: &mut Vec<u8>) {
    out.extend(value.to_le_bytes());
}

fn pack_instruction_records(records: Vec<InstructionRecord>) -> anyhow::Result<Vec<InstructionRecord>> {
    let mut packed = Vec::with_capacity(records.len());
    for record in records {
        if record.tokens.is_empty() {
            anyhow::bail!("empty VM instruction record");
        }
        let (opcode, operands) = record
            .tokens
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("empty VM instruction record"))?;
        let mut tokens = Vec::new();
        tokens.push(*opcode);
        for operand in operands {
            bitpack_operand(*operand, &mut tokens);
        }
        packed.push(InstructionRecord {
            decoded_width: record.decoded_width,
            tokens,
        });
    }
    Ok(packed)
}

fn encode_varint_records(records: Vec<InstructionRecord>) -> anyhow::Result<Vec<u8>> {
    let total_len = records
        .iter()
        .map(|record| record.decoded_width as usize)
        .sum::<usize>();
    let mut bytes = Vec::with_capacity(total_len);
    for record in records {
        let mut record_bytes = Vec::new();
        for token in record.tokens {
            encode_varint(token, &mut record_bytes);
        }
        let width = record.decoded_width as usize;
        if record_bytes.len() > width {
            anyhow::bail!(
                "encoded VM instruction record needs {} decoded bytes but profile width is {}",
                record_bytes.len(),
                width
            );
        }
        record_bytes.resize(width, 0);
        bytes.extend(record_bytes);
    }
    Ok(bytes)
}

fn bitpack_operand(value: u64, out: &mut Vec<u64>) {
    let bit_width = u64::BITS - value.leading_zeros();
    out.push(bit_width as u64);
    for shift in (0..bit_width).step_by(7) {
        out.push((value >> shift) & 0x7f);
    }
}

fn operand(name: &str, value: u64) -> (String, u64) {
    (name.to_owned(), value)
}

fn operands<const N: usize>(items: [(&str, u64); N]) -> Vec<(String, u64)> {
    items.into_iter().map(|(name, value)| operand(name, value)).collect()
}

fn record_tokens(
    profile: &ProfilePackage,
    instruction_name: &str,
    semantic: &HandlerSemantic,
    key: u64,
    pc: usize,
    operands: impl IntoIterator<Item = (String, u64)>,
) -> anyhow::Result<InstructionRecord> {
    let desc = profile
        .isa
        .by_name(instruction_name)
        .ok_or_else(|| anyhow::anyhow!("profile has no instruction named {instruction_name}"))?;
    if desc.semantic != *semantic {
        anyhow::bail!(
            "VM instruction encoded as {instruction_name} expects semantic {:?}, profile declares {:?}",
            semantic,
            desc.semantic
        );
    }
    let operand_values = operands.into_iter().collect::<HashMap<_, _>>();
    let mut tokens = Vec::with_capacity(1 + desc.operand_descs.len());
    tokens.push(desc.opcode_for_site(key, pc) as u64);
    for operand in &desc.operand_descs {
        tokens.push(
            *operand_values
                .get(&operand.name)
                .ok_or_else(|| anyhow::anyhow!("missing operand {} for {}", operand.name, desc.name))?,
        );
    }
    Ok(InstructionRecord {
        decoded_width: desc.decoded_width,
        tokens,
    })
}

fn label_pc(label_pcs: &HashMap<LabelId, usize>, label: LabelId) -> anyhow::Result<usize> {
    label_pcs
        .get(&label)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("label {:?} is not bound", label))
}

fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn key_byte(key: u64, index: usize) -> u8 {
    let index = index as u64;
    let rotate = ((index.wrapping_mul(13)) & 63) as u32;
    let index_rot = index.rotate_left(17);
    let mixed = key.rotate_left(rotate) ^ index.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ index_rot ^ (index >> 7);
    (mixed ^ (mixed >> 32) ^ (mixed >> 16) ^ (mixed >> 8)) as u8
}

fn function_key(function: &VmFunction) -> u64 {
    let mut hasher = DefaultHasher::new();
    function.name.hash(&mut hasher);
    function.vreg_count.hash(&mut hasher);
    function.return_width.hash(&mut hasher);
    function.instructions.hash(&mut hasher);
    function.profile_instructions.hash(&mut hasher);
    let key = hasher.finish() | 1;
    key ^ 0xa6d3_9f74_c2b1_580d
}

fn bytecode_key(profile: &ProfilePackage, function: &VmFunction) -> u64 {
    // `polymorph.scope` 决定允许哪个表面发生变化。func scope 会让 opcode/key 选择绑定到
    // 具体 VM IR；module scope 则刻意只从已校验 profile 表面派生 key，使同一模块内所有
    // 被保护函数共享同一套多态计划。
    match profile.runtime.polymorph_scope {
        RuntimeScope::Func => function_key(function),
        RuntimeScope::Module => profile_key(profile),
    }
}

fn profile_key(profile: &ProfilePackage) -> u64 {
    let mut hasher = DefaultHasher::new();
    profile.manifest.version.hash(&mut hasher);
    profile.manifest.name.hash(&mut hasher);
    profile.manifest.target.pointer_bits.hash(&mut hasher);
    profile.manifest.target.endian.hash(&mut hasher);
    for instruction in &profile.isa.instructions {
        instruction.name.hash(&mut hasher);
        instruction.opcodes().hash(&mut hasher);
        instruction.operands.hash(&mut hasher);
        instruction.decoded_width.hash(&mut hasher);
    }
    for step in &profile.decoder.steps {
        decoder_step_tag(*step).hash(&mut hasher);
    }
    let key = hasher.finish() | 1;
    key ^ 0x5d8e_91b4_27c3_f06b
}

fn decoder_step_tag(step: DecoderStep) -> u16 {
    match step {
        DecoderStep::XorStream => 0,
        DecoderStep::AddStream => 1,
        DecoderStep::Rol { amount } => 0x100 | amount as u16,
        DecoderStep::Ror { amount } => 0x200 | amount as u16,
        DecoderStep::VarintDecode => 3,
        DecoderStep::BitUnpack => 4,
    }
}

impl Hash for VmInstruction {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            VmInstruction::MovImm { dst, imm, width } => {
                dst.hash(state);
                imm.hash(state);
                width.hash(state);
            },
            VmInstruction::ConstLoad { dst, value, width } => {
                dst.hash(state);
                value.hash(state);
                width.hash(state);
            },
            VmInstruction::SuperAddXor {
                dst,
                lhs,
                rhs,
                xor_rhs,
                width,
            } => {
                dst.hash(state);
                lhs.hash(state);
                rhs.hash(state);
                xor_rhs.hash(state);
                width.hash(state);
            },
            VmInstruction::SuperIcmpBrIf {
                pred,
                lhs,
                rhs,
                width,
                then_label,
                else_label,
            } => {
                (*pred as u8).hash(state);
                lhs.hash(state);
                rhs.hash(state);
                width.hash(state);
                then_label.hash(state);
                else_label.hash(state);
            },
            VmInstruction::SuperGepLoad {
                dst,
                base,
                offset,
                width,
            } => {
                dst.hash(state);
                base.hash(state);
                offset.hash(state);
                width.hash(state);
            },
            VmInstruction::SuperLoadAdd {
                dst,
                ptr,
                addend,
                width,
            } => {
                dst.hash(state);
                ptr.hash(state);
                addend.hash(state);
                width.hash(state);
            },
            VmInstruction::Mov { dst, src, width } => {
                dst.hash(state);
                src.hash(state);
                width.hash(state);
            },
            VmInstruction::Bin {
                op,
                dst,
                lhs,
                rhs,
                width,
            } => {
                bin_tag(*op).hash(state);
                dst.hash(state);
                lhs.hash(state);
                rhs.hash(state);
                width.hash(state);
            },
            VmInstruction::Icmp {
                pred,
                dst,
                lhs,
                rhs,
                width,
            } => {
                (*pred as u8).hash(state);
                dst.hash(state);
                lhs.hash(state);
                rhs.hash(state);
                width.hash(state);
            },
            VmInstruction::FloatBin {
                op,
                dst,
                lhs,
                rhs,
                width,
            } => {
                float_bin_tag(*op).hash(state);
                dst.hash(state);
                lhs.hash(state);
                rhs.hash(state);
                width.hash(state);
            },
            VmInstruction::FloatUnary { op, dst, src, width } => {
                float_unary_tag(*op).hash(state);
                dst.hash(state);
                src.hash(state);
                width.hash(state);
            },
            VmInstruction::FloatCast {
                op,
                dst,
                src,
                from_width,
                to_width,
            } => {
                float_cast_tag(*op).hash(state);
                dst.hash(state);
                src.hash(state);
                from_width.hash(state);
                to_width.hash(state);
            },
            VmInstruction::Fcmp {
                pred,
                dst,
                lhs,
                rhs,
                width,
            } => {
                (*pred as u8).hash(state);
                dst.hash(state);
                lhs.hash(state);
                rhs.hash(state);
                width.hash(state);
            },
            VmInstruction::IntUnary { op, dst, src, width } => {
                int_unary_tag(*op).hash(state);
                dst.hash(state);
                src.hash(state);
                width.hash(state);
            },
            VmInstruction::IntTernary {
                op,
                dst,
                lhs,
                rhs,
                third,
                width,
            } => {
                int_ternary_tag(*op).hash(state);
                dst.hash(state);
                lhs.hash(state);
                rhs.hash(state);
                third.hash(state);
                width.hash(state);
            },
            VmInstruction::Cast {
                op,
                dst,
                src,
                from_width,
                to_width,
            } => {
                cast_tag(*op).hash(state);
                dst.hash(state);
                src.hash(state);
                from_width.hash(state);
                to_width.hash(state);
            },
            VmInstruction::Alloca { dst, bytes, align } => {
                dst.hash(state);
                bytes.hash(state);
                align.hash(state);
            },
            VmInstruction::Load { dst, ptr, width } => {
                dst.hash(state);
                ptr.hash(state);
                width.hash(state);
            },
            VmInstruction::Store { src, ptr, width } => {
                src.hash(state);
                ptr.hash(state);
                width.hash(state);
            },
            VmInstruction::AtomicLoad {
                dst,
                ptr,
                width,
                ordering,
            } => {
                dst.hash(state);
                ptr.hash(state);
                width.hash(state);
                memory_ordering_tag(*ordering).hash(state);
            },
            VmInstruction::AtomicStore {
                src,
                ptr,
                width,
                ordering,
            } => {
                src.hash(state);
                ptr.hash(state);
                width.hash(state);
                memory_ordering_tag(*ordering).hash(state);
            },
            VmInstruction::AtomicRmw {
                op,
                dst,
                ptr,
                src,
                width,
                ordering,
            } => {
                atomic_rmw_tag(*op).hash(state);
                dst.hash(state);
                ptr.hash(state);
                src.hash(state);
                width.hash(state);
                memory_ordering_tag(*ordering).hash(state);
            },
            VmInstruction::CmpXchg {
                old,
                success,
                ptr,
                cmp,
                new,
                width,
                success_ordering,
                failure_ordering,
            } => {
                old.hash(state);
                success.hash(state);
                ptr.hash(state);
                cmp.hash(state);
                new.hash(state);
                width.hash(state);
                memory_ordering_tag(*success_ordering).hash(state);
                memory_ordering_tag(*failure_ordering).hash(state);
            },
            VmInstruction::Fence { ordering } => {
                memory_ordering_tag(*ordering).hash(state);
            },
            VmInstruction::Gep { dst, base, offset } => {
                dst.hash(state);
                base.hash(state);
                offset.hash(state);
            },
            VmInstruction::CallNative { call_id, args, returns } => {
                call_id.hash(state);
                args.hash(state);
                returns.iter().for_each(|ret| {
                    ret.dst.hash(state);
                    ret.width.hash(state);
                });
            },
            VmInstruction::Br { target } => target.hash(state),
            VmInstruction::BrCond {
                cond,
                then_label,
                else_label,
            } => {
                cond.hash(state);
                then_label.hash(state);
                else_label.hash(state);
            },
            VmInstruction::VmCall { target } => target.hash(state),
            VmInstruction::VmRet => 1_u8.hash(state),
            VmInstruction::Ret { src } => src.hash(state),
            VmInstruction::RetVoid => 0_u8.hash(state),
        }
    }
}

fn bin_tag(op: BinOp) -> u8 {
    match op {
        BinOp::Add => 0,
        BinOp::Sub => 1,
        BinOp::Mul => 2,
        BinOp::UDiv => 3,
        BinOp::SDiv => 4,
        BinOp::URem => 5,
        BinOp::SRem => 6,
        BinOp::Xor => 7,
        BinOp::And => 8,
        BinOp::Or => 9,
        BinOp::Shl => 10,
        BinOp::LShr => 11,
        BinOp::AShr => 12,
    }
}

fn int_unary_tag(op: IntUnaryOp) -> u8 {
    match op {
        IntUnaryOp::CtPop => 0,
        IntUnaryOp::BSwap => 1,
        IntUnaryOp::BitReverse => 2,
    }
}

fn int_ternary_tag(op: IntTernaryOp) -> u8 {
    match op {
        IntTernaryOp::FShl => 0,
        IntTernaryOp::FShr => 1,
    }
}

fn float_bin_tag(op: FloatBinOp) -> u8 {
    match op {
        FloatBinOp::Add => 0,
        FloatBinOp::Sub => 1,
        FloatBinOp::Mul => 2,
        FloatBinOp::Div => 3,
        FloatBinOp::Rem => 4,
    }
}

fn float_unary_tag(op: FloatUnaryOp) -> u8 {
    match op {
        FloatUnaryOp::Neg => 0,
    }
}

fn float_cast_tag(op: FloatCastOp) -> u8 {
    match op {
        FloatCastOp::SignedIntToFloat => 0,
        FloatCastOp::UnsignedIntToFloat => 1,
        FloatCastOp::FloatToSignedInt => 2,
        FloatCastOp::FloatToUnsignedInt => 3,
        FloatCastOp::FloatTrunc => 4,
        FloatCastOp::FloatExt => 5,
    }
}

fn cast_tag(op: CastOp) -> u8 {
    match op {
        CastOp::ZExt => 0,
        CastOp::SExt => 1,
        CastOp::Trunc => 2,
        CastOp::Bitcast => 3,
    }
}

fn memory_ordering_tag(ordering: MemoryOrdering) -> u8 {
    ordering as u8
}

fn atomic_rmw_tag(op: AtomicRmwOp) -> u8 {
    match op {
        AtomicRmwOp::Xchg => 0,
        AtomicRmwOp::Add => 1,
        AtomicRmwOp::Sub => 2,
        AtomicRmwOp::And => 3,
        AtomicRmwOp::Or => 4,
        AtomicRmwOp::Xor => 5,
        AtomicRmwOp::Nand => 6,
        AtomicRmwOp::Max => 7,
        AtomicRmwOp::Min => 8,
        AtomicRmwOp::UMax => 9,
        AtomicRmwOp::UMin => 10,
    }
}

#[cfg(test)]
mod tests {
    use crate::isa::Opcode;

    use super::*;
    use crate::isa::CmpPredicate;
    use crate::lowering::{NativeReturn, VmFunctionBuilder, VmInstruction, fuse_superinstructions};
    use crate::profile::Manifest;
    use crate::verify::verify_profile;

    #[test]
    fn encodes_u8_stream() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let function = add_function();

        let image = BytecodeEncoder::new(&profile).encode(&function).expect("bytecode");

        assert!(!image.bytes.is_empty());
        assert!(!image.code_bytes().is_empty());
        assert_eq!(image.instruction_count, 6);
        assert_eq!(image.code_len, 36);
    }

    #[test]
    fn mixed_decoded_record_widths_are_profile_driven() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("mixed_widths", 4, 64);
        let entry = builder.new_label();
        builder.bind_label(entry);
        builder.push(VmInstruction::MovImm {
            dst: 0,
            imm: 0x1234_5678,
            width: 64,
        });
        builder.push(VmInstruction::Mov {
            dst: 1,
            src: 0,
            width: 64,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: 2,
            lhs: 0,
            rhs: 1,
            width: 64,
        });
        builder.push(VmInstruction::IntTernary {
            op: IntTernaryOp::FShl,
            dst: 3,
            lhs: 0,
            rhs: 1,
            third: 2,
            width: 64,
        });
        builder.push(VmInstruction::CallNative {
            call_id: 0,
            args: vec![0, 1],
            returns: vec![NativeReturn { dst: 0, width: 64 }],
        });
        builder.push(VmInstruction::Ret { src: 0 });
        let function = builder.finish().expect("vm function");

        let image = BytecodeEncoder::new(&profile).encode(&function).expect("bytecode");
        let widths = decode_records_for_test(&profile, &image)
            .into_iter()
            .map(|record| record.decoded_width)
            .collect::<Vec<_>>();

        assert_eq!(widths, vec![32, 4, 8, 4, 16, 4, 48, 4, 64, 4, 4, 4, 4, 4]);
        assert_eq!(image.code_len, widths.iter().sum::<usize>());
    }

    #[test]
    fn super_add_xor_fusion_matches_unfused_behavior() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("super_add_xor", 4, 32);
        let entry = builder.new_label();
        builder.bind_label(entry);
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: 3,
            lhs: 0,
            rhs: 1,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Xor,
            dst: 0,
            lhs: 3,
            rhs: 2,
            width: 32,
        });
        builder.push(VmInstruction::Ret { src: 0 });
        let function = builder.finish().expect("vm function");
        let fused = fuse_superinstructions(function, &profile.isa, &profile.lowering);

        assert_eq!(fused.profile_instructions[0], "iadd_xor");
        assert!(matches!(fused.instructions[0], VmInstruction::SuperAddXor { .. }));
        let image = BytecodeEncoder::new(&profile).encode(&fused).expect("bytecode");

        assert_eq!(execute_for_test(&profile, &image, &[5, 7, 0x33]), (5 + 7) ^ 0x33);
        assert!(image.debug_dump.contains("iadd_xor width=48"));
    }

    #[test]
    fn super_add_xor_does_not_fuse_when_add_result_has_extra_use() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("super_add_xor_no_fuse", 5, 32);
        let entry = builder.new_label();
        builder.bind_label(entry);
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: 3,
            lhs: 0,
            rhs: 1,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Xor,
            dst: 0,
            lhs: 3,
            rhs: 2,
            width: 32,
        });
        builder.push(VmInstruction::Mov {
            dst: 4,
            src: 3,
            width: 32,
        });
        builder.push(VmInstruction::Ret { src: 0 });
        let function = builder.finish().expect("vm function");
        let fused = fuse_superinstructions(function, &profile.isa, &profile.lowering);

        assert_eq!(fused.profile_instructions[0], "iadd");
        assert_eq!(fused.profile_instructions[1], "ixor");
    }

    #[test]
    fn super_icmp_br_if_fusion_branches_with_relocated_labels() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("super_icmp_br_if", 3, 32);
        let entry = builder.new_label();
        let then_label = builder.new_label();
        let else_label = builder.new_label();
        builder.bind_label(entry);
        builder.push(VmInstruction::Icmp {
            pred: CmpPredicate::Slt,
            dst: 2,
            lhs: 0,
            rhs: 1,
            width: 32,
        });
        builder.push(VmInstruction::BrCond {
            cond: 2,
            then_label,
            else_label,
        });
        builder.bind_label(then_label);
        builder.push(VmInstruction::MovImm {
            dst: 0,
            imm: 111,
            width: 32,
        });
        builder.push(VmInstruction::Ret { src: 0 });
        builder.bind_label(else_label);
        builder.push(VmInstruction::MovImm {
            dst: 0,
            imm: 222,
            width: 32,
        });
        builder.push(VmInstruction::Ret { src: 0 });
        let function = builder.finish().expect("vm function");
        let fused = fuse_superinstructions(function, &profile.isa, &profile.lowering);

        assert_eq!(fused.profile_instructions[0], "icmp_br_if");
        assert!(matches!(fused.instructions[0], VmInstruction::SuperIcmpBrIf { .. }));
        let image = BytecodeEncoder::new(&profile).encode(&fused).expect("bytecode");

        assert_eq!(execute_for_test(&profile, &image, &[1, 2]), 111);
        assert_eq!(execute_for_test(&profile, &image, &[3, 2]), 222);
        assert!(image.debug_dump.contains("icmp_br_if width=32"));
    }

    #[test]
    fn super_gep_load_fusion_uses_wide_record() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("super_gep_load", 3, 32);
        let entry = builder.new_label();
        builder.bind_label(entry);
        builder.push(VmInstruction::Gep {
            dst: 1,
            base: 0,
            offset: 8,
        });
        builder.push(VmInstruction::Load {
            dst: 2,
            ptr: 1,
            width: 32,
        });
        builder.push(VmInstruction::Ret { src: 2 });
        let function = builder.finish().expect("vm function");
        let fused = fuse_superinstructions(function, &profile.isa, &profile.lowering);

        assert_eq!(fused.profile_instructions[0], "gep_load");
        assert!(matches!(fused.instructions[0], VmInstruction::SuperGepLoad { .. }));
        let image = BytecodeEncoder::new(&profile).encode(&fused).expect("bytecode");

        assert!(image.debug_dump.contains("gep_load width=32"));
    }

    #[test]
    fn super_gep_load_does_not_fuse_when_pointer_has_extra_use() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("super_gep_load_no_fuse", 4, 32);
        let entry = builder.new_label();
        builder.bind_label(entry);
        builder.push(VmInstruction::Gep {
            dst: 1,
            base: 0,
            offset: 8,
        });
        builder.push(VmInstruction::Load {
            dst: 2,
            ptr: 1,
            width: 32,
        });
        builder.push(VmInstruction::Mov {
            dst: 3,
            src: 1,
            width: 64,
        });
        builder.push(VmInstruction::Ret { src: 2 });
        let function = builder.finish().expect("vm function");
        let fused = fuse_superinstructions(function, &profile.isa, &profile.lowering);

        assert_eq!(fused.profile_instructions[0], "gep");
        assert_eq!(fused.profile_instructions[1], "load");
    }

    #[test]
    fn super_load_add_fusion_uses_wide_record() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("super_load_iadd", 4, 32);
        let entry = builder.new_label();
        builder.bind_label(entry);
        builder.push(VmInstruction::Load {
            dst: 2,
            ptr: 0,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: 3,
            lhs: 2,
            rhs: 1,
            width: 32,
        });
        builder.push(VmInstruction::Ret { src: 3 });
        let function = builder.finish().expect("vm function");
        let fused = fuse_superinstructions(function, &profile.isa, &profile.lowering);

        assert_eq!(fused.profile_instructions[0], "load_iadd");
        assert!(matches!(fused.instructions[0], VmInstruction::SuperLoadAdd { .. }));
        let image = BytecodeEncoder::new(&profile).encode(&fused).expect("bytecode");

        assert!(image.debug_dump.contains("load_iadd width=32"));
    }

    #[test]
    fn super_load_add_does_not_fuse_when_loaded_value_has_extra_use() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("super_load_iadd_no_fuse", 5, 32);
        let entry = builder.new_label();
        builder.bind_label(entry);
        builder.push(VmInstruction::Load {
            dst: 2,
            ptr: 0,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: 3,
            lhs: 2,
            rhs: 1,
            width: 32,
        });
        builder.push(VmInstruction::Mov {
            dst: 4,
            src: 2,
            width: 32,
        });
        builder.push(VmInstruction::Ret { src: 3 });
        let function = builder.finish().expect("vm function");
        let fused = fuse_superinstructions(function, &profile.isa, &profile.lowering);

        assert_eq!(fused.profile_instructions[0], "load");
        assert_eq!(fused.profile_instructions[1], "iadd");
    }

    #[test]
    fn bytecode_package_contains_declared_segments() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let function = add_function();

        let image = BytecodeEncoder::new(&profile).encode(&function).expect("bytecode");

        assert_eq!(&image.bytes[..BYTECODE_MAGIC.len()], BYTECODE_MAGIC);
        assert_eq!(read_u32_le_for_test(&image.bytes, 8), BYTECODE_PACKAGE_VERSION);
        assert_eq!(read_u32_le_for_test(&image.bytes, 16), image.instruction_count);
        assert_eq!(read_u64_le_for_test(&image.bytes, 24), image.key);
        assert_eq!(read_u32_le_for_test(&image.bytes, 32) as usize, image.const_pool_offset);
        assert_eq!(read_u32_le_for_test(&image.bytes, 36) as usize, image.const_pool_len);
        assert_eq!(read_u32_le_for_test(&image.bytes, 40) as usize, image.code_offset);
        assert_eq!(read_u32_le_for_test(&image.bytes, 44) as usize, image.code_len);
        assert_eq!(read_u32_le_for_test(&image.bytes, 48) as usize, image.reloc_offset);
        assert_eq!(read_u32_le_for_test(&image.bytes, 52) as usize, image.reloc_len);
        assert_eq!(image.code_offset, BYTECODE_HEADER_LEN + image.const_pool_len);
        assert_eq!(image.reloc_offset, image.code_offset + image.code_len);
        assert_eq!(image.bytes.len(), image.reloc_offset + image.reloc_len);
        assert!(!image.code_bytes().is_empty());
        assert!(image.const_pool_len > 0);
        assert!(image.reloc_len > 0);
        assert_eq!(decrypted_const_pool_values_for_test(&image), Vec::<u64>::new());
        let encrypted_const_pool =
            &image.bytes[image.const_pool_offset..image.const_pool_offset + image.const_pool_len];
        let mut decrypted_const_pool = encrypted_const_pool.to_vec();
        decrypted_const_pool
            .iter_mut()
            .enumerate()
            .for_each(|(index, byte)| *byte ^= key_byte(image.key, index));
        assert_eq!(decrypted_const_pool, [0]);
        assert_ne!(encrypted_const_pool, decrypted_const_pool);
    }

    #[test]
    fn const_load_uses_encrypted_const_pool_index() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("const_pool_load", 1, 32);
        let entry = builder.new_label();
        builder.bind_label(entry);
        builder.push(VmInstruction::ConstLoad {
            dst: 0,
            value: 0x1234_5678,
            width: 32,
        });
        builder.push(VmInstruction::Ret { src: 0 });
        let function = builder.finish().expect("vm function");

        let image = BytecodeEncoder::new(&profile).encode(&function).expect("bytecode");
        let decoded = decode_for_test(&profile, &image);

        assert_eq!(decrypted_const_pool_values_for_test(&image), vec![0x1234_5678]);
        assert_eq!(
            profile.isa.by_opcode(decoded[0][0] as Opcode).unwrap().name,
            "const_load"
        );
        assert_eq!(&decoded[0][1..], &[0, 0, 32]);
        assert_eq!(execute_for_test(&profile, &image, &[]), 0x1234_5678);
    }

    #[test]
    fn decoder_pipeline_drives_encoded_bytes() {
        let profile_ror3 = ProfilePackage::builtin_test().expect("profile");
        let profile_ror5 = profile_with_decoder(
            &include_str!("../profiles/amice-simple-vmp/decoder.vm").replace("step ror amount=3", "step ror amount=5"),
        );
        verify_profile(&profile_ror3).expect("verified ror3 profile");
        verify_profile(&profile_ror5).expect("verified ror5 profile");
        let function = add_function();

        let image_ror3 = BytecodeEncoder::new(&profile_ror3)
            .encode(&function)
            .expect("ror3 bytecode");
        let image_ror5 = BytecodeEncoder::new(&profile_ror5)
            .encode(&function)
            .expect("ror5 bytecode");

        assert_ne!(image_ror3.code_bytes(), image_ror5.code_bytes());
        assert_eq!(execute_for_test(&profile_ror3, &image_ror3, &[4, 9]), 13);
        assert_eq!(execute_for_test(&profile_ror5, &image_ror5, &[4, 9]), 13);
    }

    #[test]
    fn integer_div_rem_handlers_round_trip() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("integer_div_rem", 8, 32);
        let entry = builder.new_label();
        builder.bind_label(entry);
        for (op, dst, lhs, rhs) in [
            (BinOp::UDiv, 8, 0, 1),
            (BinOp::SDiv, 9, 2, 3),
            (BinOp::URem, 10, 4, 5),
            (BinOp::SRem, 11, 6, 7),
        ] {
            builder.push(VmInstruction::Bin {
                op,
                dst,
                lhs,
                rhs,
                width: 32,
            });
        }
        builder.push(VmInstruction::MovImm {
            dst: 12,
            imm: 10,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: 13,
            lhs: 9,
            rhs: 11,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: 14,
            lhs: 13,
            rhs: 12,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: 15,
            lhs: 14,
            rhs: 8,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: 16,
            lhs: 15,
            rhs: 10,
            width: 32,
        });
        builder.push(VmInstruction::Ret { src: 16 });
        let function = builder.finish().expect("vm function");

        let image = BytecodeEncoder::new(&profile).encode(&function).expect("bytecode");
        let decoded_names = decode_for_test(&profile, &image)
            .into_iter()
            .map(|record| profile.isa.by_opcode(record[0] as Opcode).unwrap().name.as_str())
            .filter(|name| *name != "fake_nop")
            .take(4)
            .collect::<Vec<_>>();

        assert_eq!(decoded_names, ["iudiv", "isdiv", "iurem", "isrem"]);
        assert_eq!(
            execute_for_test(
                &profile,
                &image,
                &[100, 7, (-45_i32) as u32 as u64, 6, 100, 9, (-45_i32) as u32 as u64, 6]
            ),
            15
        );
    }

    #[test]
    fn bit_unpack_packs_operands_after_opcode() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let function = add_function();
        let image = BytecodeEncoder::new(&profile).encode(&function).expect("bytecode");
        let decoded_bytes = decoded_byte_stream_for_test(&profile, &image);
        let mut offset = 0;
        let mut packed_tokens = Vec::new();
        while offset < decoded_bytes.len() {
            packed_tokens.push(decode_varint_for_test(&decoded_bytes, &mut offset));
        }

        let add_opcode = profile
            .isa
            .by_semantic(&HandlerSemantic::Bin(BinOp::Add))
            .expect("add opcode")
            .opcode_for_site(image.key, 0) as u64;
        let ret_opcode = profile
            .isa
            .by_semantic(&HandlerSemantic::Ret)
            .expect("ret opcode")
            .opcode_for_site(image.key, 20) as u64;
        let fake_desc = profile.isa.by_semantic(&HandlerSemantic::Nop).expect("fake opcode");
        let flat_tokens = [
            add_opcode,
            0,
            0,
            1,
            32,
            fake_desc.opcode_for_site(image.key, 16) as u64,
            ret_opcode,
            0,
            fake_desc.opcode_for_site(image.key, 24) as u64,
            fake_desc.opcode_for_site(image.key, 28) as u64,
            fake_desc.opcode_for_site(image.key, 32) as u64,
        ];

        assert_ne!(packed_tokens, flat_tokens);
        assert!(packed_tokens.len() > flat_tokens.len());
        assert_eq!(
            decode_for_test(&profile, &image),
            vec![
                flat_tokens[..5].to_vec(),
                flat_tokens[5..6].to_vec(),
                flat_tokens[6..8].to_vec(),
                flat_tokens[8..9].to_vec(),
                flat_tokens[9..10].to_vec(),
                flat_tokens[10..].to_vec()
            ]
        );
    }

    #[test]
    fn key_stream_uses_byte_index() {
        let key = 0xa6d3_9f74_c2b1_580d;

        assert_ne!(key_byte(key, 0), key_byte(key, 1));
        assert_ne!(key_byte(key, 1), key_byte(key, 2));
    }

    #[test]
    fn opcode_alias_selection_uses_function_key() {
        let profile = profile_with_isa(&include_str!("../profiles/amice-simple-vmp/isa.vm").replace(
            "opcode alias [0x10, 0x2c, 0x5a, 0x6d, 0x7a]",
            "opcode alias [0x10, 0x1f0]",
        ));
        verify_profile(&profile).expect("profile with add aliases should verify");

        let mut seen = std::collections::HashSet::new();
        for index in 0..64 {
            let function = add_function_named(&format!("add_alias_{index}"));
            let image = BytecodeEncoder::new(&profile).encode(&function).expect("bytecode");
            let decoded = decode_for_test(&profile, &image);
            seen.insert(decoded[0][0]);
        }

        assert!(seen.contains(&0x10));
        assert!(seen.contains(&0x1f0));
    }

    #[test]
    fn bytecode_uses_vm_ir_profile_instruction_identity() {
        let alt_iadd = r#"instr iadd_alt(dst: vreg<i64>, lhs: vreg<i64>, rhs: vreg<i64>, width: imm<u8>) { # 第二条同语义整数加法处理器
opcode alias [0x1f1] # iadd_alt 使用独立操作码 0x1f1
semantic { # iadd_alt 保持与 iadd 相同的加法语义
reg[dst] = trunc_width(reg[lhs] + reg[rhs], width) # 加法结果按目标宽度掩码
pc = next # 执行继续到下一条字节码指令
} # 结束 iadd_alt 语义块
} # 结束 iadd_alt 指令
"#;
        let profile = profile_with_isa(
            &include_str!("../profiles/amice-simple-vmp/isa.vm")
                .replace("instr isub", &format!("{alt_iadd}instr isub")),
        );
        verify_profile(&profile).expect("profile with duplicate add semantic should verify");

        let mut builder = VmFunctionBuilder::new("add_alt", 2, 32);
        let entry = builder.new_label();
        builder.bind_label(entry);
        builder.push_profile(
            VmInstruction::Bin {
                op: BinOp::Add,
                dst: 0,
                lhs: 0,
                rhs: 1,
                width: 32,
            },
            "iadd_alt",
        );
        builder.push(VmInstruction::Ret { src: 0 });
        let function = builder.finish().expect("vm function");

        let image = BytecodeEncoder::new(&profile).encode(&function).expect("bytecode");
        let decoded = decode_for_test(&profile, &image);

        assert_eq!(profile.isa.by_opcode(decoded[0][0] as Opcode).unwrap().name, "iadd_alt");
        assert_ne!(profile.isa.by_opcode(decoded[0][0] as Opcode).unwrap().name, "iadd");
        assert!(image.debug_dump.contains("iadd_alt"));
        assert_eq!(execute_for_test(&profile, &image, &[4, 9]), 13);
    }

    #[test]
    fn module_polymorph_scope_uses_profile_key() {
        let default_profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&default_profile).expect("verified default profile");
        let module_profile = profile_with_runtime(&include_str!("../profiles/amice-simple-vmp/runtime.vm").replace(
            "polymorph.scope = func # 每个被保护函数独立派生 key、opcode 选择和 handler 克隆后缀",
            "polymorph.scope = module # 测试模块级多态密钥由 profile 派生",
        ));
        verify_profile(&module_profile).expect("verified module polymorph profile");
        let first = add_function_named("module_key_first");
        let second = add_function_named("module_key_second");

        let default_first = BytecodeEncoder::new(&default_profile)
            .encode(&first)
            .expect("default first bytecode");
        let default_second = BytecodeEncoder::new(&default_profile)
            .encode(&second)
            .expect("default second bytecode");
        let module_first = BytecodeEncoder::new(&module_profile)
            .encode(&first)
            .expect("module first bytecode");
        let module_second = BytecodeEncoder::new(&module_profile)
            .encode(&second)
            .expect("module second bytecode");

        assert_ne!(default_first.key, default_second.key);
        assert_eq!(module_first.key, module_second.key);
    }

    #[test]
    fn encoded_vm_internal_call_round_trips_lr() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("vm_call_lr", 3, 32);
        let entry = builder.new_label();
        let callee = builder.new_label();

        builder.bind_label(entry);
        builder.push(VmInstruction::MovImm {
            dst: 0,
            imm: 2,
            width: 32,
        });
        builder.push(VmInstruction::VmCall { target: callee });
        builder.push(VmInstruction::MovImm {
            dst: 1,
            imm: 5,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: 0,
            lhs: 0,
            rhs: 1,
            width: 32,
        });
        builder.push(VmInstruction::Ret { src: 0 });

        builder.bind_label(callee);
        builder.push(VmInstruction::MovImm {
            dst: 2,
            imm: 11,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Mul,
            dst: 0,
            lhs: 0,
            rhs: 2,
            width: 32,
        });
        builder.push(VmInstruction::VmRet);

        let function = builder.finish().expect("vm function");
        let image = BytecodeEncoder::new(&profile).encode(&function).expect("bytecode");

        assert_eq!(execute_for_test(&profile, &image, &[]), 27);
    }

    #[test]
    fn call_native_record_carries_profile_return_slots() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("native_multi_ret", 6, 64);
        let entry = builder.new_label();
        builder.bind_label(entry);
        builder.push(VmInstruction::CallNative {
            call_id: 7,
            args: vec![0, 1],
            returns: vec![
                NativeReturn { dst: 2, width: 64 },
                NativeReturn { dst: 3, width: 32 },
                NativeReturn { dst: 4, width: 16 },
            ],
        });
        builder.push(VmInstruction::Ret { src: 2 });

        let function = builder.finish().expect("vm function");
        let image = BytecodeEncoder::new(&profile).encode(&function).expect("bytecode");
        let decoded = decode_for_test(&profile, &image);
        let native = &decoded[0];

        assert_eq!(profile.isa.by_opcode(native[0] as Opcode).unwrap().name, "call_native");
        assert_eq!(native[1], 7);
        assert_eq!(native[2], 2);
        assert_eq!(&native[3..11], &[0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(native[11], 3);
        assert_eq!(&native[12..18], &[2, 64, 3, 32, 4, 16]);
        assert!(native[18..28].iter().all(|value| *value == 0));
    }

    #[test]
    fn encoded_loop_executes_with_builtin_profile() {
        let profile = ProfilePackage::builtin_test().expect("profile");
        verify_profile(&profile).expect("verified profile");
        let mut builder = VmFunctionBuilder::new("sum_masked_loop", 1, 32);
        let entry = builder.new_label();
        let loop_head = builder.new_label();
        let body = builder.new_label();
        let exit = builder.new_label();
        let acc = builder.alloc_vreg().expect("acc");
        let index = builder.alloc_vreg().expect("index");
        let cond = builder.alloc_vreg().expect("cond");
        let mixed = builder.alloc_vreg().expect("mixed");
        let mask = builder.alloc_vreg().expect("mask");
        let masked = builder.alloc_vreg().expect("masked");
        let one = builder.alloc_vreg().expect("one");

        builder.bind_label(entry);
        builder.push(VmInstruction::MovImm {
            dst: acc,
            imm: 0,
            width: 32,
        });
        builder.push(VmInstruction::MovImm {
            dst: index,
            imm: 0,
            width: 32,
        });
        builder.push(VmInstruction::Br { target: loop_head });

        builder.bind_label(loop_head);
        builder.push(VmInstruction::Icmp {
            pred: CmpPredicate::Slt,
            dst: cond,
            lhs: index,
            rhs: 0,
            width: 32,
        });
        builder.push(VmInstruction::BrCond {
            cond,
            then_label: body,
            else_label: exit,
        });

        builder.bind_label(body);
        builder.push(VmInstruction::Bin {
            op: BinOp::Xor,
            dst: mixed,
            lhs: index,
            rhs: 0,
            width: 32,
        });
        builder.push(VmInstruction::MovImm {
            dst: mask,
            imm: 7,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::And,
            dst: masked,
            lhs: mixed,
            rhs: mask,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: acc,
            lhs: acc,
            rhs: masked,
            width: 32,
        });
        builder.push(VmInstruction::MovImm {
            dst: one,
            imm: 1,
            width: 32,
        });
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: index,
            lhs: index,
            rhs: one,
            width: 32,
        });
        builder.push(VmInstruction::Br { target: loop_head });

        builder.bind_label(exit);
        builder.push(VmInstruction::Ret { src: acc });

        let function = builder.finish().expect("vm function");
        let image = BytecodeEncoder::new(&profile).encode(&function).expect("bytecode");

        assert_eq!(
            execute_for_test(&profile, &image, &[9]),
            (0..9).map(|i| (i ^ 9) & 7).sum()
        );
    }

    fn add_function() -> VmFunction {
        add_function_named("add")
    }

    fn add_function_named(name: &str) -> VmFunction {
        let mut builder = VmFunctionBuilder::new(name, 2, 32);
        let entry = builder.new_label();
        builder.bind_label(entry);
        builder.push(VmInstruction::Bin {
            op: BinOp::Add,
            dst: 0,
            lhs: 0,
            rhs: 1,
            width: 32,
        });
        builder.push(VmInstruction::Ret { src: 0 });
        builder.finish().expect("vm function")
    }

    fn profile_with_isa(isa: &str) -> ProfilePackage {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            isa,
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile")
    }

    fn profile_with_decoder(decoder: &str) -> ProfilePackage {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            decoder,
            include_str!("../profiles/amice-simple-vmp/runtime.vm"),
        )
        .expect("profile")
    }

    fn profile_with_runtime(runtime: &str) -> ProfilePackage {
        let manifest: Manifest =
            toml::from_str(include_str!("../profiles/amice-simple-vmp/manifest.toml")).expect("manifest");
        ProfilePackage::from_sources(
            manifest,
            include_str!("../profiles/amice-simple-vmp/abi.vm"),
            include_str!("../profiles/amice-simple-vmp/isa.vm"),
            include_str!("../profiles/amice-simple-vmp/lowering.vm"),
            include_str!("../profiles/amice-simple-vmp/bytecode.vm"),
            include_str!("../profiles/amice-simple-vmp/decoder.vm"),
            runtime,
        )
        .expect("profile")
    }

    fn execute_for_test(profile: &ProfilePackage, image: &BytecodeImage, args: &[u64]) -> u64 {
        let instructions = decode_records_for_test(profile, image);
        let pc_to_index = instructions
            .iter()
            .enumerate()
            .map(|(index, record)| (record.pc, index))
            .collect::<HashMap<_, _>>();
        let mut regs = [0_u64; 32];
        for (index, value) in args.iter().enumerate() {
            regs[index] = *value;
        }

        let mut pc = 0_usize;
        loop {
            let record_index = *pc_to_index.get(&pc).expect("valid bytecode pc");
            let record = &instructions[record_index];
            let operands = &record.operands;
            let semantic = &profile
                .isa
                .by_opcode(operands[0] as Opcode)
                .expect("known opcode")
                .semantic;

            match semantic {
                HandlerSemantic::MovImm => {
                    regs[operands[1] as usize] = mask_width(operands[2], operands[3] as u8);
                    pc += record.decoded_width;
                },
                HandlerSemantic::ConstLoad => {
                    let values = decrypted_const_pool_values_for_test(image);
                    regs[operands[1] as usize] = mask_width(values[operands[2] as usize], operands[3] as u8);
                    pc += record.decoded_width;
                },
                HandlerSemantic::Super(SuperOp::AddXor) => {
                    let dst = operands[1] as usize;
                    let lhs = regs[operands[2] as usize];
                    let rhs = regs[operands[3] as usize];
                    let xor_rhs = regs[operands[4] as usize];
                    let width = operands[5] as u8;
                    regs[dst] = mask_width(lhs.wrapping_add(rhs) ^ xor_rhs, width);
                    pc += record.decoded_width;
                },
                HandlerSemantic::Super(SuperOp::IcmpBrIf) => {
                    let pred = operands[1];
                    let lhs = regs[operands[2] as usize];
                    let rhs = regs[operands[3] as usize];
                    let width = operands[4] as u8;
                    pc = if eval_icmp_for_test(pred, lhs, rhs, width) {
                        operands[5] as usize
                    } else {
                        operands[6] as usize
                    };
                },
                HandlerSemantic::Super(SuperOp::GepLoad | SuperOp::LoadAdd) => {
                    unreachable!("not emitted by this test interpreter")
                },
                HandlerSemantic::Bin(op) => {
                    let dst = operands[1] as usize;
                    let lhs = regs[operands[2] as usize];
                    let rhs = regs[operands[3] as usize];
                    let width = operands[4] as u8;
                    let value = match op {
                        BinOp::Add => lhs.wrapping_add(rhs),
                        BinOp::Sub => lhs.wrapping_sub(rhs),
                        BinOp::Mul => lhs.wrapping_mul(rhs),
                        BinOp::UDiv => mask_width(lhs, width) / mask_width(rhs, width),
                        BinOp::SDiv => (sign_extend(lhs, width) / sign_extend(rhs, width)) as u64,
                        BinOp::URem => mask_width(lhs, width) % mask_width(rhs, width),
                        BinOp::SRem => (sign_extend(lhs, width) % sign_extend(rhs, width)) as u64,
                        BinOp::Xor => lhs ^ rhs,
                        BinOp::And => lhs & rhs,
                        BinOp::Or => lhs | rhs,
                        BinOp::Shl => lhs << (rhs & 63),
                        BinOp::LShr => lhs >> (rhs & 63),
                        BinOp::AShr => (sign_extend(lhs, width) >> (rhs & 63)) as u64,
                    };
                    regs[dst] = mask_width(value, width);
                    pc += record.decoded_width;
                },
                HandlerSemantic::Icmp => {
                    let pred = operands[1];
                    let dst = operands[2] as usize;
                    let lhs = regs[operands[3] as usize];
                    let rhs = regs[operands[4] as usize];
                    let width = operands[5] as u8;
                    regs[dst] = eval_icmp_for_test(pred, lhs, rhs, width) as u64;
                    pc += record.decoded_width;
                },
                HandlerSemantic::Alloca
                | HandlerSemantic::Load
                | HandlerSemantic::Store
                | HandlerSemantic::AtomicLoad
                | HandlerSemantic::AtomicStore
                | HandlerSemantic::AtomicRmw(_)
                | HandlerSemantic::CmpXchg
                | HandlerSemantic::Fence
                | HandlerSemantic::Gep
                | HandlerSemantic::CallNative
                | HandlerSemantic::IntUnary(_)
                | HandlerSemantic::IntTernary(_)
                | HandlerSemantic::FloatBin(_)
                | HandlerSemantic::FloatUnary(_)
                | HandlerSemantic::FloatCast(_)
                | HandlerSemantic::Fcmp => unreachable!("not emitted by this test"),
                HandlerSemantic::Nop => pc += record.decoded_width,
                HandlerSemantic::Br => pc = operands[1] as usize,
                HandlerSemantic::BrCond => {
                    pc = if regs[operands[1] as usize] != 0 {
                        operands[2] as usize
                    } else {
                        operands[3] as usize
                    };
                },
                HandlerSemantic::VmCall => {
                    regs[30] = (pc + record.decoded_width) as u64;
                    pc = operands[1] as usize;
                },
                HandlerSemantic::VmRet => pc = regs[30] as usize,
                HandlerSemantic::Ret => return regs[operands[1] as usize],
                HandlerSemantic::Mov | HandlerSemantic::Cast(_) => unreachable!("not emitted by this test"),
            }
        }
    }

    fn eval_icmp_for_test(pred: u64, lhs: u64, rhs: u64, width: u8) -> bool {
        match pred {
            0 => mask_width(lhs, width) == mask_width(rhs, width),
            1 => mask_width(lhs, width) != mask_width(rhs, width),
            2 => mask_width(lhs, width) > mask_width(rhs, width),
            3 => mask_width(lhs, width) >= mask_width(rhs, width),
            4 => mask_width(lhs, width) < mask_width(rhs, width),
            5 => mask_width(lhs, width) <= mask_width(rhs, width),
            6 => sign_extend(lhs, width) > sign_extend(rhs, width),
            7 => sign_extend(lhs, width) >= sign_extend(rhs, width),
            8 => sign_extend(lhs, width) < sign_extend(rhs, width),
            9 => sign_extend(lhs, width) <= sign_extend(rhs, width),
            _ => false,
        }
    }

    fn decrypted_const_pool_values_for_test(image: &BytecodeImage) -> Vec<u64> {
        let mut bytes = image.bytes[image.const_pool_offset..image.const_pool_offset + image.const_pool_len].to_vec();
        bytes
            .iter_mut()
            .enumerate()
            .for_each(|(index, byte)| *byte ^= key_byte(image.key, index));
        let mut offset = 0;
        let count = decode_varint_for_test(&bytes, &mut offset);
        (0..count)
            .map(|_| decode_varint_for_test(&bytes, &mut offset))
            .collect()
    }

    #[derive(Debug, Clone)]
    struct DecodedRecordForTest {
        pc: usize,
        decoded_width: usize,
        operands: Vec<u64>,
    }

    fn decode_for_test(profile: &ProfilePackage, image: &BytecodeImage) -> Vec<Vec<u64>> {
        decode_records_for_test(profile, image)
            .into_iter()
            .map(|record| record.operands)
            .collect()
    }

    fn decode_records_for_test(profile: &ProfilePackage, image: &BytecodeImage) -> Vec<DecodedRecordForTest> {
        let bytes = decoded_byte_stream_for_test(profile, image);
        let mut offset = 0;
        let mut decoded = Vec::with_capacity(image.instruction_count as usize);
        while decoded.len() < image.instruction_count as usize {
            let record_start = offset;
            let opcode = decode_varint_for_test(&bytes, &mut offset);
            let desc = profile.isa.by_opcode(opcode as Opcode).expect("known opcode");
            let mut operands = vec![opcode];
            for _ in 0..desc.operands {
                operands.push(decode_bitpacked_operand_for_test(&bytes, &mut offset));
            }
            offset = record_start + desc.decoded_width as usize;
            decoded.push(DecodedRecordForTest {
                pc: record_start,
                decoded_width: desc.decoded_width as usize,
                operands,
            });
        }
        decoded
    }

    fn decoded_byte_stream_for_test(profile: &ProfilePackage, image: &BytecodeImage) -> Vec<u8> {
        let mut bytes = image.code_bytes().to_vec();
        for step in &profile.decoder.steps {
            match step {
                DecoderStep::XorStream => {
                    bytes
                        .iter_mut()
                        .enumerate()
                        .for_each(|(index, byte)| *byte ^= key_byte(image.key, index));
                },
                DecoderStep::AddStream => {
                    bytes
                        .iter_mut()
                        .enumerate()
                        .for_each(|(index, byte)| *byte = byte.wrapping_sub(key_byte(image.key, index)));
                },
                DecoderStep::Rol { amount } => {
                    bytes
                        .iter_mut()
                        .for_each(|byte| *byte = byte.rotate_left(*amount as u32));
                },
                DecoderStep::Ror { amount } => {
                    bytes
                        .iter_mut()
                        .for_each(|byte| *byte = byte.rotate_right(*amount as u32));
                },
                DecoderStep::VarintDecode | DecoderStep::BitUnpack => {},
            }
        }

        bytes
    }

    fn decode_bitpacked_operand_for_test(bytes: &[u8], offset: &mut usize) -> u64 {
        let bit_width = decode_varint_for_test(bytes, offset);
        let mut value = 0_u64;
        let mut shift = 0_u64;
        while shift < bit_width && shift < 64 {
            let chunk = decode_varint_for_test(bytes, offset) & 0x7f;
            value |= chunk << shift;
            shift += 7;
        }
        value
    }

    fn decode_varint_for_test(bytes: &[u8], offset: &mut usize) -> u64 {
        let mut result = 0;
        let mut shift = 0;
        loop {
            let byte = bytes[*offset];
            *offset += 1;
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return result;
            }
            shift += 7;
        }
    }

    fn read_u32_le_for_test(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 header field"))
    }

    fn read_u64_le_for_test(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("u64 header field"))
    }

    fn mask_width(value: u64, width: u8) -> u64 {
        if width == 64 {
            value
        } else {
            value & ((1_u64 << width) - 1)
        }
    }

    fn sign_extend(value: u64, width: u8) -> i64 {
        if width == 64 {
            value as i64
        } else {
            let shift = 64 - width;
            ((value << shift) as i64) >> shift
        }
    }
}
