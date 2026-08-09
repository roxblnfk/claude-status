//! Records the build target so self-update can pick the right release asset.
//!
//! Cargo tells a build script which triple it is building for, but does not
//! pass it on to the crate itself.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // `rust_i18n::i18n!` reads the translations in a proc macro, which Cargo
    // cannot see into: without this, editing a translation changes nothing
    // until something else forces the crate to be built again.
    println!("cargo:rerun-if-changed=../../locales/app.yml");
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=CLAUDE_STATUS_TARGET={target}");
}
