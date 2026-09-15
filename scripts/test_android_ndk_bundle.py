#!/usr/bin/env python3
"""Compile Android fixtures with an extracted bundle; optionally run them via adb."""

import argparse
import json
import os
from pathlib import Path
import platform
import shlex
import struct
import subprocess
import tempfile
import uuid


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "crates/amice/tests/c/fixtures/android_ndk"
PARAM_AGGREGATE_FIXTURE = ROOT / "crates/amice/tests/c/fixtures/param_aggregate/basic.c"
TARGETS = {
    "arm64-v8a": ("aarch64-linux-android", 183, 2, 23),
    "armeabi-v7a": ("armv7a-linux-androideabi", 40, 1, 21),
    "x86_64": ("x86_64-linux-android", 62, 2, 23),
    "x86": ("i686-linux-android", 3, 1, 21),
}
MODES = {"O0": ["-O0"], "O2": ["-O2"], "thin": ["-O2", "-flto=thin"], "full": ["-O2", "-flto"]}
STRING_PROFILES = {
    "lazy-xor": {"AMICE_STRING_DECRYPT_TIMING": "lazy", "AMICE_STRING_ALGORITHM": "xor"},
    "global-xor": {"AMICE_STRING_DECRYPT_TIMING": "global", "AMICE_STRING_ALGORITHM": "xor"},
}


def run(command, **kwargs):
    result = subprocess.run(command, capture_output=True, text=True, timeout=180, **kwargs)
    if result.returncode:
        raise RuntimeError(f"Failed: {shlex.join(map(str, command))}\n{result.stdout}\n{result.stderr}")
    return result.stdout.strip()


def check_elf(path, abi):
    data = path.read_bytes()
    _, machine, elf_class, _ = TARGETS[abi]
    if data[:4] != b"\x7fELF" or data[4:6] != bytes([elf_class, 1]):
        raise RuntimeError(f"Not a little-endian Android {abi} ELF: {path}")
    if struct.unpack_from("<H", data, 18)[0] != machine:
        raise RuntimeError(f"Wrong target architecture: {path}")
    return data


def compile_suite(args):
    bundle = args.bundle.resolve()
    host = "darwin-x86_64" if platform.system() == "Darwin" else "linux-x86_64"
    ndk = bundle / f"android-ndk-{args.ndk_release}"
    toolchain = ndk / "toolchains/llvm/prebuilt" / host
    properties = dict(
        line.strip().split(" = ", 1)
        for line in (ndk / "source.properties").read_text().splitlines()
        if " = " in line
    )
    if args.ndk_release == "r30" and properties.get("Pkg.Revision") != "30.0.16248370":
        raise RuntimeError("Expected final NDK r30 (30.0.16248370)")
    version = run([toolchain / "bin/clang", "--version"])
    if f"based on {args.clang_revision})" not in version:
        raise RuntimeError(f"Wrong Android clang revision: {version}")
    print(version, flush=True)
    for path in (bundle / "amice/llvm-lib").iterdir():
        if path.is_symlink() and (os.path.isabs(os.readlink(path)) or not path.exists()):
            raise RuntimeError(f"Non-relocatable runtime symlink: {path}")

    # Do not let the build machine's libLLVM or user pass settings mask bundle defects.
    env = {
        key: value for key, value in os.environ.items()
        if not key.startswith("AMICE_") and key not in ("LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH")
    }
    env["AMICE_STRING_ENCRYPTION"] = "true"
    env["AMICE_PARAM_AGGREGATE"] = "true"
    minimal = args.output_dir / "load-plugin.c"
    minimal.write_text("int main(void) { return 0; }\n")
    run([bundle / "amice/bin/aarch64-linux-android-clang", minimal,
         "-o", args.output_dir / "load-plugin"],
        env={**env, "AMICE_STRING_ENCRYPTION": "false", "AMICE_PARAM_AGGREGATE": "false"})
    print("PASS plugin loading and process shutdown with all passes disabled", flush=True)

    param_ir = args.output_dir / "param-aggregate.ll"
    run([bundle / "amice/bin/aarch64-linux-android-clang", "-O2", "-S", "-emit-llvm",
         PARAM_AGGREGATE_FIXTURE, "-o", param_ir],
        env={**env, "AMICE_STRING_ENCRYPTION": "false"})
    if "param.aggregate" not in param_ir.read_text():
        raise RuntimeError("ParamAggregate was enabled but did not create an Android aggregate ABI wrapper")
    print("PASS Android ParamAggregate creates an aggregate ABI wrapper", flush=True)

    records = []
    for abi in args.abi:
        triple, _, _, api = TARGETS[abi]
        for language, suffix in (("c", ""), ("cpp", "++")):
            source = FIXTURES / f"smoke.{language}"
            marker = f"AMICE_NDK_{language.upper()}_MARKER".encode()
            for profile, profile_env in STRING_PROFILES.items():
                for mode, flags in MODES.items():
                    stem = f"{abi}-{language}-{profile}-{mode}"
                    baseline = args.output_dir / f"{stem}-baseline"
                    protected = args.output_dir / f"{stem}-protected"
                    common = [*flags, "-Werror=unused-command-line-argument"]
                    if language == "cpp":
                        common += ["-std=c++17", "-static-libstdc++"]
                    run([toolchain / f"bin/clang{suffix}", f"--target={triple}{api}",
                         *common, source, "-o", baseline], env=env)
                    wrapper = bundle / f"amice/bin/{triple}-clang{suffix}"
                    run([wrapper, *common, source, "-o", protected], env={**env, **profile_env})
                    if marker not in check_elf(baseline, abi):
                        raise RuntimeError(f"Baseline lost the marker: {baseline}")
                    if marker in check_elf(protected, abi):
                        raise RuntimeError(f"String encryption did not hide the marker: {protected}")
                    records.append({"abi": abi, "language": language, "profile": profile,
                                    "baseline": baseline.name, "protected": protected.name})
                    print(f"PASS {stem}: compile, link, ELF target and string encryption", flush=True)

            shared = args.output_dir / f"{abi}-{language}.so"
            shared_flags = ["-std=c++17", "-static-libstdc++"] if language == "cpp" else []
            run([wrapper, "-O2", "-fPIC", "-shared", "-Wl,--no-undefined",
                 *shared_flags, source, "-o", shared], env=env)
            if marker in check_elf(shared, abi):
                raise RuntimeError(f"Shared library retained plaintext: {shared}")
            print(f"PASS {abi}-{language}: shared library", flush=True)

    # Generic wrappers must honor an explicit target, and an explicit target must
    # take precedence over the ABI wrapper's default (including its API override).
    for wrapper_name in ("amice-clang", "aarch64-linux-android-clang"):
        for target_flags in (["--target=i686-linux-android21"], ["-target", "i686-linux-android21"]):
            output = args.output_dir / "target-override"
            run([bundle / "amice/bin" / wrapper_name, *target_flags,
                 FIXTURES / "smoke.c", "-o", output], env={**env, "AMICE_ANDROID_API": "23"})
            check_elf(output, "x86")
    (args.output_dir / "manifest.json").write_text(json.dumps(records, indent=2) + "\n")
    print(f"PASS {len(records)} executable pairs, shared libraries and target overrides", flush=True)


