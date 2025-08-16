#include <iostream>
#include <stdexcept>

// Simple function that throws an exception
void may_throw(int value) {
    if (value < 0) {
        throw std::runtime_error("Negative value not allowed");
    }
    if (value > 100) {
        throw std::invalid_argument("Value too large");
    }
}

// Function with try-catch to test invoke instructions
int test_invoke(int input) {
    int result = 0;
    
    try {
        may_throw(input);
        result = input * 2;
    } catch (const std::runtime_error& e) {
        std::cout << "Runtime error: " << e.what() << std::endl;
        result = -1;
    } catch (const std::invalid_argument& e) {
        std::cout << "Invalid argument: " << e.what() << std::endl;
        result = -2;
    } catch (...) {
        std::cout << "Unknown exception" << std::endl;
        result = -3;
    }
    
    return result;
}

// Nested try-catch blocks
int nested_exceptions(int a, int b) {
    try {
        try {
            may_throw(a);
            may_throw(b);
            return a + b;
        } catch (const std::runtime_error& e) {
            std::cout << "Inner catch: " << e.what() << std::endl;
            throw std::logic_error("Converted runtime error");
        }
    } catch (const std::logic_error& e) {
        std::cout << "Outer catch: " << e.what() << std::endl;
        return 0;
    } catch (...) {
        return -1;
    }
}

int main() {
    std::cout << "Testing invoke instructions with VM flattening:" << std::endl;
    
    // Test cases that should trigger invoke instructions
    int test_values[] = {-5, 10, 150, 50};
    
    for (int i = 0; i < 4; i++) {
        int value = test_values[i];
        std::cout << "Testing value: " << value << std::endl;
        
        int result1 = test_invoke(value);
        std::cout << "Result from test_invoke: " << result1 << std::endl;
        
        int result2 = nested_exceptions(value, value/2);
        std::cout << "Result from nested_exceptions: " << result2 << std::endl;
        
        std::cout << "---" << std::endl;
    }
    
    return 0;
}