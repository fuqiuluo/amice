// Rust indirect branch obfuscation test
// Tests various control flow patterns

use std::sync::atomic::{AtomicI32, Ordering};

// Prevent optimizer from removing code
static SINK: AtomicI32 = AtomicI32::new(0);

fn set_sink(val: i32) {
    SINK.store(val, Ordering::SeqCst);
}

fn get_sink() -> i32 {
    SINK.load(Ordering::SeqCst)
}

// Test 1: Simple if-else
fn test_conditional_br(x: i32) {
    let result = if x > 0 {
        println!("test_conditional_br: x > 0");
        10
    } else if x < 0 {
        println!("test_conditional_br: x < 0");
        -10
    } else {
        println!("test_conditional_br: x == 0");
        0
    };
    set_sink(result);
}

// Test 2: Match expression (similar to switch)
fn test_match_br(choice: i32) {
    let value = match choice {
        1 => {
            println!("test_match_br: choice = 1");
            100
        }
        2 => {
            println!("test_match_br: choice = 2");
            200
        }
        3 => {
            println!("test_match_br: choice = 3");
            300
        }
        _ => {
            println!("test_match_br: choice = default");
            -1
        }
    };
    set_sink(value);
}

// Test 3: While loop
fn test_loop_while(mut n: i32) {
    let mut sum = 0;
    while n > 0 {
        sum += n;
        n -= 1;
    }
    println!("test_loop_while: sum = {}", sum);
    set_sink(sum);
}

// Test 4: For loop with continue/break
fn test_loop_for(start: i32, end: i32) {
    let mut count = 0;
    for i in start..end {
        if i % 2 != 0 {
            continue;
        }
        count += 1;
        if count > 10 {
            break;
        }
    }
    println!("test_loop_for: count = {}", count);
    set_sink(count);
}

// Test 5: Nested if-else
fn test_nested_if_else(a: i32, b: i32, c: i32) {
    let result = if a > 0 {
        if b > 0 {
            println!("test_nested_if_else: a>0, b>0");
            1
        } else if c > 0 {
            println!("test_nested_if_else: a>0, b<=0, c>0");
            2
        } else {
            println!("test_nested_if_else: a>0, b<=0, c<=0");
            3
        }
    } else {
        println!("test_nested_if_else: a<=0");
        4
    };
    set_sink(result);
}

// Test 6: Loop with labeled break
fn test_labeled_loop(outer_count: i32) {
    let mut total = 0;
    'outer: for i in 0..outer_count {
        for j in 0..10 {
            total += 1;
            if j == 5 && i == 2 {
                println!("test_labeled_loop: breaking at i={}, j={}", i, j);
                break 'outer;
            }
        }
    }
    println!("test_labeled_loop: total = {}", total);
    set_sink(total);
}

// Test 7: Match with guards
fn test_match_guards(x: i32, y: i32) {
    let result = match (x, y) {
        (a, b) if a > 0 && b > 0 => {
            println!("test_match_guards: both positive");
            1
        }
        (a, _) if a < 0 => {
            println!("test_match_guards: x negative");
            2
        }
        (_, b) if b < 0 => {
            println!("test_match_guards: y negative");
            3
        }
        _ => {
            println!("test_match_guards: default");
            0
        }
    };
    set_sink(result);
}

// Test 8: Early return
fn test_early_return(values: &[i32]) -> i32 {
    for &v in values {
        if v < 0 {
            println!("test_early_return: found negative {}", v);
            return v;
        }
        if v > 100 {
            println!("test_early_return: found large {}", v);
            return v;
        }
    }
    println!("test_early_return: no special value found");
    0
}

// Test 9: Option/Result pattern matching
fn test_option_match(opt: Option<i32>) {
    let result = match opt {
        Some(v) if v > 0 => {
            println!("test_option_match: Some positive {}", v);
            v
        }
        Some(v) => {
            println!("test_option_match: Some non-positive {}", v);
            -v
        }
        None => {
            println!("test_option_match: None");
            0
        }
    };
    set_sink(result);
}

// Test 10: Complex control flow with multiple conditions
fn test_complex_flow(a: i32, b: i32, c: i32) {
    let mut result = 0;

    if a > 0 {
        result += 1;
        if b > 0 {
            result += 10;
            if c > 0 {
                result += 100;
            }
        } else if c > 0 {
            result += 20;
        }
    } else if b > 0 {
        result += 2;
        if c > 0 {
            result += 200;
        }
    } else {
        result += 3;
    }

    println!("test_complex_flow: result = {}", result);
    set_sink(result);
}

fn main() {
    println!("=== Running Rust Indirect Branch Test Suite ===");

    println!("\n--- Test 1: Conditional Branch ---");
    test_conditional_br(1);
    test_conditional_br(-1);
    test_conditional_br(0);

    println!("\n--- Test 2: Match Expression ---");
    test_match_br(1);
    test_match_br(2);
    test_match_br(3);
    test_match_br(99);

    println!("\n--- Test 3: While Loop ---");
    test_loop_while(5);

    println!("\n--- Test 4: For Loop ---");
    test_loop_for(1, 20);

    println!("\n--- Test 5: Nested If-Else ---");
    test_nested_if_else(1, 1, 1);
    test_nested_if_else(1, 0, 1);
    test_nested_if_else(1, 0, 0);
    test_nested_if_else(0, 0, 0);

    println!("\n--- Test 6: Labeled Loop ---");
    test_labeled_loop(5);

    println!("\n--- Test 7: Match Guards ---");
    test_match_guards(1, 1);
    test_match_guards(-1, 1);
    test_match_guards(1, -1);
    test_match_guards(0, 0);

    println!("\n--- Test 8: Early Return ---");
    let r1 = test_early_return(&[1, 2, 3, -5, 6]);
    println!("Early return result: {}", r1);
    let r2 = test_early_return(&[1, 2, 150, 4]);
    println!("Early return result: {}", r2);
    let r3 = test_early_return(&[1, 2, 3]);
    println!("Early return result: {}", r3);

    println!("\n--- Test 9: Option Match ---");
    test_option_match(Some(42));
    test_option_match(Some(-10));
    test_option_match(None);

    println!("\n--- Test 10: Complex Flow ---");
    test_complex_flow(1, 1, 1);
    test_complex_flow(1, 0, 1);
    test_complex_flow(0, 1, 1);
    test_complex_flow(0, 0, 0);

    println!("\n=== All tests completed! sink = {} ===", get_sink());
}
