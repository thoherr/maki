//! `collection` subcommands.

use crate::common::*;

#[test]
fn collection_create_and_list() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["collection", "create", "Portfolio", "--description", "Best shots"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created collection 'Portfolio'"));

    maki()
        .current_dir(&root)
        .args(["collection", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Portfolio"))
        .stdout(predicate::str::contains("0 assets"));
}

#[test]
fn collection_alias_works() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["col", "create", "Test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created collection 'Test'"));

    maki()
        .current_dir(&root)
        .args(["col", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Test"));
}

#[test]
fn collection_add_and_show() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Import a file
    create_test_file(&root, "col_photo.jpg", b"collection-test-photo");
    maki()
        .current_dir(&root)
        .args(["import", root.join("col_photo.jpg").to_str().unwrap()])
        .assert()
        .success();

    // Get the asset ID
    let output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "col_photo"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .to_string();
    assert!(!asset_id.is_empty());

    // Create collection and add asset
    maki()
        .current_dir(&root)
        .args(["col", "create", "MyPicks"])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["col", "add", "MyPicks", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added 1 asset"));

    // Show collection contents
    maki()
        .current_dir(&root)
        .args(["col", "show", "MyPicks"])
        .assert()
        .success()
        .stdout(predicate::str::contains("col_photo"));

    // List shows count
    maki()
        .current_dir(&root)
        .args(["col", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 assets"));
}

#[test]
fn collection_remove_and_delete() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Import a file
    create_test_file(&root, "rm_photo.jpg", b"collection-remove-test");
    maki()
        .current_dir(&root)
        .args(["import", root.join("rm_photo.jpg").to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "rm_photo"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .to_string();

    maki()
        .current_dir(&root)
        .args(["col", "create", "Temp"])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["col", "add", "Temp", &asset_id])
        .assert()
        .success();

    // Remove asset from collection
    maki()
        .current_dir(&root)
        .args(["col", "remove", "Temp", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed 1 asset"));

    // Delete collection
    maki()
        .current_dir(&root)
        .args(["col", "delete", "Temp"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deleted collection 'Temp'"));

    // List shows empty
    maki()
        .current_dir(&root)
        .args(["col", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No collections"));
}

#[test]
fn collection_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["--json", "col", "create", "JTest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\""))
        .stdout(predicate::str::contains("JTest"));

    maki()
        .current_dir(&root)
        .args(["--json", "col", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"asset_count\""));
}

#[test]
fn collection_search_filter() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Import two files with unique names
    create_test_file(&root, "alpha_col.jpg", b"alpha-collection-unique");
    create_test_file(&root, "beta_col.jpg", b"beta-collection-unique");
    maki()
        .current_dir(&root)
        .args(["import", root.join("alpha_col.jpg").to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", root.join("beta_col.jpg").to_str().unwrap()])
        .assert()
        .success();

    // Get alpha's asset ID
    let output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "alpha_col"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .to_string();
    assert!(!asset_id.is_empty(), "alpha_col asset should exist");

    // Create collection with only alpha
    maki()
        .current_dir(&root)
        .args(["col", "create", "Filtered"])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["col", "add", "Filtered", &asset_id])
        .assert()
        .success();

    // Search with collection filter should find only the one in the collection
    maki()
        .current_dir(&root)
        .args(["search", "collection:Filtered"])
        .assert()
        .success()
        .stdout(predicate::str::contains("alpha_col"))
        .stdout(predicate::str::contains("1 result"));
}
