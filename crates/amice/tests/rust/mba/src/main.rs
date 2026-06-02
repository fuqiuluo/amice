// Rust MBA (Mixed Boolean Arithmetic) test
// Tests MBA obfuscation on arithmetic and bitwise operations

use std::sync::atomic::{AtomicI32, Ordering};

// Prevent optimizer from removing code
static SINK: AtomicI32 = AtomicI32::new(0);

fn set_sink(val: i32) {
    SINK.store(val, Ordering::SeqCst);
}

fn get_sink() -> i32 {
    SINK.load(Ordering::SeqCst)
}

// Test 1: Basic arithmetic operations
fn basic_arithmetic(a: i32, b: i32) -> i32 {
    let sum = a + b;
    let diff = a - b;
    let product = a * b;
    let quotient = if b != 0 { a / b } else { 0 };

    let result = sum + diff - product + quotient;
    set_sink(result);
    result
}

// Test 2: Bitwise operations (XOR, AND, OR)
fn bitwise_ops(a: i32, b: i32) -> i32 {
    let xor_result = a ^ b;
    let and_result = a & b;
    let or_result = a | b;
    let not_a = !a;

    let result = xor_result + and_result - or_result + not_a;
    set_sink(result);
    result
}

// Test 3: Mixed arithmetic and bitwise
fn mixed_operations(a: i32, b: i32, c: i32) -> i32 {
    let t1 = (a + b) ^ c;
    let t2 = (a - b) & c;
    let t3 = (a * b) | c;
    let t4 = (a ^ b) + (b & c);

    let result = t1 + t2 - t3 + t4;
    set_sink(result);
    result
}

// Test 4: Shift operations
fn shift_ops(a: i32, b: u32) -> i32 {
    let b_clamped = b % 31; // Prevent overflow
    let left = a << b_clamped;
    let right = a >> b_clamped;
    let unsigned_right = ((a as u32) >> b_clamped) as i32;

    let result = left + right - unsigned_right;
    set_sink(result);
    result
}

// Test 5: Complex expression (multiple operations)
fn complex_expression(a: i32, b: i32, c: i32, d: i32) -> i32 {
    // (a + b) * (c - d) + (a ^ c) - (b & d)
    let sum = a + b;
    let diff = c - d;
    let xor_ac = a ^ c;
    let and_bd = b & d;

    let result = sum * diff + xor_ac - and_bd;
    set_sink(result);
    result
}

// Test 6: Loop with arithmetic operations
fn loop_arithmetic(n: i32) -> i32 {
    let mut sum = 0i32;
    let mut product = 1i32;

    for i in 1..=n.abs().min(20) {
        sum = sum.wrapping_add(i);
        product = product.wrapping_mul(i.wrapping_add(1) % 7 + 1);

        // Mix in some bitwise ops
        sum ^= i & 0xF;
        product &= 0x7FFFFFFF;
    }

    let result = sum.wrapping_add(product);
    set_sink(result);
    result
}

// Test 7: Constants in expressions (MBA should obfuscate these)
fn constant_arithmetic(a: i32) -> i32 {
    // Various constant operations that MBA should transform
    let t1 = a + 42;
    let t2 = a - 17;
    let t3 = a * 3;
    let t4 = a ^ 0xFF;
    let t5 = a & 0xAAAA;
    let t6 = a | 0x5555;

    let result = t1 + t2 - t3 + t4 - t5 + t6;
    set_sink(result);
    result
}

// Test 8: Nested function calls with arithmetic
fn nested_arithmetic(a: i32, b: i32) -> i32 {
    let step1 = basic_arithmetic(a, b);
    let step2 = bitwise_ops(step1, a);
    let step3 = mixed_operations(step2, b, a);

    let result = step1 + step2 - step3;
    set_sink(result);
    result
}

// Test 9: Conditional arithmetic
fn conditional_arithmetic(a: i32, b: i32) -> i32 {
    let result = if a > b {
        (a + b) * 2 - (a ^ b)
    } else if a < b {
        (a - b) * 3 + (a & b)
    } else {
        a * 4 | b
    };

    set_sink(result);
    result
}

// Test 10: Array operations with arithmetic
fn array_arithmetic(arr: &[i32]) -> i32 {
    let mut result = 0i32;

    for (i, &val) in arr.iter().enumerate() {
        let idx = i as i32;
        result = result.wrapping_add(val.wrapping_mul(idx + 1));
        result ^= val & 0xFF;
        result = result.wrapping_add(idx);
    }

    set_sink(result);
    result
}

