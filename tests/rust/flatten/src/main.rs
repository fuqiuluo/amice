// Rust VM Flatten test
// Tests VM-based control flow flattening

use std::sync::atomic::{AtomicI32, Ordering};

// Prevent optimizer from removing code
static SINK: AtomicI32 = AtomicI32::new(0);

fn set_sink(val: i32) {
    SINK.store(val, Ordering::SeqCst);
}

fn get_sink() -> i32 {
    SINK.load(Ordering::SeqCst)
}

// Test 1: Simple arithmetic operations with switch
fn calculate(a: i32, b: i32, op: u8) -> i32 {
    let result = match op {
        0 => a + b,
        1 => a - b,
        2 => a * b,
        3 => if b != 0 { a / b } else { 0 },
        _ => a,
    };
    set_sink(result);
    result
}

// Test 2: Complex function with nested control flow
fn complex_function(input: i32) -> i32 {
    let mut result = 0;

    if input > 0 {
        if input < 10 {
            // Nested for loops
            for i in 0..input {
                for j in 0..3 {
                    result += calculate(i, j, (j % 4) as u8);
                }

                // Nested while loop
                let mut temp = input;
                while temp > 0 {
                    result += temp % 2;
                    temp /= 2;
                }
            }
        } else {
            // Another branch
            let mut i = input;
            while i > 10 {
                if i % 2 == 0 {
                    result += i / 2;
                    i -= 3;
                } else {
                    result += i * 2;
                    i -= 5;
                }
            }
        }
    } else if input < 0 {
        // Negative number handling
        let mut pos_input = -input;
        for i in (1..=pos_input).rev() {
            if i % 3 == 0 {
                result -= i;
            } else if i % 3 == 1 {
                result += i * 2;
            } else {
                result += i / 2;
            }
        }
    } else {
        // input == 0
        result = 42;
    }

    set_sink(result);
    result
}

// Test 3: Array processing with multiple nested loops
fn process_array(arr: &mut [i32]) {
    for i in 0..arr.len() {
        if arr[i] > 0 {
            let iterations = (arr[i] % 5 + 1) as usize;
            for j in 0..iterations {
                for k in 0..3 {
                    if j * k > 0 {
                        arr[i] += calculate(j as i32, k as i32, (k % 3) as u8);
                    } else {
                        arr[i] -= (j + k) as i32;
                    }
                }

                // Inner conditional branch
                if arr[i] % 2 == 0 {
                    arr[i] /= 2;
                } else {
                    arr[i] = arr[i] * 3 + 1;
                }
            }
        } else {
            // Handle negative or zero
            let mut temp = arr[i];
            while temp != 0 {
                if temp > 0 {
                    temp -= 1;
                    arr[i] += 1;
                } else {
                    temp += 1;
                    arr[i] -= 1;
                }
            }
        }
    }
}

// Test 4: Fibonacci-like function (iterative with branches)
fn fibonacci(n: i32) -> i32 {
    if n <= 1 {
        return n;
    } else if n == 2 {
        return 1;
    }

    let mut a = 0;
    let mut b = 1;
    for _i in 2..=n {
        let mut c = a + b;
        a = b;
        b = c;

        // Add some conditional branches
        if c % 3 == 0 {
            c += 1;
        } else if c % 5 == 0 {
            c -= 1;
        }
        b = c;
    }

    set_sink(b);
    b
}

// Test 5: State machine pattern
fn state_machine_test(input: &[u8]) -> i32 {
    let mut state = 0;
    let mut result = 0;

    for &byte in input {
        match state {
            0 => {
                if byte == b'a' {
                    state = 1;
                    result += 10;
                } else if byte == b'b' {
                    state = 2;
                    result += 20;
                } else {
                    state = 0;
                    result += 1;
                }
            }
            1 => {
                if byte == b'b' {
                    state = 3;
                    result += 100;
                } else {
                    state = 0;
                    result -= 5;
                }
            }
            2 => {
                if byte == b'a' {
                    state = 4;
                    result += 200;
                } else {
                    state = 0;
                    result -= 10;
                }
            }
            3 => {
                if byte == b'c' {
                    result += 1000;
                }
                state = 0;
            }
            4 => {
                if byte == b'd' {
                    result += 2000;
                }
                state = 0;
            }
            _ => {
                state = 0;
            }
        }
    }

    set_sink(result);
    result
}

// Test 6: Deeply nested conditionals
fn nested_conditions(x: i32, y: i32, z: i32) -> i32 {
    let mut result = 0;

    if x > 0 {
        if y > 0 {
            if z > 0 {
                result = x + y + z;
            } else if z < 0 {
                result = x + y - z;
            } else {
                result = x + y;
            }
        } else if y < 0 {
            if z > 0 {
                result = x - y + z;
            } else {
                result = x - y;
            }
        } else {
            result = x;
        }
    } else if x < 0 {
        if y > 0 {
            result = -x + y;
        } else {
            result = -x - y;
        }
    } else {
        result = y + z;
    }

    set_sink(result);
    result
}

