use crate::aotu::vmp::bytecode::{BytecodeOp, BytecodeValueType};
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::values::FunctionValue;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VMPOpcode {
    /// 将一个 VMPValue 压入栈顶
    ///
    /// 栈: [] -> [value]
    Push {
        value: VMPValue,
    },
    /// 从栈顶弹出一个 VMPValue
    ///
    /// 栈: [value] -> []
    Pop,
    /// 将栈顶的值弹出并写入指定编号的寄存器（寄存器宽度为 8 字节）
    ///
    /// 栈: [value] -> []
    PopToReg {
        reg: u32,
    },
    /// 将指定寄存器的值压入栈
    ///
    /// 栈: [] -> [value]
    PushFromReg {
        reg: u32,
    },
    /// 清空指定寄存器
    ///
    /// 栈: [] -> []
    ClearReg {
        reg: u32,
    },

    /// 分配 n 个 Value 槽，返回其基址指针并压入栈
    ///
    /// 栈: [] -> [ptr]
    Alloca {
        size: usize,
    },
    /// 从栈顶弹出一个 VMPValue 作为大小，分配对应数量的 Value 槽，返回其基址指针并压入栈
    ///
    /// 栈: [size] -> [ptr]
    Alloca2,

    Store {
        address: usize,
    },
    /// 存储一个 VMPValue；结构体或向量请勿使用该指令
    ///
    /// 栈: [value, ptr] -> []
    StoreValue,
    Load {
        address: usize,
    },
    /// 加载一个 VMPValue；结构体或向量请勿使用该指令
    ///
    /// 栈: [ptr] -> [value]
    LoadValue,

    Call {
        function_name: String,
        #[serde(skip)] // LLVMValueRef 不能序列化，运行时需要重新解析
        function: Option<LLVMValueRef>,
        is_void: bool,
        arg_num: u32,
        #[serde(skip)] // 同样跳过，运行时重新解析
        args: Vec<LLVMValueRef>,
    },

    // 算术运算（要求同类型）
    Add {
        nsw: bool,
        nuw: bool,
    },
    Sub,
    Mul,
    Div,

    // 控制流
    Ret, // 函数返回，退栈一个值作为返回值

    // 虚拟指令
    Nop,
    Swap,
    Dup,

    /// 检查栈顶值的类型是否为指定宽度的基本类型（整型或浮点型）
    /// 若类型不符则抛出运行时错误
    /// 栈: [value] -> [value]
    TypeCheckInt {
        width: u32,
    },

    // 跳转指令
    Jump {
        target: String,
    },
    JumpIf {
        target: String,
    },
    JumpIfNot {
        target: String,
    },

    // 比较指令
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

    // 位运算
    And,
    Or,
    Xor,
    Shl,
    LShr,
    AShr,

    // 类型转换
    Trunc {
        target_width: u32,
    },
    ZExt {
        target_width: u32,
    },
    SExt {
        target_width: u32,
    },
    FPToSI {
        target_width: u32,
    },
    FPToUI {
        target_width: u32,
    },
    SIToFP {
        is_double: bool,
    },
    UIToFP {
        is_double: bool,
    },

    // 标签（用于跳转目标）
    Label {
        name: String,
    },

    // 元信息
    MetaGVar {
        reg: u32,
        name: String,
    },
}

