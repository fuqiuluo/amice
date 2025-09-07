use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq)]
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
    }, // PushFromReg(reg) - 将寄存器值压入栈
    /// 清空指定寄存器
    ///
    /// 栈: [] -> []
    ClearReg {
        reg: u32,
    }, // ClearReg(reg) - 清空寄存器

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
}

impl Display for AVMOpcode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AVMOpcode::Push { value } => write!(f, "push {:?}", value),
            AVMOpcode::Pop => write!(f, "pop"),
            AVMOpcode::PopToReg { reg } => write!(f, "pop reg {}", reg),
            AVMOpcode::PushFromReg { reg } => write!(f, "push reg {}", reg),
            AVMOpcode::ClearReg { reg } => write!(f, "clear reg {}", reg),
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
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AVMValue {
    // 整型
    I1(bool),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    // 不做区分
    // U8(u8),
    // U16(u16),
    // U32(u32),
    // U64(u64),
    // 浮点
    F32(f32),
    F64(f64),
    Ptr(usize),
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
        }
    }

    pub fn width_in_bits(&self) -> usize {
        self.size_in_bytes() * 8
    }
}
