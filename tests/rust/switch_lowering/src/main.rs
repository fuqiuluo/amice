// Rust switch lowering (switch to if-else) test
// Tests various switch/match patterns

use std::sync::atomic::{AtomicI32, Ordering};

// Prevent optimizer from removing code
static SINK: AtomicI32 = AtomicI32::new(0);

fn set_sink(val: i32) {
    SINK.store(val, Ordering::SeqCst);
}

fn get_sink() -> i32 {
    SINK.load(Ordering::SeqCst)
}

// Test 1: Simple match with consecutive integers
fn test_simple_match(value: i32) -> i32 {
    let result = match value {
        1 => {
            println!("test_simple_match: value = 1");
            10
        }
        2 => {
            println!("test_simple_match: value = 2");
            20
        }
        3 => {
            println!("test_simple_match: value = 3");
            30
        }
        4 => {
            println!("test_simple_match: value = 4");
            40
        }
        5 => {
            println!("test_simple_match: value = 5");
            50
        }
        _ => {
            println!("test_simple_match: value = default");
            -1
        }
    };
    set_sink(result);
    result
}

// Test 2: Sparse match with non-consecutive values
fn test_sparse_match(value: i32) -> i32 {
    let result = match value {
        0 => {
            println!("test_sparse_match: value = 0");
            0
        }
        10 => {
            println!("test_sparse_match: value = 10");
            100
        }
        100 => {
            println!("test_sparse_match: value = 100");
            1000
        }
        255 => {
            println!("test_sparse_match: value = 255");
            2550
        }
        1000 => {
            println!("test_sparse_match: value = 1000");
            10000
        }
        _ => {
            println!("test_sparse_match: value = default");
            -1
        }
    };
    set_sink(result);
    result
}

// Test 3: Character match (equivalent to switch on char)
fn test_char_match(ch: char) -> i32 {
    let result = match ch {
        'a' => {
            println!("test_char_match: ch = 'a'");
            1
        }
        'b' => {
            println!("test_char_match: ch = 'b'");
            2
        }
        'c' => {
            println!("test_char_match: ch = 'c'");
            3
        }
        'x' => {
            println!("test_char_match: ch = 'x'");
            24
        }
        'y' => {
            println!("test_char_match: ch = 'y'");
            25
        }
        'z' => {
            println!("test_char_match: ch = 'z'");
            26
        }
        _ => {
            println!("test_char_match: ch = default");
            0
        }
    };
    set_sink(result);
    result
}

// Test 4: Enum match (similar to switch on enum tag)
#[derive(Debug, Clone, Copy, PartialEq)]
enum Operation {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Power,
}

fn test_enum_match(op: Operation, a: i32, b: i32) -> i32 {
    let result = match op {
        Operation::Add => {
            println!("test_enum_match: Add({}, {})", a, b);
            a + b
        }
        Operation::Subtract => {
            println!("test_enum_match: Subtract({}, {})", a, b);
            a - b
        }
        Operation::Multiply => {
            println!("test_enum_match: Multiply({}, {})", a, b);
            a * b
        }
        Operation::Divide => {
            println!("test_enum_match: Divide({}, {})", a, b);
            if b != 0 {
                a / b
            } else {
                0
            }
        }
        Operation::Modulo => {
            println!("test_enum_match: Modulo({}, {})", a, b);
            if b != 0 {
                a % b
            } else {
                0
            }
        }
        Operation::Power => {
            println!("test_enum_match: Power({}, {})", a, b);
            if b >= 0 {
                (0..b).fold(1, |acc, _| acc * a)
            } else {
                0
            }
        }
    };
    set_sink(result);
    result
}

// Test 5: Nested match (switch within switch)
fn test_nested_match(outer: i32, inner: i32) -> i32 {
    let result = match outer {
        1 => match inner {
            1 => {
                println!("test_nested_match: outer=1, inner=1");
                11
            }
            2 => {
                println!("test_nested_match: outer=1, inner=2");
                12
            }
            3 => {
                println!("test_nested_match: outer=1, inner=3");
                13
            }
            _ => {
                println!("test_nested_match: outer=1, inner=default");
                10
            }
        },
        2 => match inner {
            1 => {
                println!("test_nested_match: outer=2, inner=1");
                21
            }
            2 => {
                println!("test_nested_match: outer=2, inner=2");
                22
            }
            _ => {
                println!("test_nested_match: outer=2, inner=default");
                20
            }
        },
        3 => {
            println!("test_nested_match: outer=3");
            30 + inner
        }
        _ => {
            println!("test_nested_match: outer=default");
            -1
        }
    };
    set_sink(result);
    result
}

// Test 6: Match with multiple patterns per arm
fn test_multi_pattern_match(value: i32) -> i32 {
    let result = match value {
        1 | 2 | 3 => {
            println!("test_multi_pattern_match: value in [1,2,3]");
            100
        }
        10 | 20 | 30 => {
            println!("test_multi_pattern_match: value in [10,20,30]");
            200
        }
        100 | 200 => {
            println!("test_multi_pattern_match: value in [100,200]");
            300
        }
        _ => {
            println!("test_multi_pattern_match: value = default");
            0
        }
    };
    set_sink(result);
    result
}

// Test 7: Large switch with many cases
fn test_large_switch(value: u8) -> i32 {
    let result = match value {
        0 => 0,
        1 => 1,
        2 => 4,
        3 => 9,
        4 => 16,
        5 => 25,
        6 => 36,
        7 => 49,
        8 => 64,
        9 => 81,
        10 => 100,
        11 => 121,
        12 => 144,
        13 => 169,
        14 => 196,
        15 => 225,
        16 => 256,
        _ => -1,
    };

    if value <= 16 {
        println!("test_large_switch: value = {}, result = {}", value, result);
    } else {
        println!("test_large_switch: value = {}, result = default", value);
    }

    set_sink(result);
    result
}

