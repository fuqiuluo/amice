from pathlib import Path
import sys
import tempfile
import unittest

from elf_fixture import GLOBALS, elf_runtime

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from prepare_android_llvm import prepare_runtime


class RuntimePreparationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.library = Path(temporary.name) / "libLLVM.so"

    def test_only_owning_global_visibilities_change(self):
        original = elf_runtime()
        self.library.write_bytes(original)
        self.assertEqual(prepare_runtime(self.library), 4)
        updated = self.library.read_bytes()
        offsets = [index for index, (before, after) in enumerate(zip(original, updated)) if before != after]
        self.assertEqual(offsets, [64 + 3 * 64 + index * 24 + 5 for index in range(1, 5)])
        self.assertTrue(all(updated[offset] == 3 for offset in offsets))
        self.assertEqual(prepare_runtime(self.library), 0)
        self.assertEqual(self.library.read_bytes(), updated)

    def test_missing_global_leaves_input_unchanged(self):
        original = elf_runtime(symbols=GLOBALS[:3])
        self.library.write_bytes(original)
        with self.assertRaisesRegex(ValueError, "missing"):
            prepare_runtime(self.library)
        self.assertEqual(self.library.read_bytes(), original)

    def test_unexpected_symbol_kind_leaves_input_unchanged(self):
        original = elf_runtime(info=0x12)
        self.library.write_bytes(original)
        with self.assertRaisesRegex(ValueError, "Unexpected definition"):
            prepare_runtime(self.library)
        self.assertEqual(self.library.read_bytes(), original)

    def test_duplicate_global_is_rejected(self):
        self.library.write_bytes(elf_runtime(symbols=(*GLOBALS, GLOBALS[0])))
        with self.assertRaisesRegex(ValueError, "Unexpected definition"):
            prepare_runtime(self.library)

    def test_invalid_or_truncated_elf_is_rejected(self):
        for original in (b"not ELF", elf_runtime()[:100], elf_runtime()[:-1]):
            with self.subTest(size=len(original)):
                self.library.write_bytes(original)
                with self.assertRaises(ValueError):
                    prepare_runtime(self.library)
                self.assertEqual(self.library.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
