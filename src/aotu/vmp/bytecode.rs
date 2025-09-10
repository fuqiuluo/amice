/// VMP 字节码编码器
/// 将 AVM 指令序列编码为二进制字节码
use crate::aotu::vmp::isa::{VMPOpcode, VMPValue};
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
pub struct VMPBytecodeEncoder {
    opcode_map: HashMap<BytecodeOp, u16>,
    value_type_map: HashMap<BytecodeValueType, u8>,
    /// 标签到位置的映射（用于跳转指令）
    label_positions: HashMap<String, u32>,
}

impl VMPBytecodeEncoder {
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

    pub fn get_value_type_map(&self) -> &HashMap<BytecodeValueType, u8> {
        &self.value_type_map
    }

    pub fn get_opcode_map(&self) -> &HashMap<BytecodeOp, u16> {
        &self.opcode_map
    }

    /// 将指令序列编码为字节码
    pub fn encode_instructions(&mut self, instructions: &[VMPOpcode]) -> Result<Vec<u8>> {
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

        let mut inst = "".to_string();
        // 编码每条指令
        for (index, instruction) in instructions.iter().enumerate() {
            if log_enabled!(Level::Debug) {
                inst = format!("{}\n {}: {}", inst, index, instruction);
            }
            self.encode_instruction(&mut cursor, instruction)?;
        }

        if log_enabled!(Level::Debug) {
            debug!("Generated {} bytes of bytecode: {}", bytecode.len(), inst);
        }
        Ok(bytecode)
    }

    fn siphash_u64<T: Hash>(data: &T) -> u64 {
        let mut hasher = DefaultHasher::new();
        data.hash(&mut hasher);
        hasher.finish()
    }

