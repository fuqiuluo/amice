use crate::aotu::vmp::avm::AVMOpcode;
use crate::aotu::vmp::translator::IRConverter;
use crate::config::VMPFlag;
use amice_llvm::inkwell2::{InstructionExt, LLVMValueRefExt};
use anyhow::anyhow;
use bitflags::Flags;
use llvm_plugin::FunctionAnalysisManager;
use llvm_plugin::inkwell::llvm_sys::LLVMValueKind;
use llvm_plugin::inkwell::llvm_sys::core::LLVMGetValueKind;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::values::{
    AnyValue, AsValueRef, BasicValue, FunctionValue, InstructionOpcode, InstructionValue,
};
use log::{Level, debug, log_enabled, warn};
use rand::prelude::IndexedRandom;
use sparse_list::SparseList;
use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::fmt::{Display, Formatter};
use std::process::id;

pub struct AVMCompilerContext {
    /// 变量到内存地址的映射
    variable_to_address: HashMap<LLVMValueRef, VarInfo>,
    /// 变量到寄存器的映射
    variable_to_register: HashMap<LLVMValueRef, VarInfo>,
    /// 下一个可用的内存地址
    next_address: u32,
    /// 下一个可用的寄存器
    next_register: SparseList<LLVMValueRef>,
    /// 生成的VM指令
    vm_instructions: Vec<AVMOpcode>,
    /// 指令使用计数
    use_counts: BTreeMap<LLVMValueRef, InstUseCount>,
    /// Global Variables used in the function
    used_globals: Vec<LLVMValueRef>,
    /// VMP标志
    flags: VMPFlag,
}

#[derive(Debug, Default, Clone)]
pub struct VarInfo {
    /// LLVMRef
    pub(crate) llvm_ref: LLVMValueRef,
    /// 是否是持久变量（如alloca结果）
    pub(crate) is_persistent: bool,
    /// 虚拟寄存器id 或者 虚拟地址
    pub(crate) value: u32,
}

impl VarInfo {
    pub fn new(llvm_ref: LLVMValueRef, is_persistent: bool, value: u32) -> Self {
        Self {
            llvm_ref,
            is_persistent,
            value,
        }
    }
}

impl AVMCompilerContext {
    pub fn new(function: FunctionValue, flags: VMPFlag) -> anyhow::Result<Self> {
        let use_counts = IRAnalyzer::collect_inst_use_counts(function)?;
        let used_globals = IRAnalyzer::collect_global_variables(function);
        let mut ctx = Self {
            vm_instructions: Vec::new(),
            variable_to_address: HashMap::new(),
            variable_to_register: HashMap::new(),
            next_address: 0x1000,
            next_register: SparseList::new(),
            use_counts,
            used_globals: used_globals.clone(),
            flags,
        };
        if flags.contains(VMPFlag::RandomVRegMapping) {
            for _ in 0..rand::random_range(10..10000) {
                ctx.next_register.insert(LLVMValueRef::default());
            }
            ctx.next_register.clear();
        }

        for llvm_ref in used_globals {
            let global_value = llvm_ref.into_global_value();
            let var_info = ctx.get_or_allocate_register(llvm_ref, true);
            ctx.emit(AVMOpcode::MetaGVar {
                reg: var_info.value,
                name: global_value.get_name().to_str()?.to_string(),
            });

            if log_enabled!(Level::Debug) {
                debug!(
                    "(VMP) Allocated register {} for global variable {}",
                    var_info.value, global_value
                );
            }
        }

        Ok(ctx)
    }

    pub fn is_polymorphic_inst(&self) -> bool {
        self.flags.contains(VMPFlag::PolyInstruction)
    }

    pub fn is_type_check_enabled(&self) -> bool {
        self.flags.contains(VMPFlag::TypeCheck)
    }

    pub fn emit(&mut self, opcode: AVMOpcode) {
        self.vm_instructions.push(opcode);
    }

    /// 分配新的内存地址
    fn allocate_address(&mut self) -> u32 {
        let addr = self.next_address;
        self.next_address += 8; // 假设每个变量占8字节
        addr
    }

