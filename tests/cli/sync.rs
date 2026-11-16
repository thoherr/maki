//! `sync`, `refresh` and `cleanup`.

use crate::common::*;

#[test]
fn sync_detects_unchanged() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"photo data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["sync", root.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("unchanged"));
}

#[test]
fn sync_detects_moved_file() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let sub = root.join("originals");
    std::fs::create_dir_all(&sub).unwrap();
    let file = create_test_file(&sub, "photo.jpg", b"moved photo data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Move the file to a new directory
    let new_sub = root.join("renamed");
    std::fs::create_dir_all(&new_sub).unwrap();
    std::fs::rename(&file, new_sub.join("photo.jpg")).unwrap();

    // Dry run — should detect moved
    maki()
        .current_dir(&root)
        .args(["sync", root.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved"));

    // Apply — should update location
    maki()
        .current_dir(&root)
        .args(["sync", "--apply", root.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved"));

    // Verify the location was updated by running show (should show new path)
    let search_output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "*"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&search_output.get_output().stdout);
    let asset_id = stdout.trim();

    maki()
        .current_dir(&root)
        .args(["--json", "show", asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("renamed/photo.jpg"));
}

#[test]
fn sync_detects_new_file() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file1 = create_test_file(&root, "existing.jpg", b"existing data");

    maki()
        .current_dir(&root)
        .args(["import", file1.to_str().unwrap()])
        .assert()
        .success();

    // Create a new file that wasn't imported
    create_test_file(&root, "brand_new.jpg", b"brand new data");

    maki()
        .current_dir(&root)
        .args(["sync", root.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("new"))
        .stdout(predicate::str::contains("Tip: run 'maki import'"));
}

#[test]
fn sync_detects_missing_file() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "gone.jpg", b"will be deleted");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Delete the file
    std::fs::remove_file(&file).unwrap();

    maki()
        .current_dir(&root)
        .args(["sync", root.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("missing"));
}

#[test]
fn sync_remove_stale() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "stale.jpg", b"stale data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Get asset id before deleting
    let search_output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "stale"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&search_output.get_output().stdout);
    let asset_id = stdout.trim().to_string();
    assert!(!asset_id.is_empty(), "search returned the imported asset id");

    std::fs::remove_file(&file).unwrap();

    // --remove-stale requires --apply
    maki()
        .current_dir(&root)
        .args(["sync", "--remove-stale", root.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--remove-stale requires --apply"));

    // Apply with --remove-stale
    maki()
        .current_dir(&root)
        .args(["sync", "--apply", "--remove-stale", root.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("stale removed"));

    // The asset lost its only location, so sync purged it completely —
    // sidecar and every catalog row (formerly the sidecar went and a
    // phantom asset row stayed behind, which `doctor` then flagged).
    maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("no asset found"));
    maki()
        .current_dir(&root)
        .args(["stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Assets:    0"));
    maki()
        .current_dir(&root)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no issues found"));
}

#[test]
fn sync_detects_modified_recipe() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Create a NEF + XMP pair
    create_test_file(&root, "DSC_001.nef", b"raw image data");
    let xmp = create_test_file(&root, "DSC_001.xmp", b"<xmp>original</xmp>");

    maki()
        .current_dir(&root)
        .args(["import", "--include", "captureone", root.to_str().unwrap()])
        .assert()
        .success();

    // Modify the XMP
    std::fs::write(&xmp, b"<xmp>modified content</xmp>").unwrap();

    maki()
        .current_dir(&root)
        .args(["sync", root.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("modified"));
}

/// `maki sync --apply` should handle the case where a media variant
/// file (e.g. a JPEG export from CaptureOne) was overwritten with
/// fresh content but kept the same path. Used to error with "Hash
/// mismatch at <path>: expected A, got B" and refuse to touch the
/// catalog. Now drops the stale file_location for the old hash,
/// inserts a stub variant for the new hash (inheriting role +
/// original_filename from the old variant), and attaches it to the
/// asset's sidecar. The old variant becomes locationless and can be
/// removed by `maki cleanup --apply`.
#[test]
fn sync_handles_modified_media_with_apply() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Real JPEGs so the EXIF and preview-regeneration paths exercise
    // (raw byte strings would short-circuit both with errors). Use two
    // different dimensions so the assertion that the variant's
    // image_width / image_height got REFRESHED is meaningful — a CO
    // re-export at a different output size is the classic case.
    let jpeg = root.join("exports/IMG_001.jpg");
    std::fs::create_dir_all(jpeg.parent().unwrap()).unwrap();
    let img_v1 = image::DynamicImage::new_rgb8(400, 300);
    img_v1.save(&jpeg).unwrap();

    maki()
        .current_dir(&root)
        .args(["import", jpeg.to_str().unwrap()])
        .assert()
        .success();

    let asset_id = String::from_utf8_lossy(
        &maki()
            .current_dir(&root)
            .args(["search", "-q", ""])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert!(!asset_id.is_empty(), "import should yield an asset");

    // Re-export to the same path at a different resolution — what
    // CaptureOne does on a "Process Recipe" re-run with new size.
    let img_v2 = image::DynamicImage::new_rgb8(800, 600);
    img_v2.save(&jpeg).unwrap();

    // Without --apply: sync reports the modification but doesn't
    // touch the catalog. Used to be an error; now it's a Modified
    // entry like a modified recipe.
    let preview = maki()
        .current_dir(&root)
        .args(["sync", root.to_str().unwrap()])
        .assert()
        .success();
    let preview_out = String::from_utf8_lossy(&preview.get_output().stdout);
    assert!(
        preview_out.contains("modified"),
        "dry-run sync should report the modified media file. Got:\n{preview_out}"
    );

    // With --apply: catalog learns about the new hash. The asset
    // gains a new variant entry for the new bytes; the old variant
    // loses its location at this path.
    maki()
        .current_dir(&root)
        .args(["sync", "--apply", root.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("modified"))
        .stdout(predicate::str::contains("Hash mismatch").not());

    // Sidecar reflects the new variant attached to the asset.
    let sidecar = root
        .join("metadata")
        .join(&asset_id[..2])
        .join(format!("{asset_id}.yaml"));
    let yaml = std::fs::read_to_string(&sidecar).unwrap();

    // Compute the expected new hash to match against the sidecar.
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(std::fs::read(&jpeg).unwrap());
    let new_hash = format!("sha256:{}", sha256_hex_of(&h.finalize()));
    assert!(
        yaml.contains(&new_hash),
        "Sidecar must reference the new content_hash {new_hash}. Got:\n{yaml}"
    );

    // A preview file keyed by the new hash should exist on disk —
    // sync regenerates previews from the fresh bytes so the asset is
    // immediately viewable without a separate `maki refresh` /
    // `maki generate-previews` step. (We can't easily assert EXIF
    // values via the test JPEG without baking in an EXIF block, so
    // preview existence is the end-to-end signal that the full
    // "re-process this variant" path ran.)
    let new_hash_hex = new_hash.trim_start_matches("sha256:");
    let prefix = &new_hash_hex[..2];
    let preview_dir = root.join("previews").join(prefix);
    assert!(
        preview_dir.exists(),
        "Preview directory for new hash should exist: {}",
        preview_dir.display()
    );
    let has_preview = std::fs::read_dir(&preview_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().starts_with(new_hash_hex));
    assert!(
        has_preview,
        "Preview file for new hash {new_hash_hex} should exist in {}",
        preview_dir.display()
    );

    // A second sync run with no further changes should report no
    // modifications — the catalog is already current.
    let resync = maki()
        .current_dir(&root)
        .args(["sync", root.to_str().unwrap()])
        .assert()
        .success();
    let resync_out = String::from_utf8_lossy(&resync.get_output().stdout);
    assert!(
        !resync_out.contains("modified"),
        "Second sync after apply should be a no-op. Got:\n{resync_out}"
    );
    assert!(
        !resync_out.contains("Hash mismatch"),
        "Second sync should not report a hash mismatch. Got:\n{resync_out}"
    );
}

#[test]
fn sync_default_is_dry_run() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let sub = root.join("before");
    std::fs::create_dir_all(&sub).unwrap();
    let file = create_test_file(&sub, "moveme.jpg", b"dry run data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Move the file
    let new_sub = root.join("after");
    std::fs::create_dir_all(&new_sub).unwrap();
    std::fs::rename(&file, new_sub.join("moveme.jpg")).unwrap();

    // Sync without --apply (dry run)
    maki()
        .current_dir(&root)
        .args(["sync", root.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved"));

    // Show should still have the old path (catalog unchanged)
    let search_output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "*"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&search_output.get_output().stdout);
    let asset_id = stdout.trim();

    maki()
        .current_dir(&root)
        .args(["--json", "show", asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("before/moveme.jpg"));
}

#[test]
fn sync_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "json_test.jpg", b"json test data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "sync", root.to_str().unwrap()])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(json.get("unchanged").is_some());
    assert!(json.get("moved").is_some());
    assert!(json.get("new_files").is_some());
    assert!(json.get("modified").is_some());
    assert!(json.get("missing").is_some());
    assert!(json.get("stale_removed").is_some());
    assert!(json.get("errors").is_some());
}

#[test]
fn sync_no_paths_errors() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["sync"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no paths specified"));
}

#[test]
fn cleanup_no_stale() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photo.jpg", b"photo data");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["cleanup"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 stale"));
}

#[test]
fn cleanup_detects_stale() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "gone.jpg", b"gone data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    std::fs::remove_file(&file).unwrap();

    maki()
        .current_dir(&root)
        .args(["cleanup"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 stale"));
}

#[test]
fn cleanup_apply_removes_stale() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "remove_me.jpg", b"remove data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    std::fs::remove_file(&file).unwrap();

    maki()
        .current_dir(&root)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 removed"))
        .stdout(predicate::str::contains("1 orphaned assets removed"));

    // Asset should be fully removed (no results from search)
    let search_output = maki()
        .current_dir(&root)
        .args(["search", "-q", "*"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(search_output).unwrap();
    assert!(stdout.trim().is_empty(), "orphaned asset should be removed");
}

#[test]
fn cleanup_default_is_report_only() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "keep_it.jpg", b"keep data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let search_output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "*"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&search_output.get_output().stdout);
    let asset_id = stdout.trim().to_string();

    std::fs::remove_file(&file).unwrap();

    // Without --apply: reports stale but doesn't remove
    maki()
        .current_dir(&root)
        .args(["cleanup"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 stale"))
        .stdout(predicate::str::contains("--apply"));

    // Location should still be in the catalog
    let show_output = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .assert()
        .success();
    let show_stdout = String::from_utf8_lossy(&show_output.get_output().stdout);
    let show_json: serde_json::Value = serde_json::from_str(&show_stdout).expect("valid JSON");
    let locations = &show_json["variants"][0]["locations"];
    assert_eq!(locations.as_array().unwrap().len(), 1, "location should still exist");
}

#[test]
fn cleanup_volume_filter() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Add a second volume
    let dir2 = tempdir().unwrap();
    let vol2_path = dir2.path().canonicalize().unwrap();
    maki()
        .current_dir(&root)
        .args(["volume", "add", "vol2", vol2_path.to_str().unwrap()])
        .assert()
        .success();

    // Import a file on the main volume
    let file1 = create_test_file(&root, "on_vol1.jpg", b"vol1 data");
    maki()
        .current_dir(&root)
        .args(["import", file1.to_str().unwrap()])
        .assert()
        .success();

    // Import a file on volume 2
    let file2 = create_test_file(&vol2_path, "on_vol2.jpg", b"vol2 data");
    maki()
        .current_dir(&root)
        .args(["import", "--volume", "vol2", file2.to_str().unwrap()])
        .assert()
        .success();

    // Delete both files
    std::fs::remove_file(&file1).unwrap();
    std::fs::remove_file(&file2).unwrap();

    // Cleanup only vol2 — should only see 1 stale
    maki()
        .current_dir(&root)
        .args(["cleanup", "--volume", "vol2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 stale"));
}

#[test]
fn cleanup_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "json_cleanup.jpg", b"json cleanup data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "cleanup"])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(json.get("checked").is_some());
    assert!(json.get("stale").is_some());
    assert!(json.get("removed").is_some());
    assert!(json.get("skipped_offline").is_some());
    assert!(json.get("errors").is_some());
}

#[test]
fn cleanup_stale_recipe() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Create a NEF + XMP pair
    create_test_file(&root, "DSC_001.nef", b"raw image data for cleanup");
    let xmp = create_test_file(&root, "DSC_001.xmp", b"<xmp>recipe data</xmp>");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    // Get asset id
    let search_output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "*"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&search_output.get_output().stdout);
    let asset_id = stdout.trim().to_string();

    // Confirm recipe exists
    let show_output = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .assert()
        .success();
    let show_stdout = String::from_utf8_lossy(&show_output.get_output().stdout);
    let show_json: serde_json::Value = serde_json::from_str(&show_stdout).expect("valid JSON");
    assert!(!show_json["recipes"].as_array().unwrap().is_empty(), "recipe should exist");

    // Delete the XMP file
    std::fs::remove_file(&xmp).unwrap();

    // Cleanup --apply should remove the stale recipe
    maki()
        .current_dir(&root)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("stale"));

    // Recipe should be gone
    let show_output2 = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .assert()
        .success();
    let show_stdout2 = String::from_utf8_lossy(&show_output2.get_output().stdout);
    let show_json2: serde_json::Value = serde_json::from_str(&show_stdout2).expect("valid JSON");
    assert!(show_json2["recipes"].as_array().unwrap().is_empty(), "recipe should be removed");
}

