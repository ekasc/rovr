use std::env;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    // Embed an Info.plist into rovrd's __TEXT segment (yabai's approach): TCC
    // attributes Accessibility to the CFBundleIdentifier inside the binary,
    // which makes the grant survive rebuilds and apply under launchd.
    let plist = env::var("CARGO_MANIFEST_DIR")
        .map(|dir| format!("{}/Info.plist", dir))
        .expect("CARGO_MANIFEST_DIR is always set for build scripts");
    println!("cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist");
    println!("cargo:rustc-link-arg={}", plist);
}