    /// 分配新的寄存器
    fn allocate_register(&mut self, var: LLVMValueRef) -> u32 {
        if self.flags.contains(VMPFlag::RandomVRegMapping) {
            let free_indices = self.next_register.free_indices();
            if !free_indices.is_empty() {
                let &index = free_indices.choose(&mut rand::rng()).unwrap();
                self.next_register.insert_at(index, var);
                return index as u32;
            }
            // no free register, fall through to normal allocation
        }
        let id = self.next_register.insert(var);
        id as u32
    }

    /// 获取变量对应的地址，如果不存在则分配新地址
    // pub fn get_or_allocate_address(&mut self, var: LLVMValueRef) -> VarInfo {
    //     if let Some(addr) = self.variable_to_address.get(&var) {
    //         addr.clone()
    //     } else {
    //         let addr = self.allocate_address();
    //         let info = VarInfo::new(var, false, addr);
    //         self.variable_to_address.insert(var, info.clone());
    //         info
    //     }
    // }

    /// 获取变量对应的寄存器，如果不存在则分配新寄存器
    pub fn get_or_allocate_register(&mut self, var: LLVMValueRef, is_persistent: bool) -> VarInfo {
        if let Some(reg) = self.variable_to_register.get(&var) {
            reg.clone()
        } else {
            let reg = self.allocate_register(var);
            let info = VarInfo::new(var, is_persistent, reg);
            self.variable_to_register.insert(var, info.clone());
            info
        }
    }

    /// 销毁变量对应的寄存器
    pub fn destroy_register(&mut self, var: LLVMValueRef) -> Option<u32> {
        if let Some(info) = self.variable_to_register.remove(&var) {
            if self.next_register.get(info.value as usize).is_none() {
                panic!("Trying to remove non-existing register");
            }
            self.next_register.remove(info.value as usize);
            return Some(info.value);
        }
        None
    }

    pub fn get_register(&mut self, val: LLVMValueRef) -> anyhow::Result<u32> {
        if let Some(var_info) = self.variable_to_register.get(&val) {
            return Ok(var_info.value);
        };

        for llvm_ref in &self.used_globals {
            if *llvm_ref == val {
                let var_info = self.get_or_allocate_register(*llvm_ref, true);
                return Ok(var_info.value);
            }
        }

        Err(anyhow!("No variable to register: {:?}", val))
    }

