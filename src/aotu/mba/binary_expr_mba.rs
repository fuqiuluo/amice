use rand::Rng;
use crate::aotu::mba::config::ConstantMbaConfig;
use crate::aotu::mba::constant_mba::{gen_base_mask_term, gen_base_zero_term, rand_u128_mod2n, rewrite_n};
use crate::aotu::mba::expr::Expr;

#[derive(Copy, Clone, Debug)]
pub(super) enum BinOp {
    Or,
    Xor,
    Add,
    Sub,
}

fn add_zero_noise<R: Rng + ?Sized>(rng: &mut R, mut e: Expr, cfg: &ConstantMbaConfig, max_terms: usize) -> Expr {
    let bits = cfg.width.bits();
    let nmask = cfg.width.mask_u128();

    // 随机加入至多 max_terms 个“恒等为 0”的加性噪声项
    let k = rng.random_range(0..=max_terms);
    for _ in 0..k {
        let coeff = rand_u128_mod2n(rng, bits) & nmask;
        if coeff == 0 { continue; }
        let z = gen_base_zero_term(rng, cfg.aux_count);
        e = Expr::Add(Box::new(e), Box::new(Expr::MulConst(coeff, Box::new(z))));
    }

    // 也可加一对’mask‘招生（总和为 0）
    if rng.random_bool(0.5) {
        let c = rand_u128_mod2n(rng, bits) & nmask;
        if c != 0 {
            let cmpl = (nmask.wrapping_add(1)).wrapping_sub(c) & nmask; // (-c) mod 2^n == (2^n - c)
            let m1 = gen_base_mask_term(rng, cfg.aux_count);
            let m2 = gen_base_mask_term(rng, cfg.aux_count);
            // e + c*mask_term1 + (-c)*mask_term2  → 恒等 0
            e = Expr::Add(
                Box::new(e),
                Box::new(Expr::Add(
                    Box::new(Expr::MulConst(c, Box::new(m1))),
                    Box::new(Expr::MulConst(cmpl, Box::new(m2))),
                )),
            );
        }
    }

    e
}

fn perturb<R: Rng + ?Sized>(rng: &mut R, e: Expr, cfg: &ConstantMbaConfig) -> Expr {
    let nmask = cfg.width.mask_u128();
    rewrite_n(rng, e, cfg.rewrite_depth, nmask, cfg.aux_count)
}

pub(super) fn mba_or<R: Rng + ?Sized>(rng: &mut R, a: Expr, b: Expr, cfg: &ConstantMbaConfig) -> Expr {
    use Expr::*;
    // 在多个等价式中随机挑选一种主形态
    let core = match rng.random_range(0..3) {
        0 => Xor(Box::new(Xor(Box::new(a.clone()), Box::new(b.clone()))), Box::new(And(Box::new(a.clone()), Box::new(b.clone())))),
        1 => Sub(Box::new(Add(Box::new(a.clone()), Box::new(b.clone()))), Box::new(And(Box::new(a.clone()), Box::new(b.clone())))),
        _ => Not(Box::new(And(Box::new(Not(Box::new(a.clone()))), Box::new(Not(Box::new(b.clone())))))), // 德摩根
    };
    let with_noise = add_zero_noise(rng, core, cfg, 2);
    perturb(rng, with_noise, cfg)
}

pub(super) fn mba_xor<R: Rng + ?Sized>(rng: &mut R, a: Expr, b: Expr, cfg: &ConstantMbaConfig) -> Expr {
    use Expr::*;
    let two = Expr::MulConst(2, Box::new(And(Box::new(a.clone()), Box::new(b.clone()))));
    let core = match rng.random_range(0..3) {
        0 => Sub(Box::new(Add(Box::new(a.clone()), Box::new(b.clone()))), Box::new(two)),
        1 => Sub(Box::new(Or(Box::new(a.clone()), Box::new(b.clone()))), Box::new(And(Box::new(a.clone()), Box::new(b.clone())))),
        _ => {
            // 直接 XOR 再加噪声，最终交给 rewrite_n 打散
            Xor(Box::new(a.clone()), Box::new(b.clone()))
        }
    };
    let with_noise = add_zero_noise(rng, core, cfg, 2);
    perturb(rng, with_noise, cfg)
}

pub(super) fn mba_add<R: Rng + ?Sized>(rng: &mut R, a: Expr, b: Expr, cfg: &ConstantMbaConfig) -> Expr {
    use Expr::*;
    let two = Expr::MulConst(2, Box::new(And(Box::new(a.clone()), Box::new(b.clone()))));
    let core = match rng.random_range(0..2) {
        0 => Add(Box::new(Xor(Box::new(a.clone()), Box::new(b.clone()))), Box::new(two)),
        _ => Add(Box::new(Or(Box::new(a.clone()), Box::new(b.clone()))), Box::new(And(Box::new(a.clone()), Box::new(b.clone())))),
    };
    let with_noise = add_zero_noise(rng, core, cfg, 3);
    perturb(rng, with_noise, cfg)
}

pub(super) fn mba_sub<R: Rng + ?Sized>(rng: &mut R, a: Expr, b: Expr, cfg: &ConstantMbaConfig) -> Expr {
    use Expr::*;
    let core = match rng.random_range(0..2) {
        0 => {
            // a - b = a + (~b + 1)
            let nb = Not(Box::new(b.clone()));
            Add(
                Box::new(a.clone()),
                Box::new(Add(Box::new(nb), Box::new(Const(1)))),
            )
        }
        _ => {
            // a - b = (a ^ b) - 2*(~a & b)
            let carry = MulConst(2, Box::new(And(Box::new(Not(Box::new(a.clone()))), Box::new(b.clone()))));
            Sub(Box::new(Xor(Box::new(a.clone()), Box::new(b.clone()))), Box::new(carry))
        }
    };
    let with_noise = add_zero_noise(rng, core, cfg, 3);
    perturb(rng, with_noise, cfg)
}

