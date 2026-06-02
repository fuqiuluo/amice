// Rust Clone Function (Constant Argument Specialization) test
// Tests function cloning with constant arguments

use std::sync::atomic::{AtomicI32, Ordering};

static SINK: AtomicI32 = AtomicI32::new(0);

#[inline(never)]
fn set_sink(val: i32) {
    SINK.store(val, Ordering::SeqCst);
}

#[inline(never)]
fn get_sink() -> i32 {
    SINK.load(Ordering::SeqCst)
}

// Functions with constant arguments that should be specialized

#[inline(never)]
fn multiply_by(x: i32, factor: i32) -> i32 {
    x * factor
}

#[inline(never)]
fn add_offset(x: i32, offset: i32) -> i32 {
    x + offset
}

#[inline(never)]
fn power_of(base: i32, exp: i32) -> i32 {
    let mut result = 1;
    for _ in 0..exp {
        result *= base;
    }
    result
}

#[inline(never)]
fn scale_and_shift(x: i32, scale: i32, shift: i32) -> i32 {
    x * scale + shift
}

#[inline(never)]
fn combine(a: i32, b: i32, op: i32) -> i32 {
    match op {
        0 => a + b,
        1 => a - b,
        2 => a * b,
        3 => if b != 0 { a / b } else { 0 },
        _ => a ^ b,
    }
}

#[inline(never)]
fn conditional_calc(x: i32, mode: i32, param: i32) -> i32 {
    if mode == 0 {
        x + param
    } else if mode == 1 {
        x - param
    } else if mode == 2 {
        x * param
    } else {
        x / param.max(1)
    }
}

#[inline(never)]
fn array_op(arr: &[i32], op: i32) -> i32 {
    let mut result = if op == 0 { 0 } else { 1 };
    for &val in arr {
        match op {
            0 => result += val,      // sum
            1 => result *= val.max(1), // product
            2 => result = result.max(val), // max
            3 => result = result.min(val), // min
            _ => result ^= val,      // xor
        }
    }
    result
}

