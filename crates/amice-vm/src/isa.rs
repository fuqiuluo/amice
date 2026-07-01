//! 从 `isa.vm` 解析出的 ISA 与 semantic AST。
//!
//! # 契约
//! 指令名、opcode alias、operand 顺序和 handler 语义都来自 profile。AMICE 会把解析后的
//! `semantic {}` block 映射到这个有限 AST，使 verifier 和 runtime emitter 可以在不执行
//! profile 提供代码的前提下推导副作用。
//!
//! # 不变量
//! - `InstructionDesc::name` 是保留到 VM IR 和 bytecode 的 identity。
//! - `opcode_aliases` 不能为空，并且在整个 profile 内必须唯一。
//! - `effect` 来自 semantic AST，并会和选中的后端 handler template 交叉检查。
//! - q-register 引用以文本形式跟踪，使 `q.lowering = disabled` 能在 lowering 开始前拒绝它们。

use serde::{Deserialize, Serialize};

use crate::lowering::NATIVE_CALL_MAX_RETURNS;

/// 内置 VM profile 支持的整数 ALU 操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinOp {
    /// wrapping 整数加法。
    Add,
    /// wrapping 整数减法。
    Sub,
    /// wrapping 整数乘法。
    Mul,
    /// 按位异或。
    Xor,
    /// 按位与。
    And,
    /// 按位或。
    Or,
    /// 逻辑左移。
    Shl,
    /// 逻辑右移。
    LShr,
    /// 算术右移。
    AShr,
}

/// VM `icmp` 使用的整数比较谓词。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum CmpPredicate {
    /// 相等比较。
    Eq,
    /// 不相等比较。
    Ne,
    /// 无符号大于比较。
    Ugt,
    /// 无符号大于等于比较。
    Uge,
    /// 无符号小于比较。
    Ult,
    /// 无符号小于等于比较。
    Ule,
    /// 有符号大于比较。
    Sgt,
    /// 有符号大于等于比较。
    Sge,
    /// 有符号小于比较。
    Slt,
    /// 有符号小于等于比较。
    Sle,
}

impl CmpPredicate {
    /// 当谓词按有符号整数解释 operand 时返回 true。
    pub fn is_signed(self) -> bool {
        matches!(self, Self::Sgt | Self::Sge | Self::Slt | Self::Sle)
    }
}

/// 当前 lowering 子集支持的整数 cast 操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CastOp {
    /// 对整数值做零扩展。
    ZExt,
    /// 对整数值做符号扩展。
    SExt,
    /// 截断整数值。
    Trunc,
    /// 在等宽整数/指针 cast 中保留原始 bit。
    Bitcast,
}

/// 从解析后的 `semantic {}` program 选择出的 runtime template。
///
/// profile 拥有指令名和 opcode。这个 enum 不是从名称硬编码推导的；
/// 它是 semantic DSL 被解析并校验后，AMICE 能够 lowering 的有限后端操作集合。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandlerSemantic {
    /// 物化 inline immediate。
    MovImm,
    /// 从 bytecode const pool 加载值。
    ConstLoad,
    /// 复制一个 VM 寄存器。
    Mov,
    /// 整数二元运算。
    Bin(BinOp),
    /// 整数比较。
    Icmp,
    /// 整数或指针 cast。
    Cast(CastOp),
    /// 固定大小栈分配。
    Alloca,
    /// 标量内存读取。
    Load,
    /// 标量内存写入。
    Store,
    /// 按字节偏移做指针运算。
    Gep,
    /// 直接 native LLVM call bridge。
    CallNative,
    /// 保持状态不变的 fake 或 no-op 指令。
    Nop,
    /// 无条件分支。
    Br,
    /// 条件分支。
    BrCond,
    /// VM 内部调用。
    VmCall,
    /// VM 内部返回。
    VmRet,
    /// 受保护函数返回。
    Ret,
}

/// 从 `isa.vm` 解析出的 handler semantic program。
///
/// DSL 被刻意限制为小集合：赋值、寄存器引用、基础算术、内存 helper、native-call 结果读取和显式
/// PC 副作用。保持类型化可以让 verifier 在不执行宿主代码、也不接受字符串模式语义的前提下推导副作用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticProgram {
    /// 按源码顺序解析出的语句。
    pub statements: Vec<SemanticStmt>,
    /// 从 `statements` 派生的静态副作用摘要。
    pub effect: HandlerEffect,
    /// 此 program 引用到的 `q` 组寄存器名。
    pub q_register_references: Vec<String>,
}