#[test]
fn cleanup_list_shows_only_stale() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "present.jpg", b"present data");
    let gone = create_test_file(&root, "gone.jpg", b"gone data");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    std::fs::remove_file(&gone).unwrap();

    let output = maki()
        .current_dir(&root)
        .args(["cleanup", "--list"])
        .assert()
        .success();

    let stderr = String::from_utf8_lossy(&output.get_output().stderr);
    // Should list the stale file
    assert!(stderr.contains("gone.jpg"), "should list stale file on stderr");
    // Should NOT list the present file (--list filters to stale only)
    assert!(!stderr.contains("present.jpg"), "should not list ok files on stderr");
}

#[test]
fn cleanup_removes_orphaned_assets() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "orphan_cleanup.jpg", b"orphan cleanup data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    std::fs::remove_file(&file).unwrap();

    maki()
        .current_dir(&root)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 orphaned assets removed"));

    // search orphan:true should return nothing — the orphan was removed
    let search_output = maki()
        .current_dir(&root)
        .args(["search", "-q", "orphan:true"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(search_output).unwrap();
    assert!(stdout.trim().is_empty(), "orphaned asset should be removed by cleanup --apply");
}

#[test]
fn cleanup_reports_orphaned_without_apply() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "report_orphan.jpg", b"report orphan data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Get asset id
    let search_output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&search_output.get_output().stdout);
    let asset_id = stdout.trim().to_string();

    std::fs::remove_file(&file).unwrap();

    // Report-only mode: should count orphaned assets but not remove them
    maki()
        .current_dir(&root)
        .args(["cleanup"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 orphaned assets"));

    // Asset should still exist
    maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .assert()
        .success();
}

#[test]
fn cleanup_removes_orphaned_previews() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    // Use .mp3 extension — generates an info card preview (always succeeds, audio is on by default)
    let file = create_test_file(&root, "preview_orphan.mp3", b"preview orphan data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Check that a preview was generated
    let previews_dir = root.join("previews");
    let preview_count_before = count_preview_files(&previews_dir);
    assert!(preview_count_before > 0, "preview should exist after import");

    std::fs::remove_file(&file).unwrap();

    maki()
        .current_dir(&root)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("orphaned assets removed"));

    // Preview should be gone (removed as part of orphaned asset cleanup)
    let preview_count_after = count_preview_files(&previews_dir);
    assert_eq!(preview_count_after, 0, "orphaned previews should be removed");
}

