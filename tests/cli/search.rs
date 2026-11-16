//! `search` filters and output formats, `saved-search`.

use crate::common::*;

#[test]
fn search_finds_imported_asset() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "sunset.jpg", b"sunset data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["search", "sunset"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sunset"));
}

#[test]
fn search_format_ids_outputs_uuids() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photo.jpg", b"ids-format-test");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "--format=ids", "type:image"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 1);
    // Should be a UUID (36 chars with hyphens)
    assert_eq!(lines[0].len(), 36, "Expected full UUID, got: {}", lines[0]);
    // Should NOT have "result(s)" count
    assert!(!stdout.contains("result(s)"));
}

#[test]
fn search_quiet_shorthand() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photo.jpg", b"quiet-test");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].len(), 36);
}

#[test]
fn search_format_json_outputs_valid_json() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photo.jpg", b"json-format-test");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "--format=json", "type:image"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let arr = parsed.as_array().expect("should be array");
    assert_eq!(arr.len(), 1);
    assert!(arr[0]["asset_id"].is_string());
    assert!(arr[0]["content_hash"].is_string());
    assert!(arr[0]["tags"].is_array());
}

#[test]
fn search_format_template_renders_placeholders() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "sunset.jpg", b"template-test");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "--format={short_id}\t{filename}\t{format}", "type:image"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 1);
    let parts: Vec<&str> = lines[0].split('\t').collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].len(), 8, "short_id should be 8 chars");
    assert_eq!(parts[1], "sunset.jpg");
    assert_eq!(parts[2], "jpg");
}

#[test]
fn search_json_global_flag() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photo.jpg", b"global-json-test");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "search", "type:image"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed.is_array());
}

#[test]
fn search_orphan_filter() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "orphan_test.jpg", b"orphan data");

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
    assert!(!asset_id.is_empty(), "should find imported asset");

    // Delete the file, then cleanup (report only) to remove just locations
    std::fs::remove_file(&file).unwrap();

    // Use report-only cleanup — doesn't remove anything
    maki()
        .current_dir(&root)
        .args(["cleanup"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 stale"));

    // Manually remove the stale location via cleanup --apply but the orphan
    // asset is also removed now. So test orphan search by using --json cleanup
    // without --apply first, then verifying the orphan count.
    // For the orphan:true search test, we need an asset with no locations.
    // Use SQLite directly to delete the file_location.
    let db_path = root.join("catalog.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute("DELETE FROM file_locations", []).unwrap();
    drop(conn);

    // Now search orphan:true should find the asset
    maki()
        .current_dir(&root)
        .args(["search", "orphan:true"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&asset_id[..8]));
}

#[test]
fn search_orphan_filter_excludes_located() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "located.jpg", b"located data");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    // File still exists on disk, locations intact — orphan:true should return nothing
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "orphan:true"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.trim().is_empty(), "orphan:true should find nothing when locations exist");
}

#[test]
fn search_missing_filter() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "missing_test.jpg", b"missing data");

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
    assert!(!asset_id.is_empty(), "should find imported asset");

    // Delete the file but DON'T run cleanup (location record still exists)
    std::fs::remove_file(&file).unwrap();

    // missing:true should find it (file missing from disk but location in catalog)
    maki()
        .current_dir(&root)
        .args(["search", "missing:true"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&asset_id[..8]));
}

#[test]
fn search_missing_filter_excludes_present() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "present.jpg", b"present data");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    // File still exists — missing:true should find nothing
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "missing:true"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.trim().is_empty(), "missing:true should find nothing when files exist");
}

#[test]
fn search_stale_filter() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "stale_test.jpg", b"stale data");

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
    assert!(!asset_id.is_empty(), "should find imported asset");

    // Never explicitly verified, so verified_at is NULL — stale:0 should match
    maki()
        .current_dir(&root)
        .args(["search", "stale:0"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&asset_id[..8]));
}

#[test]
fn search_volume_none_filter() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "volnone_test.jpg", b"volnone data");

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
    assert!(!asset_id.is_empty(), "should find imported asset");

    // Remove locations directly via SQLite (not cleanup --apply, which also removes orphaned assets)
    let db_path = root.join("catalog.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute("DELETE FROM file_locations", []).unwrap();
    drop(conn);

    // volume:none should find the asset (no locations on any online volume)
    maki()
        .current_dir(&root)
        .args(["search", "volume:none"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&asset_id[..8]));
}