impl SemanticProgram {
    /// 为测试和旧默认 profile 构建只包含契约的 program。
    pub fn from_template(template: &HandlerSemantic) -> Self {
        Self {
            statements: Vec::new(),
            effect: template.expected_effect(),
            q_register_references: Vec::new(),
        }
    }
}

/// 支持的 handler semantic DSL 中的一条语句。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticStmt {
    /// 把 semantic expression 赋给 VM register operand 或别名。
    AssignReg {
        /// 目标 register operand 或别名名称。
        dst: String,
        /// 写入目标的值。
        value: SemanticExpr,
    },
    /// 赋值下一条 VM program counter。
    AssignPc {
        /// 新 PC 表达式。
        value: PcExpr,
    },
    /// 向内存写入标量值。
    StoreWidth {
        /// 指针表达式。
        ptr: SemanticExpr,
        /// 值表达式。
        value: SemanticExpr,
        /// 位宽表达式。
        width: SemanticExpr,
    },
    /// 声明 handler 不改变 VM 数据状态。
    StateUnchanged,
}

/// handler semantic statement 内接受的值表达式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticExpr {
    /// 引用 profile 声明的 instruction operand。
    Operand(String),
    /// 引用 VM register operand 或别名。
    Register(String),
    /// 按 operand 名称从 bytecode const pool 读取。
    ConstPool(String),
    /// 表示下一条 bytecode PC 的伪值。
    NextPc,
    /// 将值截断或 mask 到目标位宽。
    TruncWidth {
        /// 需要截断的值。
        value: Box<SemanticExpr>,
        /// 位宽表达式。
        width: Box<SemanticExpr>,
    },
    /// 对值做零扩展。
    ZeroExtend {
        /// 源值。
        value: Box<SemanticExpr>,
        /// 源位宽。
        from_width: Box<SemanticExpr>,
        /// 目标位宽。
        to_width: Box<SemanticExpr>,
    },
    /// 对值做符号扩展。
    SignExtend {
        /// 源值。
        value: Box<SemanticExpr>,
        /// 源位宽。
        from_width: Box<SemanticExpr>,
        /// 可选目标位宽；缺省表示保持 handler 结果位宽。
        to_width: Option<Box<SemanticExpr>>,
    },
    /// 在整数/指针位宽之间重新解释 bit。
    BitcastWidth {
        /// 源值。
        value: Box<SemanticExpr>,
        /// 源位宽。
        from_width: Box<SemanticExpr>,
        /// 目标位宽。
        to_width: Box<SemanticExpr>,
    },
    /// 整数二元表达式。
    Binary {
        /// 要应用的操作。
        op: SemanticBinOp,
        /// 左操作数。
        lhs: Box<SemanticExpr>,
        /// 右操作数。
        rhs: Box<SemanticExpr>,
    },
    /// 整数比较表达式。
    Compare {
        /// 按 LLVM `icmp` 方式编码的谓词表达式。
        pred: Box<SemanticExpr>,
        /// 左操作数。
        lhs: Box<SemanticExpr>,
        /// 右操作数。
        rhs: Box<SemanticExpr>,
        /// 操作数位宽。
        width: Box<SemanticExpr>,
    },
    /// 无分支 select 表达式。
    Select {
        /// 条件表达式；零表示 false。
        cond: Box<SemanticExpr>,
        /// 条件非零时选择的值。
        then_value: Box<SemanticExpr>,
        /// 条件为零时选择的值。
        else_value: Box<SemanticExpr>,
    },
    /// 分配 VM stack slot。
    StackAlloc {
        /// 分配大小，单位为字节。
        bytes: Box<SemanticExpr>,
        /// 所需对齐，单位为字节。
        align: Box<SemanticExpr>,
    },
    /// 从内存加载标量值。
    LoadWidth {
        /// 指针表达式。
        ptr: Box<SemanticExpr>,
        /// 位宽表达式。
        width: Box<SemanticExpr>,
    },
    /// 从生成的 native-call bridge table 读取一个返回槽。
    CallTableReturn {
        /// callee operand 名称。
        callee: String,
        /// 返回槽序号。
        index: u8,
        /// 传给 bridge 的参数与元数据。
        args: Vec<SemanticExpr>,
    },
}