fn main() {
    println!("=== Running Rust Clone Function Test Suite ===");

    // Test 1: multiply_by with constant factors
    println!("\n--- Test 1: Multiply By Constant ---");
    let r1 = multiply_by(10, 2);  // Should specialize for factor=2
    let r2 = multiply_by(10, 3);  // Should specialize for factor=3
    let r3 = multiply_by(10, 5);  // Should specialize for factor=5
    assert_eq!(r1, 20);
    assert_eq!(r2, 30);
    assert_eq!(r3, 50);
    println!("multiply_by(10, 2) = {}", r1);
    println!("multiply_by(10, 3) = {}", r2);
    println!("multiply_by(10, 5) = {}", r3);
    println!("Multiply by constant tests passed!");

    // Test 2: add_offset with constant offsets
    println!("\n--- Test 2: Add Offset Constant ---");
    let r1 = add_offset(100, 10);
    let r2 = add_offset(100, 20);
    let r3 = add_offset(100, -5);
    assert_eq!(r1, 110);
    assert_eq!(r2, 120);
    assert_eq!(r3, 95);
    println!("add_offset(100, 10) = {}", r1);
    println!("add_offset(100, 20) = {}", r2);
    println!("add_offset(100, -5) = {}", r3);
    println!("Add offset constant tests passed!");

    // Test 3: power_of with constant exponents
    println!("\n--- Test 3: Power Of Constant ---");
    let r1 = power_of(2, 3);  // 2^3 = 8
    let r2 = power_of(2, 4);  // 2^4 = 16
    let r3 = power_of(3, 3);  // 3^3 = 27
    assert_eq!(r1, 8);
    assert_eq!(r2, 16);
    assert_eq!(r3, 27);
    println!("power_of(2, 3) = {}", r1);
    println!("power_of(2, 4) = {}", r2);
    println!("power_of(3, 3) = {}", r3);
    println!("Power of constant tests passed!");

    // Test 4: scale_and_shift with constant scale and shift
    println!("\n--- Test 4: Scale And Shift ---");
    let r1 = scale_and_shift(10, 2, 5);   // 10*2 + 5 = 25
    let r2 = scale_and_shift(10, 3, 10);  // 10*3 + 10 = 40
    let r3 = scale_and_shift(5, 4, -2);   // 5*4 - 2 = 18
    assert_eq!(r1, 25);
    assert_eq!(r2, 40);
    assert_eq!(r3, 18);
    println!("scale_and_shift(10, 2, 5) = {}", r1);
    println!("scale_and_shift(10, 3, 10) = {}", r2);
    println!("scale_and_shift(5, 4, -2) = {}", r3);
    println!("Scale and shift tests passed!");

    // Test 5: combine with constant operation
    println!("\n--- Test 5: Combine With Constant Op ---");
    let r1 = combine(10, 5, 0);  // add: 15
    let r2 = combine(10, 5, 1);  // sub: 5
    let r3 = combine(10, 5, 2);  // mul: 50
    let r4 = combine(10, 5, 3);  // div: 2
    let r5 = combine(10, 5, 4);  // xor: 15
    assert_eq!(r1, 15);
    assert_eq!(r2, 5);
    assert_eq!(r3, 50);
    assert_eq!(r4, 2);
    assert_eq!(r5, 15);
    println!("combine(10, 5, 0) = {} (add)", r1);
    println!("combine(10, 5, 1) = {} (sub)", r2);
    println!("combine(10, 5, 2) = {} (mul)", r3);
    println!("combine(10, 5, 3) = {} (div)", r4);
    println!("combine(10, 5, 4) = {} (xor)", r5);
    println!("Combine with constant op tests passed!");

    // Test 6: conditional_calc with constant mode
    println!("\n--- Test 6: Conditional Calc ---");
    let r1 = conditional_calc(100, 0, 10);  // add: 110
    let r2 = conditional_calc(100, 1, 10);  // sub: 90
    let r3 = conditional_calc(100, 2, 2);   // mul: 200
    let r4 = conditional_calc(100, 3, 5);   // div: 20
    assert_eq!(r1, 110);
    assert_eq!(r2, 90);
    assert_eq!(r3, 200);
    assert_eq!(r4, 20);
    println!("conditional_calc(100, 0, 10) = {}", r1);
    println!("conditional_calc(100, 1, 10) = {}", r2);
    println!("conditional_calc(100, 2, 2) = {}", r3);
    println!("conditional_calc(100, 3, 5) = {}", r4);
    println!("Conditional calc tests passed!");

    // Test 7: array_op with constant operation
    println!("\n--- Test 7: Array Op With Constant ---");
    let arr = [1, 2, 3, 4, 5];
    let r1 = array_op(&arr, 0);  // sum: 15
    let r2 = array_op(&arr, 1);  // product: 120
    let r3 = array_op(&arr, 2);  // max: 5
    let r4 = array_op(&arr, 3);  // min: 1
    assert_eq!(r1, 15);
    assert_eq!(r2, 120);
    assert_eq!(r3, 5);
    assert_eq!(r4, 1);
    println!("array_op([1,2,3,4,5], 0) = {} (sum)", r1);
    println!("array_op([1,2,3,4,5], 1) = {} (product)", r2);
    println!("array_op([1,2,3,4,5], 2) = {} (max)", r3);
    println!("array_op([1,2,3,4,5], 3) = {} (min)", r4);
    println!("Array op tests passed!");

    // Test 8: Multiple calls with same constant
    println!("\n--- Test 8: Multiple Calls Same Constant ---");
    let mut sum = 0;
    for i in 1..=5 {
        sum += multiply_by(i, 10);  // All calls use factor=10
    }
    assert_eq!(sum, 150);  // (1+2+3+4+5) * 10 = 150
    println!("Sum of multiply_by(i, 10) for i=1..5 = {}", sum);
    println!("Multiple calls same constant tests passed!");

    // Test 9: Chain of specialized calls
    println!("\n--- Test 9: Chain Of Specialized Calls ---");
    let x = 5;
    let r = multiply_by(add_offset(x, 10), 2);  // (5+10)*2 = 30
    assert_eq!(r, 30);
    println!("multiply_by(add_offset(5, 10), 2) = {}", r);
    println!("Chain of specialized calls tests passed!");

    // Test 10: Mixed constant and variable args
    println!("\n--- Test 10: Mixed Args ---");
    let var = get_sink();
    set_sink(42);
    let r1 = scale_and_shift(get_sink(), 2, 0);  // 42*2 + 0 = 84
    let r2 = scale_and_shift(10, 2, get_sink()); // 10*2 + 42 = 62 (but sink is 42)
    set_sink(r1);
    assert_eq!(r1, 84);
    println!("scale_and_shift(42, 2, 0) = {}", r1);
    println!("Mixed args tests passed!");

    println!("\n=== Final Sink Value: {} ===", get_sink());
    println!("SUCCESS: All clone function tests passed!");
}
