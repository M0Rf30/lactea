//! Builds the C examples against the C ABI library and runs them:
//!
//! * `examples/c/minimal.c`: 10 frames through `lactea_render_to_buffer`, last frame not black;
//! * `examples/c/vulkan.c`: a Vulkan host that creates its own `VkDevice`/`VkImage`, lets lactea
//!   render into the image (`lactea_create_vulkan`, `lactea_render_to_vk_image`) and reads it back.
//!
//! Skipped (with a note) without a GPU adapter, a C compiler, or (Vulkan example) the Vulkan
//! headers / loader. Headers outside the default include path: `VULKAN_INCLUDE_DIR=/path/include`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `target/<profile>`: the test binary lives in `target/<profile>/deps`.
fn profile_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().to_path_buf()
}

fn have_gpu() -> bool {
    let instance = wgpu::Instance::default();
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).is_ok()
}

fn lib_present(dir: &Path) -> bool {
    ["so", "dylib"]
        .iter()
        .any(|e| dir.join(format!("liblactea_capi.{e}")).exists())
}

/// The directory holding the shared library, (re)built first: cargo does not build the cdylib for
/// integration tests, and a library left over from an older build would link stale symbols.
fn library_dir() -> PathBuf {
    let libdir = profile_dir();
    // The outer cargo no longer holds the build lock while tests run; the build is incremental.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["build", "-p", "lactea-capi", "--target-dir"])
        .arg(libdir.parent().unwrap());
    if libdir.file_name().is_some_and(|n| n == "release") {
        cmd.arg("--release");
    }
    let st = cmd.status().expect("spawn cargo build");
    assert!(st.success(), "cargo build -p lactea-capi failed");
    assert!(
        lib_present(&libdir),
        "liblactea_capi shared library not found in {}",
        libdir.display()
    );
    libdir
}

/// Compile `examples/c/<name>.c` into `<libdir>/lactea_capi_<name>_c`. `Ok(None)`: no C compiler.
fn compile(
    name: &str,
    libdir: &Path,
    extra: &[String],
) -> Result<Option<(PathBuf, Output)>, String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = libdir.join(format!("lactea_capi_{name}_c"));
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let build = Command::new(&cc)
        .arg(manifest.join(format!("examples/c/{name}.c")))
        .arg("-I")
        .arg(manifest.join("include"))
        .arg("-L")
        .arg(libdir)
        .args(["-llactea_capi", "-lm", "-Wall", "-Werror"])
        .args(extra)
        .arg(format!("-Wl,-rpath,{}", libdir.display()))
        .arg("-o")
        .arg(&out)
        .output();
    match build {
        Ok(b) => Ok(Some((out, b))),
        Err(e) => {
            eprintln!("skipping: cannot run C compiler `{cc}`: {e}");
            Ok(None)
        }
    }
}

fn run_ok(exe: &Path, args: &[&str]) {
    let run = Command::new(exe).args(args).output().expect("run example");
    assert!(
        run.status.success(),
        "{} exited with {}:\nstdout: {}\nstderr: {}",
        exe.display(),
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    print!("{}", String::from_utf8_lossy(&run.stdout));
}

#[test]
fn c_example_renders_non_black_frames() {
    if !have_gpu() {
        eprintln!("skipping: no wgpu adapter");
        return;
    }
    let libdir = library_dir();
    let Some((exe, build)) = compile("minimal", &libdir, &[]).unwrap() else {
        return;
    };
    assert!(
        build.status.success(),
        "C compile failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    run_ok(&exe, &[]);
}

#[test]
fn vulkan_host_renders_into_its_own_image() {
    if !have_gpu() {
        eprintln!("skipping: no wgpu adapter");
        return;
    }
    let libdir = library_dir();
    let mut extra = vec![
        "-lvulkan".to_string(),
        "-Wno-missing-field-initializers".to_string(),
    ];
    if let Ok(dir) = std::env::var("VULKAN_INCLUDE_DIR") {
        extra.push(format!("-I{dir}"));
    }
    let Some((exe, build)) = compile("vulkan", &libdir, &extra).unwrap() else {
        return;
    };
    let stderr = String::from_utf8_lossy(&build.stderr);
    if !build.status.success() {
        if stderr.contains("vulkan/vulkan.h") || stderr.contains("-lvulkan") {
            eprintln!(
                "skipping: Vulkan headers or loader not installed (VULKAN_INCLUDE_DIR?):\n{stderr}"
            );
            return;
        }
        panic!("C compile failed:\n{stderr}");
    }
    // The host picks the first Vulkan device the loader offers; exit status 2 = none.
    let run = Command::new(&exe).output().expect("run vulkan example");
    if run.status.code() == Some(2) {
        eprintln!("skipping: {}", String::from_utf8_lossy(&run.stderr));
        return;
    }
    run_ok(&exe, &[]);
    run_ok(&exe, &["bgra"]);
}