/// handler semantic DSL 中的二元运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticBinOp {
    /// wrapping 整数加法。
    Add,
    /// wrapping 整数减法。
    Sub,
    /// wrapping 整数乘法。
    Mul,
    /// 按位异或。
    Xor,
    /// 按位与。
    And,
    /// 按位或。
    Or,
    /// 逻辑左移。
    Shl,
    /// 逻辑右移。
    LShr,
    /// 算术右移。
    AShr,
}

/// `pc = ...` 赋值中接受的 program-counter 表达式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PcExpr {
    /// 继续执行下一条 bytecode 指令。
    Next,
    /// 退出 VM dispatch loop。
    Return,
    /// 跳转到 label operand。
    Label(String),
    /// 跳转到寄存器别名保存的 PC，通常是 `lr`。
    Register(String),
    /// 在两个 label operand 之间选择。
    Select {
        /// 条件表达式；零表示 false。
        cond: Box<SemanticExpr>,
        /// 条件非零时选择的 label。
        then_pc: String,
        /// 条件为零时选择的 label。
        else_pc: String,
    },
}

/// handler 改变 VM program counter 的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PcEffect {
    /// handler fallthrough 到下一条 bytecode 指令。
    Next,
    /// handler 可能跳转到非顺序 bytecode PC。
    Branch,
    /// handler 退出 dispatcher。
    Return,
}

/// 从 ISA `semantic` block 派生的静态副作用摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandlerEffect {
    /// handler 需要产生的 program-counter 副作用。
    pub pc: PcEffect,
    /// handler 读取的 register operand 或别名。
    pub register_reads: Vec<String>,
    /// handler 写入的 register operand 或别名。
    pub register_writes: Vec<String>,
    /// handler 是否读取宿主/VM 内存。
    pub memory_read: bool,
    /// handler 是否写入宿主/VM 内存。
    pub memory_write: bool,
    /// handler 是否调用 native LLVM 代码。
    pub native_call: bool,
}

/// profile DSL 接受的 operand kind。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperandKind {
    /// VM 寄存器操作数。
    VReg,
    /// inline immediate 操作数。
    Imm,
    /// constant-pool index 操作数。
    ConstPoolIndex,
    /// bytecode label 操作数。
    Label,
    /// 仅供旧测试构造器使用的 parser fallback。
    Unknown,
}

/// 从 `isa.vm` 指令 header 解析出的 operand 声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperandDesc {
    /// `isa.vm` 中声明的 operand 名称。
    pub name: String,
    /// operand 编码/语义 kind。
    pub kind: OperandKind,
    /// profile 值类型，例如 `i64`、`ptr` 或 `label`。
    pub value_type: String,
}

impl HandlerSemantic {
    /// 为内置语义类别构建 verifier 已知的副作用契约。
    pub fn expected_effect(&self) -> HandlerEffect {
        match self {
            Self::MovImm => HandlerEffect::new(PcEffect::Next).writes(["dst"]),
            Self::ConstLoad => HandlerEffect::new(PcEffect::Next).writes(["dst"]),
            Self::Mov => HandlerEffect::new(PcEffect::Next).reads(["src"]).writes(["dst"]),
            Self::Bin(_) => HandlerEffect::new(PcEffect::Next).reads(["lhs", "rhs"]).writes(["dst"]),
            Self::Icmp => HandlerEffect::new(PcEffect::Next).reads(["lhs", "rhs"]).writes(["dst"]),
            Self::Cast(_) => HandlerEffect::new(PcEffect::Next).reads(["src"]).writes(["dst"]),
            Self::Alloca => HandlerEffect::new(PcEffect::Next).writes(["dst"]),
            Self::Load => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr"])
                .writes(["dst"])
                .with_memory_read(),
            Self::Store => HandlerEffect::new(PcEffect::Next)
                .reads(["ptr", "src"])
                .with_memory_write(),
            Self::Gep => HandlerEffect::new(PcEffect::Next).reads(["base"]).writes(["dst"]),
            Self::CallNative => HandlerEffect::new(PcEffect::Next)
                .reads(["arg0", "arg1", "arg2", "arg3", "arg4", "arg5", "arg6", "arg7"])
                .writes((0..NATIVE_CALL_MAX_RETURNS).map(|index| format!("ret{index}")))
                .with_native_call(),
            Self::Nop => HandlerEffect::new(PcEffect::Next),
            Self::Br => HandlerEffect::new(PcEffect::Branch),
            Self::BrCond => HandlerEffect::new(PcEffect::Branch).reads(["cond"]),
            Self::VmCall => HandlerEffect::new(PcEffect::Branch).writes(["lr"]),
            Self::VmRet => HandlerEffect::new(PcEffect::Branch).reads(["lr"]),
            Self::Ret => HandlerEffect::new(PcEffect::Return).reads(["src"]).writes(["ret0"]),
        }
    }
}