def runtime_suite(args):
    adb = ["adb", "-s", args.adb_serial]
    device_abis = run([*adb, "shell", "getprop", "ro.product.cpu.abilist"]).split(",")
    # Emulator abilists may advertise ARM translation for APKs; adb shell cannot
    # execute those ELF files. Restrict standalone tests to the kernel's ISA.
    machine = run([*adb, "shell", "uname", "-m"])
    native_abis = {
        "aarch64": {"arm64-v8a", "armeabi-v7a"},
        "armv7l": {"armeabi-v7a"},
        "armv8l": {"armeabi-v7a"},
        "x86_64": {"x86_64", "x86"},
        "i686": {"x86"},
        "i386": {"x86"},
    }.get(machine, set())
    records = json.loads((args.output_dir / "manifest.json").read_text())
    runnable = native_abis.intersection(device_abis, args.abi)
    records = [record for record in records if record["abi"] in runnable]
    if not records:
        raise RuntimeError(f"No test binaries match device ABIs: {device_abis}")
    remote = f"/data/local/tmp/amice-ndk-test-{uuid.uuid4().hex}"
    run([*adb, "shell", "mkdir", remote])
    try:
        for record in records:
            for kind in ("baseline", "protected"):
                name = record[kind]
                run([*adb, "push", args.output_dir / name, f"{remote}/{name}"])
                run([*adb, "shell", "chmod", "700", f"{remote}/{name}"])
            for value in (0, 1, 7, 42, 255):
                transformed = ((value * 7) ^ 0x55) if value & 1 else ((value + 11) ^ 0x33)
                expected = f"AMICE_NDK_{record['language'].upper()}_MARKER\n{transformed + 17}"
                if record["language"] == "cpp":
                    expected += ":6:2"
                for kind in ("baseline", "protected"):
                    actual = run([*adb, "shell", f"{remote}/{record[kind]}", str(value)]).replace("\r\n", "\n")
                    if actual != expected:
                        raise RuntimeError(f"Incorrect runtime output for {record[kind]}({value}): {actual!r}")
            print(f"PASS runtime {record['protected']}: 5 inputs match baseline and expected output", flush=True)
    finally:
        run([*adb, "shell", "rm", "-rf", remote])
    print(f"PASS Android runtime: {len(records)} pairs, {len(records) * 10} executions", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--ndk-release", default="r30")
    parser.add_argument("--clang-revision", default="r574158c")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--abi", nargs="+", choices=TARGETS, default=list(TARGETS))
    parser.add_argument("--adb-serial", help="Run compatible binaries on this device or emulator")
    parser.add_argument("--runtime-only", action="store_true", help="Run previously compiled output-dir/manifest.json")
    args = parser.parse_args()
    if args.runtime_only and (not args.adb_serial or not args.output_dir):
        parser.error("--runtime-only requires --adb-serial and --output-dir")
    if not args.runtime_only and not args.bundle:
        parser.error("--bundle is required for compilation")
    with tempfile.TemporaryDirectory(prefix="amice-ndk-tests-") as temporary:
        args.output_dir = (args.output_dir or Path(temporary)).resolve()
        args.output_dir.mkdir(parents=True, exist_ok=True)
        if not args.runtime_only:
            compile_suite(args)
        if args.adb_serial:
            runtime_suite(args)


if __name__ == "__main__":
    main()
