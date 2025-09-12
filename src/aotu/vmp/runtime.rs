use crate::aotu::vmp::isa::{VMPOpcode, VMPProgram, VMPValue};
use anyhow::{Result, anyhow};
use log::debug;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};

/// 对象头类型标识符
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
enum TypeTag {
    Undef = 0,
    I1 = 1,
    I8 = 2,
    I16 = 3,
    I32 = 4,
    I64 = 5,
    F32 = 6,
    F64 = 7,
    Ptr = 8,
}

impl TypeTag {
    fn from_value(value: &VMPValue) -> Self {
        match value {
            VMPValue::Undef => TypeTag::Undef,
            VMPValue::I1(_) => TypeTag::I1,
            VMPValue::I8(_) => TypeTag::I8,
            VMPValue::I16(_) => TypeTag::I16,
            VMPValue::I32(_) => TypeTag::I32,
            VMPValue::I64(_) => TypeTag::I64,
            VMPValue::F32(_) => TypeTag::F32,
            VMPValue::F64(_) => TypeTag::F64,
            VMPValue::Ptr(_) => TypeTag::Ptr,
        }
    }

    fn from_u8(tag: u8) -> Result<Self> {
        match tag {
            0 => Ok(TypeTag::Undef),
            1 => Ok(TypeTag::I1),
            2 => Ok(TypeTag::I8),
            3 => Ok(TypeTag::I16),
            4 => Ok(TypeTag::I32),
            5 => Ok(TypeTag::I64),
            6 => Ok(TypeTag::F32),
            7 => Ok(TypeTag::F64),
            8 => Ok(TypeTag::Ptr),
            _ => Err(anyhow!("Invalid type tag: {}", tag)),
        }
    }

    fn value_size(&self) -> usize {
        match self {
            TypeTag::Undef => 0,
            TypeTag::I1 => 1,
            TypeTag::I8 => 1,
            TypeTag::I16 => 2,
            TypeTag::I32 => 4,
            TypeTag::I64 => 8,
            TypeTag::F32 => 4,
            TypeTag::F64 => 8,
            TypeTag::Ptr => 8,
        }
    }
}

/// 完整的虚拟机运行时，支持跳转和控制流
#[repr(C)]
pub struct VMPRuntime {
    /// 虚拟栈
    stack: Vec<VMPValue>,
    /// 寄存器组（稀疏存储）
    registers: HashMap<u32, VMPValue>,
    /// 内存堆
    memory: Vec<u8>,
    /// 内存分配器状态
    memory_allocator: MemoryAllocator,
    /// 函数调用表
    syscalls_table: HashMap<u64, Box<dyn Fn(&VMPRuntime) -> Result<VMPValue>>>,
    function_table: HashMap<String, Box<dyn Fn(&VMPRuntime) -> Result<VMPValue>>>,
    /// 程序计数器
    pc: usize,
    /// 标签映射表（标签名 -> 指令索引）
    labels: HashMap<String, usize>,
    /// 调用栈（用于函数调用和返回）
    call_stack: Vec<CallFrame>,
    /// 执行统计
    stats: ExecutionStats,
    /// 调试信息
    debug_mode: bool,
}

#[repr(C)]
struct MemoryAllocator {
    next_address: usize,
    allocations: HashMap<usize, usize>, // address -> size
}

#[derive(Debug, Clone)]
struct CallFrame {
    return_pc: usize,
    saved_registers: HashMap<u32, VMPValue>,
    frame_pointer: usize,
}

#[derive(Debug, Default)]
pub struct ExecutionStats {
    pub instructions_executed: usize,
    pub function_calls: usize,
    pub memory_allocations: usize,
    pub stack_max_depth: usize,
    pub execution_time_ns: u64,
}

impl VMPRuntime {
    pub fn new() -> Self {
        let mut this = Self {
            stack: Vec::new(),
            registers: HashMap::new(),
            memory: vec![0; 1024 * 1024], // 1MB initial memory
            memory_allocator: MemoryAllocator {
                next_address: 0x1000,
                allocations: HashMap::new(),
            },
            syscalls_table: HashMap::new(),
            function_table: HashMap::new(),
            pc: 0,
            labels: HashMap::new(),
            call_stack: Vec::new(),
            stats: ExecutionStats::default(),
            debug_mode: false,
        };
        this.register_builtins();
        this
    }

