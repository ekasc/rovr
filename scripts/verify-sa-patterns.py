#!/usr/bin/env python3
"""Offline regression guard for the SA payload's Dock symbol scan.

The payload locates Dock's private Space internals by byte-pattern scanning
Dock's mapped code, starting at a per-version offset. Those offsets and
patterns rot whenever Apple ships a new build of the SAME macOS major version
(verified: 27.0/26A428 and 27.2/26B5101f need different windows), which is
exactly what broke `rovr space create`.

This replays hex_find_seq's semantics -- byte-granular wildcards, whole-byte
wildcard when a token's first character is '?', and the 0x1286a0 scan cap --
against the Dock binary on THIS machine, then asserts every expected symbol
resolves inside its configured window. Run it after touching anything in
crates/rovr-sa-payload/vendor/.

Usage:  python3 scripts/verify-sa-patterns.py [--dock PATH]

Read-only against Dock. Exits non-zero on any regression so a stale offset
fails loudly instead of silently disabling spaces.
"""

import re
import subprocess
import sys
import tempfile
import os

# Must match hex_find_seq in crates/rovr-sa-payload/src/payload.m.
SCAN_CAP = 0x1286A0

DOCK_DEFAULT = "/System/Library/CoreServices/Dock.app/Contents/MacOS/Dock"

# pylint: disable=line-too-long
# Expected (name, build gate, offset, pattern) for build 26B5101f, mirroring
# the get_*_offset / get_*_pattern build-gated branches in vendor/arm64_payload.m.
# Add a row per supported build; this table is the contract the payload must
# satisfy on this machine.
EXPECTED_26B5101F = [
    (
        "dock_spaces",
        0x2F000,
        "?8 ?? ?? ?? 08 ?? ?? 91 00 01 40 F9 E2 03 13 AA ?? ?? ?? 94 ?? ?? ?? ?? 08",
    ),
    (
        "add_space",
        0x180000,
        "7F 23 03 D5 E1 03 1E AA ?? ?? ?? 97 FE 03 01 AA FD 7B 05 A9 FD 43 01 91 F3 03 14 AA F5 03",
    ),
    (
        "remove_space",
        0x120000,
        "7F 23 03 D5 FF ?? ?? D1 FC ?? ?? A9 FA ?? ?? A9 F8 ?? ?? A9 F6 ?? ?? A9 F4 ?? ?? A9 FD ?? ?? A9 FD ?? ?? 91 ?? 03 03 AA F5 03 02 AA F4 03 01 AA",
    ),
    (
        "move_space",
        0xE0000,
        "7F 23 03 D5 E3 03 1E AA ?? ?? ?? 97 FE 03 03 AA FD 7B ?? A9 FD ?? ?? 91 FF 03 01 D1 FA 03 02 AA FC 03 01 AA F6 03 00 AA",
    ),
    # dppm's access pattern changed structurally in 26B5101f and its
    # `moveSpace:toDisplay:displayUUID:` lookup no longer resolves, so reorder
    # stays honestly disabled. Reversing a new pattern requires a disassembler.
    (
        "dppm",
        0x50000,
        "?? ?? 00 ?? 08 ?? ?? 91 00 01 40 F9 E2 03 16 AA E3 03 19 AA ?? ?? ?? 94",
    ),
]


def parse_hex(hexstr):
    """Tokenise into (expected byte or None, is-wildcard) per payload rules."""
    want, wild = [], []
    for token in hexstr.split():
        if token[0] == "?":
            wild.append(True)
            want.append(0)
        else:
            wild.append(False)
            want.append(int(token, 16))
    return want, wild


def find_seq(data, hexstr, offset, cap=SCAN_CAP):
    """Byte-granular scan with a whole-byte wildcard rule, like hex_find_seq."""
    want, wild = parse_hex(hexstr)
    length = len(want)
    end = min(len(data), offset + cap) - length
    for i in range(offset, end + 1):
        if all(wild[k] or data[i + k] == want[k] for k in range(length)):
            return i
    return -1


def os_build():
    try:
        out = subprocess.check_output(["sw_vers", "-buildVersion"], text=True)
        return out.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return ""


def arm64e_slice(dock):
    """Extract the arm64e thin slice, the arch Dock loads and the payload sits in."""
    fd, path = tempfile.mkstemp(suffix=".arm64e")
    os.close(fd)
    try:
        subprocess.run(
            ["/usr/bin/lipo", "-thin", "arm64e", "-output", path, dock],
            check=True,
        )
        return path
    except subprocess.CalledProcessError:
        os.unlink(path)
        raise


def main():
    dock = DOCK_DEFAULT
    if len(sys.argv) > 2 and sys.argv[1] == "--dock":
        dock = sys.argv[2]

    build = os_build()
    print(f"Dock   : {dock}")
    print(f"build  : {build or 'unknown'}")
    if not build:
        print("cannot determine OS build; refusing to claim a pass")
        return 1

    slice_path = arm64e_slice(dock)
    try:
        with open(slice_path, "rb") as handle:
            data = handle.read()
    finally:
        os.unlink(slice_path)
    print(f"slice  : {len(data)} bytes of arm64e")

    if build != "26B5101f":
        print(
            f"no expectations recorded for build {build}; "
            "add a row to EXPECTED_26B5101F before trusting this"
        )

    # dppm is expected to stay unresolved until its pattern is re-derived.
    must_resolve = {"dock_spaces", "add_space", "remove_space", "move_space"}

    failures = 0
    for name, offset, pattern in EXPECTED_26B5101F:
        hit = find_seq(data, pattern, offset)
        window = (offset, min(len(data), offset + SCAN_CAP))
        if hit >= 0:
            print(
                f"  ok   {name:<13} window [{window[0]:#x},{window[1]:#x}) -> {hit:#x}"
            )
            if name not in must_resolve:
                # An unexpected resolution means someone fixed the pattern;
                # promote it out of the dppm gap.
                print(f"  note {name} now resolves; move it out of the known-gap list")
        else:
            required = name in must_resolve
            print(
                f"  {'FAIL' if required else 'none'} {name:<13} "
                f"window [{window[0]:#x},{window[1]:#x}) "
                f"{'expected to resolve but did not' if required else 'unresolved (known gap)'}"
            )
            if required:
                failures += 1

    if failures:
        print(f"\n{failures} symbol(s) failed to resolve; spaces will be disabled")
        return 1
    print("\nall required symbols resolve")
    return 0


if __name__ == "__main__":
    sys.exit(main())
