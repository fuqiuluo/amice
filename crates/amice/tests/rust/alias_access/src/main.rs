// Rust Alias Access (Pointer Chain) Obfuscation Test
// Tests local variable access through obfuscated pointer chains

use std::sync::atomic::{AtomicI32, Ordering};

static SINK: AtomicI32 = AtomicI32::new(0);

#[inline(never)]
fn set_sink(val: i32) {
    SINK.store(val, Ordering::SeqCst);
}

#[inline(never)]
fn test_simple(a: i32, b: i32) -> i32 {
    let x = a + 10;
    let y = b * 2;
    let result = x + y;
    set_sink(result);
    result
}

#[inline(never)]
fn test_array(n: i32) -> i32 {
    let mut arr = [0i32; 4];
    for i in 0..4 {
        arr[i] = (i as i32) * n;
    }
    let mut sum = 0;
    for i in 0..4 {
        sum += arr[i];
    }
    set_sink(sum);
    sum
}

#[inline(never)]
fn test_multiple(input: i32) -> i32 {
    let a = input;
    let b = a * 2;
    let c = b + 10;
    let d = c / 2;
    let result = d - a;
    set_sink(result);
    result
}

#[inline(never)]
fn test_conditional(input: i32) -> i32 {
    let mut a = input;
    let mut b = 0;
    if a > 0 {
        b = a * 2;
        a = b + 10;
    } else {
        b = -a;
        a = b * 3;
    }
    let result = a + b;
    set_sink(result);
    result
}

#[inline(never)]
fn test_loop(n: i32) -> i32 {
    let mut sum = 0;
    let mut i = 0;
    while i < n {
        sum += i;
        i += 1;
    }
    set_sink(sum);
    sum
}

fn main() {
    println!("=== Rust Alias Access Test Suite ===");

    println!("\n--- Test 1: Simple Locals ---");
    let r1 = test_simple(5, 3);
    println!("test_simple(5, 3) = {}", r1);
    assert_eq!(r1, 21);
    println!("PASS");

    println!("\n--- Test 2: Local Array ---");
    let r2 = test_array(2);
    println!("test_array(2) = {}", r2);
    assert_eq!(r2, 12);
    println!("PASS");

    println!("\n--- Test 3: Multiple Locals ---");
    let r3 = test_multiple(10);
    println!("test_multiple(10) = {}", r3);
    assert_eq!(r3, 5);
    println!("PASS");

    println!("\n--- Test 4: Conditional ---");
    let r4 = test_conditional(5);
    println!("test_conditional(5) = {}", r4);
    assert_eq!(r4, 30);
    println!("PASS");

    println!("\n--- Test 5: Loop ---");
    let r5 = test_loop(5);
    println!("test_loop(5) = {}", r5);
    assert_eq!(r5, 10);
    println!("PASS");

    println!("\nSUCCESS: All alias access tests passed!");
}