#[test]
fn cleanup_preserves_non_orphaned_assets() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "keep_me.jpg", b"keep me data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Get asset id
    let search_output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&search_output.get_output().stdout);
    let asset_id = stdout.trim().to_string();

    // File still on disk — cleanup --apply should not remove anything
    maki()
        .current_dir(&root)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 stale"));

    // Asset should still exist
    maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .assert()
        .success();
}

#[test]
fn cleanup_json_includes_orphan_fields() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "json_orphan.jpg", b"json orphan data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    std::fs::remove_file(&file).unwrap();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "cleanup", "--apply"])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(json["orphaned_assets"], 1);
    assert_eq!(json["removed_assets"], 1);
    assert!(json.get("orphaned_previews").is_some());
    assert!(json.get("removed_previews").is_some());
}

#[test]
fn refresh_detects_unchanged_recipes() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    create_test_file(&root, "photos/DSC_400.ARW", b"raw-refresh-test");
    create_test_file(
        &root,
        "photos/DSC_400.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"3\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    // Refresh without changes — should report unchanged
    maki()
        .current_dir(&root)
        .args(["refresh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 unchanged"));
}

#[test]
fn refresh_detects_modified_recipe() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let xmp_path = root.join("photos/DSC_500.xmp");
    create_test_file(&root, "photos/DSC_500.ARW", b"raw-refresh-modify");
    create_test_file(
        &root,
        "photos/DSC_500.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"2\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    // Modify the XMP file externally
    std::fs::write(
        &xmp_path,
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"5\"/></rdf:RDF></x:xmpmeta>",
    )
    .unwrap();

    // Refresh should detect the change
    maki()
        .current_dir(&root)
        .args(["refresh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 refreshed"));
}