impl VMPOpcode {
    pub fn to_bytecode(&self) -> BytecodeOp {
        match self {
            VMPOpcode::Push { .. } => BytecodeOp::Push,
            VMPOpcode::Pop => BytecodeOp::Pop,
            VMPOpcode::PopToReg { .. } => BytecodeOp::PopToReg,
            VMPOpcode::PushFromReg { .. } => BytecodeOp::PushFromReg,
            VMPOpcode::ClearReg { .. } => BytecodeOp::ClearReg,
            VMPOpcode::Alloca { .. } => BytecodeOp::Alloca,
            VMPOpcode::Alloca2 => BytecodeOp::Alloca2,
            VMPOpcode::Store { .. } => BytecodeOp::Store,
            VMPOpcode::StoreValue => BytecodeOp::StoreValue,
            VMPOpcode::Load { .. } => BytecodeOp::Load,
            VMPOpcode::LoadValue => BytecodeOp::LoadValue,
            VMPOpcode::Call { .. } => BytecodeOp::Call,
            VMPOpcode::Add { .. } => BytecodeOp::Add,
            VMPOpcode::Sub => BytecodeOp::Sub,
            VMPOpcode::Mul => BytecodeOp::Mul,
            VMPOpcode::Div => BytecodeOp::Div,
            VMPOpcode::Ret => BytecodeOp::Ret,
            VMPOpcode::Nop => BytecodeOp::Nop,
            VMPOpcode::Swap => BytecodeOp::Swap,
            VMPOpcode::Dup => BytecodeOp::Dup,
            VMPOpcode::TypeCheckInt { .. } => BytecodeOp::TypeCheckInt,
            VMPOpcode::Jump { .. } => BytecodeOp::Jump,
            VMPOpcode::JumpIf { .. } => BytecodeOp::JumpIf,
            VMPOpcode::JumpIfNot { .. } => BytecodeOp::JumpIfNot,
            VMPOpcode::ICmpEq => BytecodeOp::ICmpEq,
            VMPOpcode::ICmpNe => BytecodeOp::ICmpNe,
            VMPOpcode::ICmpSlt => BytecodeOp::ICmpSlt,
            VMPOpcode::ICmpSle => BytecodeOp::ICmpSle,
            VMPOpcode::ICmpSgt => BytecodeOp::ICmpSgt,
            VMPOpcode::ICmpSge => BytecodeOp::ICmpSge,
            VMPOpcode::ICmpUlt => BytecodeOp::ICmpUlt,
            VMPOpcode::ICmpUle => BytecodeOp::ICmpUle,
            VMPOpcode::ICmpUgt => BytecodeOp::ICmpUgt,
            VMPOpcode::ICmpUge => BytecodeOp::ICmpUge,
            VMPOpcode::And => BytecodeOp::And,
            VMPOpcode::Or => BytecodeOp::Or,
            VMPOpcode::Xor => BytecodeOp::Xor,
            VMPOpcode::Shl => BytecodeOp::Shl,
            VMPOpcode::LShr => BytecodeOp::LShr,
            VMPOpcode::AShr => BytecodeOp::AShr,
            VMPOpcode::Trunc { .. } => BytecodeOp::Trunc,
            VMPOpcode::ZExt { .. } => BytecodeOp::ZExt,
            VMPOpcode::SExt { .. } => BytecodeOp::SExt,
            VMPOpcode::FPToSI { .. } => BytecodeOp::FPToSI,
            VMPOpcode::FPToUI { .. } => BytecodeOp::FPToUI,
            VMPOpcode::SIToFP { .. } => BytecodeOp::SIToFP,
            VMPOpcode::UIToFP { .. } => BytecodeOp::UIToFP,
            VMPOpcode::Label { .. } => BytecodeOp::Label,
            VMPOpcode::MetaGVar { .. } => BytecodeOp::MetaGVar,
        }
    }
}

