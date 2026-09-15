struct Empty {};

static long make_duration(long high, long low) {
    return high + low;
}

static long normalize_duration(long value, long adjustment) {
    return make_duration(value, adjustment);
}

static long from_integer(long value, Empty) {
    return normalize_duration(value, 0);
}

static long seconds(long value) {
    return from_integer(value, Empty{});
}

int main(int argc, char **) {
    long input = 1700000000L + argc;
    return seconds(input) == input ? 0 : 1;
}
