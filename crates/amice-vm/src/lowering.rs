//! LLVM-to-VMP translator 在 bytecode 编码前生成的 VM IR。
//!
//! # 契约
//! `VmInstruction` 记录 runtime handler 将执行的语义 operand。
//! `VmFunction::profile_instructions` 记录 `lowering.vm` 选中的精确 ISA 指令名；
//! encoder 必须用这个 identity 选择 opcode alias 和 operand 顺序。
//!
//! # 坑点
//! `push` 只服务测试和内置默认值。profile 驱动 lowering 必须使用 `push_profile`，
//! 这样同语义的两条指令仍能编码成不同 opcode 和 layout。

use crate::isa::{BinOp, CastOp, CmpPredicate};
use std::collections::HashMap;
use std::collections::HashSet;

/// `native_call` thunk 使用固定参数向量，使每个调用点无论 callee LLVM 类型如何，
/// 都能使用一条可被 profile 序列化的 bytecode record。
pub const NATIVE_CALL_MAX_ARGS: usize = 8;

/// runtime 返回固定宽度 tuple，并且只存储前 `ret_count` 个元素。
/// 固定上限能让间接 thunk 调用拥有稳定 LLVM 函数类型，同时仍允许 profile 选择更少返回槽。
pub const NATIVE_CALL_MAX_RETURNS: usize = 8;

/// VM 函数内部的 label 标识符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabelId(pub u32);

/// profile 声明的 `native_call` 返回目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeReturn {
    /// 此 native-call 返回槽写入的目标 `x` 寄存器。
    pub dst: u8,
    /// 截断 native 返回值时使用的整数位宽。
    pub width: u8,
}

/// bytecode 编码前的 VM 指令流。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmInstruction {
    /// 将 inline immediate 物化到 `x` 寄存器。
    MovImm {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// profile 位宽截断前的 inline immediate 值。
        imm: u64,
        /// 结果位宽。
        width: u8,
    },
    /// 从 bytecode const pool 加载值到 `x` 寄存器。
    ConstLoad {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 逻辑常量值；encoder 会分配 const-pool index。
        value: u64,
        /// 结果位宽。
        width: u8,
    },
    /// 在两个 VM 寄存器之间复制值。
    Mov {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 源 `x` 寄存器。
        src: u8,
        /// 结果位宽。
        width: u8,
    },
    /// 整数二元运算。
    Bin {
        /// lowering 选中的后端语义运算。
        op: BinOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 左操作数 `x` 寄存器。
        lhs: u8,
        /// 右操作数 `x` 寄存器。
        rhs: u8,
        /// 结果位宽。
        width: u8,
    },
    /// 整数比较，在 `x` 寄存器中生成 `i1` 值。
    Icmp {
        /// 归一化为 VM 形式的 LLVM 比较谓词。
        pred: CmpPredicate,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 左操作数 `x` 寄存器。
        lhs: u8,
        /// 右操作数 `x` 寄存器。
        rhs: u8,
        /// 操作数位宽。
        width: u8,
    },
    /// 整数或指针位宽转换。
    Cast {
        /// lowering 选中的 cast 操作。
        op: CastOp,
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 源 `x` 寄存器。
        src: u8,
        /// 源位宽。
        from_width: u8,
        /// 目标位宽。
        to_width: u8,
    },
    /// 在 VM runtime frame 内进行固定大小栈分配。
    Alloca {
        /// 目标指针寄存器。
        dst: u8,
        /// 分配大小，单位为字节。
        bytes: u64,
        /// 所需对齐，单位为字节。
        align: u8,
    },
    /// 从 `x` 寄存器保存的地址加载标量。
    Load {
        /// 目标 `x` 寄存器。
        dst: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 加载位宽。
        width: u8,
    },
    /// 向 `x` 寄存器保存的地址存储标量。
    Store {
        /// 源值寄存器。
        src: u8,
        /// 指针寄存器。
        ptr: u8,
        /// 存储位宽。
        width: u8,
    },
    /// 常量字节偏移指针运算。
    Gep {
        /// 目标指针寄存器。
        dst: u8,
        /// 基址指针寄存器。
        base: u8,
        /// 加到基址上的字节偏移。
        offset: u64,
    },
    /// 通过生成的 runtime call table 执行直接 native LLVM 调用。
    CallNative {
        /// runtime call table 槽位。
        call_id: u16,
        /// 参数寄存器；encoder 会填充到 profile record 形状。
        args: Vec<u8>,
        /// wrapper 使用的返回寄存器与位宽。
        returns: Vec<NativeReturn>,
    },
    /// 无条件 bytecode 分支。
    Br {
        /// 目标 bytecode label。
        target: LabelId,
    },
    /// 条件 bytecode 分支。
    BrCond {
        /// 保存 `0` 或 `1` 的条件寄存器。
        cond: u8,
        /// `cond != 0` 时的目标 label。
        then_label: LabelId,
        /// `cond == 0` 时的目标 label。
        else_label: LabelId,
    },
    /// VM 内部调用，会把 return PC 存入 profile 的 `lr` 别名。
    VmCall {
        /// 目标 bytecode label。
        target: LabelId,
    },
    /// 使用 profile `lr` 别名的 VM 内部返回。
    VmRet,
    /// 从受保护函数返回一个标量返回槽。
    Ret {
        /// 复制到 ABI 返回槽的源寄存器。
        src: u8,
    },
    /// 从 `void` 受保护函数返回。
    RetVoid,
}

