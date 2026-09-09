"""Small ELF64 symbol table for testing runtime preparation without LLVM installed."""

import struct


GLOBALS = (
    b"_ZN4llvm22KnownAssumptionStringsE",
    b"_ZN4llvm19DefaultDecisionSpecE",
    b"_ZN4llvm18InlineDecisionSpecE",
    b"_ZN4llvm10FeatureMapE",
)


def elf_runtime(symbols=GLOBALS, visibility=0, info=0x11):
    names = b"\0"
    entries = bytes(24)
    # Keep an unrelated analysis key to verify that preparation preserves it.
    for symbol in (*symbols, b"_ZN4llvm18TargetIRAnalysis3KeyE"):
        entries += struct.pack("<IBBHQQ", len(names), info, visibility, 1, 0, 24)
        names += symbol + b"\0"
    header = bytearray(64)
    header[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<HH", header, 16, 3, 62)
    struct.pack_into("<Q", header, 40, 64)
    struct.pack_into("<HHH", header, 58, 64, 3, 0)
    symbol_offset = 64 + 3 * 64
    sections = bytes(64)
    sections += struct.pack("<IIQQQQIIQQ", 0, 11, 0, 0, symbol_offset, len(entries), 2, 0, 8, 24)
    sections += struct.pack("<IIQQQQIIQQ", 0, 3, 0, 0, symbol_offset + len(entries), len(names), 0, 0, 1, 0)
    return header + sections + entries + names
