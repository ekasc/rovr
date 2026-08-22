//! Builds the Rovr SA payload dylib with clang. macOS-only; on other hosts
//! this is a no-op so `cargo check --workspace` works everywhere.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let dylib = out.join("librovr_sa_payload.dylib");
    println!("cargo:rerun-if-changed=src/payload.m");
    println!("cargo:rerun-if-changed=include/common.h");
    println!("cargo:rerun-if-changed=vendor/arm64_payload.m");
    println!("cargo:rerun-if-changed=vendor/x64_payload.m");
    println!("cargo:warning=rovr-sa payload dylib: {}", dylib.display());

    // Ad-hoc signature keeps the dylib loadable by the loader on arm64.
    let status = Command::new("clang")
        .args([
            "-dynamiclib",
            "-O2",
            "-Iinclude",
            "-framework",
            "Foundation",
            "-framework",
            "AppKit",
            "-framework",
            "CoreGraphics",
            "-framework",
            "CoreFoundation",
            "-F/System/Library/PrivateFrameworks",
            "-weak_framework",
            "SkyLight",
            "-o",
        ])
        .arg(&dylib)
        .arg("src/payload.m")
        .status()
        .expect("run clang for rovr-sa-payload");
    if !status.success() {
        panic!("clang failed to build rovr-sa payload dylib");
    }

    let codesign = Command::new("codesign")
        .args(["--force", "--sign", "-", &dylib.to_string_lossy()])
        .status();
    if codesign.map(|s| !s.success()).unwrap_or(true) {
        println!(
            "cargo:warning=codesign unavailable or failed; injection may require manual signing"
        );
    }
}
