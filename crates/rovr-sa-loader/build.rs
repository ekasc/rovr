//! Builds the Rovr SA loader binary with clang. macOS-only; no-op elsewhere.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let bin = out.join("rovr-sa-loader");
    println!("cargo:rerun-if-changed=src/loader.m");
    println!("cargo:cargo:loader={}", bin.display());

    let status = Command::new("clang")
        .args([
            "-O2",
            "-F/System/Library/PrivateFrameworks",
            "-framework",
            "Cocoa",
            // The loader writes arm64e thread states into Dock (arm64e) and
            // calls thread_create_running; building the loader itself as
            // arm64e keeps ptrauth semantics consistent (upstream does this).
            "-arch",
            "arm64e",
            "-o",
        ])
        .arg(&bin)
        .arg("src/loader.m")
        .status()
        .expect("run clang for rovr-sa-loader");
    if !status.success() {
        panic!("clang failed to build rovr-sa loader");
    }

    // Patch the Mach-O header CPU capabilities from PAC ABI v1 (caps 0x81)
    // to v0 (caps 0x80): modern clang stamps arm64e EXECUTABLES with ABI v1,
    // while system binaries like Dock.app still run ABI v0. The kernel
    // refuses thread_create_running into a v0 process from a v1 binary with
    // KERN_PROTECTION_FAILURE ("could not spawn remote thread") — upstream
    // yabai hit the identical failure (issues #2686/#2747). The payload
    // dylib already comes out v0; only the executable slice needs the patch.
    // Byte offsets per Razboy20's analysis in yabai#2686.
    let patch = Command::new("python3")
        .args([
            "-c",
            r#"
import sys
path = sys.argv[1]
with open(path, 'rb') as f:
    data = bytearray(f.read())
magic = int.from_bytes(data[0:4], 'little')
if magic == 0xfeedfacf:            # 64-bit Mach-O, thin
    if data[11] == 0x81:
        data[11] = 0x80
        print('patched caps 0x81 -> 0x80')
    else:
        print('caps already', hex(data[11]))
elif magic == 0xcafebabe:          # fat binary: patch every arm64e slice
    n = int.from_bytes(data[4:8], 'big')
    for i in range(n):
        base = 8 + i*20
        cputype = int.from_bytes(data[base+4:base+8], 'big')
        if cputype == 16777228 and data[base+7] == 0x81:
            data[base+7] = 0x80
            off = int.from_bytes(data[base+8:base+12], 'big')
            data[off+11] = 0x80
            print(f'patched fat slice {i} caps 0x81 -> 0x80')
else:
    sys.exit(f'unknown magic {hex(magic)}')
with open(path, 'wb') as f:
    f.write(data)
"#,
        ])
        .arg(&bin)
        .status()
        .expect("run python3 for loader caps patch");
    if !patch.success() {
        panic!("failed to patch loader Mach-O caps");
    }
}
