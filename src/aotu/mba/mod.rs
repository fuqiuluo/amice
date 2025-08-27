mod binary_expr_mba;
mod config;
mod constant_mba;
mod expr;
mod generator;

use crate::aotu::mba::binary_expr_mba::{BinOp, mba_binop};
use crate::aotu::mba::config::{BitWidth, ConstantMbaConfig, NumberType};
use crate::aotu::mba::constant_mba::{generate_const_mba, verify_const_mba};
use crate::aotu::mba::expr::Expr;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::inkwell2::AdvancedInkwellBuilder;
use amice_llvm::ir::function::{fix_stack, get_basic_block_entry};
use amice_llvm::module_utils::verify_function2;
use amice_macro::amice;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::values::{
    BasicValue, GlobalValue, InstructionOpcode, InstructionValue, IntValue, PointerValue,
};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error, warn};
use std::cmp::max;
use std::collections::HashMap;

#[amice(
    priority = 955,
    name = "Mba",
    position = PassPosition::PipelineStart | PassPosition::OptimizerLast
)]
#[derive(Default)]
pub struct Mba {
    enable: bool,
    aux_count: u32,
    rewrite_ops: u32,
    rewrite_depth: u32,
    alloc_aux_params_in_global: bool, // 仅测试用途
    fix_stack: bool,
}

impl AmicePassLoadable for Mba {
    fn init(&mut self, cfg: &crate::config::Config, position: PassPosition) -> bool {
        self.enable = cfg.mba.enable;
        self.aux_count = max(2, cfg.mba.aux_count);
        self.rewrite_ops = max(24, cfg.mba.rewrite_ops);
        self.rewrite_depth = max(3, cfg.mba.rewrite_depth);
        self.alloc_aux_params_in_global = cfg.mba.alloc_aux_params_in_global;
        self.fix_stack = cfg.mba.fix_stack;

        if !self.enable {
            return false;
        }

        // 如果alloc_aux_params_in_global为true则允许在没有优化的时候注册该Pass
        if cfg.mba.alloc_aux_params_in_global {
            // 如果直接返回true，你回收获一个超级大的可执行文件，hhh
            return position == PassPosition::PipelineStart;
        }

        position == PassPosition::OptimizerLast
    }
}

impl LlvmModulePass for Mba {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        let mba_int_widths = [
            BitWidth::W8,
            BitWidth::W16,
            BitWidth::W32,
            BitWidth::W64,
            /*BitWidth::W128,*/
        ];

        let global_aux_params = if self.alloc_aux_params_in_global {
            let ctx = module.get_context();
            let mut aux_params_map = HashMap::new();
            for mba_int_width in mba_int_widths {
                let mut global_aux_params = vec![];
                for _ in 0..self.aux_count {
                    let rand = match mba_int_width {
                        BitWidth::W8 => rand::random::<u8>() as u64,
                        BitWidth::W16 => rand::random::<u16>() as u64,
                        BitWidth::W32 => rand::random::<u32>() as u64,
                        BitWidth::W64 => rand::random::<u64>(),
                        BitWidth::W128 => panic!("(mba) not support 128 bit"),
                    };
                    let value_type = mba_int_width.to_llvm_int_type(ctx);
                    let aux_param = value_type.const_int(rand, false);
                    let global_aux = module.add_global(value_type, None, "");
                    global_aux.set_initializer(&aux_param);
                    global_aux.set_linkage(Linkage::Internal);
                    global_aux.set_constant(false);
                    global_aux_params.push(global_aux);
                }
                aux_params_map.insert(mba_int_width, global_aux_params);
            }
            aux_params_map.into()
        } else {
            None
        };

