volatile int branch_value;

int unused_branch(int input) {
    if (input)
        branch_value = 1;
    else
        branch_value = 2;
    return branch_value;
}

int main(void) { return 0; }
