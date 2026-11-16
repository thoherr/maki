//! `generate-previews`, `preview` and preview selection.

use crate::common::*;

#[test]
fn generate_previews_command_runs() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "preview_test.jpg", b"preview data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .arg("generate-previews")
        .assert()
        .success()
        .stdout(predicate::str::contains("preview(s)"));
}

#[test]
fn show_displays_preview_status() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "preview_show.jpg", b"show preview data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "preview_show"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Preview:"));
}

#[test]
fn generate_previews_with_volume_filter() {
    let dir = tempdir().unwrap();
    let (root, vol1, vol2) = init_two_volumes(dir.path());

    create_test_file(&vol1, "vol1_photo.jpg", b"vol1 preview data");
    create_test_file(&vol2, "vol2_photo.jpg", b"vol2 preview data");

    // Import both
    maki()
        .current_dir(&root)
        .args(["import", vol1.join("vol1_photo.jpg").to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", vol2.join("vol2_photo.jpg").to_str().unwrap()])
        .assert()
        .success();

    // Generate previews only for vol1
    maki()
        .current_dir(&root)
        .args(["generate-previews", "--volume", "vol1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("preview(s)"));
}

#[test]
fn generate_previews_with_paths() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let sub = root.join("photos");
    std::fs::create_dir_all(&sub).unwrap();
    create_test_file(&sub, "path_test.jpg", b"path preview data");

    // Import first
    maki()
        .current_dir(&root)
        .args(["import", sub.to_str().unwrap()])
        .assert()
        .success();

    // Generate previews using PATHS mode
    maki()
        .current_dir(&root)
        .args(["generate-previews", sub.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("preview(s)"));
}

#[test]
fn generate_previews_log_shows_per_file_progress() {
    use image::{ImageBuffer, Rgb};

    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Create a real 1x1 PNG so preview generation succeeds
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(1, 1, Rgb([255, 0, 0]));
    let img_path = root.join("log_test.png");
    img.save(&img_path).unwrap();

    maki()
        .current_dir(&root)
        .args(["import", img_path.to_str().unwrap()])
        .assert()
        .success();

    // With --log, per-file progress appears on stderr
    maki()
        .current_dir(&root)
        .args(["--log", "generate-previews"])
        .assert()
        .success()
        .stderr(predicate::str::contains("log_test.png"));

    // Without --log, no per-file output on stderr
    maki()
        .current_dir(&root)
        .args(["generate-previews"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
}

/// Helper: compute the SHA-256 hex of some content (matches maki's content_hash minus "sha256:" prefix).
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Sha256, Digest};
    sha256_hex_of(&Sha256::digest(data))
}

#[test]
fn show_preview_prefers_export_variant() {
    use image::{ImageBuffer, Rgb};

    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Create a RAW + real JPG pair with the same stem — auto-groups with JPG as export
    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    let raw_content = b"raw image data for preview test nef";
    create_test_file(&photos, "DSC_900.nef", raw_content);

    // Create a real 1x1 JPEG so preview generation succeeds for the JPG
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(1, 1, Rgb([0, 255, 0]));
    let jpg_path = photos.join("DSC_900.jpg");
    img.save(&jpg_path).unwrap();
    let jpg_content = std::fs::read(&jpg_path).unwrap();

    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success();

    // Get asset ID — may return multiple rows (one per variant), take the first
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "DSC_900"])
        .output()
        .unwrap();
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let asset_id = stdout_str.lines().next().unwrap().trim().to_string();
    assert_eq!(asset_id.len(), 36, "Should get a UUID");

    // Verify via show --json that the asset has an export variant
    let show_json = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&show_json.stdout).unwrap();
    let variants = parsed["variants"].as_array().unwrap();
    assert_eq!(variants.len(), 2, "Should have 2 variants (RAW + JPG)");

    // The JPG should have role "alternate" (non-RAW alongside RAW)
    let has_alternate = variants.iter().any(|v| v["role"].as_str() == Some("alternate"));
    assert!(has_alternate, "JPG variant should have alternate role");

    // maki show should show the JPG's hash in the Preview line (export preferred)
    let jpg_hash = sha256_hex(&jpg_content);
    let show_output = maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&show_output.stdout);

    // The preview line should reference the JPG export hash
    let preview_line = stdout.lines().find(|l| l.starts_with("Preview:"));
    assert!(
        preview_line.is_some(),
        "Should have a Preview: line"
    );
    assert!(
        preview_line.unwrap().contains(&jpg_hash),
        "Preview should use JPG hash ({jpg_hash}), got: {}",
        preview_line.unwrap()
    );
}

