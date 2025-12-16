mod binary_expr_mba;
mod config;
mod constant_mba;
mod expr;
mod generator;

use crate::aotu::mba::binary_expr_mba::{BinOp, mba_binop};
use crate::aotu::mba::config::{BitWidth, ConstantMbaConfig, NumberType};
use crate::aotu::mba::constant_mba::{generate_const_mba, verify_const_mba};
use crate::aotu::mba::expr::Expr;
use crate::config::{Config, MbaConfig};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{BuilderExt, FunctionExt};
use amice_macro::amice;
use llvm_plugin::PreservedAnalyses;
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::values::{
    BasicValue, GlobalValue, InstructionOpcode, InstructionValue, IntValue, PointerValue,
};
use std::cmp::max;
use std::collections::HashMap;

#[amice(
    priority = 955,
    name = "Mba",
    flag = AmicePassFlag::OptimizerLast | AmicePassFlag::FunctionLevel,
    config = MbaConfig,
)]
#[derive(Default)]
pub struct Mba {
    pub alloc_aux_params_in_global: bool,
}

impl AmicePass for Mba {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.mba.clone();
        self.default_config.enable = cfg.mba.enable;
        self.default_config.aux_count = max(2, cfg.mba.aux_count);
        self.default_config.rewrite_ops = max(24, cfg.mba.rewrite_ops);
        self.default_config.rewrite_depth = max(3, cfg.mba.rewrite_depth);
        self.default_config.alloc_aux_params_in_global = cfg.mba.alloc_aux_params_in_global;
        self.default_config.fix_stack = cfg.mba.fix_stack;
        self.default_config.opt_none = cfg.mba.opt_none;

        self.alloc_aux_params_in_global = false;
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut functions = Vec::new();
        for function in module.get_functions() {
            if function.is_undef_function() || function.is_llvm_function() {
                continue;
            }

            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                continue;
            }

