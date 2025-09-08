use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::values::FunctionValue;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AVMOpcode {
    /// 将一个 AVMValue 压入栈顶
    ///
    /// 栈: [] -> [value]
    Push {
        value: AVMValue,
    },
    /// 从栈顶弹出一个 AVMValue
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
    /// 从栈顶弹出一个 AVMValue 作为大小，分配对应数量的 Value 槽，返回其基址指针并压入栈
    ///
    /// 栈: [size] -> [ptr]
    Alloca2,

    Store {
        address: usize,
    },
    /// 存储一个 AVMValue；结构体或向量请勿使用该指令
    ///
    /// 栈: [value, ptr] -> []
    StoreValue,
    Load {
        address: usize,
    },
    /// 加载一个 AVMValue；结构体或向量请勿使用该指令
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

impl Display for AVMOpcode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AVMOpcode::Push { value } => write!(f, "push {:?}", value),
            AVMOpcode::Pop => write!(f, "pop"),
            AVMOpcode::PopToReg { reg } => write!(f, "pop to r{}", reg),
            AVMOpcode::PushFromReg { reg } => write!(f, "push from r{}", reg),
            AVMOpcode::ClearReg { reg } => write!(f, "clear r{}", reg),
            AVMOpcode::Alloca { size } => write!(f, "alloca {}", size),
            AVMOpcode::Alloca2 => write!(f, "alloca2"),
            AVMOpcode::Store { address } => write!(f, "store 0x{:x}", address),
            AVMOpcode::StoreValue => write!(f, "store value"),
            AVMOpcode::Load { address } => write!(f, "load 0x{:x}", address),
            AVMOpcode::LoadValue => write!(f, "load value"),
            AVMOpcode::Add { nsw, nuw } => {
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
            AVMOpcode::Sub => write!(f, "sub"),
            AVMOpcode::Mul => write!(f, "mul"),
            AVMOpcode::Div => write!(f, "div"),
            AVMOpcode::Ret => write!(f, "ret"),
            AVMOpcode::Nop => write!(f, "nop"),
            AVMOpcode::Swap => write!(f, "swap"),
            AVMOpcode::Dup => write!(f, "dup"),
            AVMOpcode::TypeCheckInt { width } => write!(f, "type_ck {}", width),
            AVMOpcode::Call { function_name, .. } => {
                write!(f, "call {}(...)", function_name)
            },
            AVMOpcode::Jump { target } => write!(f, "jmp {}", target),
            AVMOpcode::JumpIf { target } => write!(f, "jmp_if {}", target),
            AVMOpcode::JumpIfNot { target } => write!(f, "jmp_if_not {}", target),
            AVMOpcode::ICmpEq => write!(f, "icmp_eq"),
            AVMOpcode::ICmpNe => write!(f, "icmp_ne"),
            AVMOpcode::ICmpSlt => write!(f, "icmp_slt"),
            AVMOpcode::ICmpSle => write!(f, "icmp_sle"),
            AVMOpcode::ICmpSgt => write!(f, "icmp_sgt"),
            AVMOpcode::ICmpSge => write!(f, "icmp_sge"),
            AVMOpcode::ICmpUlt => write!(f, "icmp_ult"),
            AVMOpcode::ICmpUle => write!(f, "icmp_ule"),
            AVMOpcode::ICmpUgt => write!(f, "icmp_ugt"),
            AVMOpcode::ICmpUge => write!(f, "icmp_uge"),
            AVMOpcode::And => write!(f, "and"),
            AVMOpcode::Or => write!(f, "or"),
            AVMOpcode::Xor => write!(f, "xor"),
            AVMOpcode::Shl => write!(f, "shl"),
            AVMOpcode::LShr => write!(f, "lshr"),
            AVMOpcode::AShr => write!(f, "ashr"),
            AVMOpcode::Trunc { target_width } => write!(f, "trunc i{}", target_width),
            AVMOpcode::ZExt { target_width } => write!(f, "zext i{}", target_width),
            AVMOpcode::SExt { target_width } => write!(f, "sext i{}", target_width),
            AVMOpcode::FPToSI { target_width } => write!(f, "fptosi i{}", target_width),
            AVMOpcode::FPToUI { target_width } => write!(f, "fptoui i{}", target_width),
            AVMOpcode::SIToFP { is_double } => {
                if *is_double {
                    write!(f, "sitofp double")
                } else {
                    write!(f, "sitofp float")
                }
            },
            AVMOpcode::UIToFP { is_double } => {
                if *is_double {
                    write!(f, "uitofp double")
                } else {
                    write!(f, "uitofp float")
                }
            },
            AVMOpcode::Label { name } => write!(f, "{}:", name),
            AVMOpcode::MetaGVar { reg, .. } => write!(f, ".global var: r{}", reg),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AVMValue {
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

impl AVMValue {
    pub fn size_in_bytes(&self) -> usize {
        match self {
            AVMValue::I1(_) => 1,
            AVMValue::I8(_) => 1,
            AVMValue::I16(_) => 2,
            AVMValue::I32(_) => 4,
            AVMValue::I64(_) => 8,
            AVMValue::F32(_) => 4,
            AVMValue::F64(_) => 8,
            AVMValue::Ptr(_) => 8,
            AVMValue::Undef => 0,
        }
    }

    pub fn width_in_bits(&self) -> usize {
        match self {
            AVMValue::Undef => 0,
            _ => self.size_in_bytes() * 8,
        }
    }

    /// 检查两个值是否类型兼容
    pub fn is_type_compatible(&self, other: &AVMValue) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// 将值转换为指定宽度的整数
    pub fn to_int_with_width(&self, width: u32) -> Result<AVMValue, String> {
        let int_val = match self {
            AVMValue::I1(v) => {
                if *v {
                    1i64
                } else {
                    0i64
                }
            },
            AVMValue::I8(v) => *v as i64,
            AVMValue::I16(v) => *v as i64,
            AVMValue::I32(v) => *v as i64,
            AVMValue::I64(v) => *v,
            _ => return Err(format!("Cannot convert {:?} to integer", self)),
        };

        match width {
            1 => Ok(AVMValue::I1(int_val != 0)),
            8 => Ok(AVMValue::I8(int_val as i8)),
            16 => Ok(AVMValue::I16(int_val as i16)),
            32 => Ok(AVMValue::I32(int_val as i32)),
            64 => Ok(AVMValue::I64(int_val)),
            _ => Err(format!("Unsupported integer width: {}", width)),
        }
    }

    /// 获取整数值（用于比较和位运算）
    pub fn as_i64(&self) -> Result<i64, String> {
        match self {
            AVMValue::I1(v) => Ok(if *v { 1 } else { 0 }),
            AVMValue::I8(v) => Ok(*v as i64),
            AVMValue::I16(v) => Ok(*v as i64),
            AVMValue::I32(v) => Ok(*v as i64),
            AVMValue::I64(v) => Ok(*v),
            _ => Err(format!("Cannot convert {:?} to i64", self)),
        }
    }

    /// 获取无符号整数值
    pub fn as_u64(&self) -> Result<u64, String> {
        match self {
            AVMValue::I1(v) => Ok(if *v { 1 } else { 0 }),
            AVMValue::I8(v) => Ok(*v as u8 as u64),
            AVMValue::I16(v) => Ok(*v as u16 as u64),
            AVMValue::I32(v) => Ok(*v as u32 as u64),
            AVMValue::I64(v) => Ok(*v as u64),
            _ => Err(format!("Cannot convert {:?} to u64", self)),
        }
    }

    /// 检查是否为真值（用于条件跳转）
    pub fn is_true(&self) -> bool {
        match self {
            AVMValue::I1(v) => *v,
            AVMValue::I8(v) => *v != 0,
            AVMValue::I16(v) => *v != 0,
            AVMValue::I32(v) => *v != 0,
            AVMValue::I64(v) => *v != 0,
            AVMValue::F32(v) => *v != 0.0,
            AVMValue::F64(v) => *v != 0.0,
            AVMValue::Ptr(v) => *v != 0,
            AVMValue::Undef => false,
        }
    }
}

/// 指令编码/解码的辅助结构
#[derive(Debug, Serialize, Deserialize)]
pub struct SerializableInstruction {
    pub opcode: AVMOpcode,
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
pub struct AVMProgram {
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

impl AVMProgram {
    pub fn new(instructions: Vec<AVMOpcode>) -> Self {
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

    pub fn to_opcodes(&self) -> Vec<AVMOpcode> {
        self.instructions.iter().map(|inst| inst.opcode.clone()).collect()
    }
}
