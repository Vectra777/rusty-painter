fn main() {
    // `x11_pen`: the desktop Unix builds, where the patched winit reports an
    // X11 tablet pen's pressure (see `src/tablet/mod.rs`).
    println!("cargo::rustc-check-cfg=cfg(x11_pen)");
    // `mobile`: Android and iOS, touch-first and sandboxed (no file dialogs,
    // no subprocesses; files come and go through the system's pickers).
    println!("cargo::rustc-check-cfg=cfg(mobile)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let unix =
        std::env::var("CARGO_CFG_TARGET_FAMILY").is_ok_and(|f| f.split(',').any(|f| f == "unix"));
    if matches!(os.as_str(), "android" | "ios") {
        println!("cargo::rustc-cfg=mobile");
    }
    if unix && !matches!(os.as_str(), "android" | "macos" | "ios") {
        println!("cargo::rustc-cfg=x11_pen");
    }
}
