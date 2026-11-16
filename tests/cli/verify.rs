//! `verify`, `doctor`, `fix-roles` and `rebuild-catalog`.

use crate::common::*;

#[test]
fn rebuild_catalog_restores_data() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "keeper.jpg", b"keeper data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Verify it's searchable
    maki()
        .current_dir(&root)
        .args(["search", "keeper"])
        .assert()
        .success()
        .stdout(predicate::str::contains("keeper"));

    // Rebuild
    maki()
        .current_dir(&root)
        .arg("rebuild-catalog")
        .assert()
        .success()
        .stdout(predicate::str::contains("Rebuild complete"));

    // Still searchable after rebuild
    maki()
        .current_dir(&root)
        .args(["search", "keeper"])
        .assert()
        .success()
        .stdout(predicate::str::contains("keeper"));
}

#[test]
fn rebuild_catalog_log_prints_per_sidecar_progress() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "logged.jpg", b"logged data");
    maki().current_dir(&root).args(["import", file.to_str().unwrap()]).assert().success();

    // --log: one stderr line per sidecar (id + name); summary still on stdout.
    maki()
        .current_dir(&root)
        .args(["--log", "rebuild-catalog"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rebuild complete"))
        .stderr(predicate::str::contains("logged"));

    // Without --log the per-sidecar lines are absent.
    maki()
        .current_dir(&root)
        .arg("rebuild-catalog")
        .assert()
        .success()
        .stderr(predicate::str::contains("logged").not());
}

#[test]
fn verify_all_passes() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "intact.jpg", b"intact file data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .arg("verify")
        .assert()
        .success()
        .stdout(predicate::str::contains("verified"));
}

#[test]
fn verify_detects_corruption() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "corrupt.jpg", b"original data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Corrupt the file
    std::fs::write(&file, b"corrupted data!!!").unwrap();

    maki()
        .current_dir(&root)
        .arg("verify")
        .assert()
        .failure()
        .stdout(predicate::str::contains("FAILED"));
}

#[test]
fn verify_with_volume_flag() {
    let dir = tempdir().unwrap();
    let (root, vol1, _vol2) = init_two_volumes(dir.path());
    let file = create_test_file(&vol1, "vol_verify.jpg", b"vol verify data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["verify", "--volume", "vol1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("verified"));
}

#[test]
fn verify_specific_path() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file1 = create_test_file(&root, "file_a.jpg", b"data a");
    let file2 = create_test_file(&root, "file_b.jpg", b"data b");

    maki()
        .current_dir(&root)
        .args(["import", file1.to_str().unwrap(), file2.to_str().unwrap()])
        .assert()
        .success();

    // Verify only file_a
    maki()
        .current_dir(&root)
        .args(["verify", file1.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 verified"));
}

#[test]
fn verify_path_recognizes_recipe_sidecars() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    create_test_file(&photos, "DSC_500.nef", b"raw image for verify");
    create_test_file(&photos, "DSC_500.xmp", b"xmp sidecar for verify");

    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1 imported")
                .and(predicate::str::contains("1 recipe")),
        );

    // Verify the whole directory — both the NEF and XMP should be verified, not untracked
    maki()
        .current_dir(&root)
        .args(["verify", photos.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 verified"));
}

#[test]
fn verify_recipe_modification_not_failure() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    create_test_file(&photos, "DSC_400.nef", b"raw image for verify modify test");
    create_test_file(&photos, "DSC_400.xmp", b"xmp original content");

    // Import
    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1 imported")
                .and(predicate::str::contains("1 recipe")),
        );

    // Modify XMP on disk
    std::fs::write(photos.join("DSC_400.xmp"), b"xmp modified content").unwrap();

    // Verify — should report "modified", NOT "FAILED", and exit 0
    maki()
        .current_dir(&root)
        .arg("verify")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("modified")
                .and(predicate::str::contains("FAILED").not()),
        );
}

#[test]
fn fix_roles_dry_run_reports() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    create_test_file(&root, "photos/DSC_100.ARW", b"raw-fixroles-1");
    create_test_file(&root, "photos/DSC_100.JPG", b"jpg-fixroles-1");
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    // Since auto-grouping now sets roles correctly, fix-roles should report already correct
    maki()
        .current_dir(&root)
        .args(["fix-roles"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already correct"));
}

#[test]
fn fix_roles_apply_corrects_roles() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Import RAW and JPG from separate directories → 2 separate assets, both Original
    let raw_dir = root.join("raw");
    let jpg_dir = root.join("jpg");
    std::fs::create_dir_all(&raw_dir).unwrap();
    std::fs::create_dir_all(&jpg_dir).unwrap();
    std::fs::write(raw_dir.join("DSC_200.ARW"), b"raw-fixroles-apply").unwrap();
    std::fs::write(jpg_dir.join("DSC_200.JPG"), b"jpg-fixroles-apply").unwrap();

    maki()
        .current_dir(&root)
        .args(["import", raw_dir.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", jpg_dir.to_str().unwrap()])
        .assert()
        .success();

    // Auto-group to merge them into one asset
    maki()
        .current_dir(&root)
        .args(["auto-group", "--apply"])
        .assert()
        .success();

    // After auto-group the JPG should already be Export — fix-roles reports 0 fixed
    maki()
        .current_dir(&root)
        .args(["fix-roles"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 fixed"));
}

#[test]
fn fix_roles_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    create_test_file(&root, "photos/DSC_300.ARW", b"raw-fixroles-json");
    create_test_file(&root, "photos/DSC_300.JPG", b"jpg-fixroles-json");
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "fix-roles"])
        .output()
        .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert!(parsed["checked"].is_number());
    assert!(parsed["fixed"].is_number());
    assert!(parsed["variants_fixed"].is_number());
    assert!(parsed["already_correct"].is_number());
    assert_eq!(parsed["dry_run"].as_bool(), Some(true));
}

#[test]
fn verify_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "check.jpg", b"verify json test data");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "verify"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).expect("valid JSON");
    assert!(json["verified"].as_u64().unwrap() > 0, "should verify at least one file");
}

