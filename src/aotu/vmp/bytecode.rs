/// VMP 字节码编码器
/// 将 AVM 指令序列编码为二进制字节码
use crate::aotu::vmp::avm::{AVMOpcode, AVMValue};
use anyhow::Result;
use log::{Level, debug, log_enabled};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Cursor, Write};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

const VMP_NAME: &str = "VMP1";
const VMP_VERSION: u32 = 1;

/// 字节码操作码常量
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter)]
pub enum BytecodeOp {
    Push,
    Pop,
    PopToReg,
    PushFromReg,
    ClearReg,

    Alloca,
    Alloca2,
    Store,
    StoreValue,
    Load,
    LoadValue,

    Call,

    Add,
    Sub,
    Mul,
    Div,

    Ret,

    Nop,
    Swap,
    Dup,
    TypeCheckInt,

    Jump,
    JumpIf,
    JumpIfNot,

    ICmpEq,
    ICmpNe,
    ICmpSlt,
    ICmpSle,
    ICmpSgt,
    ICmpSge,
    ICmpUlt,
    ICmpUle,
    ICmpUgt,
    ICmpUge,

    And,
    Or,
    Xor,
    Shl,
    LShr,
    AShr,

    Trunc,
    ZExt,
    SExt,
    FPToSI,
    FPToUI,
    SIToFP,
    UIToFP,

    Label,
    MetaGVar,
}

/// 字节码值类型标识符
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter)]
pub enum BytecodeValueType {
    Undef,
    I1,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
    Ptr,
}

/// 字节码编码器
pub struct BytecodeEncoder {
    opcode_map: HashMap<BytecodeOp, u16>,
    value_type_map: HashMap<BytecodeValueType, u8>,
    /// 标签到位置的映射（用于跳转指令）
    label_positions: HashMap<String, u32>,
}

impl BytecodeEncoder {
    pub fn new() -> Self {
        let mut encoder = Self {
            opcode_map: HashMap::new(),
            value_type_map: HashMap::new(),
            label_positions: HashMap::new(),
        };
        encoder.init_mapping();
        encoder
    }

    fn init_mapping(&mut self) {
        BytecodeOp::iter().enumerate().for_each(|(index, op)| {
            self.opcode_map.insert(op, index as u16);
        });
        BytecodeValueType::iter().enumerate().for_each(|(index, vt)| {
            self.value_type_map.insert(vt, index as u8);
        });
    }

    /// 将指令序列编码为字节码
    pub fn encode_instructions(&mut self, instructions: &[AVMOpcode]) -> Result<Vec<u8>> {
        if log_enabled!(Level::Debug) {
            debug!("Encoding {} instructions to bytecode", instructions.len());
        }

        // 收集所有标签位置
        self.collect_labels(instructions)?;

        // 编码指令
        let mut bytecode = Vec::new();
        let mut cursor = Cursor::new(&mut bytecode);

        // 写入文件头
        self.write_header(&mut cursor)?;

        // 编码每条指令
        for (index, instruction) in instructions.iter().enumerate() {
            if log_enabled!(Level::Debug) {
                debug!("Encoding instruction {}: {}", index, instruction);
            }
            self.encode_instruction(&mut cursor, instruction)?;
        }

        if log_enabled!(Level::Debug) {
            debug!("Generated {} bytes of bytecode", bytecode.len());
        }
        Ok(bytecode)
    }

    fn siphash_u64<T: Hash>(data: &T) -> u64 {
        let mut hasher = DefaultHasher::new();
        data.hash(&mut hasher);
        hasher.finish()
    }

    /// 收集所有标签位置
    fn collect_labels(&mut self, instructions: &[AVMOpcode]) -> Result<()> {
        let mut position = (size_of_val(&VMP_VERSION) + VMP_NAME.len()) as u32; // 从文件头大小开始计算

        for instruction in instructions {
            if let AVMOpcode::Label { name } = instruction {
                self.label_positions.insert(name.clone(), position);
            }
            position += self.calculate_instruction_size(instruction)?;
        }

        if log_enabled!(Level::Debug) {
            debug!("Collected {} labels", self.label_positions.len());
        }

        Ok(())
    }

