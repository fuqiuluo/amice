// translate from https://github.com/za233/Polaris-Obfuscator/blob/main/src/llvm/lib/Transforms/Obfuscation/BogusControlFlow.cpp
// A little improvement

use crate::aotu::bogus_control_flow::{BogusControlFlow, BogusControlFlowAlgo};
use crate::config::BogusControlFlowConfig;
use amice_llvm::inkwell2::{BasicBlockExt, BuilderExt, FunctionExt, InstructionExt};
use anyhow::anyhow;
use llvm_plugin::inkwell::IntPredicate;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::context::{Context, ContextRef};
use llvm_plugin::inkwell::llvm_sys::core::LLVMAddIncoming;
use llvm_plugin::inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::types::IntType;
use llvm_plugin::inkwell::values::{
    AsValueRef, FunctionValue, InstructionOpcode, InstructionValue, IntValue, PhiValue, PointerValue,
};
use log::error;
use rand::Rng;

#[derive(Default)]
pub struct BogusControlFlowPolarisPrimes;

impl BogusControlFlowAlgo for BogusControlFlowPolarisPrimes {
    fn initialize(&mut self, _cfg: &BogusControlFlowConfig, _module: &mut Module<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    fn apply_bogus_control_flow(
        &mut self,
        _cfg: &BogusControlFlowConfig,
        module: &mut Module<'_>,
        function: FunctionValue,
    ) -> anyhow::Result<()> {
        let ctx = module.get_context();

        let i64_ty = ctx.i64_type();

        let builder = ctx.create_builder();

        let entry_block = function
            .get_entry_block()
            .ok_or_else(|| anyhow!("Failed to get entry block for function {:?}", function.get_name()))?;
        let first_insertion_pt = entry_block.get_first_insertion_pt();
        builder.position_before(&first_insertion_pt);

        let var0 = builder.build_alloca(i64_ty, "var0")?;
        let var1 = builder.build_alloca(i64_ty, "var1")?;

        let module = 0x100000000u64 - rand::random::<u32>() as u64;
        let x = PRIMES[rand::random_range(0..PRIMES.len())] % module;

        builder.build_store(var0, i64_ty.const_int(x, false))?;
        builder.build_store(var1, i64_ty.const_int(x, false))?;

        let mut basic_blocks = function.get_basic_blocks();
        basic_blocks.retain(|bb| bb != &entry_block);

        let expr = [PreserveExpr::SingleInverseAffine, PreserveExpr::DoubleInverseAffine];
        let mut unconditional_branch_blocks = Vec::new();
        for bb in basic_blocks {
            let Some(terminator) = bb.get_terminator() else {
                continue;
            };

            if !matches!(terminator.get_opcode(), InstructionOpcode::Br) {
                continue;
            }

            let terminator = terminator.into_branch_inst();
            if terminator.is_conditional() {
                continue;
            }

            if rand::random::<bool>() {
                let first_insertion_pt = bb.get_first_insertion_pt();
                builder.position_before(&first_insertion_pt);
            } else {
                builder.position_before(&terminator);
            }

            let expr = expr[rand::random_range(0..expr.len())];
            let modify_var = if rand::random::<bool>() { var0 } else { var1 };
            if let Err(err) = expr.build(&ctx, &builder, module, x, modify_var) {
                error!("(PolarisPrimes) failed to build preserve expr: {:?}({})", expr, err);
                continue;
            }

            unconditional_branch_blocks.push(bb);
        }

        for bb in &unconditional_branch_blocks {
            let Some(terminator) = bb.get_terminator() else {
                continue;
            };
            let Some(next_bb) = terminator.into_branch_inst().get_successor(0) else {
                continue;
            };

            builder.position_before(&terminator);
            let var0_val = builder.build_load2(i64_ty, var0, "")?.into_int_value();
            let var1_val = builder.build_load2(i64_ty, var1, "")?.into_int_value();
            let is_eq = rand::random::<bool>();
            let condition = if is_eq {
                builder.build_int_compare(IntPredicate::EQ, var0_val, var1_val, "var0 == var1")
            } else {
                builder.build_int_compare(IntPredicate::NE, var0_val, var1_val, "var0 != var1")
            }?;
            let fake_bb = unconditional_branch_blocks[rand::random_range(0..unconditional_branch_blocks.len())];

            for phi in fake_bb.get_first_instruction().iter() {
                if phi.get_opcode() != InstructionOpcode::Phi {
                    break;
                }

                let phi = phi.into_phi_inst().into_phi_value();
                let incoming_vec = phi.get_incomings().collect::<Vec<_>>();
                // 如果真的有这个前支，则不需要再添加
                if incoming_vec.iter().any(|(_, pred)| pred == bb) {
                    break;
                }
                let (_value, old_pred) = incoming_vec[rand::random_range(0..incoming_vec.len())];

                fake_bb.fix_phi_node(old_pred, *bb);
            }

            if is_eq {
                builder.build_conditional_branch(condition, next_bb, fake_bb)?;
            } else {
                builder.build_conditional_branch(condition, fake_bb, next_bb)?;
            }
            terminator.erase_from_basic_block();
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum PreserveExpr {
    SingleInverseAffine,
    DoubleInverseAffine,
}

impl PreserveExpr {
    fn build<'a>(
        &self,
        ctx: &ContextRef<'a>,
        builder: &Builder<'a>,
        module: u64,
        x: u64,
        var0: PointerValue<'a>,
    ) -> anyhow::Result<()> {
        let i64_ty = ctx.i64_type();

        let module_int = i64_ty.const_int(module, false);
        match self {
            PreserveExpr::SingleInverseAffine => {
                let b = rand::random::<u64>() % module;
                let inv = get_inverse(x, module).unwrap();
                let a = ((b * inv) % module + 1) % module;

                let var0_val = builder.build_load2(i64_ty, var0, "")?.into_int_value();
                let var0_val = builder.build_int_unsigned_rem(
                    builder.build_int_mul(var0_val, i64_ty.const_int(a, false), "var0 * a")?,
                    module_int,
                    "(var0 * a) % m",
                )?;
                let var0_val = builder.build_int_unsigned_rem(
                    builder.build_int_sub(var0_val, i64_ty.const_int(b, false), "(var0 * a) % m - b")?,
                    module_int,
                    "((var0 * a) % m - b) % m",
                )?;
                builder.build_store(var0, var0_val)?;
            },
            PreserveExpr::DoubleInverseAffine => {
                let mut k;
                loop {
                    k = rand::random::<u64>() % module;
                    if k != 0 && gcd(k, module) == 1 {
                        break;
                    }
                }
                let c = rand::random::<u64>() % module;
                let inv_k = get_inverse(k, module).unwrap();

                let c = i64_ty.const_int(c, false);
                let ink_k = i64_ty.const_int(inv_k, false);

                let var0_val = builder.build_load2(i64_ty, var0, "")?.into_int_value();
                let var0_val = builder.build_int_unsigned_rem(
                    builder.build_int_sub(
                        builder.build_int_add(
                            builder.build_int_unsigned_rem(
                                builder.build_int_add(
                                    builder.build_int_mul(var0_val, i64_ty.const_int(k, false), "var0 * k")?,
                                    c,
                                    "var0 * k + c",
                                )?,
                                module_int,
                                "(var0 * k + c) % m",
                            )?,
                            module_int,
                            "(var0 * k + c) % m + m",
                        )?,
                        c,
                        "(var0 * k + c) % m + m - c",
                    )?,
                    module_int,
                    "((var0 * k + c) % m + m - c) % m",
                )?;
                builder.build_store(var0, var0_val)?;
                let var0_val = builder.build_load2(i64_ty, var0, "")?.into_int_value();
                let var0_val = builder.build_int_unsigned_rem(
                    builder.build_int_mul(var0_val, ink_k, "var0 * inv_k")?,
                    module_int,
                    "(var0 * inv_k) % m",
                )?;
                builder.build_store(var0, var0_val)?;
            },
        }
        Ok(())
    }
}

const PRIMES: &[u64] = &[
    1009, 1013, 1019, 1021, 1031, 1033, 1039, 1049, 1051, 1061, 1063, 1069, 1087, 1091, 1093, 1097, 1103, 1109, 1117,
    1123, 1129, 1151, 1153, 1163, 1171, 1181, 1187, 1193, 1201, 1213, 1217, 1223, 1229, 1231, 1237, 1249, 1259, 1277,
    1279, 1283, 1289, 1291, 1297, 1301, 1303, 1307, 1319, 1321, 1327, 1361, 1367, 1373, 1381, 1399, 1409, 1423, 1427,
    1429, 1433, 1439, 1447, 1451, 1453, 1459, 1471, 1481, 1483, 1487, 1489, 1493, 1499, 1511, 1523, 1531, 1543, 1549,
    1553, 1559, 1567, 1571, 1579, 1583, 1597, 1601, 1607, 1609, 1613, 1619, 1621, 1627, 1637, 1657, 1663, 1667, 1669,
    1693, 1697, 1699, 1709, 1721, 1723, 1733, 1741, 1747, 1753, 1759, 1777, 1783, 1787, 1789, 1801, 1811, 1823, 1831,
    1847, 1861, 1867, 1871, 1873, 1877, 1879, 1889, 1901, 1907, 1913, 1931, 1933, 1949, 1951, 1973, 1979, 1987, 1993,
    1997, 1999, 2003, 2011, 2017, 2027, 2029, 2039, 2053, 2063, 2069, 2081, 2083, 2087, 2089, 2099, 2111, 2113, 2129,
    2131, 2137, 2141, 2143, 2153, 2161, 2179, 2203, 2207, 2213, 2221, 2237, 2239, 2243, 2251, 2267, 2269, 2273, 2281,
    2287, 2293, 2297, 2309, 2311, 2333, 2339, 2341, 2347, 2351, 2357, 2371, 2377, 2381, 2383, 2389, 2393, 2399, 2411,
    2417, 2423, 2437, 2441, 2447, 2459, 2467, 2473, 2477, 2503, 2521, 2531, 2539, 2543, 2549, 2551, 2557, 2579, 2591,
    2593, 2609, 2617, 2621, 2633, 2647, 2657, 2659, 2663, 2671, 2677, 2683, 2687, 2689, 2693, 2699, 2707, 2711, 2713,
    2719, 2729, 2731, 2741, 2749, 2753, 2767, 2777, 2789, 2791, 2797, 2801, 2803, 2819, 2833, 2837, 2843, 2851, 2857,
    2861, 2879, 2887, 2897, 2903, 2909, 2917, 2927, 2939, 2953, 2957, 2963, 2969, 2971, 2999, 3001, 3011, 3019, 3023,
    3037, 3041, 3049, 3061, 3067, 3079, 3083, 3089, 3109, 3119, 3121, 3137, 3163, 3167, 3169, 3181, 3187, 3191, 3203,
    3209, 3217, 3221, 3229, 3251, 3253, 3257, 3259, 3271, 3299, 3301, 3307, 3313, 3319, 3323, 3329, 3331, 3343, 3347,
    3359, 3361, 3371, 3373, 3389, 3391, 3407, 3413, 3433, 3449, 3457, 3461, 3463, 3467, 3469, 3491, 3499, 3511, 3517,
    3527, 3529, 3533, 3539, 3541, 3547, 3557, 3559, 3571, 3581, 3583, 3593, 3607, 3613, 3617, 3623, 3631, 3637, 3643,
    3659, 3671, 3673, 3677, 3691, 3697, 3701, 3709, 3719, 3727, 3733, 3739, 3761, 3767, 3769, 3779, 3793, 3797, 3803,
    3821, 3823, 3833, 3847, 3851, 3853, 3863, 3877, 3881, 3889, 3907, 3911, 3917, 3919, 3923, 3929, 3931, 3943, 3947,
    3967, 3989, 4001, 4003, 4007, 4013, 4019, 4021, 4027, 4049, 4051, 4057, 4073, 4079, 4091, 4093, 4099, 4111, 4127,
    4129, 4133, 4139, 4153, 4157, 4159, 4177, 4201, 4211, 4217, 4219, 4229, 4231, 4241, 4243, 4253, 4259, 4261, 4271,
    4273, 4283, 4289, 4297, 4327, 4337, 4339, 4349, 4357, 4363, 4373, 4391, 4397, 4409, 4421, 4423, 4441, 4447, 4451,
    4457, 4463, 4481, 4483, 4493, 4507, 4513, 4517, 4519, 4523, 4547, 4549, 4561, 4567, 4583, 4591, 4597, 4603, 4621,
    4637, 4639, 4643, 4649, 4651, 4657, 4663, 4673, 4679, 4691, 4703, 4721, 4723, 4729, 4733, 4751, 4759, 4783, 4787,
    4789, 4793, 4799, 4801, 4813, 4817, 4831, 4861, 4871, 4877, 4889, 4903, 4909, 4919, 4931, 4933, 4937, 4943, 4951,
    4957, 4967, 4969, 4973, 4987, 4993, 4999, 5003, 5009, 5011, 5021, 5023, 5039, 5051, 5059, 5077, 5081, 5087, 5099,
    5101, 5107, 5113, 5119, 5147, 5153, 5167, 5171, 5179, 5189, 5197, 5209, 5227, 5231, 5233, 5237, 5261, 5273, 5279,
    5281, 5297, 5303, 5309, 5323, 5333, 5347, 5351, 5381, 5387, 5393, 5399, 5407, 5413, 5417, 5419, 5431, 5437, 5441,
    5443, 5449, 5471, 5477, 5479, 5483, 5501, 5503, 5507, 5519, 5521, 5527, 5531, 5557, 5563, 5569, 5573, 5581, 5591,
    5623, 5639, 5641, 5647, 5651, 5653, 5657, 5659, 5669, 5683, 5689, 5693, 5701, 5711, 5717, 5737, 5741, 5743, 5749,
    5779, 5783, 5791, 5801, 5807, 5813, 5821, 5827, 5839, 5843, 5849, 5851, 5857, 5861, 5867, 5869, 5879, 5881, 5897,
    5903, 5923, 5927, 5939, 5953, 5981, 5987, 6007, 6011, 6029, 6037, 6043, 6047, 6053, 6067, 6073, 6079, 6089, 6091,
    6101, 6113, 6121, 6131, 6133, 6143, 6151, 6163, 6173, 6197, 6199, 6203, 6211, 6217, 6221, 6229, 6247, 6257, 6263,
    6269, 6271, 6277, 6287, 6299, 6301, 6311, 6317, 6323, 6329, 6337, 6343, 6353, 6359, 6361, 6367, 6373, 6379, 6389,
    6397, 6421, 6427, 6449, 6451, 6469, 6473, 6481, 6491, 6521, 6529, 6547, 6551, 6553, 6563, 6569, 6571, 6577, 6581,
    6599, 6607, 6619, 6637, 6653, 6659, 6661, 6673, 6679, 6689, 6691, 6701, 6703, 6709, 6719, 6733, 6737, 6761, 6763,
    6779, 6781, 6791, 6793, 6803, 6823, 6827, 6829, 6833, 6841, 6857, 6863, 6869, 6871, 6883, 6899, 6907, 6911, 6917,
    6947, 6949, 6959, 6961, 6967, 6971, 6977, 6983, 6991, 6997, 7001, 7013, 7019, 7027, 7039, 7043, 7057, 7069, 7079,
    7103, 7109, 7121, 7127, 7129, 7151, 7159, 7177, 7187, 7193, 7207, 7211, 7213, 7219, 7229, 7237, 7243, 7247, 7253,
    7283, 7297, 7307, 7309, 7321, 7331, 7333, 7349, 7351, 7369, 7393, 7411, 7417, 7433, 7451, 7457, 7459, 7477, 7481,
    7487, 7489, 7499, 7507, 7517, 7523, 7529, 7537, 7541, 7547, 7549, 7559, 7561, 7573, 7577, 7583, 7589, 7591, 7603,
    7607, 7621, 7639, 7643, 7649, 7669, 7673, 7681, 7687, 7691, 7699, 7703, 7717, 7723, 7727, 7741, 7753, 7757, 7759,
    7789, 7793, 7817, 7823, 7829, 7841, 7853, 7867, 7873, 7877, 7879, 7883, 7901, 7907, 7919, 7927, 7933, 7937, 7949,
    7951, 7963, 7993, 8009, 8011, 8017, 8039, 8053, 8059, 8069, 8081, 8087, 8089, 8093, 8101, 8111, 8117, 8123, 8147,
    8161, 8167, 8171, 8179, 8191, 8209, 8219, 8221, 8231, 8233, 8237, 8243, 8263, 8269, 8273, 8287, 8291, 8293, 8297,
    8311, 8317, 8329, 8353, 8363, 8369, 8377, 8387, 8389, 8419, 8423, 8429, 8431, 8443, 8447, 8461, 8467, 8501, 8513,
    8521, 8527, 8537, 8539, 8543, 8563, 8573, 8581, 8597, 8599, 8609, 8623, 8627, 8629, 8641, 8647, 8663, 8669, 8677,
    8681, 8689, 8693, 8699, 8707, 8713, 8719, 8731, 8737, 8741, 8747, 8753, 8761, 8779, 8783, 8803, 8807, 8819, 8821,
    8831, 8837, 8839, 8849, 8861, 8863, 8867, 8887, 8893, 8923, 8929, 8933, 8941, 8951, 8963, 8969, 8971, 8999, 9001,
    9007, 9011, 9013, 9029, 9041, 9043, 9049, 9059, 9067, 9091, 9103, 9109, 9127, 9133, 9137, 9151, 9157, 9161, 9173,
    9181, 9187, 9199, 9203, 9209, 9221, 9227, 9239, 9241, 9257, 9277, 9281, 9283, 9293, 9311, 9319, 9323, 9337, 9341,
    9343, 9349, 9371, 9377, 9391, 9397, 9403, 9413, 9419, 9421, 9431, 9433, 9437, 9439, 9461, 9463, 9467, 9473, 9479,
    9491, 9497, 9511, 9521, 9533, 9539, 9547, 9551, 9587, 9601, 9613, 9619, 9623, 9629, 9631, 9643, 9649, 9661, 9677,
    9679, 9689, 9697, 9719, 9721, 9733, 9739, 9743, 9749, 9767, 9769, 9781, 9787, 9791, 9803, 9811, 9817, 9829, 9833,
    9839, 9851, 9857, 9859, 9871, 9883, 9887, 9901, 9907, 9923, 9929, 9931, 9941, 9949, 9967, 9973, 10141,
];

fn exgcd(a: u64, b: u64) -> (u64, i64, i64) {
    let mut old_r = a as i64;
    let mut r = b as i64;
    let mut old_s = 1i64;
    let mut s = 0i64;
    let mut old_t = 0i64;
    let mut t = 1i64;

    while r != 0 {
        let quotient = old_r / r;

        let temp_r = r;
        r = old_r - quotient * r;
        old_r = temp_r;

        let temp_s = s;
        s = old_s - quotient * s;
        old_s = temp_s;

        let temp_t = t;
        t = old_t - quotient * t;
        old_t = temp_t;
    }

    (old_r as u64, old_s, old_t)
}

fn get_inverse(a: u64, m: u64) -> Option<u64> {
    assert_ne!(a, 0);
    let (d, x, _y) = exgcd(a, m);

    if d == 1 {
        let result = ((x % m as i64) + m as i64) % m as i64;
        Some(result as u64)
    } else {
        None
    }
}

fn mod_exp(mut base: u64, mut exponent: u64, modulus: u64) -> u64 {
    if modulus == 1 {
        return 0;
    }
    let mut result = 1;
    base = base % modulus;
    while exponent > 0 {
        if exponent % 2 == 1 {
            result = (result * base) % modulus;
        }
        exponent = exponent >> 1;
        base = (base * base) % modulus;
    }
    result
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

mod tests {
    use crate::aotu::bogus_control_flow::polaris_primes::{PRIMES, exgcd, gcd, get_inverse, mod_exp};
    use std::ops::{Rem, Sub};

    #[test]
    fn test_exgcd() {
        let a = 30;
        let b = 18;
        let (g, x, y) = exgcd(a, b);
        println!("g = {}, x = {}, y = {}", g, x, y);
        assert_eq!(a as i64 * x + b as i64 * y, g as i64);
    }

    #[test]
    fn test_get_inverse() {
        // 3 的模 7 逆元是 5，因为 3 * 5 = 15 ≡ 1 (mod 7)
        assert_eq!(get_inverse(3, 7), Some(5));

        // 6 在模 9 下没有逆元，因为 gcd(6, 9) = 3 ≠ 1
        assert_eq!(get_inverse(6, 9), None);
    }

    #[test]
    fn test_mod_inv() {
        let mut var0 = 0;
        let mut var1 = 0;

        let module = 0x100000000u64 - rand::random::<u32>() as u64;
        // PRIMES.max() = 9973
        let x = PRIMES[rand::random_range(0..PRIMES.len())] % module;

        println!("x = {}", x);
        println!("module = {}", module);

        var0 = x;
        var1 = x;

        let n = rand::random_range(100..10000);
        for i in 0..n {
            let b = rand::random::<u64>() % module;
            let inv = get_inverse(x, module).unwrap();
            let a = ((b * inv) % module + 1) % module;
            var0 = (a * var0).rem(module).sub(b).rem(module);
            println!("[{}] var0 = {}, var1 = {}", i, var0, var1);
            assert_eq!(var0, var1)
        }
    }

    #[test]
    fn test_double_inverse_affine_chain() {
        let mut var0 = 0;
        let mut var1 = 0;

        let module = 0x100000000u64 - rand::random::<u32>() as u64;
        let x = PRIMES[rand::random_range(0..PRIMES.len())] % module;

        println!("x = {}", x);
        println!("module = {}", module);

        var0 = x;
        var1 = x;

        let n = 100;

        for i in 0..n {
            // 随机选择 k 和 c，确保 k 与 module 互质
            let mut k;
            loop {
                k = rand::random::<u64>() % module;
                if k != 0 && gcd(k, module) == 1 {
                    break;
                }
            }

            let c = rand::random::<u64>() % module;

            // 计算 k 的模逆元
            let inv_k = get_inverse(k, module).unwrap();

            // 双重模逆
            var0 = ((var0 * k + c) % module + module - c) % module;
            var0 = (var0 * inv_k) % module;

            println!("[{}] var0 = {}, var1 = {}, c = {}, k = {}", i, var0, var1, c, k);
            assert_eq!(var0, var1);
        }
    }

    #[test]
    fn test_fermat_little_theorem() {
        let mut var0 = 0;
        let mut var1 = 0;

        let p = PRIMES[rand::random_range(0..PRIMES.len())];
        let x = PRIMES[rand::random_range(0..PRIMES.len())] % p;

        println!("x = {}", x);
        println!("p = {}", p);

        var0 = x;
        var1 = x;

        let n = 1000;

        for i in 0..n {
            // 生成随机指数 r，范围在 1 到 p-2 之间
            let r = rand::random_range(1..(p - 1));

            // 使用费马小定理保持 var0 不变
            let part1 = mod_exp(var0, r, p);
            let part2 = mod_exp(var0, (p - 1) - r, p);
            var0 = (part1 * part2 % p) * var0 % p;

            println!("[{}] var0 = {}, var1 = {}", i, var0, var1);
            assert_eq!(var0, var1);
        }
    }
}