impl HandlerEffect {
    /// 创建不包含寄存器和内存副作用的摘要。
    pub fn new(pc: PcEffect) -> Self {
        Self {
            pc,
            register_reads: Vec::new(),
            register_writes: Vec::new(),
            memory_read: false,
            memory_write: false,
            native_call: false,
        }
    }

    /// 向副作用摘要添加寄存器读取。
    pub fn reads<I, S>(mut self, registers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.register_reads.extend(registers.into_iter().map(Into::into));
        self
    }

    /// 向副作用摘要添加寄存器写入。
    pub fn writes<I, S>(mut self, registers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.register_writes.extend(registers.into_iter().map(Into::into));
        self
    }

    /// 标记 handler 会读取内存。
    pub fn with_memory_read(mut self) -> Self {
        self.memory_read = true;
        self
    }

    /// 标记 handler 会写入内存。
    pub fn with_memory_write(mut self) -> Self {
        self.memory_write = true;
        self
    }

    /// 标记 handler 会调用生成的 native-call bridge 代码。
    pub fn with_native_call(mut self) -> Self {
        self.native_call = true;
        self
    }
}

/// profile ISA 声明的 VM 指令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionDesc {
    /// 保留到 VM IR 和 bytecode 的 profile 指令名。
    pub name: String,
    /// 第一个 opcode alias；为兼容旧调用点而保留。
    pub opcode: u8,
    /// runtime dispatcher 接受的完整 opcode alias 集合。
    pub opcode_aliases: Vec<u8>,
    /// 编码 operand 数量。
    pub operands: u8,
    /// 来自指令 header 的有序 operand 声明。
    pub operand_descs: Vec<OperandDesc>,
    /// 从 semantic AST 选择的后端 handler template。
    pub semantic: HandlerSemantic,
    /// 从 `isa.vm` 解析出的 semantic program。
    pub semantic_program: SemanticProgram,
    /// 从 `semantic_program` 派生的静态副作用。
    pub effect: HandlerEffect,
}

impl InstructionDesc {
    /// 用一个或多个 profile 声明的 opcode alias 创建指令描述。
    pub fn new(name: impl Into<String>, opcode_aliases: Vec<u8>, operands: u8, semantic: HandlerSemantic) -> Self {
        let semantic_program = SemanticProgram::from_template(&semantic);
        let effect = semantic_program.effect.clone();
        let operand_descs = (0..operands)
            .map(|index| OperandDesc {
                name: format!("op{index}"),
                kind: OperandKind::Unknown,
                value_type: "unknown".to_owned(),
            })
            .collect();
        Self::new_with_semantic_program(
            name,
            opcode_aliases,
            operands,
            operand_descs,
            semantic,
            semantic_program,
            effect,
        )
    }

    /// 用 parser 派生的副作用创建指令描述。
    pub fn new_with_semantic_program(
        name: impl Into<String>,
        opcode_aliases: Vec<u8>,
        operands: u8,
        operand_descs: Vec<OperandDesc>,
        semantic: HandlerSemantic,
        semantic_program: SemanticProgram,
        effect: HandlerEffect,
    ) -> Self {
        let opcode = opcode_aliases
            .first()
            .copied()
            .expect("instruction descriptors need at least one opcode alias");

        Self {
            name: name.into(),
            opcode,
            opcode_aliases,
            operands,
            operand_descs,
            semantic,
            semantic_program,
            effect,
        }
    }

