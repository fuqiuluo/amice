#include <stdio.h>
#include <stdlib.h>

// Target function for specialization - has constant parameters in some calls
int compute_value(int base, int multiplier, int offset) {
    int result = base * multiplier;
    result += offset;

    // Some additional computation to make it worth specializing
    if (multiplier > 10) {
        result += base / 2;
    } else {
        result -= base / 4;
    }

    return result;
}

// Another function that could benefit from specialization
float calculate_score(float input, int mode, float threshold) {
    float score = input;

    switch (mode) {
        case 1:
            score *= 1.5f;
            break;
        case 2:
            score *= 2.0f;
            score += 10.0f;
            break;
        case 3:
            score = score * 0.8f + 5.0f;
            break;
        default:
            score += 1.0f;
    }

    if (score > threshold) {
        score *= 0.9f;
    }

    return score;
}

// Function with mixed constant and variable parameters
int process_data(int *array, int size, int operation_type, int scale_factor) {
    int sum = 0;

    for (int i = 0; i < size; i++) {
        switch (operation_type) {
            case 0: // Sum
                sum += array[i];
                break;
            case 1: // Scaled sum
                sum += array[i] * scale_factor;
                break;
            case 2: // Squared sum
                sum += array[i] * array[i];
                break;
        }
    }

    return sum;
}

int main() {
    printf("=== CloneFunction Test Case ===\n\n");

    // Test case 1: compute_value with constant parameters
    // These calls should be specialized since multiplier and offset are constants
    printf("Test 1 - compute_value specialization:\n");
    int val1 = compute_value(5, 10, 3);    // Should create specialized version for (multiplier=10, offset=3)
    int val2 = compute_value(8, 10, 3);    // Should use same specialized version
    int val3 = compute_value(12, 15, 7);   // Should create new specialized version for (multiplier=15, offset=7)
    int val4 = compute_value(3, 10, 3);    // Should use first specialized version

    printf("compute_value(5, 10, 3) = %d\n", val1);
    printf("compute_value(8, 10, 3) = %d\n", val2);
    printf("compute_value(12, 15, 7) = %d\n", val3);
    printf("compute_value(3, 10, 3) = %d\n", val4);

    // Test case 2: calculate_score with constant mode and threshold
    printf("\nTest 2 - calculate_score specialization:\n");
    float score1 = calculate_score(100.0f, 1, 150.0f);  // Specialize for mode=1, threshold=150.0
    float score2 = calculate_score(80.0f, 1, 150.0f);   // Use same specialized version
    float score3 = calculate_score(200.0f, 2, 100.0f);  // New specialization for mode=2, threshold=100.0
    float score4 = calculate_score(90.0f, 2, 100.0f);   // Use second specialized version

    printf("calculate_score(100.0, 1, 150.0) = %.2f\n", score1);
    printf("calculate_score(80.0, 1, 150.0) = %.2f\n", score2);
    printf("calculate_score(200.0, 2, 100.0) = %.2f\n", score3);
    printf("calculate_score(90.0, 2, 100.0) = %.2f\n", score4);

    // Test case 3: process_data with constant operation_type
    printf("\nTest 3 - process_data specialization:\n");
    int data1[] = {1, 2, 3, 4, 5};
    int data2[] = {10, 20, 30};
    int data3[] = {2, 4, 6, 8};

    // These should be specialized based on operation_type and scale_factor
    int result1 = process_data(data1, 5, 0, 1);    // Specialize for operation_type=0
    int result2 = process_data(data2, 3, 1, 2);    // Specialize for operation_type=1, scale_factor=2
    int result3 = process_data(data1, 5, 1, 2);    // Use same specialized version as result2
    int result4 = process_data(data3, 4, 2, 1);    // Specialize for operation_type=2

    printf("process_data(data1, 5, 0, 1) = %d\n", result1);
    printf("process_data(data2, 3, 1, 2) = %d\n", result2);
    printf("process_data(data1, 5, 1, 2) = %d\n", result3);
    printf("process_data(data3, 4, 2, 1) = %d\n", result4);

    // Test case 4: Mixed constant and variable parameters
    printf("\nTest 4 - Mixed parameters:\n");
    int base_var = 10;
    int multiplier_var = 5;

    // These should NOT be specialized since parameters are variables
    int val5 = compute_value(base_var, multiplier_var, 2);     // Only offset is constant
    int val6 = compute_value(15, multiplier_var, 4);          // Only base is constant

    printf("compute_value(base_var=%d, multiplier_var=%d, 2) = %d\n", base_var, multiplier_var, val5);
    printf("compute_value(15, multiplier_var=%d, 4) = %d\n", multiplier_var, val6);

    return 0;
}