#[test]
fn show_preview_falls_back_to_original() {
    use image::{ImageBuffer, Rgb};

    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Create a real 1x1 PNG so preview generation succeeds
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(1, 1, Rgb([128, 0, 0]));
    let img_path = root.join("solo.png");
    img.save(&img_path).unwrap();
    let content = std::fs::read(&img_path).unwrap();

    maki()
        .current_dir(&root)
        .args(["import", img_path.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "solo"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let hash = sha256_hex(&content);
    maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains(&hash));
}

#[test]
fn group_shows_export_preview() {
    use image::{ImageBuffer, Rgb};

    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Import RAW and JPG as separate assets (different directories, different stems)
    let dir_a = root.join("dir_a");
    let dir_b = root.join("dir_b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();

    create_test_file(&dir_a, "IMG_001.nef", b"raw data for group preview test");

    // Real image for the JPG so preview works
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(1, 1, Rgb([0, 0, 255]));
    let jpg_path = dir_b.join("IMG_001_export.jpg");
    img.save(&jpg_path).unwrap();
    let jpg_content = std::fs::read(&jpg_path).unwrap();

    // Import separately so they become different assets
    maki()
        .current_dir(&root)
        .args(["import", dir_a.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", dir_b.to_str().unwrap()])
        .assert()
        .success();

    // Get variant hashes via show --json
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "IMG_001.nef"])
        .output()
        .unwrap();
    let raw_asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "IMG_001_export"])
        .output()
        .unwrap();
    let jpg_asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Get content hashes from show --json
    let raw_show = maki()
        .current_dir(&root)
        .args(["--json", "show", &raw_asset_id])
        .output()
        .unwrap();
    let raw_json: serde_json::Value = serde_json::from_slice(&raw_show.stdout).unwrap();
    let raw_variant_hash = raw_json["variants"][0]["content_hash"].as_str().unwrap().to_string();

    let jpg_show = maki()
        .current_dir(&root)
        .args(["--json", "show", &jpg_asset_id])
        .output()
        .unwrap();
    let jpg_json: serde_json::Value = serde_json::from_slice(&jpg_show.stdout).unwrap();
    let jpg_variant_hash = jpg_json["variants"][0]["content_hash"].as_str().unwrap().to_string();

    // Group them
    maki()
        .current_dir(&root)
        .args(["group", &raw_variant_hash, &jpg_variant_hash])
        .assert()
        .success();

    // After grouping, the merged asset should prefer JPG (export) preview.
    // The target of `group` is the oldest asset (the RAW one, imported first).
    let jpg_hash = sha256_hex(&jpg_content);
    let show_output = maki()
        .current_dir(&root)
        .args(["show", &raw_asset_id])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&show_output.stdout);

    let preview_line = stdout.lines().find(|l| l.starts_with("Preview:"));
    assert!(
        preview_line.is_some_and(|l| l.contains(&jpg_hash)),
        "After group, preview should use JPG hash ({jpg_hash}), got:\n{stdout}"
    );
}

#[test]
fn generate_previews_upgrade_flag() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Import RAW + JPG pair
    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    create_test_file(&photos, "DSC_800.nef", b"raw for upgrade test");
    create_test_file(&photos, "DSC_800.jpg", b"jpg for upgrade test");

    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success();

    // --upgrade should run without error and report results
    maki()
        .current_dir(&root)
        .args(["generate-previews", "--upgrade"])
        .assert()
        .success()
        .stdout(predicate::str::contains("preview(s)"));

    // --upgrade --json should include upgraded field
    let output = maki()
        .current_dir(&root)
        .args(["--json", "generate-previews", "--upgrade"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed["upgraded"].is_number(), "JSON should include upgraded field");
}

#[test]
fn preview_json_reports_null_when_no_preview_file() {
    // The fixture bytes are not a decodable image, so import produces
    // no preview file and `preview` has nothing to open — the one
    // branch that is safe to exercise without spawning a viewer.
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["no_preview.jpg"]);

    let out = maki()
        .current_dir(&root)
        .args(["--json", "preview", &ids[0]])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(parsed["id"].as_str(), Some(ids[0].as_str()));
    assert!(parsed["preview"].is_null(), "no preview file exists: {parsed}");
    assert_eq!(count_preview_files(&root.join("previews")), 0);

    // Human-readable variant reports on stderr and still exits 0.
    maki()
        .current_dir(&root)
        .args(["preview", &ids[0]])
        .assert()
        .success()
        .stderr(predicate::str::contains("No preview available"));
}

#[test]
fn preview_unknown_asset_fails() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    maki()
        .current_dir(&root)
        .args(["preview", "ffffffff"])
        .assert()
        .failure();
}