    /// 设置调试模式
    pub fn set_debug_mode(&mut self, enabled: bool) {
        self.debug_mode = enabled;
    }

    /// 注册内置函数（如 printf）
    fn register_builtins(&mut self) {
        self.function_table
            .insert("printf".to_string(), Box::new(|runtime| Ok(VMPValue::Undef)));
    }

    pub fn push_stack(&mut self, value: VMPValue) {
        self.stack.push(value);
        self.stats.stack_max_depth = self.stats.stack_max_depth.max(self.stack.len());
    }

    pub fn pop_stack(&mut self) -> Option<VMPValue> {
        self.stack.pop()
    }

    pub fn peek_stack(&self) -> Result<&VMPValue> {
        self.stack.last().ok_or_else(|| anyhow!("Stack underflow"))
    }

    pub fn set_register(&mut self, reg: u32, value: VMPValue) {
        self.registers.insert(reg, value);
    }

    /// 获取执行统计信息
    pub fn get_stats(&self) -> &ExecutionStats {
        &self.stats
    }

    /// 重置运行时状态
    pub fn reset(&mut self) {
        self.stack.clear();
        self.registers.clear();
        self.pc = 0;
        self.labels.clear();
        self.call_stack.clear();
        self.stats = ExecutionStats::default();
    }

    /// 注册外部函数
    pub fn register_function(&mut self, id: u64, func: Box<dyn Fn(&VMPRuntime) -> Result<VMPValue>>) {
        self.syscalls_table.insert(id, func);
    }

    /// 预处理指令序列，建立标签映射
    fn preprocess_instructions(&mut self, instructions: &[VMPOpcode]) -> Result<()> {
        self.labels.clear();

        for (i, inst) in instructions.iter().enumerate() {
            if let VMPOpcode::Label { name } = inst {
                if self.labels.contains_key(name) {
                    return Err(anyhow!("Duplicate label: {}", name));
                }
                self.labels.insert(name.clone(), i);
            }
        }

        if self.debug_mode {
            debug!("Found {} labels: {:?}", self.labels.len(), self.labels);
        }

        Ok(())
    }

    /// 执行指令序列
    pub fn execute(&mut self, instructions: &[VMPOpcode]) -> Result<Option<VMPValue>> {
        let start_time = std::time::Instant::now();

        // 预处理指令建立标签映射
        self.preprocess_instructions(instructions)?;

        self.pc = 0;

        while self.pc < instructions.len() {
            let inst = &instructions[self.pc];

            if self.debug_mode {
                debug!("PC: {}, Stack: {:?}, Inst: {}", self.pc, self.stack, inst);
            }

            match self.execute_instruction(inst, instructions)? {
                ControlFlow::Continue => {
                    self.pc += 1;
                },
                ControlFlow::Jump(target_pc) => {
                    self.pc = target_pc;
                },
                ControlFlow::Return(value) => {
                    self.stats.execution_time_ns = start_time.elapsed().as_nanos() as u64;
                    return Ok(value);
                },
                ControlFlow::Call(target_pc, return_pc) => {
                    // 保存当前状态到调用栈
                    let frame = CallFrame {
                        return_pc,
                        saved_registers: self.registers.clone(),
                        frame_pointer: self.stack.len(),
                    };
                    self.call_stack.push(frame);
                    self.pc = target_pc;
                    self.stats.function_calls += 1;
                },
            }

            self.stats.instructions_executed += 1;
            self.stats.stack_max_depth = self.stats.stack_max_depth.max(self.stack.len());
        }

        self.stats.execution_time_ns = start_time.elapsed().as_nanos() as u64;

        self.stats.print_summary();
        Ok(self.pop_stack())
    }

