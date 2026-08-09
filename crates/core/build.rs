//! Ties the crate's rebuild to the translation files.
//!
//! `rust_i18n::i18n!` reads `locales/` at compile time and bakes the strings
//! into the binary, but cargo has no idea that dependency exists: without this
//! hint an edited translation is silently ignored until something else forces a
//! rebuild, and the UI keeps showing the previous text — or the raw key.

fn main() {
    println!("cargo:rerun-if-changed=../../locales");
}
