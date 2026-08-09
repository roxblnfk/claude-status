//! Delay-loading the libraries only the window needs.
//!
//! Windows resolves the whole import table before `main` runs. One binary now
//! serves both the window and the status line hook, and the hook — which
//! Claude Code runs on every assistant message — touches none of the graphics
//! stack. Marked delay-loaded, those libraries cost nothing until something
//! actually calls into them.

/// Imported by the window and by nothing on the hook's path.
const LAZY: &[&str] = &[
    "opengl32.dll",
    "dxgi.dll",
    "uiautomationcore.dll",
    "setupapi.dll",
    "comctl32.dll",
    "uxtheme.dll",
    "dwmapi.dll",
    "imm32.dll",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Only the MSVC linker has /DELAYLOAD; elsewhere the loader is lazy about
    // symbols anyway.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    println!("cargo:rustc-link-arg=delayimp.lib");
    for dll in LAZY {
        println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
    }
}
