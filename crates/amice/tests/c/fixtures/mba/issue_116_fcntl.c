// Self-contained so the Android IR can be generated without an NDK sysroot.
typedef unsigned int uint32_t;
enum { O_RDONLY = 0, F_GETFD = 1, F_SETFD = 2, FD_CLOEXEC = 1 };
extern int open(const char*, int, ...);
extern int fcntl(int, int, ...);
extern int printf(const char*, ...);
extern int close(int);

__attribute__((noinline, annotate("+mba")))
static int set_cloexec(int fd, volatile uint32_t *a, volatile uint32_t *b, volatile uint32_t *other) {
    uint32_t x = *a;
    uint32_t y = *b;
    uint32_t flags = (x + y) & FD_CLOEXEC;
    *other = x - y;
    return fcntl(fd, F_SETFD, (int)flags);
}

int main(void) {
    volatile uint32_t a = ~0u;
    volatile uint32_t b = 2;
    volatile uint32_t other = 0;
    int fd = open("/dev/null", O_RDONLY);
    if (fd < 0) return 2;
    int rc = set_cloexec(fd, &a, &b, &other);
    int flags = fcntl(fd, F_GETFD);
    printf("rc=%d flags=%d other=%u\n", rc, flags, other);
    close(fd);
    return rc == 0 && flags == FD_CLOEXEC && other == ~2u ? 0 : 1;
}