#[test]
fn refresh_dry_run_does_not_apply() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let xmp_path = root.join("photos/DSC_600.xmp");
    create_test_file(&root, "photos/DSC_600.ARW", b"raw-refresh-dry");
    create_test_file(
        &root,
        "photos/DSC_600.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"1\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    // Modify the XMP file
    std::fs::write(
        &xmp_path,
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"4\"/></rdf:RDF></x:xmpmeta>",
    )
    .unwrap();

    // Dry run — should report but not apply
    maki()
        .current_dir(&root)
        .args(["refresh", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 refreshed"))
        .stderr(predicate::str::contains("Dry run"));

    // Run again without dry-run — should still see the change (wasn't applied)
    maki()
        .current_dir(&root)
        .args(["refresh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 refreshed"));
}

#[test]
fn refresh_media_extracts_embedded_xmp() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();

    let xmp_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>nature</rdf:li>
     <rdf:li>forest</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    let jpeg_data = build_jpeg_with_xmp(xmp_xml);
    std::fs::write(photos.join("forest.jpg"), &jpeg_data).unwrap();

    // Import — metadata should be extracted
    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    // Get asset ID
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Verify initial metadata
    maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("nature")
                .and(predicate::str::contains("forest"))
                .and(predicate::str::contains("4")),
        );

    // Clear the metadata
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--clear-rating"])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["tag", &asset_id, "--remove", "nature", "forest"])
        .assert()
        .success();

    // Verify tags are cleared
    maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Tags:").not()
                .and(predicate::str::contains("Rating:").not()),
        );

    // Run refresh --media to re-extract embedded XMP
    maki()
        .current_dir(&root)
        .args(["refresh", "--media"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 refreshed"));

    // Verify metadata is restored
    maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Tags:")
                .and(predicate::str::contains("Rating:")),
        );
}

