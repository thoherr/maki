//! `edit`, `undo`/`history` and `fix-dates`.

use crate::common::*;

#[test]
fn edit_sets_name_and_description() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "editable.jpg", b"edit test data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Set name and description
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--name", "My Photo", "--description", "A lovely sunset"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Name: My Photo")
                .and(predicate::str::contains("Description: A lovely sunset")),
        );

    // Verify via show --json
    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["name"].as_str(), Some("My Photo"));
    assert_eq!(parsed["description"].as_str(), Some("A lovely sunset"));
}

#[test]
fn edit_sets_and_clears_rating() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "rated.jpg", b"rating test data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Set rating
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "4"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rating: \u{2605}\u{2605}\u{2605}\u{2605}\u{2606} (4/5)"));

    // Verify via show --json
    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["rating"].as_u64(), Some(4));

    // Clear rating
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--clear-rating"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rating: (none)"));

    // Verify cleared
    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed["rating"].is_null());
}

#[test]
fn edit_clears_name_and_description() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "clearable.jpg", b"clear test data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Set name and description
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--name", "Test Name", "--description", "Test Desc"])
        .assert()
        .success();

    // Clear them
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--clear-name", "--clear-description"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Name: (none)")
                .and(predicate::str::contains("Description: (none)")),
        );

    // Verify via show --json
    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed["name"].is_null());
    assert!(parsed["description"].is_null());
}

#[test]
fn edit_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "json_edit.jpg", b"json edit test");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "edit", &asset_id, "--name", "JSON Name", "--rating", "3"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["asset_id"].as_str().unwrap().len(), 36);
    assert_eq!(parsed["name"].as_str(), Some("JSON Name"));
    assert_eq!(parsed["rating"].as_u64(), Some(3));
}

#[test]
fn edit_no_flags_errors() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "noflags.jpg", b"no flags test");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    maki()
        .current_dir(&root)
        .args(["edit", &asset_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no edit flags provided"));
}

#[test]
fn edit_sets_and_clears_label() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "labeled.jpg", b"label test data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Set label
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--label", "Red"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Label: Red"));

    // Verify via show --json
    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .output()
        .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(parsed["color_label"].as_str(), Some("Red"));

    // Change to another label (case-insensitive)
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--label", "blue"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Label: Blue"));

    // Clear label
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--clear-label"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Label: (none)"));

    // Verify cleared
    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .output()
        .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert!(parsed["color_label"].is_null());
}

#[test]
fn edit_label_validates_color() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "badlabel.jpg", b"bad label test");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Invalid label should fail
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--label", "Magenta"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown color label"));
}

#[test]
fn undo_and_history_round_trip() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "undoable.jpg", b"undo test data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Set a rating — this records an undoable operation.
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "4"])
        .assert()
        .success();

    // `maki history <asset>` shows the operation.
    maki()
        .current_dir(&root)
        .args(["history", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("rating"));

    // Undo reverts the rating.
    maki()
        .current_dir(&root)
        .arg("undo")
        .assert()
        .success()
        .stdout(predicate::str::contains("Undid:"));

    // Rating is back to none.
    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .output()
        .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert!(parsed["rating"].is_null(), "rating should be cleared after undo");

    // The operation is gone from the active history.
    maki()
        .current_dir(&root)
        .args(["history", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("No edit history"));

    // Undo again → nothing to undo.
    maki()
        .current_dir(&root)
        .arg("undo")
        .assert()
        .success()
        .stdout(predicate::str::contains("Nothing to undo."));
}

#[test]
fn fix_dates_dry_run_then_apply_restores_oldest_date() {
    // The fixture has no EXIF, so import sets created_at from the file
    // mtime. Pushing created_at into the future via `edit --date` makes
    // the mtime the oldest candidate and fix-dates must pull it back.
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["dated.jpg"]);

    maki()
        .current_dir(&root)
        .args(["edit", &ids[0], "--date", "2099-06-15"])
        .assert()
        .success();
    assert!(show_json(&root, &ids[0])["created_at"].as_str().unwrap().starts_with("2099"));

    maki()
        .current_dir(&root)
        .args(["--log", "fix-dates"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Fix-dates: 1 checked, 1 fixed, 0 already correct")
                .and(predicate::str::contains("Run with --apply")),
        )
        .stderr(predicate::str::contains("would fix: 2099-06-15"));

    // Dry run must not touch the asset.
    assert!(show_json(&root, &ids[0])["created_at"].as_str().unwrap().starts_with("2099"));

    maki()
        .current_dir(&root)
        .args(["fix-dates", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Fix-dates: 1 checked, 1 fixed, 0 already correct"));

    let created = show_json(&root, &ids[0])["created_at"].as_str().unwrap().to_string();
    assert!(!created.starts_with("2099"), "created_at must be reset to the file mtime: {created}");

    // Idempotent: a second pass finds nothing to do.
    maki()
        .current_dir(&root)
        .arg("fix-dates")
        .assert()
        .success()
        .stdout(predicate::str::contains("Fix-dates: 1 checked, 0 fixed, 1 already correct"));
}

#[test]
fn fix_dates_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["dated_json.jpg", "untouched.jpg"]);

    maki()
        .current_dir(&root)
        .args(["edit", &ids[0], "--date", "2099-01-01"])
        .assert()
        .success();

    let out = maki()
        .current_dir(&root)
        .args(["--json", "fix-dates", "--asset", &ids[0]])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(parsed["dry_run"], true);
    assert_eq!(parsed["checked"], 1);
    assert_eq!(parsed["fixed"], 1);
    assert_eq!(parsed["already_correct"], 0);
    assert_eq!(parsed["errors"].as_array().unwrap().len(), 0);

    let out = maki()
        .current_dir(&root)
        .args(["--json", "fix-dates", "--apply"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed["dry_run"], false);
    assert_eq!(parsed["checked"], 2);
    assert_eq!(parsed["fixed"], 1);
    assert_eq!(parsed["already_correct"], 1);
}
