int f(int x) {
    switch (x) {
        case 1:
            return 10;
        case 2:
            return 20;
        case 3:
            return 30;
        default:
            return 40;
    }
}

int main(void) {
    return f(2) == 20 ? 0 : 1;
}