#[test]
fn refresh_media_dry_run_does_not_apply() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();

    let xmp_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3"/>
 </rdf:RDF>
</x:xmpmeta>"#;

    let jpeg_data = build_jpeg_with_xmp(xmp_xml);
    std::fs::write(photos.join("drytest.jpg"), &jpeg_data).unwrap();

    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Clear rating
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--clear-rating"])
        .assert()
        .success();

    // Dry run — should report but not apply
    maki()
        .current_dir(&root)
        .args(["refresh", "--media", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 refreshed"))
        .stderr(predicate::str::contains("Dry run"));

    // Verify rating is still cleared (not restored)
    maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rating: 3").not());
}

#[test]
fn refresh_without_media_ignores_jpeg() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();

    let xmp_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="5">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>mountain</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    let jpeg_data = build_jpeg_with_xmp(xmp_xml);
    std::fs::write(photos.join("mountain.jpg"), &jpeg_data).unwrap();

    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Clear metadata
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--clear-rating"])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["tag", &asset_id, "--remove", "mountain"])
        .assert()
        .success();

    // Regular refresh (no --media) — no recipes, nothing to check
    maki()
        .current_dir(&root)
        .args(["refresh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to check"));

    // Verify metadata is NOT restored (no Tags: or Rating: lines)
    maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Tags:").not()
                .and(predicate::str::contains("Rating:").not()),
        );
}