impl VmInstruction {
    /// 返回内置 simple VMP profile 使用的规范指令名。
    /// profile 驱动 lowering 应调用 `push_profile`，以便重命名或同语义指令保持精确 identity。
    pub fn default_profile_instruction(&self) -> &'static str {
        match self {
            Self::MovImm { .. } => "mov_imm",
            Self::ConstLoad { .. } => "const_load",
            Self::Mov { .. } => "mov",
            Self::Bin { op, .. } => match op {
                BinOp::Add => "iadd",
                BinOp::Sub => "isub",
                BinOp::Mul => "imul",
                BinOp::UDiv => "iudiv",
                BinOp::SDiv => "isdiv",
                BinOp::URem => "iurem",
                BinOp::SRem => "isrem",
                BinOp::Xor => "ixor",
                BinOp::And => "iand",
                BinOp::Or => "ior",
                BinOp::Shl => "ishl",
                BinOp::LShr => "ilshr",
                BinOp::AShr => "iashr",
            },
            Self::Icmp { .. } => "icmp",
            Self::Cast { op, .. } => match op {
                CastOp::ZExt => "zext",
                CastOp::SExt => "sext",
                CastOp::Trunc => "trunc",
                CastOp::Bitcast => "bitcast",
            },
            Self::Alloca { .. } => "alloca",
            Self::Load { .. } => "load",
            Self::Store { .. } => "store",
            Self::Gep { .. } => "gep",
            Self::CallNative { .. } => "call_native",
            Self::Br { .. } => "br",
            Self::BrCond { .. } => "br_if",
            Self::VmCall { .. } => "vm_call",
            Self::VmRet => "vm_ret",
            Self::Ret { .. } | Self::RetVoid => "ret",
        }
    }
}

/// 已完成、可交给 bytecode encoder 的 VM 函数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmFunction {
    /// 用于诊断和多态 key 派生的源函数名。
    pub name: String,
    /// 已分配的 `x` 寄存器数量，不能超过 32。
    pub vreg_count: u8,
    /// 宿主标量返回位宽；`void` 返回时为 0。
    pub return_width: u8,
    /// 按执行顺序排列的 VM 指令流。
    pub instructions: Vec<VmInstruction>,
    /// 每条 VM 指令对应的 profile ISA 指令名。
    pub profile_instructions: Vec<String>,
    /// 每个 label 绑定到的 bytecode PC。
    pub label_pcs: HashMap<LabelId, usize>,
}

/// 拥有 label 分配并校验所有 label 都已绑定的 builder。
#[derive(Debug)]
pub struct VmFunctionBuilder {
    name: String,
    vreg_count: u8,
    free_vregs: Vec<u8>,
    return_width: u8,
    instructions: Vec<VmInstruction>,
    profile_instructions: Vec<String>,
    label_pcs: HashMap<LabelId, usize>,
    next_label: u32,
}

impl VmFunctionBuilder {
    /// 为源函数创建 VM function builder。
    pub fn new(name: impl Into<String>, initial_vregs: u8, return_width: u8) -> Self {
        Self {
            name: name.into(),
            vreg_count: initial_vregs,
            free_vregs: Vec::new(),
            return_width,
            instructions: Vec::new(),
            profile_instructions: Vec::new(),
            label_pcs: HashMap::new(),
            next_label: 0,
        }
    }