    pub fn translate<'a>(&mut self, inst: InstructionValue<'a>) -> anyhow::Result<()> {
        match inst.get_opcode() {
            InstructionOpcode::Add => {
                let add = inst.into_add_inst();
                add.to_avm_ir(self)?;
            },
            InstructionOpcode::AddrSpaceCast => {},
            InstructionOpcode::Alloca => {
                let alloca = inst.into_alloca_inst();
                alloca.to_avm_ir(self)?;
            },
            InstructionOpcode::Store => {
                let store = inst.into_store_inst();
                store.to_avm_ir(self)?;
            },
            InstructionOpcode::Load => {
                let load = inst.into_load_inst();
                load.to_avm_ir(self)?;
            },
            InstructionOpcode::Call => {
                let call = inst.into_call_inst();
                call.to_avm_ir(self)?;
            },
            InstructionOpcode::And => {},
            InstructionOpcode::AShr => {},
            InstructionOpcode::AtomicCmpXchg => {},
            InstructionOpcode::AtomicRMW => {},
            InstructionOpcode::BitCast => {},
            InstructionOpcode::Br => {},
            InstructionOpcode::CallBr => {},
            InstructionOpcode::CatchPad => {},
            InstructionOpcode::CatchRet => {},
            InstructionOpcode::CatchSwitch => {},
            InstructionOpcode::CleanupPad => {},
            InstructionOpcode::CleanupRet => {},
            InstructionOpcode::ExtractElement => {},
            InstructionOpcode::ExtractValue => {},
            InstructionOpcode::FNeg => {},
            InstructionOpcode::FAdd => {},
            InstructionOpcode::FCmp => {},
            InstructionOpcode::FDiv => {},
            InstructionOpcode::Fence => {},
            InstructionOpcode::FMul => {},
            InstructionOpcode::FPExt => {},
            InstructionOpcode::FPToSI => {},
            InstructionOpcode::FPToUI => {},
            InstructionOpcode::FPTrunc => {},
            InstructionOpcode::Freeze => {},
            InstructionOpcode::FRem => {},
            InstructionOpcode::FSub => {},
            InstructionOpcode::GetElementPtr => {
                let gep = inst.into_gep_inst();
                gep.to_avm_ir(self)?;
            },
            InstructionOpcode::ICmp => {},
            InstructionOpcode::IndirectBr => {},
            InstructionOpcode::InsertElement => {},
            InstructionOpcode::InsertValue => {},
            InstructionOpcode::IntToPtr => {},
            InstructionOpcode::Invoke => {},
            InstructionOpcode::LandingPad => {},
            InstructionOpcode::LShr => {},
            InstructionOpcode::Mul => {},
            InstructionOpcode::Or => {},
            InstructionOpcode::Phi => {},
            InstructionOpcode::PtrToInt => {},
            InstructionOpcode::Resume => {},
            InstructionOpcode::Return => {
                let ret = inst.into_return_inst();
                ret.to_avm_ir(self)?;
            },
            InstructionOpcode::SDiv => {},
            InstructionOpcode::Select => {},
            InstructionOpcode::SExt => {},
            InstructionOpcode::Shl => {},
            InstructionOpcode::ShuffleVector => {},
            InstructionOpcode::SIToFP => {},
            InstructionOpcode::SRem => {},
            InstructionOpcode::Sub => {},
            InstructionOpcode::Switch => {},
            InstructionOpcode::Trunc => {},
            InstructionOpcode::UDiv => {},
            InstructionOpcode::UIToFP => {},
            InstructionOpcode::Unreachable => {},
            InstructionOpcode::URem => {},
            InstructionOpcode::UserOp1 => {},
            InstructionOpcode::UserOp2 => {},
            InstructionOpcode::VAArg => {},
            InstructionOpcode::Xor => {},
            InstructionOpcode::ZExt => {},
        }

        let mut destroy_registers = Vec::new();
        for operand in inst.get_operands() {
            if let Some(operand) = operand
                && let Some(left) = operand.left()
                && let Some(sub_inst) = left.as_instruction_value()
            {
                let Some(inst_use_count_info) = self.use_counts.get_mut(&(sub_inst.as_value_ref() as LLVMValueRef))
                else {
                    continue;
                };
                if inst_use_count_info.current_count > 0 {
                    inst_use_count_info.current_count -= 1;
                } else {
                    panic!(
                        "(VMP) Instruction use count underflow for {}",
                        inst_use_count_info.ssa_name
                    );
                }
                if inst_use_count_info.current_count == 0 {
                    let Some(register) = self
                        .variable_to_register
                        .get(&(sub_inst.as_value_ref() as LLVMValueRef))
                    else {
                        warn!(
                            "(VMP) Register for {} ({}) already destroyed or never allocated",
                            inst_use_count_info.ssa_name, inst
                        );
                        continue;
                    };

                    if !register.is_persistent {
                        destroy_registers.push(inst_use_count_info.clone());
                        if log_enabled!(Level::Debug) {
                            debug!("(VMP) destroy register for {} ({})", inst_use_count_info.ssa_name, inst);
                        }
                    }
                } else {
                    if log_enabled!(Level::Debug) {
                        debug!("(VMP) keep register for {} ({})", inst_use_count_info.ssa_name, inst);
                    }
                }
            }
        }

        for var in destroy_registers {
            if let Some(reg) = self.destroy_register(var.inst)
                && self.flags.contains(VMPFlag::AutoCleanupRegister)
            {
                self.emit(AVMOpcode::ClearReg { reg })
            }
        }

        Ok(())
    }

    pub fn finalize(&self) -> &Vec<AVMOpcode> {
        &self.vm_instructions
    }
}