#[test]
fn refresh_reimport_skips_embedded_xmp_when_sidecar_present() {
    // Sister regression to refresh_media_skips_when_sidecar_present
    // (v4.5.14) — that fix landed in refresh.rs and sync-metadata, but
    // `query::reimport_metadata_inner` (the path behind
    // `maki refresh --reimport` and the web UI's "Reload metadata"
    // button) had the same iterate-locations-and-read-embedded-XMP loop
    // without the sidecar-wins gate. v4.5.15 closes that.
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let xmp_with_stale = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>StaleEmbeddedTag</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    let jpeg_bytes = build_jpeg_with_xmp(xmp_with_stale);

    let jpeg = root.join("photo.jpg");
    std::fs::write(&jpeg, &jpeg_bytes).unwrap();

    // Sidecar with no keywords — establishes the "asset has recipe"
    // state that gates embedded-XMP reads.
    let xmp_sidecar = root.join("photo.xmp");
    std::fs::write(
        &xmp_sidecar,
        b"<?xml version=\"1.0\"?>\n\
          <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
          <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
          <rdf:Description rdf:about=\"\" \
            xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"/>\
          </rdf:RDF></x:xmpmeta>",
    )
    .unwrap();

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let asset_id = String::from_utf8_lossy(
        &maki()
            .current_dir(&root)
            .args(["search", "-q", "", "--format", "ids"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert!(!asset_id.is_empty());

    // Reimport clears tags first (line 2139), then re-reads everything.
    // Without the sidecar-wins gate, the JPEG's embedded
    // "StaleEmbeddedTag" lands back in asset.tags.
    maki()
        .current_dir(&root)
        .args(["refresh", "--reimport", "--asset", &asset_id])
        .assert()
        .success();

    let after_reimport = String::from_utf8_lossy(
        &maki()
            .current_dir(&root)
            .args(["show", &asset_id])
            .output()
            .unwrap()
            .stdout,
    )
    .to_string();
    assert!(
        !after_reimport.contains("StaleEmbeddedTag"),
        "refresh --reimport must NOT re-inject embedded keywords when \
         a sidecar exists. Got:\n{after_reimport}"
    );
}

#[test]
fn refresh_media_skips_when_sidecar_present() {
    // Sidecar-wins rule (v4.5.14): if an asset has an XMP sidecar recipe,
    // `refresh --media` must NOT re-extract embedded XMP from JPEG/TIFF
    // variants. The sidecar is authoritative; the JPEG's embedded XMP is
    // frozen at import time (writeback doesn't patch embedded XMP) and
    // would otherwise re-inject stale flat keywords on every refresh.
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // JPEG carries an embedded XMP block with a flat keyword that would
    // get re-imported on refresh --media if the sidecar gate weren't
    // honoured.
    let xmp_with_stale = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>StaleEmbeddedTag</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    let jpeg_bytes = build_jpeg_with_xmp(xmp_with_stale);

    let jpeg = root.join("photo.jpg");
    std::fs::write(&jpeg, &jpeg_bytes).unwrap();

    // Pre-existing XMP sidecar with no keywords. Import below picks it
    // up as the recipe — establishing the "asset has a sidecar" state
    // that gates embedded-XMP re-extraction.
    let xmp_sidecar = root.join("photo.xmp");
    std::fs::write(
        &xmp_sidecar,
        b"<?xml version=\"1.0\"?>\n\
          <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
          <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
          <rdf:Description rdf:about=\"\" \
            xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"/>\
          </rdf:RDF></x:xmpmeta>",
    )
    .unwrap();

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let asset_id = String::from_utf8_lossy(
        &maki()
            .current_dir(&root)
            .args(["search", "-q", "", "--format", "ids"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert!(!asset_id.is_empty(), "import should yield an asset");

    // Initial import absorbed the embedded keyword. Clear it from the
    // catalog so we can detect re-injection via refresh --media.
    maki()
        .current_dir(&root)
        .args(["tag", "clear", &asset_id])
        .assert()
        .success();

    let after_clear = String::from_utf8_lossy(
        &maki()
            .current_dir(&root)
            .args(["show", &asset_id])
            .output()
            .unwrap()
            .stdout,
    )
    .to_string();
    assert!(
        !after_clear.contains("StaleEmbeddedTag"),
        "tag clear should have removed the keyword. Got:\n{after_clear}"
    );

    // refresh --media. Without the sidecar-wins gate, this re-injects
    // the JPEG's embedded "StaleEmbeddedTag" into the asset's tags.
    maki()
        .current_dir(&root)
        .args(["refresh", "--media"])
        .assert()
        .success();

    let after_refresh = String::from_utf8_lossy(
        &maki()
            .current_dir(&root)
            .args(["show", &asset_id])
            .output()
            .unwrap()
            .stdout,
    )
    .to_string();
    assert!(
        !after_refresh.contains("StaleEmbeddedTag"),
        "refresh --media must NOT re-inject embedded keywords when a \
         sidecar exists. Got:\n{after_refresh}"
    );
}
