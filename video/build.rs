// Rust guideline compliant 2026-10-07
use std::{env, path::PathBuf};

fn main() {
    // Explicit opt-in native support uses the same linked libraries as Vivido.
    // Runtime DLL staging remains the executable's packaging responsibility.
    let names = ["libavcodec", "libavutil", "libswscale"];
    let detected = names.iter().all(|name| {
        pkg_config::Config::new()
            .cargo_metadata(false)
            .probe(name)
            .is_ok()
    });
    if detected {
        // Probe the whole set before emitting link metadata. Partial discovery
        // must not mix one installation with the Windows vcpkg fallback.
        for name in names {
            pkg_config::Config::new()
                .probe(name)
                .expect("FFmpeg library disappeared after discovery");
        }
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
            .expect("SDK video decoding requires FFmpeg via pkg-config or VCPKG_ROOT");
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
        for name in ["avcodec", "avutil", "swscale"] {
            assert!(
                lib.join(format!("{name}.lib")).is_file(),
                "FFmpeg import library missing"
            );
            println!("cargo:rustc-link-lib={name}");
        }
        println!("cargo:rustc-link-search=native={}", lib.display());
    } else {
        panic!(
            "SDK video decoding requires FFmpeg development libraries discoverable through pkg-config"
        );
    }
}
