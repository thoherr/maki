//! `volume` subcommands.

use crate::common::*;

#[test]
fn volume_add_and_list() {
    let dir = tempdir().unwrap();
    maki().current_dir(dir.path()).arg("init").assert().success();
    maki()
        .current_dir(dir.path())
        .args(["volume", "add", "my-vol", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("my-vol"));

    maki()
        .current_dir(dir.path())
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("my-vol"));
}

#[test]
fn volume_list_json() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let output = maki()
        .current_dir(&root)
        .args(["--json", "volume", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let arr = parsed.as_array().expect("should be array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["label"].as_str(), Some("test-vol"));
    assert!(arr[0]["is_online"].is_boolean());
}

#[test]
fn volume_add_with_purpose() {
    let dir = tempdir().unwrap();
    let canonical = dir.path().canonicalize().unwrap();
    maki().current_dir(&canonical).arg("init").assert().success();

    maki()
        .current_dir(&canonical)
        .args([
            "volume", "add", "backup-drive",
            canonical.to_str().unwrap(),
            "--purpose", "backup",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("backup-drive"))
        .stdout(predicate::str::contains("Purpose: backup"));

    // Verify it shows in list
    maki()
        .current_dir(&canonical)
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[backup]"));
}

#[test]
fn volume_add_purpose_invalid() {
    let dir = tempdir().unwrap();
    maki().current_dir(dir.path()).arg("init").assert().success();

    maki()
        .current_dir(dir.path())
        .args([
            "volume", "add", "test",
            dir.path().to_str().unwrap(),
            "--purpose", "invalid",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid purpose"));
}

#[test]
fn volume_set_purpose() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Volume starts with no purpose
    maki()
        .current_dir(&root)
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[online]"))
        .stdout(predicate::str::is_match(r"\[backup\]|\[archive\]|\[working\]|\[cloud\]").unwrap().not());

    // Set purpose
    maki()
        .current_dir(&root)
        .args(["volume", "set-purpose", "test-vol", "archive"])
        .assert()
        .success()
        .stdout(predicate::str::contains("purpose set to: archive"));

    // Verify in list
    maki()
        .current_dir(&root)
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[archive]"));

    // Clear purpose
    maki()
        .current_dir(&root)
        .args(["volume", "set-purpose", "test-vol", "none"])
        .assert()
        .success()
        .stdout(predicate::str::contains("purpose cleared"));
}

#[test]
fn volume_set_purpose_json() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let output = maki()
        .current_dir(&root)
        .args(["--json", "volume", "set-purpose", "test-vol", "backup"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["label"].as_str(), Some("test-vol"));
    assert_eq!(parsed["purpose"].as_str(), Some("backup"));
}

#[test]
fn volume_purpose_in_list_json() {
    let dir = tempdir().unwrap();
    let canonical = dir.path().canonicalize().unwrap();
    maki().current_dir(&canonical).arg("init").assert().success();

    maki()
        .current_dir(&canonical)
        .args([
            "volume", "add", "my-archive",
            canonical.to_str().unwrap(),
            "--purpose", "archive",
        ])
        .assert()
        .success();

    let output = maki()
        .current_dir(&canonical)
        .args(["--json", "volume", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let arr = parsed.as_array().expect("should be array");
    assert_eq!(arr[0]["purpose"].as_str(), Some("archive"));
}

#[test]
fn volume_remove_report_only() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"volume-remove-report");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Report-only (no --apply)
    maki()
        .current_dir(&root)
        .args(["volume", "remove", "test-vol"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would remove"))
        .stdout(predicate::str::contains("1 locations"));

    // Volume should still exist
    maki()
        .current_dir(&root)
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("test-vol"));

    // Catalog should still have the asset
    maki()
        .current_dir(&root)
        .args(["stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Assets:    1"));
}

#[test]
fn volume_remove_apply() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"volume-remove-apply");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Apply removal
    maki()
        .current_dir(&root)
        .args(["volume", "remove", "test-vol", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed"));

    // Volume should be gone
    maki()
        .current_dir(&root)
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No volumes"));

    // Asset should be gone (orphaned)
    maki()
        .current_dir(&root)
        .args(["stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Assets:    0"));
}

#[test]
fn volume_remove_empty() {
    let dir = tempdir().unwrap();
    let canonical = dir.path().canonicalize().unwrap();
    maki().current_dir(&canonical).arg("init").assert().success();
    maki()
        .current_dir(&canonical)
        .args(["volume", "add", "empty-vol", canonical.to_str().unwrap()])
        .assert()
        .success();

    // Remove empty volume
    maki()
        .current_dir(&canonical)
        .args(["volume", "remove", "empty-vol", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 locations removed"));

    // Volume should be gone
    maki()
        .current_dir(&canonical)
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No volumes"));
}

#[test]
fn volume_remove_unknown() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["volume", "remove", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no volume found"));
}

#[test]
fn volume_remove_json() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"volume-remove-json");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "volume", "remove", "test-vol"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["volume_label"].as_str(), Some("test-vol"));
    assert_eq!(parsed["locations"].as_u64(), Some(1));
    assert_eq!(parsed["apply"].as_bool(), Some(false));
    assert!(parsed["orphaned_assets"].as_u64().unwrap() >= 1);
}

#[test]
fn volume_combine_report_only() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();

    // Init catalog at root
    maki().current_dir(&root).arg("init").assert().success();

    // Create parent volume at root, child volume at root/sub
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    maki()
        .current_dir(&root)
        .args(["volume", "add", "parent-vol", root.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["volume", "add", "child-vol", sub.to_str().unwrap()])
        .assert()
        .success();

    // Import a file on the child volume
    let file = create_test_file(&sub, "photo.jpg", b"combine-report-content");
    maki()
        .current_dir(&root)
        .args(["import", "--volume", "child-vol", file.to_str().unwrap()])
        .assert()
        .success();

    // Report-only (no --apply)
    maki()
        .current_dir(&root)
        .args(["volume", "combine", "child-vol", "parent-vol"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Would combine"))
        .stdout(predicate::str::contains("1 locations"))
        .stdout(predicate::str::contains("prefix 'sub/'"));

    // Volume should still exist
    maki()
        .current_dir(&root)
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("child-vol"));

    // Asset should still exist
    maki()
        .current_dir(&root)
        .args(["stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Assets:    1"));
}

#[test]
fn volume_combine_apply() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();

    maki().current_dir(&root).arg("init").assert().success();

    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    maki()
        .current_dir(&root)
        .args(["volume", "add", "parent-vol", root.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["volume", "add", "child-vol", sub.to_str().unwrap()])
        .assert()
        .success();

    let file = create_test_file(&sub, "photo.jpg", b"combine-apply-content");
    maki()
        .current_dir(&root)
        .args(["import", "--volume", "child-vol", file.to_str().unwrap()])
        .assert()
        .success();

    // Apply combine
    maki()
        .current_dir(&root)
        .args(["volume", "combine", "child-vol", "parent-vol", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("combined into"))
        .stdout(predicate::str::contains("1 locations moved"));

    // Child volume should be gone
    maki()
        .current_dir(&root)
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("child-vol").not());

    // Asset should still exist
    maki()
        .current_dir(&root)
        .args(["stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Assets:    1"));

    // Verify path was rewritten: search for asset, then show it
    let output = maki()
        .current_dir(&root)
        .args(["search", "photo"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    maki()
        .current_dir(&root)
        .args(["show", "--json", short_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("sub/photo.jpg"));
}

#[test]
fn volume_combine_with_recipes() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();

    maki().current_dir(&root).arg("init").assert().success();

    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    maki()
        .current_dir(&root)
        .args(["volume", "add", "parent-vol", root.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["volume", "add", "child-vol", sub.to_str().unwrap()])
        .assert()
        .success();

    // Create a jpg and an xmp recipe
    let _jpg = create_test_file(&sub, "photo.jpg", b"combine-recipe-jpg");
    let _xmp = create_test_file(
        &sub,
        "photo.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"3\"/></rdf:RDF></x:xmpmeta>",
    );

    maki()
        .current_dir(&root)
        .args([
            "import",
            "--volume",
            "child-vol",
            "--include",
            "xmp",
            sub.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Combine with --apply
    maki()
        .current_dir(&root)
        .args(["volume", "combine", "child-vol", "parent-vol", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 recipes moved"));

    // Verify paths were rewritten
    let output = maki()
        .current_dir(&root)
        .args(["search", "photo"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    let show_output = maki()
        .current_dir(&root)
        .args(["show", "--json", short_id])
        .output()
        .unwrap();
    let show_stdout = String::from_utf8_lossy(&show_output.stdout);
    assert!(show_stdout.contains("sub/photo.jpg"), "variant path should be rewritten");
    assert!(show_stdout.contains("sub/photo.xmp"), "recipe path should be rewritten");
}

#[test]
fn volume_combine_same_volume_error() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["volume", "combine", "test-vol", "test-vol"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("same volume"));
}

#[test]
fn volume_combine_not_subdirectory_error() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();

    maki().current_dir(&root).arg("init").assert().success();

    // Create two sibling directories as separate volumes
    let vol_a = root.join("vol_a");
    let vol_b = root.join("vol_b");
    std::fs::create_dir_all(&vol_a).unwrap();
    std::fs::create_dir_all(&vol_b).unwrap();

    maki()
        .current_dir(&root)
        .args(["volume", "add", "vol-a", vol_a.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["volume", "add", "vol-b", vol_b.to_str().unwrap()])
        .assert()
        .success();

    // Neither is a subdirectory of the other
    maki()
        .current_dir(&root)
        .args(["volume", "combine", "vol-a", "vol-b"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a subdirectory"));
}

#[test]
fn volume_split_report_then_apply_moves_locations() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["sub/inner.jpg", "top.jpg"]);

    maki()
        .current_dir(&root)
        .args(["volume", "split", "test-vol", "subvol", "--path", "sub"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Would split 'test-vol': new volume 'subvol' with 1 locations, 0 recipes (1 assets, prefix 'sub/')")
                .and(predicate::str::contains("Run with --apply")),
        );
    maki()
        .current_dir(&root)
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("subvol").not());

    let out = maki()
        .current_dir(&root)
        .args(["--json", "volume", "split", "test-vol", "subvol", "--path", "sub", "--purpose", "archive", "--apply"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(parsed["apply"], true);
    assert_eq!(parsed["source_label"].as_str(), Some("test-vol"));
    assert_eq!(parsed["new_label"].as_str(), Some("subvol"));
    assert_eq!(parsed["path_prefix"].as_str(), Some("sub/"));
    assert_eq!(parsed["locations_moved"], 1);
    assert_eq!(parsed["assets_affected"], 1);
    assert!(!parsed["new_id"].as_str().unwrap().is_empty());

    maki()
        .current_dir(&root)
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("subvol").and(predicate::str::contains("test-vol")));

    // The moved asset now lives on the new volume with the prefix stripped.
    let loc = show_json(&root, &ids[0])["variants"][0]["locations"][0].clone();
    assert_eq!(loc["volume_label"].as_str(), Some("subvol"), "{loc}");
    assert_eq!(loc["relative_path"].as_str(), Some("inner.jpg"), "{loc}");
    assert_eq!(loc["volume_purpose"].as_str(), Some("archive"), "{loc}");

    // The untouched asset stays on the source volume.
    let loc = show_json(&root, &ids[1])["variants"][0]["locations"][0].clone();
    assert_eq!(loc["volume_label"].as_str(), Some("test-vol"), "{loc}");
    assert_eq!(loc["relative_path"].as_str(), Some("top.jpg"), "{loc}");

    // Files are still verifiable through the new mount point.
    maki()
        .current_dir(&root)
        .args(["verify", "--volume", "subvol"])
        .assert()
        .success();
}

#[test]
fn volume_split_unknown_source_fails() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    maki()
        .current_dir(&root)
        .args(["volume", "split", "ghost", "newvol", "--path", "sub"])
        .assert()
        .failure();
}

#[test]
fn volume_rename_updates_registry_and_catalog() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["renamed_vol.jpg"]);

    maki()
        .current_dir(&root)
        .args(["volume", "rename", "test-vol", "photos-2026"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Volume 'test-vol' renamed to 'photos-2026'"));

    maki()
        .current_dir(&root)
        .args(["volume", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("photos-2026").and(predicate::str::contains("test-vol").not()));

    // Locations resolve the new label (catalog side) and the old label
    // is gone from the registry.
    let loc = show_json(&root, &ids[0])["variants"][0]["locations"][0].clone();
    assert_eq!(loc["volume_label"].as_str(), Some("photos-2026"), "{loc}");
    maki()
        .current_dir(&root)
        .args(["volume", "rename", "test-vol", "whatever"])
        .assert()
        .failure();

    // JSON form, renaming back.
    let out = maki()
        .current_dir(&root)
        .args(["--json", "volume", "rename", "photos-2026", "test-vol"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(parsed["old_label"].as_str(), Some("photos-2026"));
    assert_eq!(parsed["new_label"].as_str(), Some("test-vol"));
    assert!(uuid::Uuid::parse_str(parsed["volume_id"].as_str().unwrap()).is_ok());
}