#[test]
fn search_volume_label_filter() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "vol_label_test.jpg", b"vol label data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Search by volume label should find the asset
    maki()
        .current_dir(&root)
        .args(["search", "volume:test-vol"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 result"));

    // Case-insensitive match
    maki()
        .current_dir(&root)
        .args(["search", "volume:Test-Vol"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 result"));

    // Unknown volume should error
    maki()
        .current_dir(&root)
        .args(["search", "volume:nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown volume"));

    // Negated volume should exclude (use -- to prevent clap flag parsing)
    maki()
        .current_dir(&root)
        .args(["search", "--", "-volume:test-vol"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results"));
}

#[test]
fn saved_search_save_and_list() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Save a search
    maki()
        .current_dir(&root)
        .args(["saved-search", "save", "Landscapes", "type:image tag:landscape rating:4+"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Saved search 'Landscapes'"));

    // List shows it
    maki()
        .current_dir(&root)
        .args(["saved-search", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Landscapes"))
        .stdout(predicate::str::contains("type:image tag:landscape rating:4+"));
}

#[test]
fn saved_search_alias_works() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["ss", "save", "Test", "type:video"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Saved search 'Test'"));

    maki()
        .current_dir(&root)
        .args(["ss", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Test"));
}

#[test]
fn saved_search_run() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Import a file
    create_test_file(&root, "photo.jpg", b"saved-search-test-photo");
    maki()
        .current_dir(&root)
        .args(["import", root.join("photo.jpg").to_str().unwrap()])
        .assert()
        .success();

    // Save and run a search that matches
    maki()
        .current_dir(&root)
        .args(["ss", "save", "All Images", "type:image"])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["ss", "run", "All Images"])
        .assert()
        .success()
        .stdout(predicate::str::contains("photo"));
}

#[test]
fn saved_search_run_not_found() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["ss", "run", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no saved search found matching"));
}

#[test]
fn saved_search_delete() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["ss", "save", "ToDelete", "type:video"])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["ss", "delete", "ToDelete"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deleted saved search 'ToDelete'"));

    // List is now empty
    maki()
        .current_dir(&root)
        .args(["ss", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No saved searches"));
}

#[test]
fn saved_search_delete_not_found() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["ss", "delete", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no saved search found matching"));
}

#[test]
fn saved_search_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["--json", "ss", "save", "Test", "type:image"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\""))
        .stdout(predicate::str::contains("\"saved\""));

    maki()
        .current_dir(&root)
        .args(["--json", "ss", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\""))
        .stdout(predicate::str::contains("Test"));
}

#[test]
fn saved_search_replace_existing() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["ss", "save", "My Search", "type:image"])
        .assert()
        .success();

    // Save again with same name — should replace
    maki()
        .current_dir(&root)
        .args(["ss", "save", "My Search", "type:video", "--sort", "name_asc"])
        .assert()
        .success();

    // List should show updated query
    maki()
        .current_dir(&root)
        .args(["ss", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type:video"))
        .stdout(predicate::str::contains("name_asc"));
}

#[test]
fn search_path_filter() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Create files in different subdirectories
    let file_a = create_test_file(&root, "Capture/2026-02-22/DSC_001.jpg", b"photo a");
    let file_b = create_test_file(&root, "Capture/2026-02-22/DSC_002.jpg", b"photo b");
    let file_c = create_test_file(&root, "Archive/old/sunset.jpg", b"photo c");

    maki()
        .current_dir(&root)
        .args(["import", file_a.to_str().unwrap(), file_b.to_str().unwrap(), file_c.to_str().unwrap()])
        .assert()
        .success();

    // path: filter should match only files under Capture/2026-02-22
    maki()
        .current_dir(&root)
        .args(["search", "path:Capture/2026-02-22"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DSC_001"))
        .stdout(predicate::str::contains("DSC_002"))
        .stdout(predicate::str::contains("2 result"));

    // path: filter for Archive should match only the sunset file
    maki()
        .current_dir(&root)
        .args(["search", "path:Archive/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sunset"))
        .stdout(predicate::str::contains("1 result"));

    // path: with no match
    maki()
        .current_dir(&root)
        .args(["search", "path:Nonexistent/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results found"));
}

#[test]
fn search_path_absolute_normalizes_to_relative() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Create files in a subdirectory
    let file_a = create_test_file(&root, "photos/DSC_001.jpg", b"abs path photo a");
    let file_b = create_test_file(&root, "photos/DSC_002.jpg", b"abs path photo b");
    let file_c = create_test_file(&root, "other/sunset.jpg", b"abs path photo c");

    maki()
        .current_dir(&root)
        .args(["import", file_a.to_str().unwrap(), file_b.to_str().unwrap(), file_c.to_str().unwrap()])
        .assert()
        .success();

    // Search with absolute path should find the same results as relative
    let abs_path = format!("path:{}/photos", root.display());
    maki()
        .current_dir(&root)
        .args(["search", &abs_path])
        .assert()
        .success()
        .stdout(predicate::str::contains("DSC_001"))
        .stdout(predicate::str::contains("DSC_002"))
        .stdout(predicate::str::contains("2 result"));

    // Verify relative path works identically
    maki()
        .current_dir(&root)
        .args(["search", "path:photos"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DSC_001"))
        .stdout(predicate::str::contains("DSC_002"))
        .stdout(predicate::str::contains("2 result"));

    // Bogus absolute path should return nothing
    maki()
        .current_dir(&root)
        .args(["search", "path:/nonexistent/volume/photos"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results found"));

    // ./ relative to cwd should resolve and normalize
    maki()
        .current_dir(root.join("photos"))
        .args(["search", "path:./"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DSC_001"))
        .stdout(predicate::str::contains("DSC_002"))
        .stdout(predicate::str::contains("2 result"));

    // ../ relative to cwd should resolve and normalize
    maki()
        .current_dir(root.join("photos"))
        .args(["search", "path:../other"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sunset"))
        .stdout(predicate::str::contains("1 result"));
}

#[test]
fn search_copies_filter() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Set up a second volume
    let dir2 = tempdir().unwrap();
    let vol2_path = dir2.path().canonicalize().unwrap();
    maki()
        .current_dir(&root)
        .args(["volume", "add", "vol2", vol2_path.to_str().unwrap()])
        .assert()
        .success();

    // File A: only on one volume (1 copy)
    let file_a = create_test_file(&root, "single.jpg", b"single copy data");
    maki()
        .current_dir(&root)
        .args(["import", file_a.to_str().unwrap()])
        .assert()
        .success();

    // File B: on both volumes (2 copies)
    let content_b = b"two copy data bytes";
    let file_b1 = create_test_file(&root, "multi_orig.jpg", content_b);
    let file_b2 = create_test_file(&vol2_path, "multi_backup.jpg", content_b);
    maki()
        .current_dir(&root)
        .args(["import", file_b1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", "--volume", "vol2", file_b2.to_str().unwrap()])
        .assert()
        .success();

    // copies:1 should only return the single-copy asset
    let output = maki()
        .current_dir(&root)
        .args(["--json", "search", "copies:1"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let results: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(results.len(), 1, "copies:1 should return exactly 1 result");
    assert_eq!(results[0]["original_filename"], "single.jpg");

    // copies:2 should only return the two-copy asset
    let output = maki()
        .current_dir(&root)
        .args(["--json", "search", "copies:2"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let results: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(results.len(), 1, "copies:2 should return exactly 1 result");
    assert_eq!(results[0]["original_filename"], "multi_orig.jpg");

    // copies:2+ should return the two-copy asset
    let output = maki()
        .current_dir(&root)
        .args(["--json", "search", "copies:2+"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let results: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(results.len(), 1, "copies:2+ should return exactly 1 result");
    assert_eq!(results[0]["original_filename"], "multi_orig.jpg");

    // copies:1+ should return both
    let output = maki()
        .current_dir(&root)
        .args(["--json", "search", "copies:1+"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let results: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(results.len(), 2, "copies:1+ should return 2 results");
    let filenames: Vec<&str> = results.iter()
        .map(|r| r["original_filename"].as_str().unwrap())
        .collect();
    assert!(filenames.contains(&"single.jpg"));
    assert!(filenames.contains(&"multi_orig.jpg"));
}

#[test]
fn search_negated_tag_excludes_matching() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let f1 = create_test_file(&root, "good.jpg", b"good image data");
    let f2 = create_test_file(&root, "bad.jpg", b"bad image data");

    maki().current_dir(&root).args(["import", f1.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", f2.to_str().unwrap()]).assert().success();

    // Tag f2 as "rejected"
    let output = maki().current_dir(&root).args(["search", "-q", "bad"]).output().unwrap();
    let bad_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    maki().current_dir(&root).args(["tag", &bad_id, "rejected"]).assert().success();

    // Tag f1 as "keeper"
    let output = maki().current_dir(&root).args(["search", "-q", "good"]).output().unwrap();
    let good_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    maki().current_dir(&root).args(["tag", &good_id, "keeper"]).assert().success();

    // -tag:rejected should find good but not bad
    // Use `--` to separate flags from the query containing `-`
    let output = maki().current_dir(&root).args(["search", "-q", "--", "-tag:rejected"]).output().unwrap();
    let ids = String::from_utf8_lossy(&output.stdout);
    assert!(ids.contains(&good_id), "should include non-rejected asset");
    assert!(!ids.contains(&bad_id), "should exclude rejected asset");
}

#[test]
fn search_negated_format_excludes_matching() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let f1 = create_test_file(&root, "photo.jpg", b"jpeg data here");
    let f2 = create_test_file(&root, "raw.arw", b"raw data here");

    maki().current_dir(&root).args(["import", f1.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", f2.to_str().unwrap()]).assert().success();

    // -format:arw should exclude the raw file
    let output = maki().current_dir(&root).args(["search", "--json", "--", "-format:arw"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("photo.jpg"), "should include jpg");
    assert!(!stdout.contains("raw.arw"), "should exclude arw");
}

#[test]
fn search_comma_or_format() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let f1 = create_test_file(&root, "a.jpg", b"jpg data");
    let f2 = create_test_file(&root, "b.png", b"png data");
    let f3 = create_test_file(&root, "c.tif", b"tif data");

    maki().current_dir(&root).args(["import", f1.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", f2.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", f3.to_str().unwrap()]).assert().success();

    // format:jpg,png should find both jpg and png but not tif
    let output = maki().current_dir(&root).args(["search", "--json", "format:jpg,png"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("a.jpg"), "should include jpg");
    assert!(stdout.contains("b.png"), "should include png");
    assert!(!stdout.contains("c.tif"), "should exclude tif");
}

#[test]
fn search_repeated_tags_and() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let f1 = create_test_file(&root, "both.jpg", b"both tags data");
    let f2 = create_test_file(&root, "onlya.jpg", b"only a tag data");

    maki().current_dir(&root).args(["import", f1.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", f2.to_str().unwrap()]).assert().success();

    let output = maki().current_dir(&root).args(["search", "-q", "both"]).output().unwrap();
    let both_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let output = maki().current_dir(&root).args(["search", "-q", "onlya"]).output().unwrap();
    let onlya_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Tag both assets with "landscape"
    maki().current_dir(&root).args(["tag", &both_id, "landscape"]).assert().success();
    maki().current_dir(&root).args(["tag", &onlya_id, "landscape"]).assert().success();

    // Tag only the first with "sunset"
    maki().current_dir(&root).args(["tag", &both_id, "sunset"]).assert().success();

    // tag:landscape tag:sunset should only match the asset with both tags
    let output = maki().current_dir(&root).args(["search", "-q", "tag:landscape tag:sunset"]).output().unwrap();
    let ids = String::from_utf8_lossy(&output.stdout);
    assert!(ids.contains(&both_id), "should include asset with both tags");
    assert!(!ids.contains(&onlya_id), "should exclude asset with only one tag");
}

#[test]
fn search_comma_or_tag() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let f1 = create_test_file(&root, "alice.jpg", b"alice data");
    let f2 = create_test_file(&root, "bob.jpg", b"bob data");
    let f3 = create_test_file(&root, "carol.jpg", b"carol data");

    maki().current_dir(&root).args(["import", f1.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", f2.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", f3.to_str().unwrap()]).assert().success();

    let output = maki().current_dir(&root).args(["search", "-q", "alice"]).output().unwrap();
    let alice_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let output = maki().current_dir(&root).args(["search", "-q", "bob"]).output().unwrap();
    let bob_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let output = maki().current_dir(&root).args(["search", "-q", "carol"]).output().unwrap();
    let carol_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    maki().current_dir(&root).args(["tag", &alice_id, "tagA"]).assert().success();
    maki().current_dir(&root).args(["tag", &bob_id, "tagB"]).assert().success();
    maki().current_dir(&root).args(["tag", &carol_id, "tagC"]).assert().success();

    // tag:tagA,tagB should find alice and bob but not carol
    let output = maki().current_dir(&root).args(["search", "-q", "tag:tagA,tagB"]).output().unwrap();
    let ids = String::from_utf8_lossy(&output.stdout);
    assert!(ids.contains(&alice_id), "should include alice (tagA)");
    assert!(ids.contains(&bob_id), "should include bob (tagB)");
    assert!(!ids.contains(&carol_id), "should exclude carol (tagC)");
}

#[test]
fn search_negated_text_excludes_matching() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let f1 = create_test_file(&root, "sunset_beach.jpg", b"beach data");
    let f2 = create_test_file(&root, "sunset_mountain.jpg", b"mountain data");

    maki().current_dir(&root).args(["import", f1.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", f2.to_str().unwrap()]).assert().success();

    // "sunset -mountain" should find beach but not mountain
    let output = maki().current_dir(&root).args(["search", "--json", "--", "sunset -mountain"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sunset_beach"), "should include beach");
    assert!(!stdout.contains("sunset_mountain"), "should exclude mountain");
}