    /// 计算指令的字节码大小
    fn calculate_instruction_size(&self, instruction: &AVMOpcode) -> Result<u32> {
        let size = match instruction {
            AVMOpcode::Push { value } => 1 + 1 + self.calculate_value_size(value)?, // opcode + type + value
            AVMOpcode::Pop => 1,
            AVMOpcode::PopToReg { .. } => 1 + 4, // opcode + reg(u32)
            AVMOpcode::PushFromReg { .. } => 1 + 4,
            AVMOpcode::ClearReg { .. } => 1 + 4,

            AVMOpcode::Alloca { .. } => 1 + 8, // opcode + size(u64)
            AVMOpcode::Alloca2 => 1,
            AVMOpcode::Store { .. } => 1 + 8, // opcode + address(u64)
            AVMOpcode::StoreValue => 1,
            AVMOpcode::Load { .. } => 1 + 8,
            AVMOpcode::LoadValue => 1,

            AVMOpcode::Call {
                function_name, arg_num, ..
            } => {
                1 + 4 + function_name.len() + 1 + 4 // opcode + name_len + name + is_void + arg_num
            },

            AVMOpcode::Add { .. } => 1 + 2, // opcode + flags
            AVMOpcode::Sub => 1,
            AVMOpcode::Mul => 1,
            AVMOpcode::Div => 1,

            AVMOpcode::Ret => 1,

            AVMOpcode::Nop => 1,
            AVMOpcode::Swap => 1,
            AVMOpcode::Dup => 1,
            AVMOpcode::TypeCheckInt { .. } => 1 + 4, // opcode + width

            AVMOpcode::Jump { target } => 1 + 4 + target.len(), // opcode + name_len + name
            AVMOpcode::JumpIf { target } => 1 + 4 + target.len(),
            AVMOpcode::JumpIfNot { target } => 1 + 4 + target.len(),

            AVMOpcode::ICmpEq => 1,
            AVMOpcode::ICmpNe => 1,
            AVMOpcode::ICmpSlt => 1,
            AVMOpcode::ICmpSle => 1,
            AVMOpcode::ICmpSgt => 1,
            AVMOpcode::ICmpSge => 1,
            AVMOpcode::ICmpUlt => 1,
            AVMOpcode::ICmpUle => 1,
            AVMOpcode::ICmpUgt => 1,
            AVMOpcode::ICmpUge => 1,

            AVMOpcode::And => 1,
            AVMOpcode::Or => 1,
            AVMOpcode::Xor => 1,
            AVMOpcode::Shl => 1,
            AVMOpcode::LShr => 1,
            AVMOpcode::AShr => 1,

            AVMOpcode::Trunc { .. } => 1 + 4, // opcode + target_width
            AVMOpcode::ZExt { .. } => 1 + 4,
            AVMOpcode::SExt { .. } => 1 + 4,
            AVMOpcode::FPToSI { .. } => 1 + 4,
            AVMOpcode::FPToUI { .. } => 1 + 4,
            AVMOpcode::SIToFP { .. } => 1 + 1, // opcode + is_double
            AVMOpcode::UIToFP { .. } => 1 + 1,

            AVMOpcode::Label { name } => 1 + 4 + name.len(), // opcode + name_len + name
            AVMOpcode::MetaGVar { name, .. } => 1 + 4 + 4 + name.len(), // opcode + reg + name_len + name
        };

        Ok(size as u32)
    }

    /// 计算值的字节码大小
    fn calculate_value_size(&self, value: &AVMValue) -> Result<usize> {
        let size = match value {
            AVMValue::Undef => 0,
            AVMValue::I1(_) => 1,
            AVMValue::I8(_) => 1,
            AVMValue::I16(_) => 2,
            AVMValue::I32(_) => 4,
            AVMValue::I64(_) => 8,
            AVMValue::F32(_) => 4,
            AVMValue::F64(_) => 8,
            AVMValue::Ptr(_) => 8,
        };
        Ok(size)
    }

    /// 写入文件头
    fn write_header(&self, cursor: &mut Cursor<&mut Vec<u8>>) -> Result<()> {
        // 魔数
        cursor.write_all(VMP_NAME.as_bytes())?;
        // 版本号
        cursor.write_all(&VMP_VERSION.to_le_bytes())?;
        Ok(())
    }

