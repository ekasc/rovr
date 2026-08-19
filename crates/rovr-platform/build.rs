fn main() {
    println!("cargo:rerun-if-changed=src/macos/bridge.m");
    println!("cargo:rerun-if-changed=src/macos/bridge.h");

    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() == Some("macos") {
        cc::Build::new()
            .file("src/macos/bridge.m")
            .flag("-fobjc-arc")
            .compile("rovr_macos_bridge");

        println!("cargo:rustc-link-lib=framework=ApplicationServices");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
    }
}
