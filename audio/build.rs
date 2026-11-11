// Rust guideline compliant 2026-10-07
use std::{env, path::PathBuf};

fn main() {
    // Explicit opt-in native support uses the same linked libraries as Vivido.
    // Runtime DLL staging remains the executable's packaging responsibility.
    if pkg_config::Config::new().probe("libswresample").is_ok() {
        return;
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        for name in [
            "VCPKG_ROOT",
            "VCPKG_DEFAULT_TRIPLET",
            "VCPKG_TARGET_TRIPLET",
        ] {
            println!("cargo:rerun-if-env-changed={name}");
        }
        let root = env::var_os("VCPKG_ROOT")
            .map(PathBuf::from)
            .expect("SDK audio decoding requires FFmpeg via pkg-config or VCPKG_ROOT");
        let triplet = env::var("VCPKG_TARGET_TRIPLET")
            .or_else(|_| env::var("VCPKG_DEFAULT_TRIPLET"))
            .unwrap_or_else(|_| {
                let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
                    Ok("x86_64") => "x64",
                    Ok("aarch64") => "arm64",
                    Ok("x86") => "x86",
                    _ => panic!("unsupported Windows FFmpeg architecture"),
                };
                format!("{arch}-windows")
            });
        let lib = root.join("installed").join(triplet).join("lib");
        assert!(
            lib.join("swresample.lib").is_file(),
            "FFmpeg import library missing"
        );
        println!("cargo:rustc-link-lib=swresample");
        println!("cargo:rustc-link-search=native={}", lib.display());
    } else {
        panic!(
            "SDK audio decoding requires FFmpeg development libraries discoverable through pkg-config"
        );
    }
}