impl Display for AVMCompilerContext {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "inst[\n")?;
        for opcode in self.finalize() {
            write!(f, "  {}\n", opcode)?;
        }
        write!(f, "]")
    }
}

#[derive(Debug, Clone)]
struct InstUseCount {
    inst: LLVMValueRef,
    max_count: u32,
    current_count: u32,
    ssa_name: String,
}

struct IRAnalyzer;

impl IRAnalyzer {
    fn collect_inst_use_counts(function: FunctionValue) -> anyhow::Result<BTreeMap<LLVMValueRef, InstUseCount>> {
        let mut use_counts: BTreeMap<LLVMValueRef, InstUseCount> = BTreeMap::new();
        for bb in function.get_basic_blocks() {
            for inst in bb.get_instructions() {
                // 拒绝很难被翻译的指令
                if matches!(
                    inst.get_opcode(),
                    InstructionOpcode::IndirectBr
                        | InstructionOpcode::Invoke
                        | InstructionOpcode::CallBr
                        | InstructionOpcode::Resume
                        | InstructionOpcode::CatchPad
                        | InstructionOpcode::CatchRet
                        | InstructionOpcode::CatchSwitch
                        | InstructionOpcode::CleanupPad
                        | InstructionOpcode::CleanupRet
                ) {
                    return Err(anyhow!("Unsupported instruction for VMP: {:?}", inst));
                }

                // 只统计会产生值（非 void）的指令
                let ty = inst.get_type();
                if ty.is_void_type() {
                    continue;
                }

                let mut cnt = 0usize;
                let mut use_opt = inst.get_first_use();
                while let Some(u) = use_opt {
                    cnt += 1;
                    use_opt = u.get_next_use();
                }

                let key = Self::ssa_name_of(&inst);
                use_counts.insert(
                    inst.as_value_ref() as LLVMValueRef,
                    InstUseCount {
                        inst: inst.as_value_ref() as LLVMValueRef,
                        max_count: cnt as u32,
                        current_count: cnt as u32,
                        ssa_name: key,
                    },
                );
            }
        }

        Ok(use_counts)
    }

    /// 收集函数体所有指令操作数中被使用到的全局变量（去重）
    /// 返回全局变量对应的 LLVMValueRef 列表
    fn collect_global_variables(function: FunctionValue) -> Vec<LLVMValueRef> {
        use std::collections::BTreeSet;

        let mut globals: BTreeSet<LLVMValueRef> = BTreeSet::new();

        for bb in function.get_basic_blocks() {
            for inst in bb.get_instructions() {
                for operand in inst.get_operands() {
                    if let Some(operand) = operand {
                        if let Some(val) = operand.left() {
                            let vref = val.as_value_ref() as LLVMValueRef;
                            // 识别是否为全局变量
                            let kind = unsafe { LLVMGetValueKind(vref) };
                            if kind == LLVMValueKind::LLVMGlobalVariableValueKind {
                                globals.insert(vref);
                            }
                        }
                    }
                }
            }
        }

        globals.into_iter().collect()
    }

    fn ssa_name_of(inst: &InstructionValue) -> String {
        // 优先用显式名称
        if let Some(name_cstr) = inst.get_name() {
            if !name_cstr.to_bytes().is_empty() {
                return format!("%{}", name_cstr.to_string_lossy());
            }
        }
        // 没有显式名称时，尝试从 IR 文本提取 “%N = ” 前缀
        let ir = inst.print_to_string().to_string();
        // 形如： "  %1 = add i32 2, %0"
        if let Some(eq_pos) = ir.find(" = ") {
            let left = &ir[..eq_pos];
            if let Some(percent_pos) = left.rfind('%') {
                let name = left[percent_pos..].trim();
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
        // 实在没有就：用操作码+地址做区分（避免重名）
        format!("<{:?}@{:p}>", inst.get_opcode(), inst.as_value_ref())
    }
}