    /// 返回此指令接受的所有 opcode。
    pub fn opcodes(&self) -> &[u8] {
        &self.opcode_aliases
    }

    /// 为一个具体编码指令位置选择 opcode alias。
    pub fn opcode_for_site(&self, function_key: u64, pc: usize) -> u8 {
        let aliases = self.opcodes();
        let mixed = function_key ^ (pc as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let folded = mixed ^ (mixed >> 33) ^ (mixed >> 17);
        aliases[(folded as usize) % aliases.len()]
    }
}

/// lowering、encoding 和 runtime emission 使用的已解析 ISA 表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsaProfile {
    /// 按声明顺序排列的 profile 指令。
    pub instructions: Vec<InstructionDesc>,
}

impl IsaProfile {
    /// 按 profile 声明名查找指令描述。
    pub fn by_name(&self, name: &str) -> Option<&InstructionDesc> {
        self.instructions.iter().find(|desc| desc.name == name)
    }

    /// 按请求的语义类别查找指令描述。
    pub fn by_semantic(&self, semantic: &HandlerSemantic) -> Option<&InstructionDesc> {
        self.instructions.iter().find(|desc| desc.semantic == *semantic)
    }

    /// 查找 opcode 对应的指令描述。
    pub fn by_opcode(&self, opcode: u8) -> Option<&InstructionDesc> {
        self.instructions.iter().find(|desc| desc.opcodes().contains(&opcode))
    }

    /// 当此 ISA 内所有 opcode 都唯一时返回 true。
    pub fn has_unique_opcodes(&self) -> bool {
        let alias_count = self.instructions.iter().map(|desc| desc.opcodes().len()).sum();
        let mut seen = std::collections::HashSet::with_capacity(alias_count);
        self.instructions
            .iter()
            .flat_map(InstructionDesc::opcodes)
            .all(|opcode| seen.insert(*opcode))
    }
}

impl Default for IsaProfile {
    fn default() -> Self {
        use BinOp::*;
        use CastOp::*;
        use HandlerSemantic::*;

        let instructions = vec![
            InstructionDesc::new("mov_imm", vec![0x01], 3, MovImm),
            InstructionDesc::new("const_load", vec![0x03], 3, ConstLoad),
            InstructionDesc::new("mov", vec![0x02], 3, Mov),
            InstructionDesc::new("iadd", vec![0x10], 4, Bin(Add)),
            InstructionDesc::new("isub", vec![0x11], 4, Bin(Sub)),
            InstructionDesc::new("imul", vec![0x12], 4, Bin(Mul)),
            InstructionDesc::new("ixor", vec![0x13], 4, Bin(Xor)),
            InstructionDesc::new("iand", vec![0x14], 4, Bin(And)),
            InstructionDesc::new("ior", vec![0x15], 4, Bin(Or)),
            InstructionDesc::new("ishl", vec![0x16], 4, Bin(Shl)),
            InstructionDesc::new("ilshr", vec![0x17], 4, Bin(LShr)),
            InstructionDesc::new("iashr", vec![0x18], 4, Bin(AShr)),
            InstructionDesc::new("icmp", vec![0x20], 5, Icmp),
            InstructionDesc::new("zext", vec![0x30], 4, Cast(ZExt)),
            InstructionDesc::new("sext", vec![0x31], 4, Cast(SExt)),
            InstructionDesc::new("trunc", vec![0x32], 4, Cast(Trunc)),
            InstructionDesc::new("bitcast", vec![0x33], 4, Cast(Bitcast)),
            InstructionDesc::new("alloca", vec![0x34], 3, Alloca),
            InstructionDesc::new("load", vec![0x35], 3, Load),
            InstructionDesc::new("store", vec![0x36], 3, Store),
            InstructionDesc::new("gep", vec![0x37], 3, Gep),
            InstructionDesc::new("call_native", vec![0x38], 27, CallNative),
            InstructionDesc::new("br", vec![0x40], 1, Br),
            InstructionDesc::new("br_if", vec![0x41], 3, BrCond),
            InstructionDesc::new("vm_call", vec![0x42], 1, VmCall),
            InstructionDesc::new("vm_ret", vec![0x43], 0, VmRet),
            InstructionDesc::new("ret", vec![0x7f], 1, Ret),
        ];

        Self { instructions }
    }
}
