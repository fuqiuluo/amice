#!/usr/bin/env python3
"""Isolate r30 Linux LLVM's owning globals from the statically linked NDK clang.

clang-r574158c exports these objects from both clang and libLLVM.so. ELF symbol
preemption makes both DSOs initialize and destroy clang's copy, causing a crash
even for an empty pass plugin. STV_PROTECTED binds libLLVM's references to its
own storage while retaining exported symbols, function interposition and LLVM
analysis keys. No code, symbol names, addresses or relocation entries change.

Only use on the matching r574158c libLLVM.so. Bundle packaging applies this to
its copied runtime; manual Linux builds can run it on their extracted AOSP LLVM.
"""

import argparse
from pathlib import Path
import struct


OWNING_GLOBALS = frozenset({
    b"_ZN4llvm22KnownAssumptionStringsE",
    b"_ZN4llvm19DefaultDecisionSpecE",
    b"_ZN4llvm18InlineDecisionSpecE",
    b"_ZN4llvm10FeatureMapE",
})


def prepare_runtime(path):
    data = path.read_bytes()
    if len(data) < 64 or data[:6] != b"\x7fELF\x02\x01":
        raise ValueError("Expected a little-endian ELF64 shared library")
    if struct.unpack_from("<HH", data, 16) != (3, 62):
        raise ValueError("Expected an x86_64 ELF shared library")
    section_offset = struct.unpack_from("<Q", data, 40)[0]
    section_size, section_count = struct.unpack_from("<HH", data, 58)
    if section_size != 64 or not section_count or section_offset + section_size * section_count > len(data):
        raise ValueError("Invalid ELF section table")
    sections = [struct.unpack_from("<IIQQQQIIQQ", data, section_offset + index * section_size)
                for index in range(section_count)]
    changes = []
    found = set()
    for section in sections:
        if section[1] != 11:  # SHT_DYNSYM
            continue
        offset, size, string_index, entry_size = section[4], section[5], section[6], section[9]
        if entry_size != 24 or size % entry_size or offset + size > len(data) or string_index >= section_count:
            raise ValueError("Invalid ELF dynamic symbol table")
        strings = sections[string_index]
        if strings[1] != 3 or strings[4] + strings[5] > len(data):
            raise ValueError("Invalid ELF string table")
        names = data[strings[4]:strings[4] + strings[5]]
        for position in range(offset, offset + size, entry_size):
            name, info, visibility, index, _, _ = struct.unpack_from("<IBBHQQ", data, position)
            end = names.find(b"\0", name)
            if end < 0:
                raise ValueError("Invalid ELF symbol name")
            symbol = names[name:end]
            if symbol not in OWNING_GLOBALS:
                continue
            if symbol in found or info != 0x11 or index == 0 or visibility & 3 not in (0, 3):
                raise ValueError(f"Unexpected definition for {symbol.decode()}")
            found.add(symbol)
            if visibility & 3 != 3:  # STV_PROTECTED
                changes.append((position + 5, (visibility & ~3) | 3))
    if found != OWNING_GLOBALS:
        missing = ", ".join(sorted(name.decode() for name in OWNING_GLOBALS - found))
        raise ValueError(f"Not the expected r574158c LLVM runtime; missing: {missing}")
    # Validate the entire input before changing any bytes. Repeated runs are safe.
    with path.open("r+b") as output:
        for position, value in changes:
            output.seek(position)
            output.write(bytes([value]))
    return len(changes)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", type=Path)
    args = parser.parse_args()
    print(f"Protected {prepare_runtime(args.library)} r30 LLVM owning globals in {args.library}")