impl Display for VMPOpcode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            VMPOpcode::Push { value } => write!(f, "push {:?}", value),
            VMPOpcode::Pop => write!(f, "pop"),
            VMPOpcode::PopToReg { reg } => write!(f, "pop to r{}", reg),
            VMPOpcode::PushFromReg { reg } => write!(f, "push from r{}", reg),
            VMPOpcode::ClearReg { reg } => write!(f, "clear r{}", reg),
            VMPOpcode::Alloca { size } => write!(f, "alloca {}", size),
            VMPOpcode::Alloca2 => write!(f, "alloca2"),
            VMPOpcode::Store { address } => write!(f, "store 0x{:x}", address),
            VMPOpcode::StoreValue => write!(f, "store value"),
            VMPOpcode::Load { address } => write!(f, "load 0x{:x}", address),
            VMPOpcode::LoadValue => write!(f, "load value"),
            VMPOpcode::Add { nsw, nuw } => {
                if *nsw && *nuw {
                    panic!("nsw and nuw cannot be both true");
                } else if *nsw {
                    write!(f, "nsw add")
                } else if *nuw {
                    write!(f, "nuw add")
                } else {
                    write!(f, "add")
                }
            },
            VMPOpcode::Sub => write!(f, "sub"),
            VMPOpcode::Mul => write!(f, "mul"),
            VMPOpcode::Div => write!(f, "div"),
            VMPOpcode::Ret => write!(f, "ret"),
            VMPOpcode::Nop => write!(f, "nop"),
            VMPOpcode::Swap => write!(f, "swap"),
            VMPOpcode::Dup => write!(f, "dup"),
            VMPOpcode::TypeCheckInt { width } => write!(f, "type_ck {}", width),
            VMPOpcode::Call { function_name, .. } => {
                write!(f, "call {}(...)", function_name)
            },
            VMPOpcode::Jump { target } => write!(f, "jmp {}", target),
            VMPOpcode::JumpIf { target } => write!(f, "jmp_if {}", target),
            VMPOpcode::JumpIfNot { target } => write!(f, "jmp_if_not {}", target),
            VMPOpcode::ICmpEq => write!(f, "icmp_eq"),
            VMPOpcode::ICmpNe => write!(f, "icmp_ne"),
            VMPOpcode::ICmpSlt => write!(f, "icmp_slt"),
            VMPOpcode::ICmpSle => write!(f, "icmp_sle"),
            VMPOpcode::ICmpSgt => write!(f, "icmp_sgt"),
            VMPOpcode::ICmpSge => write!(f, "icmp_sge"),
            VMPOpcode::ICmpUlt => write!(f, "icmp_ult"),
            VMPOpcode::ICmpUle => write!(f, "icmp_ule"),
            VMPOpcode::ICmpUgt => write!(f, "icmp_ugt"),
            VMPOpcode::ICmpUge => write!(f, "icmp_uge"),
            VMPOpcode::And => write!(f, "and"),
            VMPOpcode::Or => write!(f, "or"),
            VMPOpcode::Xor => write!(f, "xor"),
            VMPOpcode::Shl => write!(f, "shl"),
            VMPOpcode::LShr => write!(f, "lshr"),
            VMPOpcode::AShr => write!(f, "ashr"),
            VMPOpcode::Trunc { target_width } => write!(f, "trunc i{}", target_width),
            VMPOpcode::ZExt { target_width } => write!(f, "zext i{}", target_width),
            VMPOpcode::SExt { target_width } => write!(f, "sext i{}", target_width),
            VMPOpcode::FPToSI { target_width } => write!(f, "fptosi i{}", target_width),
            VMPOpcode::FPToUI { target_width } => write!(f, "fptoui i{}", target_width),
            VMPOpcode::SIToFP { is_double } => {
                if *is_double {
                    write!(f, "sitofp double")
                } else {
                    write!(f, "sitofp float")
                }
            },
            VMPOpcode::UIToFP { is_double } => {
                if *is_double {
                    write!(f, "uitofp double")
                } else {
                    write!(f, "uitofp float")
                }
            },
            VMPOpcode::Label { name } => write!(f, "{}:", name),
            VMPOpcode::MetaGVar { reg, .. } => write!(f, ".global var: r{}", reg),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VMPValue {
    // 整型
    I1(bool),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    // 浮点
    F32(f32),
    F64(f64),
    Ptr(usize),
    // 新增：其他类型
    Undef, // 未定义值
}

impl VMPValue {
    pub fn to_bytecode_value_type(&self) -> BytecodeValueType {
        match self {
            VMPValue::I1(_) => BytecodeValueType::I1,
            VMPValue::I8(_) => BytecodeValueType::I8,
            VMPValue::I16(_) => BytecodeValueType::I16,
            VMPValue::I32(_) => BytecodeValueType::I32,
            VMPValue::I64(_) => BytecodeValueType::I64,
            VMPValue::F32(_) => BytecodeValueType::F32,
            VMPValue::F64(_) => BytecodeValueType::F64,
            VMPValue::Ptr(_) => BytecodeValueType::Ptr,
            VMPValue::Undef => BytecodeValueType::Undef,
        }
    }

    pub fn size_in_bytes(&self) -> usize {
        match self {
            VMPValue::I1(_) => 1,
            VMPValue::I8(_) => 1,
            VMPValue::I16(_) => 2,
            VMPValue::I32(_) => 4,
            VMPValue::I64(_) => 8,
            VMPValue::F32(_) => 4,
            VMPValue::F64(_) => 8,
            VMPValue::Ptr(_) => 8,
            VMPValue::Undef => 0,
        }
    }

