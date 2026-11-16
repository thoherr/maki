//! `relocate` and `update-location`.

use crate::common::*;

#[test]
fn relocate_copies_files_between_volumes() {
    let dir = tempdir().unwrap();
    let (root, vol1, vol2) = init_two_volumes(dir.path());

    create_test_file(&vol1, "photo.jpg", b"relocate test data");

    // Import on vol1
    maki()
        .current_dir(&root)
        .args(["import", vol1.join("photo.jpg").to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    // Get asset ID
    let output = maki()
        .current_dir(&root)
        .args(["search", "photo"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    // Relocate to vol2
    maki()
        .current_dir(&root)
        .args(["relocate", short_id, "vol2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Relocate complete"));

    // Verify file exists on vol2
    assert!(vol2.join("photo.jpg").exists());
    // File still on vol1
    assert!(vol1.join("photo.jpg").exists());

    // Show should list both volumes
    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("vol1")
                .and(predicate::str::contains("vol2")),
        );
}

#[test]
fn relocate_with_remove_source_flag() {
    let dir = tempdir().unwrap();
    let (root, vol1, vol2) = init_two_volumes(dir.path());

    create_test_file(&vol1, "move_me.jpg", b"move test data");

    maki()
        .current_dir(&root)
        .args(["import", vol1.join("move_me.jpg").to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "move_me"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    // Relocate with --remove-source
    maki()
        .current_dir(&root)
        .args(["relocate", short_id, "vol2", "--remove-source"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Relocate complete"));

    // File should be on vol2 but not on vol1
    assert!(vol2.join("move_me.jpg").exists());
    assert!(!vol1.join("move_me.jpg").exists());

    // Show should only list vol2
    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("vol2")
                .and(predicate::str::contains("vol1").not()),
        );
}

#[test]
fn relocate_dry_run_no_changes() {
    let dir = tempdir().unwrap();
    let (root, vol1, vol2) = init_two_volumes(dir.path());

    create_test_file(&vol1, "dry.jpg", b"dry run test data");

    maki()
        .current_dir(&root)
        .args(["import", vol1.join("dry.jpg").to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "dry"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    // Dry run
    maki()
        .current_dir(&root)
        .args(["relocate", short_id, "vol2", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"));

    // File should NOT exist on vol2
    assert!(!vol2.join("dry.jpg").exists());
    // File still on vol1
    assert!(vol1.join("dry.jpg").exists());
}

#[test]
fn relocate_batch_with_query() {
    let dir = tempdir().unwrap();
    let (root, vol1, vol2) = init_two_volumes(dir.path());

    create_test_file(&vol1, "a.jpg", b"batch relocate A");
    create_test_file(&vol1, "b.jpg", b"batch relocate B");

    // Import both files with a tag for querying
    maki()
        .current_dir(&root)
        .args(["import", "--add-tag", "batch-test", vol1.to_str().unwrap()])
        .assert()
        .success();

    // Batch relocate all tagged assets to vol2
    maki()
        .current_dir(&root)
        .args(["relocate", "--query", "tag:batch-test", "--target", "vol2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 assets"))
        .stdout(predicate::str::contains("copied"));

    // Both files should now exist on vol2
    assert!(vol2.join("a.jpg").exists());
    assert!(vol2.join("b.jpg").exists());
    // Still on vol1 (no --remove-source)
    assert!(vol1.join("a.jpg").exists());
    assert!(vol1.join("b.jpg").exists());
}

#[test]
fn relocate_batch_dry_run() {
    let dir = tempdir().unwrap();
    let (root, vol1, vol2) = init_two_volumes(dir.path());

    create_test_file(&vol1, "dry_batch.jpg", b"dry batch data");

    maki()
        .current_dir(&root)
        .args(["import", "--add-tag", "drybatch", vol1.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["relocate", "--query", "tag:drybatch", "--target", "vol2", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"));

    // File should NOT exist on vol2
    assert!(!vol2.join("dry_batch.jpg").exists());
}

#[test]
fn update_location_moves_variant_path() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let sub = root.join("originals");
    std::fs::create_dir_all(&sub).unwrap();
    let file = create_test_file(&sub, "photo.jpg", b"update location test data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Get asset ID
    let search_output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "*"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&search_output.get_output().stdout);
    let asset_id = stdout.trim().to_string();

    // Move the file on disk
    let new_sub = root.join("moved");
    std::fs::create_dir_all(&new_sub).unwrap();
    std::fs::rename(&file, new_sub.join("photo.jpg")).unwrap();

    let old_path = "originals/photo.jpg".to_string();
    let new_path = new_sub.join("photo.jpg");

    // Run update-location
    maki()
        .current_dir(&root)
        .args([
            "update-location", &asset_id,
            "--from", &old_path,
            "--to", new_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated variant location"));

    // Verify show has the new path
    maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("moved/photo.jpg")
                .and(predicate::str::contains("originals/photo.jpg").not()),
        );
}

#[test]
fn update_location_moves_recipe_path() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    create_test_file(&root, "DSC_001.nef", b"raw image for recipe move");
    let xmp = create_test_file(&root, "DSC_001.xmp", b"<xmp>recipe</xmp>");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    // Get asset ID
    let search_output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "*"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&search_output.get_output().stdout);
    let asset_id = stdout.trim().to_string();

    // Move the XMP file on disk
    let sub = root.join("recipes");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::rename(&xmp, sub.join("DSC_001.xmp")).unwrap();

    let new_path = sub.join("DSC_001.xmp");

    // Run update-location
    maki()
        .current_dir(&root)
        .args([
            "update-location", &asset_id,
            "--from", "DSC_001.xmp",
            "--to", new_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated recipe location"));

    // Verify show has the new recipe path
    maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("recipes/DSC_001.xmp"));
}

#[test]
fn update_location_rejects_wrong_hash() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"original content");

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

    // Create a DIFFERENT file at the new path
    let new_file = create_test_file(&root, "moved/photo.jpg", b"different content entirely");

    maki()
        .current_dir(&root)
        .args([
            "update-location", &asset_id,
            "--from", "photo.jpg",
            "--to", new_file.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Hash mismatch"));
}

#[test]
fn update_location_rejects_missing_from() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"some data");

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

    // --from path doesn't exist in catalog, --to is a valid file
    let new_file = create_test_file(&root, "elsewhere/photo.jpg", b"some data");

    maki()
        .current_dir(&root)
        .args([
            "update-location", &asset_id,
            "--from", "nonexistent/photo.jpg",
            "--to", new_file.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No variant or recipe found"));
}

#[test]
fn update_location_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let sub = root.join("originals");
    std::fs::create_dir_all(&sub).unwrap();
    let file = create_test_file(&sub, "photo.jpg", b"json output test data");

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

    // Move on disk
    let new_sub = root.join("moved");
    std::fs::create_dir_all(&new_sub).unwrap();
    std::fs::rename(&file, new_sub.join("photo.jpg")).unwrap();

    let new_path = new_sub.join("photo.jpg");

    let output = maki()
        .current_dir(&root)
        .args([
            "--json", "update-location", &asset_id,
            "--from", "originals/photo.jpg",
            "--to", new_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let out = String::from_utf8_lossy(&output.get_output().stdout);
    let json: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(json["file_type"], "variant");
    assert_eq!(json["old_path"], "originals/photo.jpg");
    assert_eq!(json["new_path"], "moved/photo.jpg");
    assert_eq!(json["volume_label"], "test-vol");
    assert!(json["asset_id"].as_str().is_some());
    assert!(json["content_hash"].as_str().is_some());
}

#[test]
fn update_location_auto_detects_volume() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "auto.jpg", b"auto detect volume test");

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

    // Move on disk (same volume, no --volume flag)
    let new_sub = root.join("newdir");
    std::fs::create_dir_all(&new_sub).unwrap();
    std::fs::rename(&file, new_sub.join("auto.jpg")).unwrap();

    let new_path = new_sub.join("auto.jpg");

    maki()
        .current_dir(&root)
        .args([
            "update-location", &asset_id,
            "--from", "auto.jpg",
            "--to", new_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated variant location"));

    // Verify new path in show output
    maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("newdir/auto.jpg"));
}
