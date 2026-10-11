//! Regenerates `include/lactea.h` from the `extern "C"` surface of `src/lib.rs`.

use std::path::PathBuf;

fn main() {
    // Vulkan interop exists where wgpu has a Vulkan backend (not Apple): see src/vulkan.rs.
    println!("cargo:rustc-check-cfg=cfg(lactea_vulkan)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    let vendor = std::env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();
    if os == "windows" || (family == "unix" && vendor != "apple") {
        println!("cargo:rustc-cfg=lactea_vulkan");
    }
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let config = cbindgen::Config::from_file(dir.join("cbindgen.toml")).expect("cbindgen.toml");
    match cbindgen::Builder::new()
        .with_crate(&dir)
        .with_config(config)
        .generate()
    {
        Ok(bindings) => {
            std::fs::create_dir_all(dir.join("include")).unwrap();
            bindings.write_to_file(dir.join("include/lactea.h"));
        }
        Err(e) => panic!("cbindgen failed: {e}"),
    }
}