fn main() {
    println!("=== Running Rust MBA Test Suite ===");

    // Test 1: Basic arithmetic
    println!("\n--- Test 1: Basic Arithmetic ---");
    let r1 = basic_arithmetic(100, 25);
    // sum = 125, diff = 75, product = 2500, quotient = 4
    // result = 125 + 75 - 2500 + 4 = -2296
    let expected1 = 100 + 25 + (100 - 25) - (100 * 25) + (100 / 25);
    println!("basic_arithmetic(100, 25) = {} (expected {})", r1, expected1);
    assert_eq!(r1, expected1);
    println!("Basic arithmetic test passed!");

    // Test 2: Bitwise operations
    println!("\n--- Test 2: Bitwise Operations ---");
    let r2 = bitwise_ops(0xFF, 0x0F);
    let expected2 = (0xFF ^ 0x0F) + (0xFF & 0x0F) - (0xFF | 0x0F) + (!0xFFi32);
    println!("bitwise_ops(0xFF, 0x0F) = {} (expected {})", r2, expected2);
    assert_eq!(r2, expected2);
    println!("Bitwise operations test passed!");

    // Test 3: Mixed operations
    println!("\n--- Test 3: Mixed Operations ---");
    let r3 = mixed_operations(10, 20, 30);
    let t1 = (10 + 20) ^ 30;  // 30 ^ 30 = 0
    let t2 = (10 - 20) & 30;  // -10 & 30
    let t3 = (10 * 20) | 30;  // 200 | 30
    let t4 = (10 ^ 20) + (20 & 30);  // 30 + 20 = 50
    let expected3 = t1 + t2 - t3 + t4;
    println!("mixed_operations(10, 20, 30) = {} (expected {})", r3, expected3);
    assert_eq!(r3, expected3);
    println!("Mixed operations test passed!");

    // Test 4: Shift operations
    println!("\n--- Test 4: Shift Operations ---");
    let r4 = shift_ops(100, 3);
    let expected4 = (100 << 3) + (100 >> 3) - ((100u32 >> 3) as i32);
    println!("shift_ops(100, 3) = {} (expected {})", r4, expected4);
    assert_eq!(r4, expected4);
    println!("Shift operations test passed!");

    // Test 5: Complex expression
    println!("\n--- Test 5: Complex Expression ---");
    let r5 = complex_expression(5, 10, 15, 3);
    // (5 + 10) * (15 - 3) + (5 ^ 15) - (10 & 3)
    let expected5 = (5 + 10) * (15 - 3) + (5 ^ 15) - (10 & 3);
    println!("complex_expression(5, 10, 15, 3) = {} (expected {})", r5, expected5);
    assert_eq!(r5, expected5);
    println!("Complex expression test passed!");

    // Test 6: Loop arithmetic
    println!("\n--- Test 6: Loop Arithmetic ---");
    let r6 = loop_arithmetic(10);
    println!("loop_arithmetic(10) = {}", r6);
    // Calculate expected value
    let mut sum = 0i32;
    let mut product = 1i32;
    for i in 1..=10 {
        sum = sum.wrapping_add(i);
        product = product.wrapping_mul((i + 1) % 7 + 1);
        sum ^= i & 0xF;
        product &= 0x7FFFFFFF;
    }
    let expected6 = sum.wrapping_add(product);
    println!("Expected: {}", expected6);
    assert_eq!(r6, expected6);
    println!("Loop arithmetic test passed!");

    // Test 7: Constant arithmetic
    println!("\n--- Test 7: Constant Arithmetic ---");
    let r7 = constant_arithmetic(100);
    let expected7 = (100 + 42) + (100 - 17) - (100 * 3) + (100 ^ 0xFF) - (100 & 0xAAAA) + (100 | 0x5555);
    println!("constant_arithmetic(100) = {} (expected {})", r7, expected7);
    assert_eq!(r7, expected7);
    println!("Constant arithmetic test passed!");

    // Test 8: Nested arithmetic
    println!("\n--- Test 8: Nested Arithmetic ---");
    let r8 = nested_arithmetic(50, 25);
    // Manually calculate
    let step1 = 50 + 25 + (50 - 25) - (50 * 25) + (50 / 25); // -1148
    let step2 = (step1 ^ 50) + (step1 & 50) - (step1 | 50) + (!step1);
    let step3_t1 = (step2 + 25) ^ 50;
    let step3_t2 = (step2 - 25) & 50;
    let step3_t3 = (step2 * 25) | 50;
    let step3_t4 = (step2 ^ 25) + (25 & 50);
    let step3 = step3_t1 + step3_t2 - step3_t3 + step3_t4;
    let expected8 = step1 + step2 - step3;
    println!("nested_arithmetic(50, 25) = {} (expected {})", r8, expected8);
    assert_eq!(r8, expected8);
    println!("Nested arithmetic test passed!");

    // Test 9: Conditional arithmetic
    println!("\n--- Test 9: Conditional Arithmetic ---");
    let r9a = conditional_arithmetic(100, 50);  // a > b
    let r9b = conditional_arithmetic(50, 100);  // a < b
    let r9c = conditional_arithmetic(75, 75);   // a == b
    let expected9a = (100 + 50) * 2 - (100 ^ 50);
    let expected9b = (50 - 100) * 3 + (50 & 100);
    let expected9c = 75 * 4 | 75;
    println!("conditional_arithmetic(100, 50) = {} (expected {})", r9a, expected9a);
    println!("conditional_arithmetic(50, 100) = {} (expected {})", r9b, expected9b);
    println!("conditional_arithmetic(75, 75) = {} (expected {})", r9c, expected9c);
    assert_eq!(r9a, expected9a);
    assert_eq!(r9b, expected9b);
    assert_eq!(r9c, expected9c);
    println!("Conditional arithmetic test passed!");

    // Test 10: Array arithmetic
    println!("\n--- Test 10: Array Arithmetic ---");
    let arr = [5, 10, 15, 20, 25];
    let r10 = array_arithmetic(&arr);
    // Calculate expected
    let mut expected10 = 0i32;
    for (i, &val) in arr.iter().enumerate() {
        let idx = i as i32;
        expected10 = expected10.wrapping_add(val.wrapping_mul(idx + 1));
        expected10 ^= val & 0xFF;
        expected10 = expected10.wrapping_add(idx);
    }
    println!("array_arithmetic([5, 10, 15, 20, 25]) = {} (expected {})", r10, expected10);
    assert_eq!(r10, expected10);
    println!("Array arithmetic test passed!");

    println!("\n=== Final Sink Value: {} ===", get_sink());
    println!("SUCCESS: All MBA tests passed!");
}
