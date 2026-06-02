// Rust indirect call obfuscation test

// Simple functions
fn add(a: i32, b: i32) -> i32 {
    println!("Called: add({}, {})", a, b);
    a + b
}

fn mul(a: i32, b: i32) -> i32 {
    println!("Called: mul({}, {})", a, b);
    a * b
}

fn sub(a: i32, b: i32) -> i32 {
    println!("Called: sub({}, {})", a, b);
    a - b
}

// Function with no arguments
fn greet() {
    println!("Called: greet()");
}

// Function with multiple types
fn process(name: &str, value: i32) -> String {
    println!("Called: process({}, {})", name, value);
    format!("{}={}", name, value)
}

// Recursive function
fn factorial(n: u32) -> u32 {
    println!("Called: factorial({})", n);
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

// Generic function
fn double<T: std::ops::Add<Output = T> + Copy>(x: T) -> T {
    x + x
}

// Struct with methods
struct Calculator {
    value: i32,
}

impl Calculator {
    fn new(value: i32) -> Self {
        println!("Called: Calculator::new({})", value);
        Calculator { value }
    }

    fn add(&mut self, x: i32) {
        println!("Called: Calculator::add({})", x);
        self.value += x;
    }

    fn get(&self) -> i32 {
        println!("Called: Calculator::get()");
        self.value
    }
}

// Trait for testing trait method calls
trait Compute {
    fn compute(&self, x: i32) -> i32;
}

impl Compute for Calculator {
    fn compute(&self, x: i32) -> i32 {
        println!("Called: Calculator::compute({})", x);
        self.value * x
    }
}

fn main() {
    println!("=== Direct Function Calls ===");

    // Test simple function calls
    let r1 = add(10, 5);
    println!("Result: {}", r1);

    let r2 = mul(10, 5);
    println!("Result: {}", r2);

    let r3 = sub(10, 5);
    println!("Result: {}", r3);

    // Test function with no arguments
    greet();

    // Test function with multiple types
    let result = process("test", 42);
    println!("Process result: {}", result);

    println!("\n=== Recursive Calls ===");
    let fact = factorial(5);
    println!("Factorial(5) = {}", fact);

    println!("\n=== Generic Function Calls ===");
    let doubled = double(21);
    println!("Double(21) = {}", doubled);

    println!("\n=== Method Calls ===");
    let mut calc = Calculator::new(100);
    calc.add(50);
    calc.add(25);
    let final_value = calc.get();
    println!("Calculator value: {}", final_value);

    println!("\n=== Trait Method Calls ===");
    let computed = calc.compute(2);
    println!("Computed: {}", computed);

    println!("\n=== Closure Calls ===");
    let closure_add = |a: i32, b: i32| {
        println!("Called: closure_add({}, {})", a, b);
        a + b
    };
    let closure_result = closure_add(7, 3);
    println!("Closure result: {}", closure_result);

    println!("\n=== Function Pointer ===");
    let fn_ptr: fn(i32, i32) -> i32 = add;
    let ptr_result = fn_ptr(100, 200);
    println!("Function pointer result: {}", ptr_result);

    println!("\n=== All tests completed! ===");
}
