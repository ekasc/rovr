//! Builds the Rovr SA privileged helper binary with clang. macOS-only;
//! no-op elsewhere.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let bin = out.join("rovr-sa-helper");
    println!("cargo:rerun-if-changed=src/helper.m");
    println!("cargo:rerun-if-changed=include/helper.h");

    let status = Command::new("clang")
        .args(["-O2", "-Wall", "-I", "include", "-framework", "Cocoa", "-o"])
        .arg(&bin)
        .arg("src/helper.m")
        .status()
        .expect("run clang for rovr-sa-helper");
    if !status.success() {
        panic!("clang failed to build rovr-sa-helper");
    }
}
