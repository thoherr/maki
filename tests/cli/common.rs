//! Helpers shared by every CLI test module; each module starts with
//! `use crate::common::*;`.

pub use std::path::{Path, PathBuf};

pub use assert_cmd::cargo::cargo_bin_cmd;
pub use assert_cmd::Command;
pub use predicates::prelude::*;
pub use tempfile::tempdir;

/// Return a Command for the `maki` binary.
pub fn maki() -> Command {
    cargo_bin_cmd!(assert_cmd::pkg_name!())
}

/// Initialize a catalog and register a volume pointing at `dir`.
/// Returns the canonical path (needed on macOS where /var -> /private/var)
/// so that volume lookup matches canonicalized import paths.
pub fn init_catalog(dir: &Path) -> PathBuf {
    let canonical = dir.canonicalize().expect("canonicalize tempdir");
    maki().current_dir(&canonical).arg("init").assert().success();
    maki()
        .current_dir(&canonical)
        .args(["volume", "add", "test-vol", canonical.to_str().unwrap()])
        .assert()
        .success();
    canonical
}

/// Write a small file and return its path.
pub fn create_test_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    path
}

/// Count files in the previews directory (two levels: shard dirs containing preview files).
pub fn count_preview_files(previews_dir: &Path) -> usize {
    let mut count = 0;
    if let Ok(shards) = std::fs::read_dir(previews_dir) {
        for shard in shards.flatten() {
            if shard.path().is_dir() {
                if let Ok(files) = std::fs::read_dir(shard.path()) {
                    for file in files.flatten() {
                        if file.path().is_file() {
                            count += 1;
                        }
                    }
                }
            }
        }
    }
    count
}

/// Initialize a catalog with two volumes: vol1 and vol2.
/// Returns (canonical root, vol1 path, vol2 path).
pub fn init_two_volumes(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let canonical = dir.canonicalize().expect("canonicalize tempdir");
    maki().current_dir(&canonical).arg("init").assert().success();

    let vol1 = canonical.join("vol1");
    let vol2 = canonical.join("vol2");
    std::fs::create_dir_all(&vol1).unwrap();
    std::fs::create_dir_all(&vol2).unwrap();

    maki()
        .current_dir(&canonical)
        .args(["volume", "add", "vol1", vol1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&canonical)
        .args(["volume", "add", "vol2", vol2.to_str().unwrap()])
        .assert()
        .success();

    (canonical, vol1, vol2)
}

/// Lowercase-hex encode a digest (digest 0.11 dropped LowerHex on the output array).
pub fn sha256_hex_of(digest: &[u8]) -> String {
    use std::fmt::Write;
    digest.iter().fold(String::with_capacity(64), |mut s, b| {
        write!(s, "{b:02x}").unwrap();
        s
    })
}

/// Helper: import files and return their asset IDs.
pub fn import_and_get_ids(root: &Path, names: &[&str]) -> Vec<String> {
    for name in names {
        create_test_file(root, name, name.as_bytes());
        maki()
            .current_dir(root)
            .args(["import", root.join(name).to_str().unwrap()])
            .assert()
            .success();
    }
    let mut ids = Vec::new();
    for name in names {
        let stem = Path::new(name).file_stem().unwrap().to_str().unwrap();
        let output = maki()
            .current_dir(root)
            .args(["search", "--format", "ids", stem])
            .output()
            .unwrap();
        let id = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .to_string();
        assert!(!id.is_empty(), "asset not found for {name}");
        ids.push(id);
    }
    ids
}

/// Build a minimal JPEG with an XMP APP1 marker carrying `xmp_xml`.
/// Mirrors `embedded_xmp::tests::build_jpeg_with_xmp`, which integration
/// tests cannot reach.
pub fn build_jpeg_with_xmp(xmp_xml: &str) -> Vec<u8> {
    const XMP_NAMESPACE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    let mut data = Vec::new();
    data.extend_from_slice(&[0xFF, 0xD8]); // SOI
    data.extend_from_slice(&[0xFF, 0xE1]); // APP1
    let payload_len = XMP_NAMESPACE.len() + xmp_xml.len();
    let segment_len = (payload_len + 2) as u16;
    data.extend_from_slice(&segment_len.to_be_bytes());
    data.extend_from_slice(XMP_NAMESPACE);
    data.extend_from_slice(xmp_xml.as_bytes());
    data.extend_from_slice(&[0xFF, 0xD9]); // EOI
    data
}

/// `--json show <id>` parsed into a JSON value.
pub fn show_json(root: &Path, id: &str) -> serde_json::Value {
    let out = maki()
        .current_dir(root)
        .args(["--json", "show", id])
        .output()
        .unwrap();
    assert!(out.status.success(), "show --json failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("show --json is valid JSON")
}

/// Path of the YAML sidecar for a full asset UUID.
pub fn sidecar_path(root: &Path, id: &str) -> PathBuf {
    root.join("metadata").join(&id[..2]).join(format!("{id}.yaml"))
}
