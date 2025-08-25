use crate::aotu::mba::binary_expr_mba::{BinOp, mba_binop};
use crate::aotu::mba::config::{BitWidth, ConstantMbaConfig};
use crate::aotu::mba::expr::Expr;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::context::ContextRef;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::types::IntType;
use llvm_plugin::inkwell::values::{FunctionValue, IntValue};
use rand::SeedableRng;
use rand::prelude::StdRng;

pub fn build_u128_constant<'ctx>(
    context: ContextRef<'ctx>,
    builder: &Builder<'ctx>,
    value: u128,
    int_type: IntType<'ctx>,
) -> IntValue<'ctx> {
    match int_type.get_bit_width() {
        8 => int_type.const_int(value as u64, false),
        16 => int_type.const_int(value as u64, false),
        32 => int_type.const_int(value as u64, false),
        64 => int_type.const_int(value as u64, false),
        128 => {
            // 对于128位，需要特殊处理
            let low = (value & 0xFFFFFFFFFFFFFFFF) as u64;
            let high = (value >> 64) as u64;
            let low_val = context.i64_type().const_int(low, false);
            let high_val = context.i64_type().const_int(high, false);

            // 创建128位值：(high << 64) | low
            let high_shifted = builder
                .build_left_shift(
                    high_val.const_bit_cast(int_type),
                    int_type.const_int(64, false),
                    "high_shift",
                )
                .unwrap();

            let low_extended = low_val.const_bit_cast(int_type);
            builder.build_or(high_shifted, low_extended, "const128").unwrap()
        },
        _ => panic!("Unsupported bit width: {}", int_type.get_bit_width()),
    }
}

pub fn expr_to_llvm_value<'ctx>(
    context: ContextRef<'ctx>,
    builder: &Builder<'ctx>,
    expr: &Expr,
    aux_params: &[IntValue<'ctx>],
    int_type: IntType<'ctx>,
    width: BitWidth,
) -> IntValue<'ctx> {
    use Expr::*;

    match expr {
        Const(value) => {
            let masked_value = *value & width.mask_u128();
            build_u128_constant(context, builder, masked_value, int_type)
        },

        Var(index) => aux_params.get(*index).copied().unwrap_or_else(|| int_type.const_zero()),

        Not(inner) => {
            let inner_val = expr_to_llvm_value(context, builder, inner, aux_params, int_type, width);
            builder.build_not(inner_val, "not_op").unwrap()
        },

        And(left, right) => {
            let left_val = expr_to_llvm_value(context, builder, left, aux_params, int_type, width);
            let right_val = expr_to_llvm_value(context, builder, right, aux_params, int_type, width);
            builder.build_and(left_val, right_val, "and_op").unwrap()
        },

        Or(left, right) => {
            let left_val = expr_to_llvm_value(context, builder, left, aux_params, int_type, width);
            let right_val = expr_to_llvm_value(context, builder, right, aux_params, int_type, width);
            builder.build_or(left_val, right_val, "or_op").unwrap()
        },

        Xor(left, right) => {
            let left_val = expr_to_llvm_value(context, builder, left, aux_params, int_type, width);
            let right_val = expr_to_llvm_value(context, builder, right, aux_params, int_type, width);
            builder.build_xor(left_val, right_val, "xor_op").unwrap()
        },

        Add(left, right) => {
            let left_val = expr_to_llvm_value(context, builder, left, aux_params, int_type, width);
            let right_val = expr_to_llvm_value(context, builder, right, aux_params, int_type, width);
            builder.build_int_add(left_val, right_val, "add_op").unwrap()
        },

        Sub(left, right) => {
            let left_val = expr_to_llvm_value(context, builder, left, aux_params, int_type, width);
            let right_val = expr_to_llvm_value(context, builder, right, aux_params, int_type, width);
            builder.build_int_sub(left_val, right_val, "sub_op").unwrap()
        },

        MulConst(coeff, inner) => {
            let inner_val = expr_to_llvm_value(context, builder, inner, aux_params, int_type, width);
            let coeff_masked = *coeff & width.mask_u128();
            let coeff_val = build_u128_constant(context, builder, coeff_masked, int_type);
            builder.build_int_mul(coeff_val, inner_val, "mul_const_op").unwrap()
        },
    }
}

