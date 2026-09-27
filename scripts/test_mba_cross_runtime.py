#!/usr/bin/env python3
"""Build freestanding MBA differential executables for Windows x64 / Android arm64.

Run the mba_regions integration tests first to generate original.ll, reference.ll
and driver.c. This script only builds: run the resulting executables on the actual
target. No SDK/sysroot is required; the tiny harness uses process-local FP state
and platform write/exit primitives. It does not validate a Windows-hosted plugin.
"""
import argparse
import os
from pathlib import Path
import re
import subprocess

COMMON = r'''
typedef unsigned long long uint64_t;
#define UINT64_C(x) x##ULL
#define UINT64_MAX (~0ULL)
void *memcpy(void *dst,const void *src,uint64_t n) {
  unsigned char *d=dst; const unsigned char *s=src;
  for(uint64_t i=0;i<n;++i)d[i]=s[i]; return dst;
}
static void message(const char *s);
static void number(uint64_t n) {
  char text[32]; unsigned pos=31; text[pos]=0;
  do { text[--pos]='0'+n%10; n/=10; } while(n);
  message(text+pos);
}
static void report(uint64_t n) { number(n);message(" comparisons passed\n"); }
#define stderr 0
#define fprintf(stream,format,name,mode,n,expected,actual) do { \
  message(name);message(" mode=");number(mode);message(" case=");number(n); \
  message(" expected=");number(expected);message(" actual=");number(actual); \
  message(" flags=");number(fetestexcept(FE_ALL_EXCEPT)); \
  message(" rounding=");number(fegetround());message("\n"); \
} while(0)
#define printf(format, count) report(count)
'''

WINDOWS = r'''
__declspec(dllimport) void *GetStdHandle(unsigned long);
__declspec(dllimport) int WriteFile(void *,const void *,unsigned long,unsigned long *,void *);
__declspec(dllimport) void ExitProcess(unsigned);
int _fltused;
static unsigned control(void) { unsigned v; __asm__ volatile("stmxcsr %0":"=m"(v));return v; }
static void setcontrol(unsigned v) { __asm__ volatile("ldmxcsr %0"::"m"(v):"memory"); }
#define FE_TONEAREST 0
#define FE_DOWNWARD (1<<13)
#define FE_UPWARD (2<<13)
#define FE_TOWARDZERO (3<<13)
#define FE_DIVBYZERO 4
#define FE_ALL_EXCEPT 63
static int fegetround(void) { return control()&(3<<13); }
static int fesetround(unsigned v) { setcontrol((control()&~(3<<13))|v);return 0; }
static int feclearexcept(int mask) { setcontrol(control()&~mask);return 0; }
static int feraiseexcept(int mask) { setcontrol(control()|mask);return 0; }
static int fetestexcept(int mask) { return control()&mask; }
static void message(const char *s) {
  unsigned long n=0,written;while(s[n])++n;
  WriteFile(GetStdHandle((unsigned long)-11),s,n,&written,0);
}
int main(void);
void mba_entry(void) {
  unsigned saved=control();int result=0;
  for(unsigned ftz=0;ftz<2&&!result;++ftz) {
    setcontrol((saved&~0x8040u)|(ftz?0x8040u:0));
    result=main();
  }
  setcontrol(saved);ExitProcess(result);
}
'''