        for function in module.get_functions() {
            let mut constant_inst_vec = Vec::new();
            let mut binary_inst_vec = Vec::new();
            for bb in function.get_basic_blocks() {
                for inst in bb.get_instructions() {
                    match inst.get_opcode() {
                        InstructionOpcode::Add
                        | InstructionOpcode::Sub
                        | InstructionOpcode::Or
                        | InstructionOpcode::Xor => binary_inst_vec.push(inst),
                        _ => {
                            let mut const_operands = Vec::new();
                            for i in 0..inst.get_num_operands() {
                                let op = inst.get_operand(i);
                                if let Some(operand) = op
                                    && let Some(basic_value) = operand.left()
                                    && basic_value.is_int_value()
                                {
                                    let int_value = basic_value.into_int_value();
                                    if !int_value.is_constant_int() {
                                        continue;
                                    }

                                    if int_value.is_null() {
                                        continue;
                                    }

                                    const_operands.push((i, int_value));
                                }
                            }

                            if const_operands.is_empty() {
                                continue;
                            }

                            constant_inst_vec.push((inst, const_operands))
                        },
                    }
                }
            }

            if constant_inst_vec.is_empty() && binary_inst_vec.is_empty() {
                continue;
            }

            let stack_aux_params = if !self.alloc_aux_params_in_global
                && let Some(entry_block) = get_basic_block_entry(function)
                && let Some(first_inst) = entry_block.get_first_instruction()
            {
                let ctx = module.get_context();
                let builder = ctx.create_builder();
                builder.position_before(&first_inst);
                let mut aux_params_map = HashMap::new();
                for mba_int_width in mba_int_widths {
                    let mut stack_aux_params = vec![];
                    for _ in 0..self.aux_count {
                        let rand = match mba_int_width {
                            BitWidth::W8 => rand::random::<u8>() as u64,
                            BitWidth::W16 => rand::random::<u16>() as u64,
                            BitWidth::W32 => rand::random::<u32>() as u64,
                            BitWidth::W64 => rand::random::<u64>(),
                            BitWidth::W128 => panic!("(mba) not support 128 bit"),
                        };
                        let value_type = mba_int_width.to_llvm_int_type(ctx);
                        let aux_param = value_type.const_int(rand, false);
                        let aux_param_alloca = builder
                            .build_alloca(value_type, "")
                            .expect("(mba) failed to build alloca");
                        builder
                            .build_store(aux_param_alloca, aux_param)
                            .expect("(mba) failed to build store");
                        stack_aux_params.push(aux_param_alloca);
                    }
                    aux_params_map.insert(mba_int_width, stack_aux_params);
                }
                aux_params_map.into()
            } else {
                None
            };

            debug!(
                "(mba) rewrite constant inst with mba done: {} insts",
                constant_inst_vec.len()
            );
            for (inst, const_operands) in constant_inst_vec {
                if let Err(e) = rewrite_constant_inst_with_mba(
                    self,
                    module,
                    inst,
                    const_operands,
                    global_aux_params.as_ref(),
                    stack_aux_params.as_ref(),
                ) {
                    warn!("(mba) rewrite store with mba failed: {:?}", e);
                }
            }

            debug!(
                "(mba) rewrite binop inst with mba done: {} insts",
                binary_inst_vec.len()
            );
            for binary in binary_inst_vec {
                if let Err(e) = rewrite_binop_with_mba(
                    self,
                    module,
                    binary,
                    global_aux_params.as_ref(),
                    stack_aux_params.as_ref(),
                ) {
                    warn!("(mba) rewrite binop with mba failed: {:?}", e);
                }
            }

            if verify_function2(function) {
                warn!("(mba) function {:?} is not verified", function.get_name());
            }

            if self.fix_stack {
                unsafe {
                    fix_stack(function);
                }
            }
        }

        PreservedAnalyses::None
    }
}

fn rewrite_binop_with_mba<'a>(
    pass: &Mba,
    module: &mut Module<'a>,
    binop_inst: InstructionValue<'a>,
    global_aux_params: Option<&HashMap<BitWidth, Vec<GlobalValue>>>,
    stack_aux_params: Option<&HashMap<BitWidth, Vec<PointerValue>>>,
) -> anyhow::Result<()> {
    let Some(lhs) = binop_inst
        .get_operand(0)
        .ok_or(anyhow::anyhow!("failed to get lhs"))?
        .left()
    else {
        return Ok(());
    };
    let Some(rhs) = binop_inst
        .get_operand(1)
        .ok_or(anyhow::anyhow!("failed to get rhs"))?
        .left()
    else {
        return Ok(());
    };

    if !lhs.is_int_value() || !rhs.is_int_value() {
        return Ok(());
    }

    let lhs = lhs.into_int_value();
    let rhs = rhs.into_int_value();
    assert_eq!(lhs.get_type().get_bit_width(), rhs.get_type().get_bit_width());

    let value_type = lhs.get_type();
    let mba_int_width =
        BitWidth::from_bits(value_type.get_bit_width()).ok_or(anyhow::anyhow!("unsupported int type"))?;

    let binop = match binop_inst.get_opcode() {
        InstructionOpcode::Add => BinOp::Add,
        InstructionOpcode::Sub => BinOp::Sub,
        InstructionOpcode::Or => BinOp::Or,
        InstructionOpcode::Xor => BinOp::Xor,
        _ => return Err(anyhow::anyhow!("unsupported binop: {:?}", binop_inst)),
    };
    let mut rng = rand::rng();
    let cfg = ConstantMbaConfig::new(
        mba_int_width,
        NumberType::Signed,
        pass.aux_count as usize,
        pass.rewrite_ops as usize,
        pass.rewrite_depth as usize,
        format!("store_const_{}", rand::random::<u64>()),
    );
    let expr = mba_binop(&mut rng, binop, Expr::Var(0), Expr::Var(1), &cfg);

    let ctx = module.get_context();
    let builder = ctx.create_builder();
    builder.position_before(&binop_inst);

    let mut aux_params = vec![];
    for i in 0..cfg.aux_count {
        if i == 0 {
            aux_params.push(lhs);
            continue;
        }

        if i == 1 {
            aux_params.push(rhs);
            continue;
        }

        if pass.alloc_aux_params_in_global
            && let Some(global_aux) = global_aux_params
            && let Some(global_aux_params) = global_aux.get(&mba_int_width)
        {
            let global_aux = global_aux_params[i];
            let int = builder
                .build_load2(value_type, global_aux.as_pointer_value(), "")?
                .into_int_value();
            aux_params.push(int);
        } else if let Some(stack_aux) = stack_aux_params
            && let Some(stack_aux_params) = stack_aux.get(&mba_int_width)
        {
            let stack_aux = stack_aux_params[i];
            let int = builder.build_load2(value_type, stack_aux, "")?.into_int_value();
            aux_params.push(int);
        } else {
            let rand = match mba_int_width {
                BitWidth::W8 => rand::random::<u8>() as u64,
                BitWidth::W16 => rand::random::<u16>() as u64,
                BitWidth::W32 => rand::random::<u32>() as u64,
                BitWidth::W64 => rand::random::<u64>(),
                BitWidth::W128 => panic!("(mba) not support 128 bit"),
            };
            let aux_param = value_type.const_int(rand, false);
            aux_params.push(aux_param);
        }
    }

    let value = generator::expr_to_llvm_value(ctx, &builder, &expr, &aux_params, value_type, mba_int_width);
    let new_inst = value.as_instruction_value().unwrap();

    binop_inst.replace_all_uses_with(&new_inst);
    binop_inst.erase_from_basic_block();

    Ok(())
}

