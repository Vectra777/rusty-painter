fn main() {
    // `winit_pen`: the desktop Unix builds, where the patched winit reports a
    // tablet pen's pressure, on X11 and Wayland (see `src/tablet/mod.rs`).
    println!("cargo::rustc-check-cfg=cfg(winit_pen)");
    // `mobile`: Android and iOS, touch-first and sandboxed (no file dialogs,
    // no subprocesses; files come and go through the system's pickers).
    println!("cargo::rustc-check-cfg=cfg(mobile)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let unix =
        std::env::var("CARGO_CFG_TARGET_FAMILY").is_ok_and(|f| f.split(',').any(|f| f == "unix"));
    if matches!(os.as_str(), "android" | "ios") {
        println!("cargo::rustc-cfg=mobile");
    }
    // The .exe's icon, in Explorer and shortcuts (needs a Windows host).
    #[cfg(windows)]
    if os == "windows" {
        println!("cargo::rerun-if-changed=assets/icon/icon.ico");
        winresource::WindowsResource::new()
            .set_icon("assets/icon/icon.ico")
            .compile()
            .expect("embedding the .exe's icon");
    }
    if unix && !matches!(os.as_str(), "android" | "macos" | "ios") {
        println!("cargo::rustc-cfg=winit_pen");
    }
    embed_ibis_patterns();
}

/// `ibis_patterns.rs` in OUT_DIR: every picture in `assets/ibis/tips` and
/// `assets/ibis/papers` as `(folder, stem, bytes)`, for the ibisPaint
/// brush import (`src/brush_engine/import/ibis.rs`).
fn embed_ibis_patterns() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ibis");
    println!("cargo::rerun-if-changed={}", root.display());
    let mut out = String::from("&[\n");
    for folder in ["tips", "papers"] {
        let dir = root.join(folder);
        println!("cargo::rerun-if-changed={}", dir.display());
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.retain(|p| p.extension().is_some_and(|e| e == "png"));
        files.sort();
        for path in files {
            let stem = path.file_stem().unwrap().to_string_lossy();
            out += &format!(
                "    ({folder:?}, {stem:?}, include_bytes!({:?})),\n",
                path.display().to_string()
            );
        }
    }
    out += "]\n";
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("ibis_patterns.rs");
    std::fs::write(dest, out).expect("writing ibis_patterns.rs");
}