#[allow(dead_code)]
pub fn generate_constant_mba_function<'ctx>(
    module: &Module<'ctx>,
    expr: &Expr,
    cfg: &ConstantMbaConfig,
) -> FunctionValue<'ctx> {
    let context = module.get_context();
    let builder = context.create_builder();
    let int_type = cfg.width.to_llvm_int_type(context);

    // 创建函数参数类型
    let mut param_types = Vec::new();
    for _ in 0..cfg.aux_count {
        param_types.push(int_type.into());
    }

    // 创建函数类型
    let fn_type = int_type.fn_type(&param_types, false);

    // 创建函数
    let function = module.add_function(&cfg.func_name, fn_type, None);

    // 创建基本块
    let basic_block = context.append_basic_block(function, "entry");
    builder.position_at_end(basic_block);

    // 获取函数参数
    let mut aux_params: Vec<IntValue> = Vec::new();
    for i in 0..cfg.aux_count {
        let param = function.get_nth_param(i as u32).unwrap().into_int_value();
        param.set_name(&format!("aux{}", i));
        aux_params.push(param);
    }

    // 生成表达式的LLVM IR
    let result = expr_to_llvm_value(context, &builder, expr, &aux_params, int_type, cfg.width);

    // 返回结果
    builder.build_return(Some(&result)).unwrap();

    function
}

#[allow(dead_code)]
pub fn generate_binary_mba_function<'ctx>(
    module: &Module<'ctx>,
    op: BinOp,
    cfg: &ConstantMbaConfig,
) -> FunctionValue<'ctx> {
    assert!(
        cfg.aux_count >= 2,
        "binary MBA function needs at least 2 params for lhs and rhs"
    );

    let context = module.get_context();
    let builder = context.create_builder();
    let int_type = cfg.width.to_llvm_int_type(context);

    // 构造函数参数类型：共 cfg.aux_count 个，约定 aux0=lhs, aux1=rhs，其余为附加 aux
    let param_types: Vec<_> = (0..cfg.aux_count).map(|_| int_type.into()).collect();
    let fn_type = int_type.fn_type(&param_types, false);

    let function = module.add_function(&cfg.func_name, fn_type, None);
    let entry = context.append_basic_block(function, "entry");
    builder.position_at_end(entry);

    // 收集参数为 aux_params（索引与 Expr::Var 一致）
    let mut aux_params: Vec<IntValue> = Vec::with_capacity(cfg.aux_count);
    for i in 0..cfg.aux_count {
        let param = function.get_nth_param(i as u32).unwrap().into_int_value();
        // 命名：0->lhs, 1->rhs, 2..->aux{i}
        if i == 0 {
            param.set_name("lhs");
        } else if i == 1 {
            param.set_name("rhs");
        } else {
            param.set_name(&format!("aux{}", i));
        }
        aux_params.push(param);
    }

    // 生成二元 MBA 表达式：Var(0)=lhs, Var(1)=rhs；noise 可使用 <= cfg.aux_count-1 的变量
    let mut rng: StdRng = match cfg.seed {
        Some(s) => StdRng::seed_from_u64(s),
        None => StdRng::from_os_rng(),
    };
    let a = Expr::Var(0);
    let b = Expr::Var(1);
    let expr = mba_binop(&mut rng, op, a, b, cfg);

    // 降为 LLVM IR 并返回
    let result = expr_to_llvm_value(context, &builder, &expr, &aux_params, int_type, cfg.width);
    builder.build_return(Some(&result)).unwrap();

    function
}