    /// 编码单条指令
    fn encode_instruction(&self, cursor: &mut Cursor<&mut Vec<u8>>, instruction: &AVMOpcode) -> Result<()> {
        let bytecode = instruction.to_bytecode();
        cursor.write_all(&self.opcode_map[&bytecode].to_le_bytes())?;
        match instruction {
            AVMOpcode::Push { value } => {
                self.encode_value(cursor, value)?;
            },

            AVMOpcode::Pop => {
                // no additional data
            },

            AVMOpcode::PopToReg { reg } => {
                cursor.write_all(&reg.to_le_bytes())?;
            },

            AVMOpcode::PushFromReg { reg } => {
                cursor.write_all(&reg.to_le_bytes())?;
            },

            AVMOpcode::ClearReg { reg } => {
                cursor.write_all(&reg.to_le_bytes())?;
            },

            AVMOpcode::Alloca { size } => {
                cursor.write_all(&(*size as u64).to_le_bytes())?;
            },

            AVMOpcode::Alloca2 => {},

            AVMOpcode::Store { address } => {
                cursor.write_all(&(*address as u64).to_le_bytes())?;
            },

            AVMOpcode::StoreValue => {},

            AVMOpcode::Load { address } => {
                cursor.write_all(&(*address as u64).to_le_bytes())?;
            },

            AVMOpcode::LoadValue => {},

            AVMOpcode::Call {
                function_name,
                is_void,
                arg_num,
                ..
            } => {
                // 函数名长度和内容
                cursor.write_all(&(function_name.len() as u32).to_le_bytes())?;
                cursor.write_all(function_name.as_bytes())?;
                // 是否为void函数
                cursor.write_all(&[if *is_void { 1 } else { 0 }])?;
                // 参数数量
                cursor.write_all(&arg_num.to_le_bytes())?;
            },

            AVMOpcode::Add { nsw, nuw } => {
                let flags = (if *nsw { 1u8 } else { 0u8 }) | (if *nuw { 2u8 } else { 0u8 });
                cursor.write_all(&[flags])?;
                cursor.write_all(&[0])?; // 填充到2字节
            },

            AVMOpcode::Sub => {},
            AVMOpcode::Mul => {},
            AVMOpcode::Div => {},

            AVMOpcode::Ret => {},

            AVMOpcode::Nop => {},
            AVMOpcode::Swap => {},
            AVMOpcode::Dup => {},

            AVMOpcode::TypeCheckInt { width } => {
                cursor.write_all(&width.to_le_bytes())?;
            },

            AVMOpcode::Jump { target } => {
                self.encode_string(cursor, target)?;
            },

            AVMOpcode::JumpIf { target } => {
                self.encode_string(cursor, target)?;
            },

            AVMOpcode::JumpIfNot { target } => {
                self.encode_string(cursor, target)?;
            },

            // 比较指令
            AVMOpcode::ICmpEq => {},
            AVMOpcode::ICmpNe => {},
            AVMOpcode::ICmpSlt => {},
            AVMOpcode::ICmpSle => {},
            AVMOpcode::ICmpSgt => {},
            AVMOpcode::ICmpSge => {},
            AVMOpcode::ICmpUlt => {},
            AVMOpcode::ICmpUle => {},
            AVMOpcode::ICmpUgt => {},
            AVMOpcode::ICmpUge => {},

            // 位运算指令
            AVMOpcode::And => {},
            AVMOpcode::Or => {},
            AVMOpcode::Xor => {},
            AVMOpcode::Shl => {},
            AVMOpcode::LShr => {},
            AVMOpcode::AShr => {},

            // 类型转换指令
            AVMOpcode::Trunc { target_width } => {
                cursor.write_all(&target_width.to_le_bytes())?;
            },
            AVMOpcode::ZExt { target_width } => {
                cursor.write_all(&target_width.to_le_bytes())?;
            },
            AVMOpcode::SExt { target_width } => {
                cursor.write_all(&target_width.to_le_bytes())?;
            },
            AVMOpcode::FPToSI { target_width } => {
                cursor.write_all(&target_width.to_le_bytes())?;
            },
            AVMOpcode::FPToUI { target_width } => {
                cursor.write_all(&target_width.to_le_bytes())?;
            },
            AVMOpcode::SIToFP { is_double } => {
                cursor.write_all(&[if *is_double { 1 } else { 0 }])?;
            },
            AVMOpcode::UIToFP { is_double } => {
                cursor.write_all(&[if *is_double { 1 } else { 0 }])?;
            },

            AVMOpcode::Label { name } => {
                self.encode_string(cursor, name)?;
            },

            AVMOpcode::MetaGVar { .. } => {},
        }

        Ok(())
    }

    /// 编码值
    fn encode_value(&self, cursor: &mut Cursor<&mut Vec<u8>>, value: &AVMValue) -> Result<()> {
        let bytecode_value_type = value.to_bytecode_value_type();
        cursor.write_all(&self.value_type_map[&bytecode_value_type].to_le_bytes())?;
        match value {
            AVMValue::Undef => {
                // no additional data
            },
            AVMValue::I1(v) => {
                cursor.write_all(&[if *v { 1 } else { 0 }])?;
            },
            AVMValue::I8(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            AVMValue::I16(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            AVMValue::I32(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            AVMValue::I64(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            AVMValue::F32(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            AVMValue::F64(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            AVMValue::Ptr(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
        }
        Ok(())
    }

    /// 编码字符串
    fn encode_string(&self, cursor: &mut Cursor<&mut Vec<u8>>, s: &str) -> Result<()> {
        cursor.write_all(&(s.len() as u32).to_le_bytes())?;
        cursor.write_all(s.as_bytes())?;
        Ok(())
    }
}

impl Default for BytecodeEncoder {
    fn default() -> Self {
        Self::new()
    }
}