ANDROID = r'''
#define FE_TONEAREST 0
#define FE_UPWARD (1<<22)
#define FE_DOWNWARD (2<<22)
#define FE_TOWARDZERO (3<<22)
#define FE_DIVBYZERO 2
#define FE_ALL_EXCEPT 31
static uint64_t control(void) { uint64_t v;__asm__ volatile("mrs %0,fpcr":"=r"(v));return v; }
static void setcontrol(uint64_t v) { __asm__ volatile("msr fpcr,%0"::"r"(v):"memory"); }
static uint64_t status(void) { uint64_t v;__asm__ volatile("mrs %0,fpsr":"=r"(v));return v; }
static void setstatus(uint64_t v) { __asm__ volatile("msr fpsr,%0"::"r"(v):"memory"); }
static int fegetround(void) { return control()&(3<<22); }
static int fesetround(unsigned v) { setcontrol((control()&~(3ULL<<22))|v);return 0; }
static int feclearexcept(int mask) { setstatus(status()&~(uint64_t)mask);return 0; }
static int feraiseexcept(int mask) { setstatus(status()|mask);return 0; }
static int fetestexcept(int mask) { return status()&mask; }
static void message(const char *s) {
  uint64_t n=0;while(s[n])++n;
  register long x0 __asm__("x0")=1;
  register const char *x1 __asm__("x1")=s;
  register uint64_t x2 __asm__("x2")=n;
  register long x8 __asm__("x8")=64;
  __asm__ volatile("svc #0":"+r"(x0):"r"(x1),"r"(x2),"r"(x8):"memory","cc");
}
int main(void);
int mba_entry(void) {
  uint64_t saved=control(), flags=status();int result=0;
  for(unsigned ftz=0;ftz<2&&!result;++ftz) {
    setcontrol((saved&~(1ULL<<24))|((uint64_t)ftz<<24));result=main();
  }
  setcontrol(saved);setstatus(flags);return result;
}
__asm__(".global _start\n.type _start,%function\n_start:\nbl mba_entry\nmov x8,#93\nsvc #0\n");
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--llvm-bin", type=Path, required=True)
    parser.add_argument("--plugin", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--target", choices=["windows", "android"], required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    suffix = ".exe" if os.name == "nt" else ""

    def run(name, *params, env=None):
        cmd = [str(args.llvm_bin / (name + suffix)), *map(str, params)]
        print(" ".join(cmd), flush=True)
        subprocess.run(cmd, env=env, check=True, timeout=180)

    windows = args.target == "windows"
    triple = "x86_64-pc-windows-msvc" if windows else "aarch64-unknown-linux-android24"
    for name in ["original", "reference"]:
        source = (args.fixtures / (name + ".ll")).read_text()
        source = re.sub(r'target triple = "[^"]+"', f'target triple = "{triple}"', source)
        (args.out / (name + ".ll")).write_text(source)
    driver = re.sub(r"^#include.*\n", "", (args.fixtures / "driver.c").read_text(), flags=re.M)
    (args.out / "driver.c").write_text(COMMON + (WINDOWS if windows else ANDROID) + driver)
    if windows:
        definition = args.out / "kernel32.def"
        definition.write_text("LIBRARY KERNEL32.dll\nEXPORTS\nGetStdHandle\nWriteFile\nExitProcess\n")
        run("llvm-dlltool", "-m", "i386:x86-64", "-d", definition, "-l", args.out / "kernel32.lib")
    for fp in ["true", "false"]:
        env = {k: v for k, v in os.environ.items() if not k.startswith("AMICE_")}
        env.update(AMICE_MBA="true", AMICE_MBA_FLOAT_REGIONS=fp, AMICE_PASS_ORDER="Mba")
        transformed = args.out / f"transformed-{fp}.ll"
        run("opt", f"--load-pass-plugin={args.plugin}", "-passes=default<O0>", "-verify-each", "-S",
            args.out / "original.ll", "-o", transformed, env=env)
        for level in ["O0", "O2"]:
            optimized = args.out / f"{fp}-{level}.ll"
            run("opt", f"-passes=default<{level}>,default<{level}>", "-verify-each", "-S", transformed, "-o", optimized)
            link = ["-Wl,/entry:mba_entry", "-Wl,/subsystem:console", str(args.out / "kernel32.lib")] if windows else ["-static", "-Wl,-e,_start"]
            run("clang", f"--target={triple}", "-fuse-ld=lld", "-nostdlib", "-ffreestanding",
                "-fno-stack-protector", "-fno-builtin", f"-{level}", optimized, args.out / "reference.ll",
                args.out / "driver.c", *link, "-o", args.out / f"mba-{fp}-{level}{'.exe' if windows else ''}")


if __name__ == "__main__":
    main()
