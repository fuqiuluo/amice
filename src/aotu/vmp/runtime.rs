use crate::aotu::vmp::avm::{AVMOpcode, AVMProgram, AVMValue};
use anyhow::{Result, anyhow};
use log::debug;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};

/// 完整的虚拟机运行时，支持跳转和控制流
#[repr(C)]
pub struct JustAVMRuntime {
    /// 虚拟栈
    stack: Vec<AVMValue>,
    /// 寄存器组（稀疏存储）
    registers: HashMap<u32, AVMValue>,
    /// 内存堆
    memory: Vec<u8>,
    /// 内存分配器状态
    memory_allocator: MemoryAllocator,
    /// 类型化内存（地址 -> 最近一次写入的值，保留类型信息）
    typed_memory: HashMap<usize, AVMValue>,
    /// 函数调用表
    syscalls_table: HashMap<u64, Box<dyn Fn(&JustAVMRuntime) -> Result<AVMValue>>>,
    function_table: HashMap<String, Box<dyn Fn(&JustAVMRuntime) -> Result<AVMValue>>>,
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
    saved_registers: HashMap<u32, AVMValue>,
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

impl JustAVMRuntime {
    pub fn new() -> Self {
        let mut this = Self {
            stack: Vec::new(),
            registers: HashMap::new(),
            memory: vec![0; 1024 * 1024], // 1MB initial memory
            memory_allocator: MemoryAllocator {
                next_address: 0x1000,
                allocations: HashMap::new(),
            },
            typed_memory: HashMap::new(),
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
            .insert("printf".to_string(), Box::new(|runtime| Ok(AVMValue::Undef)));
    }

    pub fn push_stack(&mut self, value: AVMValue) {
        self.stack.push(value);
        self.stats.stack_max_depth = self.stats.stack_max_depth.max(self.stack.len());
    }

    pub fn pop_stack(&mut self) -> Option<AVMValue> {
        self.stack.pop()
    }

    pub fn peek_stack(&self) -> Result<&AVMValue> {
        self.stack.last().ok_or_else(|| anyhow!("Stack underflow"))
    }

    pub fn set_register(&mut self, reg: u32, value: AVMValue) {
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
        self.typed_memory.clear();
    }

    /// 注册外部函数
    pub fn register_function(&mut self, id: u64, func: Box<dyn Fn(&JustAVMRuntime) -> Result<AVMValue>>) {
        self.syscalls_table.insert(id, func);
    }

    /// 预处理指令序列，建立标签映射
    fn preprocess_instructions(&mut self, instructions: &[AVMOpcode]) -> Result<()> {
        self.labels.clear();

        for (i, inst) in instructions.iter().enumerate() {
            if let AVMOpcode::Label { name } = inst {
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
    pub fn execute(&mut self, instructions: &[AVMOpcode]) -> Result<Option<AVMValue>> {
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
    fn execute_instruction(&mut self, inst: &AVMOpcode, instructions: &[AVMOpcode]) -> Result<ControlFlow> {
        match inst {
            AVMOpcode::Push { value } => {
                self.stack.push(value.clone());
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::Pop => {
                if self.stack.is_empty() {
                    return Err(anyhow!("Stack underflow on POP at PC {}", self.pc));
                }
                self.stack.pop();
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::PopToReg { reg } => {
                let value = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on PopToReg at PC {}", self.pc))?;
                self.registers.insert(*reg, value);
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::PushFromReg { reg } => {
                let value = self
                    .registers
                    .get(reg)
                    .ok_or_else(|| anyhow!("Register {} not found at PC {}", reg, self.pc))?
                    .clone();
                self.stack.push(value);
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::ClearReg { reg } => {
                self.registers.remove(reg);
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::Alloca { size } => {
                let address = self.allocate_memory(*size)?;
                self.stack.push(AVMValue::Ptr(address));
                self.stats.memory_allocations += 1;
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::Alloca2 => {
                let size_val = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Alloca2 at PC {}", self.pc))?;
                let size = match size_val {
                    AVMValue::I64(s) => s as usize,
                    AVMValue::I32(s) => s as usize,
                    _ => return Err(anyhow!("Invalid size type for Alloca2 at PC {}", self.pc)),
                };
                let address = self.allocate_memory(size)?;
                self.stack.push(AVMValue::Ptr(address));
                self.stats.memory_allocations += 1;
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::Store { address } => {
                let value = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on Store at PC {}", self.pc))?;
                self.store_memory(*address, &value)?;
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::StoreValue => {
                let value = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on StoreValue at PC {}", self.pc))?;
                let ptr = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on StoreValue at PC {}", self.pc))?;
                let address = match ptr {
                    AVMValue::Ptr(addr) => addr,
                    _ => return Err(anyhow!("Invalid pointer type for StoreValue at PC {}", self.pc)),
                };
                self.store_memory(address, &value)?;
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::Load { address } => {
                let value = self.load_memory(*address)?;
                self.stack.push(value);
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::LoadValue => {
                let ptr = self
                    .stack
                    .pop()
                    .ok_or_else(|| anyhow!("Stack underflow on LoadValue at PC {}", self.pc))?;
                let address = match ptr {
                    AVMValue::Ptr(addr) => addr,
                    _ => return Err(anyhow!("Invalid pointer type for LoadValue at PC {}", self.pc)),
                };
                let value = self.load_memory(address)?;
                self.stack.push(value);
                Ok(ControlFlow::Continue)
            },

            // 算术运算
            AVMOpcode::Add { nsw: _, nuw: _ } => {
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

            AVMOpcode::Sub => {
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

            AVMOpcode::Mul => {
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

            AVMOpcode::Div => {
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
            AVMOpcode::Jump { target } => {
                let target_pc = self
                    .labels
                    .get(target)
                    .ok_or_else(|| anyhow!("Label '{}' not found at PC {}", target, self.pc))?;
                Ok(ControlFlow::Jump(*target_pc))
            },

            AVMOpcode::JumpIf { target } => {
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

            AVMOpcode::JumpIfNot { target } => {
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
            AVMOpcode::ICmpEq => {
                unimplemented!()
            },

            AVMOpcode::ICmpNe => {
                unimplemented!()
            },

            AVMOpcode::ICmpSlt => {
                unimplemented!()
            },

            AVMOpcode::ICmpSle => {
                unimplemented!()
            },

            AVMOpcode::ICmpSgt => {
                unimplemented!()
            },

            AVMOpcode::ICmpSge => {
                unimplemented!()
            },

            AVMOpcode::ICmpUlt => {
                unimplemented!()
            },

            AVMOpcode::ICmpUle => {
                unimplemented!()
            },

            AVMOpcode::ICmpUgt => {
                unimplemented!()
            },

            AVMOpcode::ICmpUge => {
                unimplemented!()
            },

            // 位运算指令
            AVMOpcode::And => {
                unimplemented!()
            },

            AVMOpcode::Or => {
                unimplemented!()
            },

            AVMOpcode::Xor => {
                unimplemented!()
            },

            AVMOpcode::Shl => {
                unimplemented!()
            },

            AVMOpcode::LShr => {
                unimplemented!()
            },

            AVMOpcode::AShr => {
                unimplemented!()
            },

            // 类型转换指令
            AVMOpcode::Trunc { target_width } => {
                unimplemented!()
            },

            AVMOpcode::ZExt { target_width } => {
                unimplemented!()
            },

            AVMOpcode::SExt { target_width } => {
                unimplemented!()
            },

            AVMOpcode::FPToSI { target_width } => {
                unimplemented!()
            },

            AVMOpcode::FPToUI { target_width } => {
                unimplemented!()
            },

            AVMOpcode::SIToFP { is_double } => {
                unimplemented!()
            },

            AVMOpcode::UIToFP { is_double } => {
                unimplemented!()
            },

            AVMOpcode::Call {
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
                //         self.stack.push(AVMValue::I32(0));
                //     }
                // } else {
                //     return Err(anyhow!("Function {} not found at PC {}", function_name, self.pc));
                // }

                self.stats.function_calls += 1;
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::Ret => {
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

            AVMOpcode::Nop => Ok(ControlFlow::Continue),

            AVMOpcode::Swap => {
                if self.stack.len() < 2 {
                    return Err(anyhow!("Stack underflow on Swap at PC {}", self.pc));
                }
                let len = self.stack.len();
                self.stack.swap(len - 1, len - 2);
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::Dup => {
                let top = self
                    .stack
                    .last()
                    .ok_or_else(|| anyhow!("Stack underflow on Dup at PC {}", self.pc))?
                    .clone();
                self.stack.push(top);
                Ok(ControlFlow::Continue)
            },

            AVMOpcode::TypeCheckInt { width } => {
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

            AVMOpcode::Label { name: _ } => {
                // 标签不需要执行任何操作
                Ok(ControlFlow::Continue)
            },
            AVMOpcode::MetaGVar { reg, name } => {
                // 全局变量元信息不需要执行任何操作
                println!("[RT] MetaGVar {} {}", reg, name);
                self.set_register(*reg, AVMValue::Undef);
                Ok(ControlFlow::Continue)
            },
        }
    }

    // 内存和算术操作方法保持不变
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

    fn store_memory(&mut self, address: usize, value: &AVMValue) -> Result<()> {
        let bytes = self.value_to_bytes(value);

        if address + bytes.len() > self.memory.len() {
            return Err(anyhow!("Memory store out of bounds"));
        }

        self.memory[address..address + bytes.len()].copy_from_slice(&bytes);
        // 记录类型信息，确保后续 LoadValue 能以正确宽度和类型还原
        self.typed_memory.insert(address, value.clone());
        Ok(())
    }

    fn load_memory(&self, address: usize) -> Result<AVMValue> {
        // 优先使用类型化内存，保证与写入时的类型和宽度一致
        if let Some(v) = self.typed_memory.get(&address) {
            return Ok(v.clone());
        }

        // 回退：保持原有的 8 字节读取（i64），用于兼容未记录类型的场景
        if address + 8 > self.memory.len() {
            return Err(anyhow!("Memory load out of bounds"));
        }

        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&self.memory[address..address + 8]);
        let value = i64::from_le_bytes(bytes);

        Ok(AVMValue::I64(value))
    }

    fn value_to_bytes(&self, value: &AVMValue) -> Vec<u8> {
        match value {
            AVMValue::I1(v) => vec![if *v { 1 } else { 0 }],
            AVMValue::I8(v) => v.to_le_bytes().to_vec(),
            AVMValue::I16(v) => v.to_le_bytes().to_vec(),
            AVMValue::I32(v) => v.to_le_bytes().to_vec(),
            AVMValue::I64(v) => v.to_le_bytes().to_vec(),
            AVMValue::F32(v) => v.to_le_bytes().to_vec(),
            AVMValue::F64(v) => v.to_le_bytes().to_vec(),
            AVMValue::Ptr(v) => v.to_le_bytes().to_vec(),
            AVMValue::Undef => vec![0u8; 8], // 未定义值用零填充
        }
    }

    fn add_values(&self, lhs: &AVMValue, rhs: &AVMValue) -> Result<AVMValue> {
        match (lhs, rhs) {
            (AVMValue::I32(l), AVMValue::I32(r)) => Ok(AVMValue::I32(l.wrapping_add(*r))),
            (AVMValue::I64(l), AVMValue::I64(r)) => Ok(AVMValue::I64(l.wrapping_add(*r))),
            (AVMValue::F32(l), AVMValue::F32(r)) => Ok(AVMValue::F32(l + r)),
            (AVMValue::F64(l), AVMValue::F64(r)) => Ok(AVMValue::F64(l + r)),
            (AVMValue::Ptr(l), AVMValue::I64(r)) => Ok(AVMValue::Ptr(l.wrapping_add(*r as usize))),
            _ => Err(anyhow!("Incompatible types for addition: {:?} + {:?}", lhs, rhs)),
        }
    }

    fn sub_values(&self, lhs: &AVMValue, rhs: &AVMValue) -> Result<AVMValue> {
        match (lhs, rhs) {
            (AVMValue::I32(l), AVMValue::I32(r)) => Ok(AVMValue::I32(l.wrapping_sub(*r))),
            (AVMValue::I64(l), AVMValue::I64(r)) => Ok(AVMValue::I64(l.wrapping_sub(*r))),
            (AVMValue::F32(l), AVMValue::F32(r)) => Ok(AVMValue::F32(l - r)),
            (AVMValue::F64(l), AVMValue::F64(r)) => Ok(AVMValue::F64(l - r)),
            _ => Err(anyhow!("Incompatible types for subtraction: {:?} - {:?}", lhs, rhs)),
        }
    }

    fn mul_values(&self, lhs: &AVMValue, rhs: &AVMValue) -> Result<AVMValue> {
        match (lhs, rhs) {
            (AVMValue::I32(l), AVMValue::I32(r)) => Ok(AVMValue::I32(l.wrapping_mul(*r))),
            (AVMValue::I64(l), AVMValue::I64(r)) => Ok(AVMValue::I64(l.wrapping_mul(*r))),
            (AVMValue::F32(l), AVMValue::F32(r)) => Ok(AVMValue::F32(l * r)),
            (AVMValue::F64(l), AVMValue::F64(r)) => Ok(AVMValue::F64(l * r)),
            _ => Err(anyhow!("Incompatible types for multiplication: {:?} * {:?}", lhs, rhs)),
        }
    }

    fn div_values(&self, lhs: &AVMValue, rhs: &AVMValue) -> Result<AVMValue> {
        match (lhs, rhs) {
            (AVMValue::I32(l), AVMValue::I32(r)) => {
                if *r == 0 {
                    return Err(anyhow!("Division by zero"));
                }
                Ok(AVMValue::I32(l / r))
            },
            (AVMValue::I64(l), AVMValue::I64(r)) => {
                if *r == 0 {
                    return Err(anyhow!("Division by zero"));
                }
                Ok(AVMValue::I64(l / r))
            },
            (AVMValue::F32(l), AVMValue::F32(r)) => Ok(AVMValue::F32(l / r)),
            (AVMValue::F64(l), AVMValue::F64(r)) => Ok(AVMValue::F64(l / r)),
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
    Return(Option<AVMValue>),
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