// Test 8: Match in loop (to test switch in loop context)
fn test_match_in_loop(values: &[i32]) -> i32 {
    let mut sum = 0;
    for &value in values {
        let contribution = match value {
            1 => 10,
            2 => 20,
            3 => 30,
            4 => 40,
            5 => 50,
            _ => 0,
        };
        sum += contribution;
    }
    println!("test_match_in_loop: sum = {}", sum);
    set_sink(sum);
    sum
}

// Test 9: Match with computation in each arm
fn test_complex_arms(x: i32, y: i32) -> i32 {
    let choice = x % 5;
    let result = match choice {
        0 => {
            println!("test_complex_arms: choice = 0");
            x * y
        }
        1 => {
            println!("test_complex_arms: choice = 1");
            x + y * 2
        }
        2 => {
            println!("test_complex_arms: choice = 2");
            (x * x) + (y * y)
        }
        3 => {
            println!("test_complex_arms: choice = 3");
            (x - y) * 3
        }
        4 => {
            println!("test_complex_arms: choice = 4");
            x ^ y
        }
        _ => {
            println!("test_complex_arms: choice = default (impossible)");
            0
        }
    };
    set_sink(result);
    result
}

// Test 10: Match on bool (simplest case)
fn test_bool_match(flag: bool) -> i32 {
    let result = match flag {
        true => {
            println!("test_bool_match: flag = true");
            1
        }
        false => {
            println!("test_bool_match: flag = false");
            0
        }
    };
    set_sink(result);
    result
}

fn main() {
    println!("=== Running Rust Switch Lowering Test Suite ===");

    println!("\n--- Test 1: Simple Match ---");
    assert_eq!(test_simple_match(1), 10);
    assert_eq!(test_simple_match(3), 30);
    assert_eq!(test_simple_match(5), 50);
    assert_eq!(test_simple_match(99), -1);

    println!("\n--- Test 2: Sparse Match ---");
    assert_eq!(test_sparse_match(0), 0);
    assert_eq!(test_sparse_match(10), 100);
    assert_eq!(test_sparse_match(100), 1000);
    assert_eq!(test_sparse_match(255), 2550);
    assert_eq!(test_sparse_match(1000), 10000);
    assert_eq!(test_sparse_match(42), -1);

    println!("\n--- Test 3: Character Match ---");
    assert_eq!(test_char_match('a'), 1);
    assert_eq!(test_char_match('b'), 2);
    assert_eq!(test_char_match('c'), 3);
    assert_eq!(test_char_match('x'), 24);
    assert_eq!(test_char_match('z'), 26);
    assert_eq!(test_char_match('q'), 0);

    println!("\n--- Test 4: Enum Match ---");
    assert_eq!(test_enum_match(Operation::Add, 5, 3), 8);
    assert_eq!(test_enum_match(Operation::Subtract, 10, 4), 6);
    assert_eq!(test_enum_match(Operation::Multiply, 6, 7), 42);
    assert_eq!(test_enum_match(Operation::Divide, 20, 4), 5);
    assert_eq!(test_enum_match(Operation::Modulo, 17, 5), 2);
    assert_eq!(test_enum_match(Operation::Power, 2, 5), 32);

    println!("\n--- Test 5: Nested Match ---");
    assert_eq!(test_nested_match(1, 1), 11);
    assert_eq!(test_nested_match(1, 2), 12);
    assert_eq!(test_nested_match(2, 1), 21);
    assert_eq!(test_nested_match(2, 2), 22);
    assert_eq!(test_nested_match(3, 5), 35);
    assert_eq!(test_nested_match(99, 99), -1);

    println!("\n--- Test 6: Multi-Pattern Match ---");
    assert_eq!(test_multi_pattern_match(1), 100);
    assert_eq!(test_multi_pattern_match(2), 100);
    assert_eq!(test_multi_pattern_match(3), 100);
    assert_eq!(test_multi_pattern_match(10), 200);
    assert_eq!(test_multi_pattern_match(20), 200);
    assert_eq!(test_multi_pattern_match(100), 300);
    assert_eq!(test_multi_pattern_match(200), 300);
    assert_eq!(test_multi_pattern_match(42), 0);

    println!("\n--- Test 7: Large Switch ---");
    assert_eq!(test_large_switch(0), 0);
    assert_eq!(test_large_switch(5), 25);
    assert_eq!(test_large_switch(10), 100);
    assert_eq!(test_large_switch(16), 256);
    assert_eq!(test_large_switch(99), -1);

    println!("\n--- Test 8: Match in Loop ---");
    assert_eq!(test_match_in_loop(&[1, 2, 3, 4, 5]), 150);
    assert_eq!(test_match_in_loop(&[1, 1, 1]), 30);
    assert_eq!(test_match_in_loop(&[5, 5, 5]), 150);

    println!("\n--- Test 9: Complex Arms ---");
    assert_eq!(test_complex_arms(10, 5), 50);  // 10*5
    assert_eq!(test_complex_arms(11, 5), 21);  // 11+5*2
    assert_eq!(test_complex_arms(12, 5), 169); // 12*12+5*5
    assert_eq!(test_complex_arms(13, 5), 24);  // (13-5)*3
    assert_eq!(test_complex_arms(14, 5), 11);  // 14^5

    println!("\n--- Test 10: Bool Match ---");
    assert_eq!(test_bool_match(true), 1);
    assert_eq!(test_bool_match(false), 0);

    println!("\n=== All tests completed! sink = {} ===", get_sink());
    println!("SUCCESS: All switch lowering tests passed!");
}
