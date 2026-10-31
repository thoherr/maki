//! Build script: link Clang's compiler runtime on macOS for the `ai` feature.
//!
//! The prebuilt ONNX Runtime archive that `ort-sys` (pyke binaries) links on
//! macOS contains Objective-C `@available` checks in its CoreML execution
//! provider (`host_utils.mm`, `model.mm`). Clang lowers those to calls of
//! `___isPlatformVersionAtLeast`, a symbol that lives in `libclang_rt.osx.a`,
//! not in `libSystem`. Rust links with `-nodefaultlibs`, so nothing pulls that
//! archive in by default. `ort-sys` 2.0.0-rc.13 added the link itself;
//! 2.0.0-rc.15 dropped it. Rust's std carries a *weak* definition that
//! satisfies plain builds, but under `lto = "thin"` (our release profile)
//! LTO discards it before the native ort objects ask for it, so
//! `cargo build --release --features ai-gpu` fails with
//! "Undefined symbols for architecture arm64: ___isPlatformVersionAtLeast".
//!
//! This script restores the link whenever the `ai` feature is on and the
//! target is macOS. Linking the static archive is harmless when nothing
//! references it (only the objects that are needed are pulled in).

use std::{env, path::Path, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");

    let target_is_macos = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let ai_enabled = env::var_os("CARGO_FEATURE_AI").is_some();
    if target_is_macos && ai_enabled {
        link_macos_compiler_rt();
    }
}

/// Emit the link directives for `libclang_rt.osx.a`, or a cargo warning when
/// the Clang resource directory cannot be determined.
fn link_macos_compiler_rt() {
    match macos_rtlib_dir() {
        Some(dir) => {
            println!("cargo:rustc-link-search=native={dir}");
            println!("cargo:rustc-link-lib=static=clang_rt.osx");
        }
        None => println!(
            "cargo:warning=could not locate libclang_rt.osx.a; the ai-gpu link may fail with \
             an undefined ___isPlatformVersionAtLeast (install the Xcode Command Line Tools)"
        ),
    }
}

/// `<clang resource dir>/lib/darwin`, where Apple's Clang ships
/// `libclang_rt.osx.a`. Tries `xcrun clang --print-resource-dir` first (the
/// toolchain Xcode selects), then `$CC --print-search-dirs` as ort-sys rc.13 did.
fn macos_rtlib_dir() -> Option<String> {
    if let Ok(output) = Command::new("xcrun")
        .args(["clang", "--print-resource-dir"])
        .output()
    {
        if output.status.success() {
            let resource_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let candidate = format!("{resource_dir}/lib/darwin");
            if Path::new(&candidate).join("libclang_rt.osx.a").is_file() {
                return Some(candidate);
            }
        }
    }

    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let output = Command::new(cc).arg("--print-search-dirs").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().find(|l| l.starts_with("libraries: ="))?;
    let base = line.trim_start_matches("libraries: =").split(':').next()?;
    let candidate = format!("{base}/lib/darwin");
    Path::new(&candidate)
        .join("libclang_rt.osx.a")
        .is_file()
        .then_some(candidate)
}
