//! `delete` and the trash.

use crate::common::*;

#[test]
fn delete_report_only_by_default() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["del_report.jpg"]);

    maki()
        .current_dir(&root)
        .args(["delete", &ids[0]])
        .assert()
        .success()
        .stdout(predicate::str::contains("would be deleted"));

    // Asset should still exist
    maki()
        .current_dir(&root)
        .args(["show", &ids[0]])
        .assert()
        .success();
}

#[test]
fn delete_apply_removes_asset() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["del_apply.jpg"]);

    maki()
        .current_dir(&root)
        .args(["delete", "--apply", &ids[0]])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 deleted"));

    // Asset should be gone
    maki()
        .current_dir(&root)
        .args(["show", &ids[0]])
        .assert()
        .failure();
}

#[test]
fn delete_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["del_json.jpg"]);

    let output = maki()
        .current_dir(&root)
        .args(["--json", "delete", "--apply", &ids[0]])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).expect("valid JSON");
    assert_eq!(json["deleted"].as_u64().unwrap(), 1);
    assert!(!json["dry_run"].as_bool().unwrap());
    assert!(json["not_found"].as_array().unwrap().is_empty());
}

#[test]
fn delete_prefix_matching() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["del_prefix.jpg"]);

    // Use first 8 chars as prefix
    let prefix = &ids[0][..8];

    maki()
        .current_dir(&root)
        .args(["delete", "--apply", prefix])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 deleted"));

    maki()
        .current_dir(&root)
        .args(["show", &ids[0]])
        .assert()
        .failure();
}

#[test]
fn delete_not_found() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["--json", "delete", "--apply", "nonexistent-id-12345"])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "delete", "--apply", "nonexistent-id-12345"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).expect("valid JSON");
    assert_eq!(json["deleted"].as_u64().unwrap(), 0);
    assert_eq!(json["not_found"].as_array().unwrap().len(), 1);
}

#[test]
fn delete_multiple_assets() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["del_multi_a.jpg", "del_multi_b.jpg"]);

    maki()
        .current_dir(&root)
        .args(["delete", "--apply", &ids[0], &ids[1]])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 deleted"));

    // Both should be gone
    for id in &ids {
        maki()
            .current_dir(&root)
            .args(["show", id])
            .assert()
            .failure();
    }
}

#[test]
fn delete_stdin_piping() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["del_stdin.jpg"]);

    maki()
        .current_dir(&root)
        .args(["delete", "--apply"])
        .write_stdin(format!("{}\n", ids[0]))
        .assert()
        .success()
        .stdout(predicate::str::contains("1 deleted"));

    maki()
        .current_dir(&root)
        .args(["show", &ids[0]])
        .assert()
        .failure();
}

#[test]
fn delete_remove_files() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "del_rm.jpg", b"delete-me-file");
    let file_path = file.clone();

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "del_rm"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8(output.stdout).unwrap().trim().to_string();

    assert!(file_path.exists(), "file should exist before delete");

    maki()
        .current_dir(&root)
        .args(["delete", "--apply", "--remove-files", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 deleted"))
        .stdout(predicate::str::contains("files removed"));

    assert!(!file_path.exists(), "file should be removed from disk");
}

#[test]
fn delete_remove_files_goes_to_trash_and_restores() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "del_trash.jpg", b"trash-me-file");
    let file_path = file.clone();

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "del_trash"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8(output.stdout).unwrap().trim().to_string();

    maki()
        .current_dir(&root)
        .args(["delete", "--apply", "--remove-files", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved to the trash"));

    assert!(!file_path.exists(), "file removed from original location");
    assert!(root.join(".trash").exists(), "trash directory created");

    // List shows the file with a restore key
    let list = maki()
        .current_dir(&root)
        .args(["trash", "list"])
        .output()
        .unwrap();
    let list_out = String::from_utf8(list.stdout).unwrap();
    assert!(list_out.contains("del_trash.jpg"), "list output: {list_out}");
    let key = list_out
        .lines()
        .find(|l| l.contains("del_trash.jpg"))
        .and_then(|l| l.split_whitespace().last())
        .unwrap()
        .to_string();

    // Restore puts the file back on the volume
    maki()
        .current_dir(&root)
        .args(["trash", "restore", &key])
        .assert()
        .success()
        .stdout(predicate::str::contains("Restored:"));
    assert!(file_path.exists(), "file restored to original path");
    assert_eq!(std::fs::read(&file_path).unwrap(), b"trash-me-file");

    // Empty --all clears whatever remains
    maki()
        .current_dir(&root)
        .args(["trash", "empty", "--all"])
        .assert()
        .success();
    let list2 = maki()
        .current_dir(&root)
        .args(["trash", "list"])
        .output()
        .unwrap();
    assert!(String::from_utf8(list2.stdout).unwrap().contains("Trash is empty"));
}

#[test]
fn delete_no_trash_deletes_permanently() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "del_notrash.jpg", b"gone-for-good");
    let file_path = file.clone();

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();
    let output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "del_notrash"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8(output.stdout).unwrap().trim().to_string();

    maki()
        .current_dir(&root)
        .args(["delete", "--apply", "--remove-files", "--no-trash", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved to the trash").not());

    assert!(!file_path.exists(), "file removed from disk");
    assert!(!root.join(".trash").exists(), "no trash dir with --no-trash");
}

#[test]
fn delete_remove_files_requires_apply() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["delete", "--remove-files", "some-id"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--remove-files requires --apply"));
}

#[test]
fn delete_removes_from_collection() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["del_col.jpg"]);

    // Add to a collection
    maki()
        .current_dir(&root)
        .args(["col", "create", "TestCol"])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["col", "add", "TestCol", &ids[0]])
        .assert()
        .success();

    // Delete the asset
    maki()
        .current_dir(&root)
        .args(["delete", "--apply", &ids[0]])
        .assert()
        .success();

    // Collection should now be empty
    maki()
        .current_dir(&root)
        .args(["col", "show", "TestCol"])
        .assert()
        .success()
        .stdout(predicate::str::contains("empty"));
}

#[test]
fn delete_removes_from_stack() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["del_stack_a.jpg", "del_stack_b.jpg", "del_stack_c.jpg"]);

    // Create a stack with all three
    maki()
        .current_dir(&root)
        .args(["stack", "create", &ids[0], &ids[1], &ids[2]])
        .assert()
        .success();

    // Delete one member
    maki()
        .current_dir(&root)
        .args(["delete", "--apply", &ids[1]])
        .assert()
        .success();

    // Stack should still exist with 2 members
    maki()
        .current_dir(&root)
        .args(["stack", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 assets"));
}