    /// 分配新的 VM label。
    pub fn new_label(&mut self) -> LabelId {
        let label = LabelId(self.next_label);
        self.next_label += 1;
        label
    }

    /// 把 label 绑定到下一条指令 PC。
    pub fn bind_label(&mut self, label: LabelId) {
        self.label_pcs.insert(label, self.instructions.len());
    }

    /// 分配新的、由 `x` 寄存器承载的 VM 虚拟寄存器。
    pub fn alloc_vreg(&mut self) -> anyhow::Result<u8> {
        if let Some(reg) = self.free_vregs.pop() {
            return Ok(reg);
        }

        if self.vreg_count >= 32 {
            anyhow::bail!("VM x-register budget exceeded: x0..x31 are available");
        }
        let reg = self.vreg_count;
        self.vreg_count += 1;
        Ok(reg)
    }

    /// 分配新的 VM `x` 寄存器，并保证它不会被 `native_call` 等 ABI 操作触碰。
    pub fn alloc_vreg_excluding(&mut self, excluded: &HashSet<u8>) -> anyhow::Result<u8> {
        if let Some(index) = self.free_vregs.iter().rposition(|reg| !excluded.contains(reg)) {
            return Ok(self.free_vregs.swap_remove(index));
        }

        while self.vreg_count < 32 {
            let reg = self.vreg_count;
            self.vreg_count += 1;
            if !excluded.contains(&reg) {
                return Ok(reg);
            }
        }

        anyhow::bail!("VM x-register budget exceeded: no register outside native_call clobbers is available");
    }

    /// 记录预分配寄存器已经被使用。
    pub fn reserve_vregs(&mut self, count: u8) -> anyhow::Result<()> {
        if count > 32 {
            anyhow::bail!("VM x-register budget exceeded: requested {count}");
        }
        self.vreg_count = self.vreg_count.max(count);
        self.free_vregs.retain(|reg| *reg < count);
        Ok(())
    }

    /// 在最后一次 SSA 使用已生成后，把 VM 寄存器标记为可复用。
    pub fn release_vreg(&mut self, reg: u8) {
        if reg < self.vreg_count && !self.free_vregs.contains(&reg) {
            self.free_vregs.push(reg);
        }
    }

    /// 追加 VM 指令。
    pub fn push(&mut self, instruction: VmInstruction) {
        let profile_instruction = instruction.default_profile_instruction().to_owned();
        self.push_profile(instruction, profile_instruction);
    }

    /// 追加 VM 指令以及 lowering 选中的精确 profile 指令名。
    pub fn push_profile(&mut self, instruction: VmInstruction, profile_instruction: impl Into<String>) {
        self.instructions.push(instruction);
        self.profile_instructions.push(profile_instruction.into());
    }

    /// 校验 label 一致性后完成 VM 函数构建。
    pub fn finish(self) -> anyhow::Result<VmFunction> {
        if self.instructions.len() != self.profile_instructions.len() {
            anyhow::bail!(
                "VM instruction stream has {} records but {} profile instruction names",
                self.instructions.len(),
                self.profile_instructions.len()
            );
        }
        for label in self.referenced_labels() {
            if !self.label_pcs.contains_key(&label) {
                anyhow::bail!("unbound VM label {:?}", label);
            }
        }

        Ok(VmFunction {
            name: self.name,
            vreg_count: self.vreg_count,
            return_width: self.return_width,
            instructions: self.instructions,
            profile_instructions: self.profile_instructions,
            label_pcs: self.label_pcs,
        })
    }

    fn referenced_labels(&self) -> Vec<LabelId> {
        self.instructions
            .iter()
            .flat_map(|instruction| match instruction {
                VmInstruction::Br { target } => vec![*target],
                VmInstruction::VmCall { target } => vec![*target],
                VmInstruction::BrCond {
                    then_label, else_label, ..
                } => vec![*then_label, *else_label],
                VmInstruction::MovImm { .. }
                | VmInstruction::ConstLoad { .. }
                | VmInstruction::Mov { .. }
                | VmInstruction::Bin { .. }
                | VmInstruction::Icmp { .. }
                | VmInstruction::Cast { .. }
                | VmInstruction::Alloca { .. }
                | VmInstruction::Load { .. }
                | VmInstruction::Store { .. }
                | VmInstruction::Gep { .. }
                | VmInstruction::CallNative { .. }
                | VmInstruction::VmRet
                | VmInstruction::Ret { .. }
                | VmInstruction::RetVoid => Vec::new(),
            })
            .collect()
    }
}