#[test]
fn verify_max_age_skips_recent() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "recent.jpg", b"max-age skip test data");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    // First verify sets timestamps
    maki()
        .current_dir(&root)
        .arg("verify")
        .assert()
        .success();

    // Second verify with --max-age should skip recently verified
    let output = maki()
        .current_dir(&root)
        .args(["--json", "verify", "--max-age", "1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).expect("valid JSON");
    assert!(json["skipped_recent"].as_u64().unwrap() > 0, "should skip recently verified");
    assert_eq!(json["verified"].as_u64().unwrap(), 0, "nothing should need re-verifying");
}

#[test]
fn verify_force_overrides_max_age() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "force.jpg", b"force override test data");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    // First verify sets timestamps
    maki()
        .current_dir(&root)
        .arg("verify")
        .assert()
        .success();

    // --force should override --max-age and re-verify everything
    let output = maki()
        .current_dir(&root)
        .args(["--json", "verify", "--force", "--max-age", "1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).expect("valid JSON");
    assert!(json["verified"].as_u64().unwrap() > 0, "should re-verify with --force");
    assert_eq!(json["skipped_recent"].as_u64().unwrap(), 0, "nothing should be skipped with --force");
}

#[test]
fn verify_recipe_verified_at_round_trip() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    create_test_file(&photos, "DSC_500.nef", b"raw image for recipe verify round trip");
    create_test_file(&photos, "DSC_500.xmp", b"xmp recipe for verify round trip");

    // Import NEF + XMP (recipe attached)
    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1 imported")
                .and(predicate::str::contains("1 recipe")),
        );

    // First verify — sets verified_at on both variant location and recipe location
    maki()
        .current_dir(&root)
        .arg("verify")
        .assert()
        .success();

    // Second verify with --max-age — both should be skipped as recently verified
    let output = maki()
        .current_dir(&root)
        .args(["--json", "verify", "--max-age", "1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).expect("valid JSON");
    assert_eq!(
        json["skipped_recent"].as_u64().unwrap(),
        2,
        "both variant location and recipe location should be skipped"
    );
    assert_eq!(json["verified"].as_u64().unwrap(), 0, "nothing should need re-verifying");
}

#[test]
fn doctor_clean_catalog_passes() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    import_and_get_ids(&root, &["clean_a.jpg", "clean_b.jpg"]);

    maki()
        .current_dir(&root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("Doctor: no issues found — 2 assets checked."));
}

#[test]
fn doctor_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    import_and_get_ids(&root, &["doc_json.jpg"]);

    let out = maki()
        .current_dir(&root)
        .args(["--json", "doctor"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(parsed["sidecars_total"], 1);
    assert_eq!(parsed["catalog_assets_total"], 1);
    assert_eq!(parsed["assets_checked"], 1);
    assert_eq!(parsed["missing_in_catalog"].as_array().unwrap().len(), 0);
    assert_eq!(parsed["missing_sidecar"].as_array().unwrap().len(), 0);
    assert_eq!(parsed["mismatched"].as_array().unwrap().len(), 0);
    assert_eq!(parsed["sampled"], false);
}

#[test]
fn doctor_detects_phantom_row_and_repairs_it() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["phantom.jpg", "healthy.jpg"]);

    // Remove one sidecar behind the catalog's back: the SQLite row is
    // now a phantom.
    std::fs::remove_file(sidecar_path(&root, &ids[0])).unwrap();

    // Unresolved issues drive the exit status (1, like `verify`).
    maki()
        .current_dir(&root)
        .arg("doctor")
        .assert()
        .code(1)
        .stdout(
            predicate::str::contains("Doctor: 1 issue(s) found")
                .and(predicate::str::contains("1 catalog row(s) without a sidecar"))
                .and(predicate::str::contains("maki doctor --repair")),
        );

    let out = maki()
        .current_dir(&root)
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "--json doctor keeps the exit status");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed["missing_sidecar"].as_array().unwrap().len(), 1);
    assert_eq!(parsed["missing_sidecar"][0].as_str(), Some(ids[0].as_str()));

    maki()
        .current_dir(&root)
        .args(["doctor", "--repair"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Repaired"));

    maki()
        .current_dir(&root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("Doctor: no issues found — 1 assets checked."));
}
