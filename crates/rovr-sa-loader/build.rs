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
            "-o",
        ])
        .arg(&bin)
        .arg("src/loader.m")
        .status()
        .expect("run clang for rovr-sa-loader");
    if !status.success() {
        panic!("clang failed to build rovr-sa loader");
    }
}