// Test 7: Loop with early exit and continue
fn loop_with_control_flow(limit: i32) -> i32 {
    let mut sum = 0;

    for i in 0..limit {
        if i % 2 == 0 {
            continue;
        }

        if i > 50 {
            break;
        }

        if i % 3 == 0 {
            sum += i * 2;
        } else if i % 5 == 0 {
            sum += i / 2;
        } else {
            sum += i;
        }
    }

    set_sink(sum);
    sum
}

fn main() {
    println!("=== Running Rust Control Flow Flatten Test Suite ===");

    // Test 1: Simple calculations
    println!("\n--- Test 1: Calculate Function ---");
    assert_eq!(calculate(10, 5, 0), 15);
    assert_eq!(calculate(10, 5, 1), 5);
    assert_eq!(calculate(10, 5, 2), 50);
    assert_eq!(calculate(10, 5, 3), 2);
    println!("Calculate tests passed!");

    // Test 2: Complex function
    println!("\n--- Test 2: Complex Function ---");
    let test_values = [-5, -1, 0, 3, 7, 15, 25];
    for &input in &test_values {
        let result = complex_function(input);
        println!("complex_function({}) = {}", input, result);
    }

    // Test expected values
    assert_eq!(complex_function(0), 42);
    assert_eq!(complex_function(3), 15);
    println!("Complex function tests passed!");

    // Test 3: Array processing
    println!("\n--- Test 3: Array Processing ---");
    let mut test_array = [5, -3, 0, 12, 8, -7, 15, 2];
    println!("Original array: {:?}", test_array);
    process_array(&mut test_array);
    println!("Processed array: {:?}", test_array);

    // Verify expected values (from C version)
    assert_eq!(test_array, [1, -6, 0, 274, 514, -14, 6, 4]);
    println!("Array processing tests passed!");

    // Test 4: Fibonacci
    println!("\n--- Test 4: Fibonacci ---");
    for i in 0..=10 {
        let fib = fibonacci(i);
        let parity = if fib % 2 == 0 { "even" } else { "odd" };
        let size = if fib > 20 { " - large" } else { "" };
        println!("fib({}) = {} ({}){}", i, fib, parity, size);
    }

    // Verify some values
    assert_eq!(fibonacci(0), 0);
    assert_eq!(fibonacci(1), 1);
    assert_eq!(fibonacci(5), 7);
    println!("Fibonacci tests passed!");

    // Test 5: State machine
    println!("\n--- Test 5: State Machine ---");
    let test1 = state_machine_test(b"abc");
    let test2 = state_machine_test(b"bad");
    let test3 = state_machine_test(b"xyz");
    println!("state_machine('abc') = {}", test1);
    println!("state_machine('bad') = {}", test2);
    println!("state_machine('xyz') = {}", test3);
    assert_eq!(test1, 1110);  // a(10) + b(100) + c(1000)
    assert_eq!(test2, 2220);  // b(20) + a(200) + d(2000)
    println!("State machine tests passed!");

    // Test 6: Nested conditions
    println!("\n--- Test 6: Nested Conditions ---");
    assert_eq!(nested_conditions(5, 3, 2), 10);
    assert_eq!(nested_conditions(5, 3, -2), 10);
    assert_eq!(nested_conditions(-5, 3, 0), 8);
    assert_eq!(nested_conditions(0, 5, 3), 8);
    println!("Nested conditions tests passed!");

    // Test 7: Loop with control flow
    println!("\n--- Test 7: Loop with Control Flow ---");
    let result = loop_with_control_flow(100);
    println!("loop_with_control_flow(100) = {}", result);
    assert_eq!(result, 783);
    println!("Loop control flow tests passed!");

    // Final comprehensive test
    println!("\n--- Final Comprehensive Test ---");
    let mut final_result = 0;

    for i in 0..5 {
        match i % 4 {
            0 => {
                final_result += complex_function(i);
                println!("Case 0: addition, i={}", i);
            }
            1 => {
                final_result -= fibonacci(i.abs() % 8);
                println!("Case 1: subtraction, i={}", i);
            }
            2 => {
                final_result *= if i == 0 { 1 } else { i };
                println!("Case 2: multiplication, i={}", i);
            }
            _ => {
                if i != 0 {
                    final_result /= i;
                } else {
                    final_result += 10;
                }
                println!("Case 3: division/addition, i={}", i);
            }
        }
        println!("Current result: {}", final_result);
    }

    println!("\nFinal result: {}", final_result);
    assert_eq!(final_result, 51);

    println!("\n=== All tests completed! sink = {} ===", get_sink());
    println!("SUCCESS: All flatten tests passed!");
}