            functions.push(function);
        }

        if functions.is_empty() {
            return Ok(PreservedAnalyses::All);
        }

        let mba_int_widths = [
            BitWidth::W8,
            BitWidth::W16,
            BitWidth::W32,
            BitWidth::W64,
            /*BitWidth::W128,*/
        ];

        let global_aux_params = if self.default_config.alloc_aux_params_in_global {
            let ctx = module.get_context();
            let mut aux_params_map = HashMap::new();
            for mba_int_width in mba_int_widths {
                let mut global_aux_params = vec![];
                for _ in 0..self.default_config.aux_count {
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

        for function in &functions {
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
                            if matches!(
                                inst.get_opcode(),
                                InstructionOpcode::Switch
                                    | InstructionOpcode::Invoke
                                    | InstructionOpcode::Phi
                                    | InstructionOpcode::LandingPad
                                    | InstructionOpcode::CallBr
                                    | InstructionOpcode::Resume
                                    | InstructionOpcode::CatchSwitch
                                    | InstructionOpcode::CleanupRet
                                    | InstructionOpcode::IndirectBr
                                    | InstructionOpcode::Unreachable
                                    | InstructionOpcode::Alloca
                                    | InstructionOpcode::Load
                                    | InstructionOpcode::GetElementPtr
                                    | InstructionOpcode::InsertElement
                                    | InstructionOpcode::VAArg
                            ) {
                                continue;
                            }

                            let mut const_operands = Vec::new();
                            for i in 0..inst.get_num_operands() {
                                let op = inst.get_operand(i);
                                if let Some(operand) = op
                                    && let Some(basic_value) = operand.value()
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

            let stack_aux_params = if !self.default_config.alloc_aux_params_in_global
                && let Some(entry_block) = function.get_entry_block()
                && let Some(first_inst) = entry_block.get_first_instruction()
            {
                let ctx = module.get_context();
                let builder = ctx.create_builder();
                builder.position_before(&first_inst);
                let mut aux_params_map = HashMap::new();
                for mba_int_width in mba_int_widths {
                    let mut stack_aux_params = vec![];
                    for _ in 0..self.default_config.aux_count {
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

            debug!("rewrite constant inst with mba done: {} insts", constant_inst_vec.len());
            for (inst, const_operands) in constant_inst_vec {
                if let Err(e) = rewrite_constant_inst_with_mba(
                    &self.default_config,
                    module,
                    inst,
                    const_operands,
                    global_aux_params.as_ref(),
                    stack_aux_params.as_ref(),
                ) {
                    warn!("rewrite_constant_inst_with_mba failed: {:?}", e);
                }
            }

            debug!("rewrite binop inst with mba done: {} insts", binary_inst_vec.len());
            for binary in binary_inst_vec {
                if let Err(e) = rewrite_binop_with_mba(
                    &self.default_config,
                    self.alloc_aux_params_in_global,
                    module,
                    binary,
                    global_aux_params.as_ref(),
                    stack_aux_params.as_ref(),
                ) {
                    warn!("rewrite binop with mba failed: {:?}", e);
                }
            }

            if self.default_config.opt_none {
                let ctx = module.get_context();
                let optnone_attr = ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("optnone"), 0);
                function.add_attribute(AttributeLoc::Function, optnone_attr);
            }

            if function.verify_function_bool() {
                warn!("function {:?} is not verified", function.get_name());
            }

            if self.default_config.fix_stack {
                unsafe { function.fix_stack() }
            }
        }

        Ok(PreservedAnalyses::None)
    }
}

fn rewrite_binop_with_mba<'a>(
    cfg: &MbaConfig,
    alloc_aux_params_in_global: bool,
    module: &mut Module<'a>,
    binop_inst: InstructionValue<'a>,
    global_aux_params: Option<&HashMap<BitWidth, Vec<GlobalValue>>>,
    stack_aux_params: Option<&HashMap<BitWidth, Vec<PointerValue>>>,
) -> anyhow::Result<()> {
    let Some(lhs) = binop_inst
        .get_operand(0)
        .ok_or(anyhow::anyhow!("failed to get lhs"))?
        .value()
    else {
        return Ok(());
    };
    let Some(rhs) = binop_inst
        .get_operand(1)
        .ok_or(anyhow::anyhow!("failed to get rhs"))?
        .value()
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
        cfg.aux_count as usize,
        cfg.rewrite_ops as usize,
        cfg.rewrite_depth as usize,
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

        if alloc_aux_params_in_global
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
    cfg: &MbaConfig,
    module: &mut Module<'a>,
    constant_inst: InstructionValue<'a>,
    const_operands: Vec<(u32, IntValue<'a>)>,
    global_aux_params: Option<&HashMap<BitWidth, Vec<GlobalValue>>>,
    stack_aux_params: Option<&HashMap<BitWidth, Vec<PointerValue>>>,
) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let builder = ctx.create_builder();
    builder.position_before(&constant_inst);
    for (index, value) in const_operands {
        let value_type = value.get_type();
        if value_type.get_bit_width() == 1 {
            continue;
        }

        let Some(signed_value) = value.get_sign_extended_constant() else {
            warn!("constant value {:?} is not constant", value);
            continue;
        };
        let mba_int_width = BitWidth::from_bits(value_type.get_bit_width())
            .ok_or(anyhow::anyhow!("unsupported int type: {}", value_type))?;
        let mba_config = ConstantMbaConfig::new(
            mba_int_width,
            NumberType::Signed,
            cfg.aux_count as usize,
            cfg.rewrite_ops as usize,
            cfg.rewrite_depth as usize,
            format!("store_const_{}", rand::random::<u64>()),
        )
        .with_signed_constant(signed_value as i128);
        let expr = generate_const_mba(&mba_config);
        let is_valid = verify_const_mba(&expr, mba_config.constant, mba_config.width, mba_config.aux_count);
        if !is_valid {
            error!("(mba) verify_const_mba failed: {:?}", expr);
            continue;
        }

        let mut aux_params = vec![];
        for i in 0..mba_config.aux_count {
            if cfg.alloc_aux_params_in_global
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
        if !constant_inst.set_operand(index, value) {
            warn!(
                "(mba) failed to set operand {} for constant instruction: {:?}",
                index, constant_inst
            );
        }
    }

    Ok(())
}