    /// 收集所有标签位置
    fn collect_labels(&mut self, instructions: &[VMPOpcode]) -> Result<()> {
        let mut position = (size_of_val(&VMP_VERSION) + VMP_NAME.len()) as u32; // 从文件头大小开始计算

        for instruction in instructions {
            if let VMPOpcode::Label { name } = instruction {
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
    fn calculate_instruction_size(&self, instruction: &VMPOpcode) -> Result<u32> {
        let size = 2 + match instruction {
            VMPOpcode::Push { value } => 1 + self.calculate_value_size(value)?, // opcode + type + value
            VMPOpcode::Pop => 0,
            VMPOpcode::PopToReg { reg } => size_of_val(reg), // opcode + reg(u32)
            VMPOpcode::PushFromReg { reg } => size_of_val(reg),
            VMPOpcode::ClearReg { reg } => size_of_val(reg),

            VMPOpcode::Alloca { size } => size_of_val(size), // opcode + size(u64)
            VMPOpcode::Alloca2 => 0,
            VMPOpcode::Store { address } => size_of_val(address), // opcode + address(u64)
            VMPOpcode::StoreValue => 0,
            VMPOpcode::Load { address } => size_of_val(address),
            VMPOpcode::LoadValue => 0,

            VMPOpcode::Call { arg_num, is_void, .. } => {
                size_of::<u64>() + size_of_val(is_void) + size_of_val(arg_num) // opcode + hash(name) + is_void + arg_num
            },

            VMPOpcode::Add { .. } => 2, // opcode + flags
            VMPOpcode::Sub => 0,
            VMPOpcode::Mul => 0,
            VMPOpcode::Div => 0,

            VMPOpcode::Ret => 0,

            VMPOpcode::Nop => 0,
            VMPOpcode::Swap => 0,
            VMPOpcode::Dup => 0,
            VMPOpcode::TypeCheckInt { width } => size_of_val(width), // opcode + width

            VMPOpcode::Jump { target } => size_of::<u64>(), // opcode + hash(target)
            VMPOpcode::JumpIf { target } => size_of::<u64>(),
            VMPOpcode::JumpIfNot { target } => size_of::<u64>(),

            VMPOpcode::ICmpEq => 0,
            VMPOpcode::ICmpNe => 0,
            VMPOpcode::ICmpSlt => 0,
            VMPOpcode::ICmpSle => 0,
            VMPOpcode::ICmpSgt => 0,
            VMPOpcode::ICmpSge => 0,
            VMPOpcode::ICmpUlt => 0,
            VMPOpcode::ICmpUle => 0,
            VMPOpcode::ICmpUgt => 0,
            VMPOpcode::ICmpUge => 0,

            VMPOpcode::And => 0,
            VMPOpcode::Or => 0,
            VMPOpcode::Xor => 0,
            VMPOpcode::Shl => 0,
            VMPOpcode::LShr => 0,
            VMPOpcode::AShr => 0,

            VMPOpcode::Trunc { target_width } => size_of_val(target_width), // opcode + target_width
            VMPOpcode::ZExt { target_width } => size_of_val(target_width),
            VMPOpcode::SExt { target_width } => size_of_val(target_width),
            VMPOpcode::FPToSI { target_width } => size_of_val(target_width),
            VMPOpcode::FPToUI { target_width } => size_of_val(target_width),
            VMPOpcode::SIToFP { is_double } => size_of_val(is_double), // opcode + is_double
            VMPOpcode::UIToFP { is_double } => size_of_val(is_double),

            VMPOpcode::Label { name } => size_of::<u64>(), // opcode + hash(name)
            VMPOpcode::MetaGVar { name, .. } => 0,
        };

        Ok(size as u32)
    }

    /// 计算值的字节码大小
    fn calculate_value_size(&self, value: &VMPValue) -> Result<usize> {
        let size = match value {
            VMPValue::Undef => 0,
            VMPValue::I1(_) => 1,
            VMPValue::I8(_) => 1,
            VMPValue::I16(_) => 2,
            VMPValue::I32(_) => 4,
            VMPValue::I64(_) => 8,
            VMPValue::F32(_) => 4,
            VMPValue::F64(_) => 8,
            VMPValue::Ptr(_) => 8,
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
    fn encode_instruction(&self, cursor: &mut Cursor<&mut Vec<u8>>, instruction: &VMPOpcode) -> Result<()> {
        let bytecode = instruction.to_bytecode();
        cursor.write_all(&self.opcode_map[&bytecode].to_le_bytes())?;
        match instruction {
            VMPOpcode::Push { value } => {
                self.encode_value(cursor, value)?;
            },

            VMPOpcode::Pop => {
                // no additional data
            },

            VMPOpcode::PopToReg { reg } => {
                cursor.write_all(&reg.to_le_bytes())?;
            },

            VMPOpcode::PushFromReg { reg } => {
                cursor.write_all(&reg.to_le_bytes())?;
            },

            VMPOpcode::ClearReg { reg } => {
                cursor.write_all(&reg.to_le_bytes())?;
            },

            VMPOpcode::Alloca { size } => {
                cursor.write_all(&size.to_le_bytes())?;
            },

            VMPOpcode::Alloca2 => {},

            VMPOpcode::Store { address } => {
                cursor.write_all(&address.to_le_bytes())?;
            },

            VMPOpcode::StoreValue => {},

            VMPOpcode::Load { address } => {
                cursor.write_all(&address.to_le_bytes())?;
            },

            VMPOpcode::LoadValue => {},

            VMPOpcode::Call {
                function_name,
                is_void,
                arg_num,
                ..
            } => {
                // function hash
                let hash = Self::siphash_u64(function_name);
                cursor.write_all(&hash.to_le_bytes())?;
                // 是否为void函数
                cursor.write_all(&[if *is_void { 1 } else { 0 }])?;
                // 参数数量
                cursor.write_all(&arg_num.to_le_bytes())?;
            },

            VMPOpcode::Add { nsw, nuw } => {
                let flags = (if *nsw { 1u8 } else { 0u8 }) | (if *nuw { 2u8 } else { 0u8 });
                cursor.write_all(&[flags])?;
                cursor.write_all(&[0])?; // 填充到2字节
            },

            VMPOpcode::Sub => {},
            VMPOpcode::Mul => {},
            VMPOpcode::Div => {},

            VMPOpcode::Ret => {},

            VMPOpcode::Nop => {},
            VMPOpcode::Swap => {},
            VMPOpcode::Dup => {},

            VMPOpcode::TypeCheckInt { width } => {
                cursor.write_all(&width.to_le_bytes())?;
            },

            VMPOpcode::Jump { target } => {
                let target_hash = Self::siphash_u64(target);
                cursor.write_all(&target_hash.to_le_bytes())?;
            },

            VMPOpcode::JumpIf { target } => {
                let target_hash = Self::siphash_u64(target);
                cursor.write_all(&target_hash.to_le_bytes())?;
            },

            VMPOpcode::JumpIfNot { target } => {
                let target_hash = Self::siphash_u64(target);
                cursor.write_all(&target_hash.to_le_bytes())?;
            },

            // 比较指令
            VMPOpcode::ICmpEq => {},
            VMPOpcode::ICmpNe => {},
            VMPOpcode::ICmpSlt => {},
            VMPOpcode::ICmpSle => {},
            VMPOpcode::ICmpSgt => {},
            VMPOpcode::ICmpSge => {},
            VMPOpcode::ICmpUlt => {},
            VMPOpcode::ICmpUle => {},
            VMPOpcode::ICmpUgt => {},
            VMPOpcode::ICmpUge => {},

            // 位运算指令
            VMPOpcode::And => {},
            VMPOpcode::Or => {},
            VMPOpcode::Xor => {},
            VMPOpcode::Shl => {},
            VMPOpcode::LShr => {},
            VMPOpcode::AShr => {},

            // 类型转换指令
            VMPOpcode::Trunc { target_width } => {
                cursor.write_all(&target_width.to_le_bytes())?;
            },
            VMPOpcode::ZExt { target_width } => {
                cursor.write_all(&target_width.to_le_bytes())?;
            },
            VMPOpcode::SExt { target_width } => {
                cursor.write_all(&target_width.to_le_bytes())?;
            },
            VMPOpcode::FPToSI { target_width } => {
                cursor.write_all(&target_width.to_le_bytes())?;
            },
            VMPOpcode::FPToUI { target_width } => {
                cursor.write_all(&target_width.to_le_bytes())?;
            },
            VMPOpcode::SIToFP { is_double } => {
                cursor.write_all(&[if *is_double { 1 } else { 0 }])?;
            },
            VMPOpcode::UIToFP { is_double } => {
                cursor.write_all(&[if *is_double { 1 } else { 0 }])?;
            },

            VMPOpcode::Label { name } => {
                let hash = Self::siphash_u64(name);
                cursor.write_all(&hash.to_le_bytes())?;
            },

            VMPOpcode::MetaGVar { .. } => {},
        }

        Ok(())
    }

    /// 编码值
    fn encode_value(&self, cursor: &mut Cursor<&mut Vec<u8>>, value: &VMPValue) -> Result<()> {
        let bytecode_value_type = value.to_bytecode_value_type();
        cursor.write_all(&self.value_type_map[&bytecode_value_type].to_le_bytes())?;
        match value {
            VMPValue::Undef => {
                // no additional data
            },
            VMPValue::I1(v) => {
                cursor.write_all(&[if *v { 1 } else { 0 }])?;
            },
            VMPValue::I8(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            VMPValue::I16(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            VMPValue::I32(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            VMPValue::I64(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            VMPValue::F32(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            VMPValue::F64(v) => {
                cursor.write_all(&v.to_le_bytes())?;
            },
            VMPValue::Ptr(v) => {
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

impl Default for VMPBytecodeEncoder {
    fn default() -> Self {
        Self::new()
    }
}
