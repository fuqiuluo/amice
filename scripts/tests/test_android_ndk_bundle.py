"""Fast packaging regressions; real clang/Android coverage lives in the CI suite."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest

from elf_fixture import elf_runtime


PACKAGE_SCRIPT = Path(__file__).resolve().parents[1] / "package_android_ndk_bundle.sh"


class BundlePackagingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix="amice package test ")
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.root = Path(cls.temporary.name)
        cls.ndk = cls.root / "ndk"
        cls.llvm = cls.root / "llvm"
        toolchain = cls.ndk / "toolchains/llvm/prebuilt/linux-x86_64"
        (toolchain / "bin").mkdir(parents=True)
        (cls.ndk / "meta").mkdir()
        (cls.llvm / "bin").mkdir(parents=True)
        (cls.llvm / "lib").mkdir()
        (cls.ndk / "meta/platforms.json").write_text('{"min": 21, "max": 37}')
        (cls.ndk / "source.properties").write_text("Pkg.Revision = 30.0.16248370\n")
        for directory in (toolchain, cls.llvm):
            (directory / "AndroidVersion.txt").write_text("21.0.0\nbased on r574158c\n")
        clang = toolchain / "bin/clang"
        clang.write_text("#!/usr/bin/env python3\nimport json, sys\nprint(json.dumps(sys.argv[1:]))\n")
        clang.chmod(0o755)
        (toolchain / "bin/clang++").symlink_to("clang")
        config = cls.llvm / "bin/llvm-config"
        config.write_text(
            "#!/usr/bin/env bash\ncase $1 in\n"
            "--version) echo 21.0.0;;\n"
            f"--libdir) echo {shlex.quote(str(cls.llvm / 'lib'))};;\nesac\n"
        )
        config.chmod(0o755)
        runtime = cls.llvm / "lib/libLLVM.so.21"
        runtime.write_bytes(elf_runtime())
        (cls.llvm / "lib/libLLVM.so").symlink_to(runtime)
        cls.plugin = cls.root / "libamice.so"
        cls.plugin.write_bytes(b"test plugin")
        result = cls.package()
        if result.returncode:
            raise RuntimeError(result.stderr)
        extracted = cls.root / "relocated bundle"
        extracted.mkdir()
        subprocess.run(["tar", "-xzf", cls.root / "dist/amice-android-ndk-r30-linux-x86_64.tar.gz",
                        "-C", extracted], check=True)
        cls.bundle = extracted / "amice-android-ndk-r30-linux-x86_64"
        # A successful wrapper must not depend on the original NDK/LLVM location.
        shutil.rmtree(cls.root / "dist/.staging")

    @classmethod
    def package(cls, *extra):
        return subprocess.run([
            "bash", PACKAGE_SCRIPT, "--ndk-home", cls.ndk, "--llvm-home", cls.llvm,
            "--plugin", cls.plugin, "--ndk-release", "r30", "--llvm-feature", "llvm21-1",
            "--clang-revision", "r574158c", "--host-tag", "linux-x86_64",
            "--out-dir", cls.root / "dist", *extra,
        ], capture_output=True, text=True, timeout=30)

    def wrapper(self, name, *arguments, api=None):
        env = {key: value for key, value in os.environ.items() if not key.startswith("AMICE_")}
        if api is not None:
            env["AMICE_ANDROID_API"] = api
        result = subprocess.run([self.bundle / "amice/bin" / name, *arguments],
                                env=env, capture_output=True, text=True, check=True)
        return json.loads(result.stdout)

    def test_default_api_matches_ndk_minimum(self):
        for triple, api in (("aarch64-linux-android", 23), ("x86_64-linux-android", 23),
                            ("armv7a-linux-androideabi", 21), ("i686-linux-android", 21)):
            for suffix in ("clang", "clang++"):
                with self.subTest(triple=triple, suffix=suffix):
                    args = self.wrapper(f"{triple}-{suffix}", "file with spaces.c")
                    self.assertEqual(args[0], f"--target={triple}{api}")
                    self.assertEqual(args[-1], "file with spaces.c")
                    self.assertEqual(args[1], f"-fpass-plugin={self.bundle}/amice/lib/libamice.so")

    def test_api_override(self):
        args = self.wrapper("aarch64-linux-android-clang", api="35")
        self.assertEqual(args[0], "--target=aarch64-linux-android35")

    def test_explicit_target_takes_precedence(self):
        for arguments in (("--target=i686-linux-android21",),
                          ("--target", "i686-linux-android21"), ("-target", "i686-linux-android21")):
            with self.subTest(arguments=arguments):
                args = self.wrapper("aarch64-linux-android-clang", *arguments, api="35")
                self.assertEqual(args[1:], list(arguments))

    def test_generic_wrapper_has_no_default_target(self):
        args = self.wrapper("amice-clang++", "source.cpp")
        self.assertEqual(len(args), 2)
        self.assertEqual(args[-1], "source.cpp")

    def test_absolute_runtime_symlink_is_relocated(self):
        runtime = self.bundle / "amice/llvm-lib/libLLVM.so"
        self.assertFalse(os.path.isabs(os.readlink(runtime)))
        self.assertTrue(runtime.read_bytes().startswith(b"\x7fELF"))
        self.assertNotEqual(runtime.read_bytes(), elf_runtime())
        self.assertEqual((self.llvm / "lib/libLLVM.so").read_bytes(), elf_runtime())

    def test_mismatched_clang_revision_is_rejected(self):
        result = self.package("--clang-revision", "r563880c")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected Android clang r563880c", result.stderr)

    def test_mismatched_llvm_feature_is_rejected(self):
        result = self.package("--llvm-feature", "llvm22-1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match LLVM 21", result.stderr)

    def test_r30_beta_is_rejected(self):
        properties = self.ndk / "source.properties"
        original = properties.read_text()
        try:
            properties.write_text("Pkg.Revision = 30.0.14904198-beta1\n")
            result = self.package()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("requires the final NDK", result.stderr)
        finally:
            properties.write_text(original)


if __name__ == "__main__":
    unittest.main()
