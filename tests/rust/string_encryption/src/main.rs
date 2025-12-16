fn main() {
    // Test 1: Simple string literal
    let msg1 = "Hello, World!";
    println!("{}", msg1);

    // Test 2: String in condition
    let msg2 = "This is a secret message";
    if !msg2.is_empty() {
        println!("Message length: {}", msg2.len());
    }

    // Test 3: Multiple strings
    let greeting = "Welcome";
    let name = "User";
    println!("{}, {}!", greeting, name);

    // Test 4: String concatenation
    let part1 = "Obfuscated ";
    let part2 = "String";
    let combined = format!("{}{}", part1, part2);
    println!("{}", combined);

    // Test 5: Unicode string
    let unicode = "你好，世界！🦀";
    println!("{}", unicode);

    // Test 6: Escaped characters
    let escaped = "Line1\nLine2\tTabbed";
    println!("{}", escaped);

    // Test 7: Empty string
    let empty = "";
    println!("Empty: '{}'", empty);

    // Test 8: Long string
    let long = "This is a very long string that should definitely be encrypted by the obfuscator plugin. It contains multiple sentences and various characters!";
    println!("{}", long);

    println!("All tests completed!");
}