fn rewrite_constant_inst_with_mba<'a>(
    pass: &Mba,
    module: &mut Module<'a>,
    store: InstructionValue<'a>,
    const_operands: Vec<(u32, IntValue<'a>)>,
    global_aux_params: Option<&HashMap<BitWidth, Vec<GlobalValue>>>,
    stack_aux_params: Option<&HashMap<BitWidth, Vec<PointerValue>>>,
) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let builder = ctx.create_builder();
    builder.position_before(&store);
    for (index, value) in const_operands {
        let value_type = value.get_type();
        let Some(signed_value) = value.get_sign_extended_constant() else {
            warn!("(mba) store value {:?} is not constant", value);
            continue;
        };
        let mba_int_width =
            BitWidth::from_bits(value_type.get_bit_width()).ok_or(anyhow::anyhow!("unsupported int type"))?;
        let cfg = ConstantMbaConfig::new(
            mba_int_width,
            NumberType::Signed,
            pass.aux_count as usize,
            pass.rewrite_ops as usize,
            pass.rewrite_depth as usize,
            format!("store_const_{}", rand::random::<u64>()),
        )
        .with_signed_constant(signed_value as i128);
        let expr = generate_const_mba(&cfg);
        let is_valid = verify_const_mba(&expr, cfg.constant, cfg.width, cfg.aux_count);
        if !is_valid {
            error!("(mba) rewrite store with mba failed: {:?}", expr);
            continue;
        }

        let mut aux_params = vec![];
        for i in 0..cfg.aux_count {
            if pass.alloc_aux_params_in_global
                && let Some(global_aux) = global_aux_params
                && let Some(global_aux_params) = global_aux.get(&mba_int_width)
            {
                let global_aux = global_aux_params[i];
                let int = builder
                    .build_load2(value_type, global_aux.as_pointer_value(), "")?
                    .into_int_value();
                aux_params.push(int);
            } else if let Some(stack_aux) = stack_aux_params
                && let Some(stack_aux_params) = stack_aux.get(&mba_int_width)
            {
                let stack_aux = stack_aux_params[i];
                let int = builder.build_load2(value_type, stack_aux, "")?.into_int_value();
                aux_params.push(int);
            } else {
                let rand = match mba_int_width {
                    BitWidth::W8 => rand::random::<u8>() as u64,
                    BitWidth::W16 => rand::random::<u16>() as u64,
                    BitWidth::W32 => rand::random::<u32>() as u64,
                    BitWidth::W64 => rand::random::<u64>(),
                    BitWidth::W128 => panic!("(mba) not support 128 bit"),
                };
                let aux_param = value_type.const_int(rand, false);
                aux_params.push(aux_param);
            }
        }

        let value = generator::expr_to_llvm_value(ctx, &builder, &expr, &aux_params, value_type, mba_int_width);
        if !store.set_operand(index, value) {
            warn!(
                "(mba) failed to set operand {} for store instruction: {:?}",
                index, store
            );
        }
    }

    Ok(())
}
