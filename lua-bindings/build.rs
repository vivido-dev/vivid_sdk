//! A Lua module resolves the Lua C API from the process that loads it, so nothing is linked here.
//! Linux shared objects allow undefined symbols by default; macOS has to be told.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-undefined");
        println!("cargo:rustc-cdylib-link-arg=dynamic_lookup");
    }
}