    /// 执行单条指令
    fn execute_instruction(&mut self, inst: &VMPOpcode, instructions: &[VMPOpcode]) -> Result<ControlFlow> {
        match inst {
            VMPOpcode::Push { value } => {
                self.stack.push(value.clone());
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::Pop => {
                if self.stack.is_empty() {
                    return Err(anyhow!("Stack underflow on POP at PC {}", self.pc));
                }
                self.stack.pop();
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::PopToReg { reg } => {
                let value = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on PopToReg at PC {}", self.pc))?;
                self.registers.insert(*reg, value);
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::PushFromReg { reg } => {
                let value = self
                    .registers
                    .get(reg)
                    .ok_or_else(|| anyhow!("Register {} not found at PC {}", reg, self.pc))?
                    .clone();
                self.stack.push(value);
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::ClearReg { reg } => {
                self.registers.remove(reg);
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::Alloca { size } => {
                let address = self.allocate_memory(*size + 1)?;
                self.stack.push(VMPValue::Ptr(address - 1));
                self.stats.memory_allocations += 1;
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::Alloca2 => {
                let size_val = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Alloca2 at PC {}", self.pc))?;
                let size = match size_val {
                    VMPValue::I64(s) => s as usize,
                    VMPValue::I32(s) => s as usize,
                    _ => return Err(anyhow!("Invalid size type for Alloca2 at PC {}", self.pc)),
                };
                let address = self.allocate_memory(size + 1)?;
                self.stack.push(VMPValue::Ptr(address - 1));
                self.stats.memory_allocations += 1;
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::Store { address } => {
                let value = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Store at PC {}", self.pc))?;
                self.store_value_memory(*address, &value)?;
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::StoreValue => {
                let value = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on StoreValue at PC {}", self.pc))?;
                let ptr = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on StoreValue at PC {}", self.pc))?;
                let address = match ptr {
                    VMPValue::Ptr(addr) => addr,
                    _ => return Err(anyhow!("Invalid pointer type for StoreValue at PC {}", self.pc)),
                };
                self.store_value_memory(address, &value)?;
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::Load { address } => {
                let value = self.load_value_memory(*address)?;
                self.stack.push(value);
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::LoadValue => {
                let ptr = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on LoadValue at PC {}", self.pc))?;
                let address = match ptr {
                    VMPValue::Ptr(addr) => addr,
                    _ => return Err(anyhow!("Invalid pointer type for LoadValue at PC {}", self.pc)),
                };
                let value = self.load_value_memory(address)?;
                self.stack.push(value);
                Ok(ControlFlow::Continue)
            },

            // 算术运算
            VMPOpcode::Add { nsw: _, nuw: _ } => {
                let rhs = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Add at PC {}", self.pc))?;
                let lhs = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Add at PC {}", self.pc))?;
                let result = self.add_values(&lhs, &rhs)?;
                self.stack.push(result);
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::Sub => {
                let rhs = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Sub at PC {}", self.pc))?;
                let lhs = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Sub at PC {}", self.pc))?;
                let result = self.sub_values(&lhs, &rhs)?;
                self.stack.push(result);
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::Mul => {
                let rhs = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Mul at PC {}", self.pc))?;
                let lhs = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Mul at PC {}", self.pc))?;
                let result = self.mul_values(&lhs, &rhs)?;
                self.stack.push(result);
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::Div => {
                let rhs = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Div at PC {}", self.pc))?;
                let lhs = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Div at PC {}", self.pc))?;
                let result = self.div_values(&lhs, &rhs)?;
                self.stack.push(result);
                Ok(ControlFlow::Continue)
            },

            // 跳转指令
            VMPOpcode::Jump { target } => {
                let target_pc = self
                    .labels
                    .get(target)
                    .ok_or_else(|| anyhow!("Label '{}' not found at PC {}", target, self.pc))?;
                Ok(ControlFlow::Jump(*target_pc))
            },

            VMPOpcode::JumpIf { target } => {
                let condition = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on JumpIf at PC {}", self.pc))?;
                if condition.is_true() {
                    let target_pc = self
                        .labels
                        .get(target)
                        .ok_or_else(|| anyhow!("Label '{}' not found at PC {}", target, self.pc))?;
                    Ok(ControlFlow::Jump(*target_pc))
                } else {
                    Ok(ControlFlow::Continue)
                }
            },

            VMPOpcode::JumpIfNot { target } => {
                let condition = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on JumpIfNot at PC {}", self.pc))?;
                if !condition.is_true() {
                    let target_pc = self
                        .labels
                        .get(target)
                        .ok_or_else(|| anyhow!("Label '{}' not found at PC {}", target, self.pc))?;
                    Ok(ControlFlow::Jump(*target_pc))
                } else {
                    Ok(ControlFlow::Continue)
                }
            },

            // 比较指令
            VMPOpcode::ICmpEq => {
                unimplemented!()
            },

            VMPOpcode::ICmpNe => {
                unimplemented!()
            },

            VMPOpcode::ICmpSlt => {
                unimplemented!()
            },

            VMPOpcode::ICmpSle => {
                unimplemented!()
            },

            VMPOpcode::ICmpSgt => {
                unimplemented!()
            },

            VMPOpcode::ICmpSge => {
                unimplemented!()
            },

            VMPOpcode::ICmpUlt => {
                unimplemented!()
            },

            VMPOpcode::ICmpUle => {
                unimplemented!()
            },

            VMPOpcode::ICmpUgt => {
                unimplemented!()
            },

            VMPOpcode::ICmpUge => {
                unimplemented!()
            },

            // 位运算指令
            VMPOpcode::And => {
                unimplemented!()
            },

            VMPOpcode::Or => {
                unimplemented!()
            },

            VMPOpcode::Xor => {
                unimplemented!()
            },

            VMPOpcode::Shl => {
                unimplemented!()
            },

            VMPOpcode::LShr => {
                unimplemented!()
            },

            VMPOpcode::AShr => {
                unimplemented!()
            },

            // 类型转换指令
            VMPOpcode::Trunc { target_width } => {
                unimplemented!()
            },

            VMPOpcode::ZExt { target_width } => {
                unimplemented!()
            },

            VMPOpcode::SExt { target_width } => {
                unimplemented!()
            },

            VMPOpcode::FPToSI { target_width } => {
                unimplemented!()
            },

            VMPOpcode::FPToUI { target_width } => {
                unimplemented!()
            },

            VMPOpcode::SIToFP { is_double } => {
                unimplemented!()
            },

            VMPOpcode::UIToFP { is_double } => {
                unimplemented!()
            },

            VMPOpcode::Call {
                function_name,
                is_void,
                arg_num,
                ..
            } => {
                // 简化的函数调用处理
                if *arg_num > 0 {
                    debug!("called {}", function_name);
                    for _ in 0..*arg_num {
                        self.stack
                            .pop()
                            .ok_or_else(|| anyhow!("Stack underflow on Call arguments at PC {}", self.pc))?;
                    }
                }

                // todo
                // if let Some(func) = self.function_table.get(function_name) {
                //     func();
                //     if !*is_void {
                //         // 假设函数返回 i32 0（简化处理）
                //         self.stack.push(VMPValue::I32(0));
                //     }
                // } else {
                //     return Err(anyhow!("Function {} not found at PC {}", function_name, self.pc));
                // }

                self.stats.function_calls += 1;
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::Ret => {
                let ret_val = if self.stack.is_empty() {
                    None
                } else {
                    Some(self.stack.pop().unwrap())
                };

                // 检查是否有调用栈帧需要恢复
                if let Some(frame) = self.call_stack.pop() {
                    // 恢复调用者状态
                    self.registers = frame.saved_registers;
                    Ok(ControlFlow::Jump(frame.return_pc))
                } else {
                    // 主函数返回
                    Ok(ControlFlow::Return(ret_val))
                }
            },

            VMPOpcode::Nop => Ok(ControlFlow::Continue),

            VMPOpcode::Swap => {
                if self.stack.len() < 2 {
                    return Err(anyhow!("Stack underflow on Swap at PC {}", self.pc));
                }
                let len = self.stack.len();
                self.stack.swap(len - 1, len - 2);
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::Dup => {
                let top = self
                    .stack
                    .last()
                    .ok_or_else(|| anyhow!("Stack underflow on Dup at PC {}", self.pc))?
                    .clone();
                self.stack.push(top);
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::TypeCheckInt { width } => {
                let top = self
                    .stack
                    .last()
                    .ok_or_else(|| anyhow!("Stack underflow on TypeCheckInt at PC {}", self.pc))?;
                let actual_width = top.width_in_bits();
                if actual_width != *width as usize {
                    return Err(anyhow!(
                        "Type check failed at PC {}: expected {}-bit int, got {}-bit",
                        self.pc,
                        width,
                        actual_width
                    ));
                }
                Ok(ControlFlow::Continue)
            },

            VMPOpcode::Label { name: _ } => {
                // 标签不需要执行任何操作
                Ok(ControlFlow::Continue)
            },
            VMPOpcode::MetaGVar { reg, name } => {
                // 全局变量元信息不需要执行任何操作
                println!("[RT] MetaGVar {} {}", reg, name);
                self.set_register(*reg, VMPValue::Undef);
                Ok(ControlFlow::Continue)
            },
        }
    }

    // 内存操作方法 - 使用对象头代替typed_memory
    fn allocate_memory(&mut self, size: usize) -> Result<usize> {
        let address = self.memory_allocator.next_address;

        if address + size > self.memory.len() {
            // Expand memory if needed
            self.memory.resize(address + size + 1024, 0);
        }

        self.memory_allocator.allocations.insert(address, size);
        self.memory_allocator.next_address = address + size;

        Ok(address)
    }

    // 存储值到内存，格式：[类型标签(1字节)] + [值数据]
    fn store_value_memory(&mut self, address: usize, value: &VMPValue) -> Result<()> {
        let type_tag = TypeTag::from_value(value);
        let value_bytes = self.value_to_bytes(value);
        let total_size = 1 + value_bytes.len(); // 1字节类型标签 + 值大小

        if address + total_size > self.memory.len() {
            return Err(anyhow!("Memory store out of bounds"));
        }

        // 写入类型标签
        self.memory[address - 1] = type_tag as u8;

        // 写入值数据
        if !value_bytes.is_empty() {
            self.memory[address..address + value_bytes.len()].copy_from_slice(&value_bytes);
        }

        if self.debug_mode {
            debug!(
                "Stored {:?} at address {:#x} with type tag {:?}",
                value, address, type_tag
            );
        }

        Ok(())
    }

    // 从内存加载值，读取对象头确定类型
    fn load_value_memory(&self, address: usize) -> Result<VMPValue> {
        if address >= self.memory.len() {
            return Err(anyhow!("Memory load out of bounds: address {:#x}", address));
        }

        // 读取类型标签
        let type_tag = TypeTag::from_u8(self.memory[address - 1])?;
        let value_size = type_tag.value_size();

        if address + value_size > self.memory.len() {
            return Err(anyhow!(
                "Memory load out of bounds: insufficient data for type {:?}",
                type_tag
            ));
        }

        // 根据类型标签读取相应大小的数据
        let value = match type_tag {
            TypeTag::Undef => VMPValue::Undef,
            TypeTag::I1 => {
                let byte = self.memory[address];
                VMPValue::I1(byte != 0)
            },
            TypeTag::I8 => {
                let byte = self.memory[address];
                VMPValue::I8(byte as i8)
            },
            TypeTag::I16 => {
                let mut bytes = [0u8; 2];
                bytes.copy_from_slice(&self.memory[address..address + type_tag.value_size()]);
                VMPValue::I16(i16::from_le_bytes(bytes))
            },
            TypeTag::I32 => {
                let mut bytes = [0u8; 4];
                bytes.copy_from_slice(&self.memory[address..address + type_tag.value_size()]);
                VMPValue::I32(i32::from_le_bytes(bytes))
            },
            TypeTag::I64 => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&self.memory[address..address + type_tag.value_size()]);
                VMPValue::I64(i64::from_le_bytes(bytes))
            },
            TypeTag::F32 => {
                let mut bytes = [0u8; 4];
                bytes.copy_from_slice(&self.memory[address..address + type_tag.value_size()]);
                VMPValue::F32(f32::from_le_bytes(bytes))
            },
            TypeTag::F64 => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&self.memory[address..address + type_tag.value_size()]);
                VMPValue::F64(f64::from_le_bytes(bytes))
            },
            TypeTag::Ptr => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&self.memory[address..address + type_tag.value_size()]);
                VMPValue::Ptr(usize::from_le_bytes(bytes))
            },
        };

        if self.debug_mode {
            debug!(
                "Loaded {:?} from address {:#x} with type tag {:?}",
                value, address, type_tag
            );
        }

        Ok(value)
    }

    fn value_to_bytes(&self, value: &VMPValue) -> Vec<u8> {
        match value {
            VMPValue::I1(v) => vec![if *v { 1 } else { 0 }],
            VMPValue::I8(v) => v.to_le_bytes().to_vec(),
            VMPValue::I16(v) => v.to_le_bytes().to_vec(),
            VMPValue::I32(v) => v.to_le_bytes().to_vec(),
            VMPValue::I64(v) => v.to_le_bytes().to_vec(),
            VMPValue::F32(v) => v.to_le_bytes().to_vec(),
            VMPValue::F64(v) => v.to_le_bytes().to_vec(),
            VMPValue::Ptr(v) => v.to_le_bytes().to_vec(),
            VMPValue::Undef => vec![], // 未定义值不需要数据
        }
    }

    fn add_values(&self, lhs: &VMPValue, rhs: &VMPValue) -> Result<VMPValue> {
        match (lhs, rhs) {
            (VMPValue::I32(l), VMPValue::I32(r)) => Ok(VMPValue::I32(l.wrapping_add(*r))),
            (VMPValue::I64(l), VMPValue::I64(r)) => Ok(VMPValue::I64(l.wrapping_add(*r))),
            (VMPValue::F32(l), VMPValue::F32(r)) => Ok(VMPValue::F32(l + r)),
            (VMPValue::F64(l), VMPValue::F64(r)) => Ok(VMPValue::F64(l + r)),
            (VMPValue::Ptr(l), VMPValue::I64(r)) => Ok(VMPValue::Ptr(l.wrapping_add(*r as usize))),
            _ => Err(anyhow!("Incompatible types for addition: {:?} + {:?}", lhs, rhs)),
        }
    }

    fn sub_values(&self, lhs: &VMPValue, rhs: &VMPValue) -> Result<VMPValue> {
        match (lhs, rhs) {
            (VMPValue::I32(l), VMPValue::I32(r)) => Ok(VMPValue::I32(l.wrapping_sub(*r))),
            (VMPValue::I64(l), VMPValue::I64(r)) => Ok(VMPValue::I64(l.wrapping_sub(*r))),
            (VMPValue::F32(l), VMPValue::F32(r)) => Ok(VMPValue::F32(l - r)),
            (VMPValue::F64(l), VMPValue::F64(r)) => Ok(VMPValue::F64(l - r)),
            _ => Err(anyhow!("Incompatible types for subtraction: {:?} - {:?}", lhs, rhs)),
        }
    }

    fn mul_values(&self, lhs: &VMPValue, rhs: &VMPValue) -> Result<VMPValue> {
        match (lhs, rhs) {
            (VMPValue::I32(l), VMPValue::I32(r)) => Ok(VMPValue::I32(l.wrapping_mul(*r))),
            (VMPValue::I64(l), VMPValue::I64(r)) => Ok(VMPValue::I64(l.wrapping_mul(*r))),
            (VMPValue::F32(l), VMPValue::F32(r)) => Ok(VMPValue::F32(l * r)),
            (VMPValue::F64(l), VMPValue::F64(r)) => Ok(VMPValue::F64(l * r)),
            _ => Err(anyhow!("Incompatible types for multiplication: {:?} * {:?}", lhs, rhs)),
        }
    }

    fn div_values(&self, lhs: &VMPValue, rhs: &VMPValue) -> Result<VMPValue> {
        match (lhs, rhs) {
            (VMPValue::I32(l), VMPValue::I32(r)) => {
                if *r == 0 {
                    return Err(anyhow!("Division by zero"));
                }
                Ok(VMPValue::I32(l / r))
            },
            (VMPValue::I64(l), VMPValue::I64(r)) => {
                if *r == 0 {
                    return Err(anyhow!("Division by zero"));
                }
                Ok(VMPValue::I64(l / r))
            },
            (VMPValue::F32(l), VMPValue::F32(r)) => Ok(VMPValue::F32(l / r)),
            (VMPValue::F64(l), VMPValue::F64(r)) => Ok(VMPValue::F64(l / r)),
            _ => Err(anyhow!("Incompatible types for division: {:?} / {:?}", lhs, rhs)),
        }
    }
}

/// 控制流类型
#[derive(Debug)]
enum ControlFlow {
    Continue,
    Jump(usize),
    Call(usize, usize), // target_pc, return_pc
    Return(Option<VMPValue>),
}

impl ExecutionStats {
    pub fn print_summary(&self) {
        println!("=== Execution Statistics ===");
        println!("Instructions executed: {}", self.instructions_executed);
        println!("Function calls: {}", self.function_calls);
        println!("Memory allocations: {}", self.memory_allocations);
        println!("Stack max depth: {}", self.stack_max_depth);
        println!(
            "Execution time: {} ns ({:.3} ms)",
            self.execution_time_ns,
            self.execution_time_ns as f64 / 1_000_000.0
        );

        if self.instructions_executed > 0 {
            let avg_time_per_inst = self.execution_time_ns / self.instructions_executed as u64;
            println!("Average time per instruction: {} ns", avg_time_per_inst);
        }
    }
}
