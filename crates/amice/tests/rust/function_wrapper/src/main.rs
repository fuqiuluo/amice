// Rust Function Wrapper test
// Tests function wrapper obfuscation

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

// Simple functions to be wrapped
#[inline(never)]
fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[inline(never)]
fn sub(a: i32, b: i32) -> i32 {
    a - b
}

#[inline(never)]
fn mul(a: i32, b: i32) -> i32 {
    a * b
}

#[inline(never)]
fn div(a: i32, b: i32) -> i32 {
    if b != 0 { a / b } else { 0 }
}

// Function with more parameters
#[inline(never)]
fn compute(a: i32, b: i32, c: i32, d: i32) -> i32 {
    (a + b) * (c - d)
}

// Recursive function
#[inline(never)]
fn factorial(n: i32) -> i32 {
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}

// Function calling other functions
#[inline(never)]
fn complex_calc(x: i32, y: i32) -> i32 {
    let sum = add(x, y);
    let diff = sub(x, y);
    let prod = mul(sum, diff);
    let result = div(prod, 2);
    set_sink(result);
    result
}

// Nested function calls
#[inline(never)]
fn nested_calls(a: i32, b: i32, c: i32) -> i32 {
    add(mul(a, b), sub(c, a))
}

// Function with array parameter
#[inline(never)]
fn sum_array(arr: &[i32]) -> i32 {
    let mut sum = 0;
    for &val in arr {
        sum = add(sum, val);
    }
    sum
}

// Higher-order style (but static dispatch)
#[inline(never)]
fn apply_twice(x: i32, y: i32) -> i32 {
    let first = add(x, y);
    let second = add(first, y);
    second
}

// Chain of function calls
#[inline(never)]
fn chain_ops(start: i32) -> i32 {
    let a = add(start, 10);
    let b = mul(a, 2);
    let c = sub(b, 5);
    let d = div(c, 3);
    d
}

fn main() {
    println!("=== Running Rust Function Wrapper Test Suite ===");

    // Test 1: Basic arithmetic functions
    println!("\n--- Test 1: Basic Arithmetic ---");
    assert_eq!(add(10, 5), 15);
    assert_eq!(sub(10, 5), 5);
    assert_eq!(mul(10, 5), 50);
    assert_eq!(div(10, 5), 2);
    assert_eq!(div(10, 0), 0);
    println!("Basic arithmetic tests passed!");

    // Test 2: Multi-parameter function
    println!("\n--- Test 2: Multi-parameter ---");
    let r = compute(5, 3, 10, 4);
    assert_eq!(r, 48); // (5+3) * (10-4) = 8 * 6 = 48
    println!("compute(5, 3, 10, 4) = {} (expected 48)", r);
    println!("Multi-parameter test passed!");

    // Test 3: Recursive function
    println!("\n--- Test 3: Factorial ---");
    assert_eq!(factorial(0), 1);
    assert_eq!(factorial(1), 1);
    assert_eq!(factorial(5), 120);
    assert_eq!(factorial(7), 5040);
    println!("Factorial tests passed!");

    // Test 4: Complex calculation
    println!("\n--- Test 4: Complex Calculation ---");
    let r = complex_calc(20, 10);
    assert_eq!(r, 150); // ((20+10) * (20-10)) / 2 = (30 * 10) / 2 = 150
    println!("complex_calc(20, 10) = {} (expected 150)", r);
    println!("Complex calculation test passed!");

    // Test 5: Nested calls
    println!("\n--- Test 5: Nested Calls ---");
    let r = nested_calls(3, 4, 10);
    assert_eq!(r, 19); // (3*4) + (10-3) = 12 + 7 = 19
    println!("nested_calls(3, 4, 10) = {} (expected 19)", r);
    println!("Nested calls test passed!");

    // Test 6: Array sum
    println!("\n--- Test 6: Array Sum ---");
    let arr = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let r = sum_array(&arr);
    assert_eq!(r, 55);
    println!("sum_array([1..10]) = {} (expected 55)", r);
    println!("Array sum test passed!");

    // Test 7: Apply twice
    println!("\n--- Test 7: Apply Twice ---");
    let r = apply_twice(5, 3);
    assert_eq!(r, 11); // (5+3) + 3 = 11
    println!("apply_twice(5, 3) = {} (expected 11)", r);
    println!("Apply twice test passed!");

    // Test 8: Chain operations
    println!("\n--- Test 8: Chain Operations ---");
    let r = chain_ops(10);
    assert_eq!(r, 11); // ((10+10)*2 - 5) / 3 = (40-5)/3 = 35/3 = 11
    println!("chain_ops(10) = {} (expected 11)", r);
    println!("Chain operations test passed!");

    // Test 9: Multiple calls in loop
    println!("\n--- Test 9: Loop Calls ---");
    let mut sum = 0;
    for i in 1..=10 {
        sum = add(sum, mul(i, i));
    }
    assert_eq!(sum, 385); // 1 + 4 + 9 + 16 + 25 + 36 + 49 + 64 + 81 + 100
    println!("Sum of squares 1..10 = {} (expected 385)", sum);
    println!("Loop calls test passed!");

    println!("\n=== Final Sink Value: {} ===", get_sink());
    println!("SUCCESS: All function wrapper tests passed!");
}