pub(super) fn mba_binop<R: Rng + ?Sized>(rng: &mut R, op: BinOp, a: Expr, b: Expr, cfg: &ConstantMbaConfig) -> Expr {
    match op {
        BinOp::Or  => mba_or(rng, a, b, cfg),
        BinOp::Xor => mba_xor(rng, a, b, cfg),
        BinOp::Add => mba_add(rng, a, b, cfg),
        BinOp::Sub => mba_sub(rng, a, b, cfg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::prelude::*;
    use crate::aotu::mba::config::{BitWidth, NumberType, ConstantMbaConfig};
    use crate::aotu::mba::constant_mba::{eval_const_mba_expr, CPrinter};

    #[derive(Copy, Clone, Debug)]
    enum OpKind { Or, Xor, Add, Sub }

    fn build_cfg(width: BitWidth) -> ConstantMbaConfig {
        // 这里常量值等参数对二元混淆并不重要，只需要 width/aux_count/rewrite_depth 等
        // 参数选择与常量生成无关，仅用于 rewrite 随机性。
        ConstantMbaConfig::new(
            width,
            NumberType::Unsigned,
            3,      // aux_count >= 2 (使用 aux0/aux1)
            24,     // rewrite_depth
            3,      // rewrite_ops（对二元混淆无直接影响）
            "binop_mba_test".to_string(),
        ).with_unsigned_constant(0) // 占位
    }

    fn baseline(op: OpKind, x: u128, y: u128, width: BitWidth) -> u128 {
        let m = width.mask_u128();
        match op {
            OpKind::Or  => (x | y) & m,
            OpKind::Xor => (x ^ y) & m,
            OpKind::Add => x.wrapping_add(y) & m,
            OpKind::Sub => x.wrapping_sub(y) & m,
        }
    }

    fn build_mba_expr<R: Rng + ?Sized>(rng: &mut R, op: OpKind, cfg: &ConstantMbaConfig) -> Expr {
        let a = Expr::Var(0);
        let b = Expr::Var(1);
        match op {
            OpKind::Or  => mba_or(rng, a, b, cfg),
            OpKind::Xor => mba_xor(rng, a, b, cfg),
            OpKind::Add => mba_add(rng, a, b, cfg),
            OpKind::Sub => mba_sub(rng, a, b, cfg),
        }
    }

    #[test]
    fn test_generate_c_code() {
        let mut rng = StdRng::seed_from_u64(0xA0B1_C2D3_E4F5_6789);
        let cfg = build_cfg(BitWidth::W32);
        let mut rng: StdRng = match cfg.seed {
            Some(s) => StdRng::seed_from_u64(s),
            None => StdRng::from_os_rng(),
        };

        let a = Expr::Var(0);
        let b = Expr::Var(1);
        let expr = mba_binop(&mut rng, BinOp::Add, a, b, &cfg);

        let printer = CPrinter { width: cfg.width, number_type: cfg.number_type };
        println!("{}", printer.emit_function(&cfg.func_name, cfg.aux_count, &expr));
    }

    fn check_width_for_all_ops(width: BitWidth) {
        let mut rng = StdRng::seed_from_u64(0xA0B1_C2D3_E4F5_6789);
        let cfg = build_cfg(width);

        let ops = [OpKind::Or, OpKind::Xor, OpKind::Add, OpKind::Sub];
        for &op in &ops {
            // 为每个 op 都构建一次独立的混淆表达式
            let expr = build_mba_expr(&mut rng, op, &cfg);

            // 一组边界用例
            let special_values = {
                let m = width.mask_u128();
                let half = if width.bits() == 0 { 0 } else { 1u128 << (width.bits().saturating_sub(1)) } & m;
                vec![
                    0u128, 1u128, 2u128, 3u128, 7u128, 15u128,
                    m, m.wrapping_sub(1), half, half.wrapping_sub(1),
                ]
            };

            // 先跑边界用例
            for &x in &special_values {
                for &y in &special_values {
                    let got = eval_const_mba_expr(&expr, &[x, y], width);
                    let exp = baseline(op, x, y, width);
                    println!("width={:?}, op={:?}, x={:#x}, y={:#x}, got={:#x}, exp={:#x}", width, op, x, y, got, exp);
                    assert_eq!(got, exp, "width={:?}, op={:?}, x={:#x}, y={:#x}", width, op, x, y);
                }
            }

            // 再跑随机用例
            for _ in 0..1000 {
                let x = rand_u128_mod2n(&mut rng, width.bits());
                let y = rand_u128_mod2n(&mut rng, width.bits());
                let got = eval_const_mba_expr(&expr, &[x, y], width);
                let exp = baseline(op, x, y, width);
                println!("width={:?}, op={:?}, x={:#x}, y={:#x}, got={:#x}, exp={:#x}", width, op, x, y, got, exp);
                assert_eq!(got, exp, "width={:?}, op={:?}, x={:#x}, y={:#x}", width, op, x, y);
            }
        }
    }

    #[test]
    fn test_mba_binops_w8()   { check_width_for_all_ops(BitWidth::W8); }
    #[test]
    fn test_mba_binops_w16()  { check_width_for_all_ops(BitWidth::W16); }
    #[test]
    fn test_mba_binops_w32()  { check_width_for_all_ops(BitWidth::W32); }
    #[test]
    fn test_mba_binops_w64()  { check_width_for_all_ops(BitWidth::W64); }

    #[test]
    fn test_mba_binops_w128() { check_width_for_all_ops(BitWidth::W128); }
}