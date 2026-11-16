//! Catalog lifecycle and read-only commands: `init`, `migrate`, `show`, `stats`, `doc`, `licenses`, `shell`.

use crate::common::*;

#[test]
fn init_creates_catalog() {
    let dir = tempdir().unwrap();
    maki()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Initialized"));
}

#[test]
fn init_fails_if_already_exists() {
    let dir = tempdir().unwrap();
    maki().current_dir(dir.path()).arg("init").assert().success();
    maki()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn commands_fail_without_init() {
    let dir = tempdir().unwrap();
    maki()
        .current_dir(dir.path())
        .args(["search", "foo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no maki catalog found"));
}

#[test]
fn show_displays_asset_details() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "rose.jpg", b"rose data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Extract asset ID from search output
    let output = maki()
        .current_dir(&root)
        .args(["search", "rose"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Asset:")
                .and(predicate::str::contains("rose.jpg"))
                .and(predicate::str::contains("Variants:")),
        );
}

#[test]
fn show_json_outputs_asset_details() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photo.jpg", b"show-json-test");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    // Get asset ID via search -q
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let asset_id = String::from_utf8(output).unwrap().trim().to_string();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", &asset_id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed["id"].is_string());
    assert!(parsed["asset_type"].is_string());
    assert!(parsed["variants"].is_array());
}

#[test]
fn stats_shows_overview() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"stats overview data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .arg("stats")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Catalog Overview")
                .and(predicate::str::contains("Assets:"))
                .and(predicate::str::contains("Variants:"))
                .and(predicate::str::contains("Volumes:")),
        );
}

#[test]
fn stats_empty_catalog() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .arg("stats")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Assets:    0")
                .and(predicate::str::contains("Variants:  0")),
        );
}

#[test]
fn stats_all_shows_all_sections() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"stats all data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["stats", "--all"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Catalog Overview")
                .and(predicate::str::contains("Asset Types"))
                .and(predicate::str::contains("Volumes"))
                .and(predicate::str::contains("Tags"))
                .and(predicate::str::contains("Verification")),
        );
}

#[test]
fn stats_types_flag() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"stats types data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["stats", "--types"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Asset Types")
                .and(predicate::str::contains("image"))
                .and(predicate::str::contains("Variant Formats"))
                .and(predicate::str::contains("jpg")),
        );
}

#[test]
fn stats_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"stats json data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "stats", "--all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed["overview"]["assets"].is_number());
    assert_eq!(parsed["overview"]["assets"].as_u64(), Some(1));
    assert!(parsed["types"]["asset_types"].is_array());
    assert!(parsed["volumes"].is_array());
    assert!(parsed["tags"]["unique_tags"].is_number());
    assert!(parsed["verified"]["total_locations"].is_number());
}

#[test]
fn stats_tags_shows_frequencies() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"stats tags data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    // Get asset ID and add tags
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "type:image"])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    maki()
        .current_dir(&root)
        .args(["tag", &asset_id, "landscape", "sunset"])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["stats", "--tags"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Tags")
                .and(predicate::str::contains("Tagged assets:   1"))
                .and(predicate::str::contains("landscape"))
                .and(predicate::str::contains("sunset")),
        );
}

#[test]
fn debug_flag_accepted() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // --debug before subcommand
    maki()
        .current_dir(&root)
        .args(["--debug", "stats"])
        .assert()
        .success();

    // -d shorthand
    maki()
        .current_dir(&root)
        .args(["-d", "stats"])
        .assert()
        .success();
}

#[test]
fn migrate_runs_successfully() {
    let tmp = tempdir().unwrap();
    let root = init_catalog(tmp.path());

    maki()
        .current_dir(&root)
        .arg("migrate")
        .assert()
        .success()
        .stdout(predicate::str::contains("Schema migrations applied successfully"));
}

#[test]
fn migrate_json_output() {
    let tmp = tempdir().unwrap();
    let root = init_catalog(tmp.path());

    let output = maki()
        .current_dir(&root)
        .args(["migrate", "--json"])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["status"], "ok");
}

#[test]
fn migrate_idempotent() {
    let tmp = tempdir().unwrap();
    let root = init_catalog(tmp.path());

    // Run twice — should succeed both times
    maki()
        .current_dir(&root)
        .arg("migrate")
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .arg("migrate")
        .assert()
        .success();
}

#[test]
fn shell_batch_tag_via_variable() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file1 = create_test_file(&root, "shell_tag1.jpg", b"shell tag data 1");
    let file2 = create_test_file(&root, "shell_tag2.jpg", b"shell tag data 2");

    maki().current_dir(&root)
        .args(["import", file1.to_str().unwrap()])
        .assert().success();
    maki().current_dir(&root)
        .args(["import", file2.to_str().unwrap()])
        .assert().success();

    // Write a script that searches then tags all results via _
    let script = root.join("batch-tag.dam");
    std::fs::write(&script, "search type:image\ntag _ batch-test\n").unwrap();

    maki().current_dir(&root)
        .args(["shell", script.to_str().unwrap()])
        .assert()
        .success();

    // Both assets should now have the "batch-test" tag
    maki().current_dir(&root)
        .args(["search", "tag:batch-test", "--format", "ids"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\n").count(2)); // two IDs, two lines
}

#[test]
fn doc_json_prints_download_url() {
    // `--json` prints the URL instead of launching a browser, so it is
    // the only form safe to run in a test.
    let out = maki()
        .args(["--json", "doc"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert!(
        parsed["url"].as_str().unwrap().ends_with("maki-manual.pdf"),
        "default document is the manual: {parsed}"
    );

    for (arg, suffix) in [
        ("cheatsheet", "cheat-sheet.pdf"),
        ("filters", "search-filters.pdf"),
        ("tagging", "tagging.pdf"),
        ("MAN", "maki-manual.pdf"),
    ] {
        let out = maki()
            .args(["--json", "doc", arg])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(parsed["url"].as_str().unwrap().ends_with(suffix), "{arg}: {parsed}");
    }
}

#[test]
fn doc_unknown_document_errors() {
    maki()
        .args(["--json", "doc", "nonsense"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown document"));
}

#[test]
fn licenses_prints_license_overview() {
    maki()
        .arg("licenses")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("MAKI")
                .and(predicate::str::contains("Apache-2.0"))
                .and(predicate::str::contains("THIRD_PARTY_LICENSES.md"))
                .and(predicate::str::contains("maki licenses --summary")),
        );

    maki()
        .args(["licenses", "--summary"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Apache-2.0")
                .and(predicate::str::contains("maki licenses --summary").not()),
        );
}

#[test]
fn licenses_json_output() {
    let out = maki()
        .args(["--json", "licenses"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(parsed["maki_license"].as_str(), Some("Apache-2.0"));
    assert!(parsed["third_party_licenses_url"].as_str().unwrap().ends_with("THIRD_PARTY_LICENSES.md"));
    assert!(parsed["ai_models"]["license"].is_string());
}