    pub fn width_in_bits(&self) -> usize {
        match self {
            VMPValue::Undef => 0,
            _ => self.size_in_bytes() * 8,
        }
    }

    /// 检查两个值是否类型兼容
    pub fn is_type_compatible(&self, other: &VMPValue) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// 将值转换为指定宽度的整数
    pub fn to_int_with_width(&self, width: u32) -> Result<VMPValue, String> {
        let int_val = match self {
            VMPValue::I1(v) => {
                if *v {
                    1i64
                } else {
                    0i64
                }
            },
            VMPValue::I8(v) => *v as i64,
            VMPValue::I16(v) => *v as i64,
            VMPValue::I32(v) => *v as i64,
            VMPValue::I64(v) => *v,
            _ => return Err(format!("Cannot convert {:?} to integer", self)),
        };

        match width {
            1 => Ok(VMPValue::I1(int_val != 0)),
            8 => Ok(VMPValue::I8(int_val as i8)),
            16 => Ok(VMPValue::I16(int_val as i16)),
            32 => Ok(VMPValue::I32(int_val as i32)),
            64 => Ok(VMPValue::I64(int_val)),
            _ => Err(format!("Unsupported integer width: {}", width)),
        }
    }

    /// 获取整数值（用于比较和位运算）
    pub fn as_i64(&self) -> Result<i64, String> {
        match self {
            VMPValue::I1(v) => Ok(if *v { 1 } else { 0 }),
            VMPValue::I8(v) => Ok(*v as i64),
            VMPValue::I16(v) => Ok(*v as i64),
            VMPValue::I32(v) => Ok(*v as i64),
            VMPValue::I64(v) => Ok(*v),
            _ => Err(format!("Cannot convert {:?} to i64", self)),
        }
    }

    /// 获取无符号整数值
    pub fn as_u64(&self) -> Result<u64, String> {
        match self {
            VMPValue::I1(v) => Ok(if *v { 1 } else { 0 }),
            VMPValue::I8(v) => Ok(*v as u8 as u64),
            VMPValue::I16(v) => Ok(*v as u16 as u64),
            VMPValue::I32(v) => Ok(*v as u32 as u64),
            VMPValue::I64(v) => Ok(*v as u64),
            _ => Err(format!("Cannot convert {:?} to u64", self)),
        }
    }

    /// 检查是否为真值（用于条件跳转）
    pub fn is_true(&self) -> bool {
        match self {
            VMPValue::I1(v) => *v,
            VMPValue::I8(v) => *v != 0,
            VMPValue::I16(v) => *v != 0,
            VMPValue::I32(v) => *v != 0,
            VMPValue::I64(v) => *v != 0,
            VMPValue::F32(v) => *v != 0.0,
            VMPValue::F64(v) => *v != 0.0,
            VMPValue::Ptr(v) => *v != 0,
            VMPValue::Undef => false,
        }
    }
}

/// 指令编码/解码的辅助结构
#[derive(Debug, Serialize, Deserialize)]
pub struct SerializableInstruction {
    pub opcode: VMPOpcode,
    pub metadata: Option<InstructionMetadata>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InstructionMetadata {
    pub source_line: Option<u32>,
    pub source_function: Option<String>,
    pub optimization_hint: Option<String>,
}

/// 指令序列的容器
#[derive(Debug, Serialize, Deserialize)]
pub struct VMPProgram {
    pub instructions: Vec<SerializableInstruction>,
    pub entry_point: String,
    pub metadata: ProgramMetadata,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProgramMetadata {
    pub version: String,
    pub target_arch: String,
    pub optimization_level: u32,
    pub compilation_timestamp: u64,
}

impl VMPProgram {
    pub fn new(instructions: Vec<VMPOpcode>) -> Self {
        let serializable_instructions = instructions
            .into_iter()
            .map(|opcode| SerializableInstruction { opcode, metadata: None })
            .collect();

        Self {
            instructions: serializable_instructions,
            entry_point: "main".to_string(),
            metadata: ProgramMetadata {
                version: "1.0.0".to_string(),
                target_arch: std::env::consts::ARCH.to_string(),
                optimization_level: 0,
                compilation_timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            },
        }
    }

    pub fn to_opcodes(&self) -> Vec<VMPOpcode> {
        self.instructions.iter().map(|inst| inst.opcode.clone()).collect()
    }
}
