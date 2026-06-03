// Rust Alias Access (Pointer Chain) Obfuscation Test
// Tests local variable access through obfuscated pointer chains

use std::sync::atomic::{AtomicI32, Ordering};

static SINK: AtomicI32 = AtomicI32::new(0);

#[derive(Clone, Copy)]
struct Pair {
    left: i32,
    right: i32,
}

trait Op {
    fn apply(&self, value: i32) -> i32;
}

struct Adder(i32);

impl Op for Adder {
    #[inline(never)]
    fn apply(&self, value: i32) -> i32 {
        value + self.0
    }
}

#[inline(never)]
fn set_sink(val: i32) {
    SINK.store(val, Ordering::SeqCst);
}

#[inline(never)]
fn bump_ref(value: &mut i32, delta: i32) {
    *value += delta;
}

#[inline(never)]
fn read_ref(value: &i32) -> i32 {
    *value
}

#[inline(never)]
fn run_op(op: &dyn Op, value: i32) -> i32 {
    op.apply(value)
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

#[inline(never)]
fn test_drop_and_zst(input: i32) -> i32 {
    let _marker = ();
    let text = format!("value:{}", input);
    let mut acc = 0;
    for byte in text.as_bytes() {
        acc += (*byte as i32) & 7;
    }
    let result = match Some(acc) {
        Some(value) if value > 0 => value + input,
        _ => input,
    };
    set_sink(result);
    result
}

#[inline(never)]
fn test_struct_tuple_refs(seed: i32) -> i32 {
    let mut pair = Pair {
        left: seed,
        right: seed * 2,
    };
    bump_ref(&mut pair.left, 3);

    let tuple = (pair.left, pair.right, pair.left + pair.right);
    let mut acc = tuple.0 + tuple.1 + tuple.2;
    let borrowed = read_ref(&acc);
    acc += borrowed / 3;
    set_sink(acc);
    acc
}

#[inline(never)]
fn test_option_result_slice(input: i32) -> i32 {
    let values = [input, input + 1, input + 2, input + 3];
    let slice = &values[1..3];
    let mut total = 0;
    for (idx, item) in slice.iter().enumerate() {
        total += *item * (idx as i32 + 1);
    }

    let result = match if total > 0 { Ok(total) } else { Err(input) } {
        Ok(value) if value % 2 == 1 => value + input,
        Ok(value) => value,
        Err(value) => -value,
    };
    set_sink(result);
    result
}

#[inline(never)]
fn test_trait_object_and_closure(input: i32) -> i32 {
    let adder = Adder(6);
    let base = run_op(&adder, input);
    let factor = 3;
    let closure = |value: i32| value * factor + base;
    let result = closure(4);
    set_sink(result);
    result
}

#[inline(never)]
fn test_vec_string_drop(input: i32) -> i32 {
    let mut words = vec![format!("a{}", input), String::from("rust")];
    let tail = words.pop().unwrap();
    let mut total = tail.len() as i32;
    for byte in words[0].bytes() {
        total += (byte as i32) & 3;
    }

    let result = match Some((total, input)) {
        Some((a, b)) => a * b,
        None => 0,
    };
    set_sink(result);
    result
}

#[inline(never)]
fn test_raw_pointer_roundtrip(input: i32) -> i32 {
    let mut value = input;
    let ptr = &mut value as *mut i32;
    unsafe {
        *ptr += 5;
    }
    let result = value * 2;
    set_sink(result);
    result
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

    println!("\n--- Test 6: Drop + ZST ---");
    let r6 = test_drop_and_zst(7);
    println!("test_drop_and_zst(7) = {}", r6);
    assert_eq!(r6, 37);
    println!("PASS");

    println!("\n--- Test 7: Struct + Tuple + Refs ---");
    let r7 = test_struct_tuple_refs(5);
    println!("test_struct_tuple_refs(5) = {}", r7);
    assert_eq!(r7, 48);
    println!("PASS");

    println!("\n--- Test 8: Option + Result + Slice ---");
    let r8 = test_option_result_slice(4);
    println!("test_option_result_slice(4) = {}", r8);
    assert_eq!(r8, 21);
    println!("PASS");

    println!("\n--- Test 9: Trait Object + Closure ---");
    let r9 = test_trait_object_and_closure(9);
    println!("test_trait_object_and_closure(9) = {}", r9);
    assert_eq!(r9, 27);
    println!("PASS");

    println!("\n--- Test 10: Vec + String Drop ---");
    let r10 = test_vec_string_drop(11);
    println!("test_vec_string_drop(11) = {}", r10);
    assert_eq!(r10, 77);
    println!("PASS");

    println!("\n--- Test 11: Raw Pointer Roundtrip ---");
    let r11 = test_raw_pointer_roundtrip(6);
    println!("test_raw_pointer_roundtrip(6) = {}", r11);
    assert_eq!(r11, 22);
    println!("PASS");

    println!("\nSUCCESS: All alias access tests passed!");
}
