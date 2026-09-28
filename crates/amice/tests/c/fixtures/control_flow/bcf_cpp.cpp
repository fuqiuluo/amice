#include "bcf_cpp.h"
#include <atomic>
#include <cassert>
#include <coroutine>
#include <csetjmp>
#include <cstdarg>
#include <memory>
#include <stdexcept>
#include <thread>
#include <vector>

static std::atomic<int> live{0};
struct Lifetime {
    Lifetime() { live.fetch_add(1, std::memory_order_relaxed); }
    ~Lifetime() { live.fetch_sub(1, std::memory_order_relaxed); }
};

struct Left {
    virtual ~Left() = default;
    virtual std::uint64_t value() const = 0;
};
struct Right {
    virtual ~Right() = default;
    std::uint32_t right = 17;
};
struct Derived final : Left, Right {
    std::uint32_t x;
    explicit Derived(std::uint32_t n) : x(n) {}
    BCF_NOINLINE std::uint64_t value() const override { return bcf_mix(x); }
};

BCF_NOINLINE static std::uint64_t virtual_call(Left *object) {
    auto *derived = dynamic_cast<Derived *>(object);
    assert(derived && dynamic_cast<Right *>(object)->right == 17);
    return object->value();
}

BCF_NOINLINE BcfPacket BcfPacket::transformed() const {
    BcfPacket result{};
    for (int i = 0; i < 8; ++i) result.words[i] = bcf_mix(words[i]);
    return result;
}

BCF_NOINLINE static std::uint64_t by_value(BcfPacket packet) {
    packet.words[0] += 7;
    auto result = packet.transformed();
    return result.words[0] ^ result.words[7];
}

BCF_NOINLINE static std::uint64_t throwing(std::uint32_t x) {
    Lifetime guard;
    std::vector<std::uint32_t> buffer(17, x);
    if (x & 1) throw std::runtime_error("expected");
    return bcf_mix(buffer.back());
}

BCF_NOINLINE static std::uint64_t catch_and_cleanup(std::uint32_t x) {
    try {
        Lifetime outer;
        return throwing(x);
    } catch (const std::runtime_error &) {
        return bcf_mix(x);
    }
}

struct Sequence {
    struct promise_type {
        std::uint64_t value = 0;
        Sequence get_return_object() {
            return Sequence{std::coroutine_handle<promise_type>::from_promise(*this)};
        }
        std::suspend_always initial_suspend() noexcept { return {}; }
        std::suspend_always final_suspend() noexcept { return {}; }
        std::suspend_always yield_value(std::uint64_t n) noexcept { value = n; return {}; }
        void return_void() noexcept {}
        void unhandled_exception() { std::terminate(); }
    };
    std::coroutine_handle<promise_type> handle;
    ~Sequence() { handle.destroy(); }
};

BCF_NOINLINE static Sequence sequence(std::uint32_t x) {
    Lifetime suspended;
    co_yield bcf_mix(x);
    co_yield bcf_mix(std::uint64_t(x) + 1);
}

BCF_NOINLINE static std::uint64_t coroutine_sum(std::uint32_t x) {
    auto seq = sequence(x);
    std::uint64_t result = 0;
    while (!seq.handle.done()) {
        seq.handle.resume();
        if (!seq.handle.done()) result += seq.handle.promise().value;
    }
    return result;
}

BCF_NOINLINE static std::uint64_t varargs(unsigned count, ...) {
    va_list args;
    va_start(args, count);
    std::uint64_t result = 0;
    for (unsigned i = 0; i < count; ++i) result += va_arg(args, std::uint64_t);
    va_end(args);
    return result;
}

static std::jmp_buf jump_buffer;
BCF_NOINLINE static void jump_back() { std::longjmp(jump_buffer, 7); }
BCF_NOINLINE static int returns_twice() {
    int result = setjmp(jump_buffer);
    if (result == 0) jump_back();
    return result;
}

BCF_NOINLINE static std::uint64_t local_static() {
    static const auto object = std::make_unique<std::uint64_t>(0xabcdef);
    return *object;
}

BCF_NOINLINE int bcf_lifetime_count() { return live.load(std::memory_order_relaxed); }

BCF_NOINLINE std::uint64_t bcf_cpp_case(std::uint32_t x) {
    Derived object(x);
    BcfPacket packet{{x, 1, 2, 3, 4, 5, 6, std::uint64_t(x) + 9}};
    std::uint64_t result = virtual_call(&object) + by_value(packet);
    assert(packet.words[0] == x);
    result += catch_and_cleanup(x) + coroutine_sum(x);
    result += varargs(3, std::uint64_t(x), std::uint64_t(5), std::uint64_t(11));
    result += local_static() + returns_twice();
    return result;
}

BCF_NOINLINE void bcf_parallel_check() {
    std::atomic<std::uint64_t> sum{0};
    std::vector<std::thread> workers;
    for (unsigned id = 0; id < 4; ++id) workers.emplace_back([&, id] {
        thread_local auto state = std::make_unique<unsigned>(0);
        for (unsigned n = 0; n < 128; ++n) {
            assert((*state)++ == n);
            sum.fetch_add(id + n + local_static(), std::memory_order_relaxed);
        }
    });
    for (auto &worker : workers) worker.join();
    assert(sum.load() == 4 * 128 * (std::uint64_t(0xabcdef) + 65));
}
