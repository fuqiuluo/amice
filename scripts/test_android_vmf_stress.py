#!/usr/bin/env python3
"""Compile and run BCF+VMF shared libraries against an unprotected reference.

The wide switch exercises several targets in each of the four target shards;
the driver also checks loop-carried values and recursive invocation frames.
Use --runtime-only on a Windows adb host with copied build artifacts.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import uuid

from test_android_ndk_bundle import FIXTURES, MODES, PROFILES, TARGETS, check_elf, run


STRESS_MODES = {**MODES, "O3": ["-O3"]}
EXPECTED = "PASS VMF shared 4608 comparisons"


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def compile_suite(args):
    bundle = args.bundle.resolve()
    host = "darwin-x86_64" if platform.system() == "Darwin" else "linux-x86_64"
    ndk = bundle / f"android-ndk-{args.ndk_release}/toolchains/llvm/prebuilt" / host
    compiler = ndk / "bin/clang"
    source = FIXTURES / "vmf_stress.c"
    driver = FIXTURES / "vmf_stress_driver.c"
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("AMICE_") and key not in ("LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH")}
    protected_env = {
        **env, **PROFILES["bcf-vmf"],
        "AMICE_STRING_ENCRYPTION": "false", "AMICE_PARAM_AGGREGATE": "false",
        "AMICE_PASS_ORDER": "VmFlatten,BogusControlFlow",
        "AMICE_BOGUS_CONTROL_FLOW_MAX_REGIONS": "2",
        "AMICE_BOGUS_CONTROL_FLOW_MAX_REGION_INSTRUCTIONS": "8",
    }
    records = []
    for abi in args.abi:
        triple, _, _, api = TARGETS[abi]
        target = f"--target={triple}{api}"
        wrapper = bundle / f"amice/bin/{triple}-clang"
        reference = args.output_dir / f"{abi}-reference.o"
        run([compiler, target, "-O2", "-DVMF_REFERENCE", "-c", source, "-o", reference], env=env)
        probe = args.output_dir / f"{abi}-stress.ll"
        run([wrapper, "-O0", "-S", "-emit-llvm", source, "-o", probe],
            env={**protected_env, "AMICE_VM_FLATTEN_SEED": "0"})
        text = probe.read_text()
        destinations = [line.count("label %") for line in text.splitlines() if "indirectbr " in line]
        if not destinations or max(destinations) < 16 or '"amice.bcf.done"' not in text:
            raise RuntimeError(f"Wide switch BCF+VMF was skipped: {probe}")
        for seed, max_ops in ((0, 1), (42, 32)):
            for mode, flags in STRESS_MODES.items():
                stem = f"{abi}-vmf-{seed}-{mode}"
                library = args.output_dir / f"lib{stem}.so"
                executable = args.output_dir / stem
                run([wrapper, *flags, "-fPIC", "-shared", "-Wl,--no-undefined",
                     f"-Wl,-soname,{library.name}", source, "-o", library],
                    env={**protected_env, "AMICE_VM_FLATTEN_SEED": str(seed),
                         "AMICE_VM_FLATTEN_MAX_OPS": str(max_ops)})
                # The driver/reference use the stock NDK compiler. Only the
                # separately loaded .so is transformed by the plugin.
                run([compiler, target, "-O2", driver, reference, library,
                     "-Wl,-rpath,$ORIGIN", "-o", executable], env=env)
                for path in (library, executable):
                    check_elf(path, abi)
                records.append({"abi": abi, "seed": seed, "max_ops": max_ops, "mode": mode,
                                "library": library.name, "executable": executable.name,
                                "library_sha256": sha256(library), "executable_sha256": sha256(executable)})
                print(f"PASS compile VMF shared {stem}", flush=True)
    extension = "dylib" if platform.system() == "Darwin" else "so"
    manifest = {"compiler": run([compiler, "--version"], env=env),
                "plugin_sha256": sha256(bundle / f"amice/lib/libamice.{extension}"),
                "source_sha256": sha256(source), "driver_sha256": sha256(driver), "records": records}
    (args.output_dir / "vmf-stress-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"PASS VMF shared compilation: {len(records)} libraries and differential drivers", flush=True)


def runtime_suite(args):
    adb = ["adb", "-s", args.adb_serial]
    available = set(run([*adb, "shell", "getprop", "ro.product.cpu.abilist"]).split(","))
    machine = run([*adb, "shell", "uname", "-m"])
    native = {"aarch64": {"arm64-v8a", "armeabi-v7a"}, "armv7l": {"armeabi-v7a"},
              "armv8l": {"armeabi-v7a"}, "x86_64": {"x86_64", "x86"},
              "i686": {"x86"}, "i386": {"x86"}}.get(machine, set())
    manifest = json.loads((args.output_dir / "vmf-stress-manifest.json").read_text())
    records = [r for r in manifest["records"] if r["abi"] in native.intersection(available, args.abi)]
    if not records:
        raise RuntimeError(f"No native VMF stress binaries for {machine}: {available}")
    remote = f"/data/local/tmp/amice-vmf-stress-{uuid.uuid4().hex}"
    run([*adb, "shell", "mkdir", remote])
    try:
        for record in records:
            for kind in ("library", "executable"):
                name = record[kind]
                if not re.fullmatch(r"[-a-zA-Z0-9_.]+", name):
                    raise RuntimeError(f"Unexpected artifact name: {name!r}")
                path = args.output_dir / name
                if sha256(path) != record[f"{kind}_sha256"]:
                    raise RuntimeError(f"Artifact changed since compilation: {path}")
                run([*adb, "push", path, f"{remote}/{name}"])
                run([*adb, "shell", "chmod", "700", f"{remote}/{name}"])
            actual = run([*adb, "shell", f"{remote}/{record['executable']}"])
            if actual != EXPECTED:
                raise RuntimeError(f"VMF shared runtime mismatch: {record}: {actual!r}")
            print(f"PASS runtime {record['executable']}: 4608 comparisons", flush=True)
    finally:
        run([*adb, "shell", "rm", "-rf", remote])
    print(f"PASS VMF shared runtime: {len(records)} libraries, {len(records) * 4608} comparisons", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--ndk-release", default="r30")
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--abi", nargs="+", choices=TARGETS, default=list(TARGETS))
    parser.add_argument("--adb-serial")
    parser.add_argument("--runtime-only", action="store_true")
    args = parser.parse_args()
    if args.runtime_only and not args.adb_serial:
        parser.error("--runtime-only requires --adb-serial")
    if not args.runtime_only and not args.bundle:
        parser.error("compilation requires --bundle")
    args.output_dir = args.output_dir.resolve()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    if not args.runtime_only:
        compile_suite(args)
    if args.adb_serial:
        runtime_suite(args)


if __name__ == "__main__":
    main()
