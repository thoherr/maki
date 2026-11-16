//! `group`, `split`, `auto-group` and `import --auto-group`.

use crate::common::*;

#[test]
fn import_auto_group_sibling_dirs() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // CaptureOne-style: RAW in Capture/, export in Output/ under a session root
    let capture = root.join("session/Capture");
    let output = root.join("session/Output");
    std::fs::create_dir_all(&capture).unwrap();
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(capture.join("DSC_100.ARW"), b"raw-auto-group-sibling").unwrap();
    std::fs::write(output.join("DSC_100.JPG"), b"jpeg-auto-group-sibling").unwrap();

    // Import both directories at once with --auto-group
    maki()
        .current_dir(&root)
        .args([
            "import",
            "--auto-group",
            capture.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 imported"))
        .stdout(predicate::str::contains("Auto-group"))
        .stdout(predicate::str::contains("merged"));

    // Should be 1 asset with 2 variants
    maki()
        .current_dir(&root)
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 result(s)"));
}

#[test]
fn import_auto_group_incremental() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // First import: RAW file only, no --auto-group
    let capture = root.join("session2/Capture");
    std::fs::create_dir_all(&capture).unwrap();
    std::fs::write(capture.join("DSC_200.ARW"), b"raw-incr-auto-group").unwrap();
    maki()
        .current_dir(&root)
        .args(["import", capture.to_str().unwrap()])
        .assert()
        .success();

    // Second import: export with --auto-group
    let output = root.join("session2/Output");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(output.join("DSC_200.JPG"), b"jpeg-incr-auto-group").unwrap();
    maki()
        .current_dir(&root)
        .args(["import", "--auto-group", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Auto-group"));

    // Should be 1 asset (existing RAW picked up the export)
    maki()
        .current_dir(&root)
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 result(s)"));
}

#[test]
fn import_auto_group_fuzzy_prefix() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // CaptureOne-style export with suffix appended
    let capture = root.join("session3/Capture");
    let output = root.join("session3/Output");
    std::fs::create_dir_all(&capture).unwrap();
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(capture.join("Z91_8561.ARW"), b"raw-fuzzy-prefix").unwrap();
    std::fs::write(
        output.join("Z91_8561-1-HighRes.tif"),
        b"tif-fuzzy-prefix",
    )
    .unwrap();

    maki()
        .current_dir(&root)
        .args([
            "import",
            "--auto-group",
            capture.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Auto-group"));

    // Should be 1 asset
    maki()
        .current_dir(&root)
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 result(s)"));
}

#[test]
fn import_auto_group_no_false_positives() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Files in unrelated session directories (different session roots)
    let session_a = root.join("2024-01-01/Capture");
    let session_b = root.join("2024-06-15/Capture");
    std::fs::create_dir_all(&session_a).unwrap();
    std::fs::create_dir_all(&session_b).unwrap();
    std::fs::write(session_a.join("DSC_001.ARW"), b"raw-session-a-001").unwrap();
    std::fs::write(session_b.join("DSC_001.ARW"), b"raw-session-b-001").unwrap();

    // Import session A first (no --auto-group)
    maki()
        .current_dir(&root)
        .args(["import", session_a.to_str().unwrap()])
        .assert()
        .success();

    // Import session B with --auto-group — should NOT merge with session A
    // because they are under different session roots
    maki()
        .current_dir(&root)
        .args(["import", "--auto-group", session_b.to_str().unwrap()])
        .assert()
        .success();

    // Should still be 2 separate assets
    maki()
        .current_dir(&root)
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 result(s)"));
}

#[test]
fn import_auto_group_deep_export_dir_in_dated_session() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Dated session: RAW in Capture/, export two levels down in Output/Web/.
    let capture = root.join("2024-03-03-shoot/Capture");
    let web = root.join("2024-03-03-shoot/Output/Web");
    std::fs::create_dir_all(&capture).unwrap();
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(capture.join("DSC_300.ARW"), b"raw-deep-export").unwrap();
    maki()
        .current_dir(&root)
        .args(["import", capture.to_str().unwrap()])
        .assert()
        .success();

    // Importing only the deep export directory must still reach the RAW:
    // the session root is the dated folder, not one level above Web/.
    std::fs::write(web.join("DSC_300.JPG"), b"jpeg-deep-export").unwrap();
    maki()
        .current_dir(&root)
        .args(["import", "--auto-group", web.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Auto-group"));

    maki()
        .current_dir(&root)
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 result(s)"));
}

#[test]
fn import_auto_group_undated_tree_uses_import_dir_as_session() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // No date anywhere. Importing the whole client folder makes it the
    // session, so the nested export groups with the RAW in Capture/.
    let session = root.join("Clients/Wedding-Smith");
    let capture = session.join("Capture");
    let web = session.join("Output/Final/Web");
    std::fs::create_dir_all(&capture).unwrap();
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(capture.join("DSC_400.ARW"), b"raw-undated-tree").unwrap();
    std::fs::write(web.join("DSC_400.JPG"), b"jpeg-undated-tree").unwrap();

    maki()
        .current_dir(&root)
        .args(["import", "--auto-group", session.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 imported"))
        .stdout(predicate::str::contains("merged"));

    maki()
        .current_dir(&root)
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 result(s)"));
}

#[test]
fn import_auto_group_undated_deep_dir_alone_stays_local() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Same layout, but only the deep export directory is imported. Without
    // a dated component the scope is one level up (Output/), which does not
    // reach Capture/ — import the session folder for that.
    let session = root.join("Clients/Wedding-Jones");
    let capture = session.join("Capture");
    let web = session.join("Output/Web");
    std::fs::create_dir_all(&capture).unwrap();
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(capture.join("DSC_500.ARW"), b"raw-undated-deep").unwrap();
    maki()
        .current_dir(&root)
        .args(["import", capture.to_str().unwrap()])
        .assert()
        .success();

    std::fs::write(web.join("DSC_500.JPG"), b"jpeg-undated-deep").unwrap();
    maki()
        .current_dir(&root)
        .args(["import", "--auto-group", web.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 result(s)"));
}

#[test]
fn import_auto_group_json() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let capture = root.join("session4/Capture");
    let output = root.join("session4/Output");
    std::fs::create_dir_all(&capture).unwrap();
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(capture.join("IMG_001.ARW"), b"raw-json-auto-group").unwrap();
    std::fs::write(output.join("IMG_001.JPG"), b"jpeg-json-auto-group").unwrap();

    let out = maki()
        .current_dir(&root)
        .args([
            "--json",
            "import",
            "--auto-group",
            capture.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&out.get_output().stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["imported"], 2);
    assert!(json["auto_group"].is_object(), "auto_group key should be present");
    assert!(json["auto_group"]["groups"].is_array());
    assert_eq!(json["auto_group"]["groups"].as_array().unwrap().len(), 1);
}

#[test]
fn auto_group_dry_run_reports() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Create two files with the same stem in different directories
    let sub1 = root.join("raw");
    let sub2 = root.join("export");
    std::fs::create_dir_all(&sub1).unwrap();
    std::fs::create_dir_all(&sub2).unwrap();
    std::fs::write(sub1.join("DSC_001.ARW"), b"raw-content-for-autogroup").unwrap();
    std::fs::write(sub2.join("DSC_001.JPG"), b"jpeg-content-for-autogroup").unwrap();

    // Import each directory separately so they become separate assets
    maki()
        .current_dir(&root)
        .args(["import", sub1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", sub2.to_str().unwrap()])
        .assert()
        .success();

    // Verify we have 2 assets (search returns one row per variant)
    maki()
        .current_dir(&root)
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 result(s)"));

    // Dry run should report the match
    maki()
        .current_dir(&root)
        .args(["auto-group"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 stem group"))
        .stdout(predicate::str::contains("would merge"));

    // Assets should still be separate (dry run)
    maki()
        .current_dir(&root)
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 result(s)"));
}

#[test]
fn auto_group_apply_merges() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let sub1 = root.join("raw");
    let sub2 = root.join("export");
    std::fs::create_dir_all(&sub1).unwrap();
    std::fs::create_dir_all(&sub2).unwrap();
    std::fs::write(sub1.join("DSC_002.ARW"), b"raw-content-ag-apply").unwrap();
    std::fs::write(sub2.join("DSC_002.JPG"), b"jpeg-content-ag-apply").unwrap();

    maki()
        .current_dir(&root)
        .args(["import", sub1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", sub2.to_str().unwrap()])
        .assert()
        .success();

    // Apply auto-group
    maki()
        .current_dir(&root)
        .args(["auto-group", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 stem group"))
        .stdout(predicate::str::contains("merged"));

    // Should now be 1 unique asset (search -q outputs one ID per variant row)
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let unique_ids: std::collections::HashSet<&str> = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(unique_ids.len(), 1, "Expected 1 unique asset after auto-group, got {}", unique_ids.len());
}

#[test]
fn auto_group_no_matches() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    create_test_file(&root, "IMG_001.JPG", b"content-ag-no-match-1");
    create_test_file(&root, "IMG_002.JPG", b"content-ag-no-match-2");

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["auto-group"])
        .assert()
        .success()
        .stderr(predicate::str::contains("No groupable assets"));
}

#[test]
fn auto_group_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let sub1 = root.join("raw2");
    let sub2 = root.join("export2");
    std::fs::create_dir_all(&sub1).unwrap();
    std::fs::create_dir_all(&sub2).unwrap();
    std::fs::write(sub1.join("DSC_003.ARW"), b"raw-content-ag-json").unwrap();
    std::fs::write(sub2.join("DSC_003.JPG"), b"jpeg-content-ag-json").unwrap();

    maki()
        .current_dir(&root)
        .args(["import", sub1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", sub2.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "auto-group"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["groups"].as_array().unwrap().len(), 1);
}

#[test]
fn auto_group_fuzzy_prefix_merges_exports() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // RAW file with short name, export with appended suffix
    let sub1 = root.join("raw");
    let sub2 = root.join("export");
    std::fs::create_dir_all(&sub1).unwrap();
    std::fs::create_dir_all(&sub2).unwrap();
    std::fs::write(sub1.join("Z91_8561.ARW"), b"raw-content-fuzzy").unwrap();
    std::fs::write(
        sub2.join("Z91_8561-1-HighRes-(c)_2025_Thomas.JPG"),
        b"export-content-fuzzy",
    )
    .unwrap();

    maki()
        .current_dir(&root)
        .args(["import", sub1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", sub2.to_str().unwrap()])
        .assert()
        .success();

    // Apply auto-group — fuzzy prefix should match
    maki()
        .current_dir(&root)
        .args(["auto-group", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 stem group"))
        .stdout(predicate::str::contains("merged"));

    // Should be 1 unique asset
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let unique_ids: std::collections::HashSet<&str> = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(unique_ids.len(), 1);
}

#[test]
fn auto_group_fuzzy_rejects_numeric_continuation() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // DSC_001 and DSC_0010 are different shots — should NOT match
    create_test_file(&root, "sub/DSC_001.ARW", b"raw-content-no-fuzzy-1");
    create_test_file(&root, "sub/DSC_0010.JPG", b"jpg-content-no-fuzzy-2");

    maki()
        .current_dir(&root)
        .args(["import", root.join("sub").to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["auto-group"])
        .assert()
        .success()
        .stderr(predicate::str::contains("No groupable assets"));
}

#[test]
fn group_merges_two_assets() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Create two separate assets (different stems in different dirs)
    let sub1 = root.join("raw");
    let sub2 = root.join("export");
    std::fs::create_dir_all(&sub1).unwrap();
    std::fs::create_dir_all(&sub2).unwrap();
    std::fs::write(sub1.join("IMG_100.ARW"), b"raw-group-test").unwrap();
    std::fs::write(sub2.join("IMG_100_edit.JPG"), b"jpg-group-test").unwrap();

    maki()
        .current_dir(&root)
        .args(["import", sub1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", sub2.to_str().unwrap()])
        .assert()
        .success();

    // Should be 2 separate assets
    maki()
        .current_dir(&root)
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 result(s)"));

    // Get variant hashes from show --json
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .output()
        .unwrap();
    let ids: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(ids.len(), 2);

    let mut hashes = Vec::new();
    for id in &ids {
        let output = maki()
            .current_dir(&root)
            .args(["--json", "show", id])
            .output()
            .unwrap();
        let parsed: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON");
        let hash = parsed["variants"][0]["content_hash"]
            .as_str()
            .unwrap()
            .to_string();
        hashes.push(hash);
    }

    // Group them
    maki()
        .current_dir(&root)
        .args(["group", &hashes[0], &hashes[1]])
        .assert()
        .success()
        .stdout(predicate::str::contains("Grouped 2 variant(s)"));

    // Should now be 1 asset
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .output()
        .unwrap();
    let unique_ids: std::collections::HashSet<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(unique_ids.len(), 1);
}

#[test]
fn group_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let sub1 = root.join("a");
    let sub2 = root.join("b");
    std::fs::create_dir_all(&sub1).unwrap();
    std::fs::create_dir_all(&sub2).unwrap();
    std::fs::write(sub1.join("GRP_001.ARW"), b"raw-grp-json").unwrap();
    std::fs::write(sub2.join("GRP_001_v2.JPG"), b"jpg-grp-json").unwrap();

    maki()
        .current_dir(&root)
        .args(["import", sub1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", sub2.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .output()
        .unwrap();
    let ids: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .collect();

    let mut hashes = Vec::new();
    for id in &ids {
        let output = maki()
            .current_dir(&root)
            .args(["--json", "show", id])
            .output()
            .unwrap();
        let parsed: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON");
        hashes.push(
            parsed["variants"][0]["content_hash"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }

    let output = maki()
        .current_dir(&root)
        .args(["--json", "group", &hashes[0], &hashes[1]])
        .output()
        .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert!(parsed["target_id"].is_string());
    assert!(parsed["variants_moved"].is_number());
    assert!(parsed["donors_removed"].is_number());
}

#[test]
fn split_extracts_variant_into_new_asset() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Create two files with the same stem so they auto-group
    std::fs::write(root.join("IMG_001.ARW"), b"raw-split-test").unwrap();
    std::fs::write(root.join("IMG_001.JPG"), b"jpg-split-test").unwrap();

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    // Should be 1 asset with 2 variants
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .output()
        .unwrap();
    let ids: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(ids.len(), 1);

    // Get variant hashes
    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", ids[0]])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let variants = parsed["variants"].as_array().unwrap();
    assert_eq!(variants.len(), 2);

    // Find the non-original (alternate) variant and split it out
    let alt_hash = variants
        .iter()
        .find(|v| v["role"].as_str().unwrap() != "original")
        .unwrap()["content_hash"]
        .as_str()
        .unwrap();

    maki()
        .current_dir(&root)
        .args(["split", ids[0], alt_hash])
        .assert()
        .success()
        .stdout(predicate::str::contains("Split 1 variant(s)"));

    // Should now be 2 assets
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .output()
        .unwrap();
    let new_ids: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(new_ids.len(), 2);
}

#[test]
fn split_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    std::fs::write(root.join("IMG_002.ARW"), b"raw-split-json").unwrap();
    std::fs::write(root.join("IMG_002.JPG"), b"jpg-split-json").unwrap();

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .output()
        .unwrap();
    let ids: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(ids.len(), 1);

    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", ids[0]])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let variants = parsed["variants"].as_array().unwrap();
    let alt_hash = variants
        .iter()
        .find(|v| v["role"].as_str().unwrap() != "original")
        .unwrap()["content_hash"]
        .as_str()
        .unwrap();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "split", ids[0], alt_hash])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(parsed["source_id"].is_string());
    assert_eq!(parsed["new_assets"].as_array().unwrap().len(), 1);
    assert!(parsed["new_assets"][0]["asset_id"].is_string());
    assert!(parsed["new_assets"][0]["variant_hash"].is_string());
    assert!(parsed["new_assets"][0]["original_filename"].is_string());
}

#[test]
fn split_refuses_all_variants() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    std::fs::write(root.join("single.jpg"), b"single-file").unwrap();

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .output()
        .unwrap();
    let id = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .trim();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", id])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let hash = parsed["variants"][0]["content_hash"].as_str().unwrap();

    // Splitting the only variant should fail
    maki()
        .current_dir(&root)
        .args(["split", id, hash])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot extract all variants"));
}

#[test]
fn split_inherits_metadata() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    std::fs::write(root.join("META_001.ARW"), b"raw-meta-split").unwrap();
    std::fs::write(root.join("META_001.JPG"), b"jpg-meta-split").unwrap();

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .output()
        .unwrap();
    let id = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .trim();

    // Add tags and rating
    maki()
        .current_dir(&root)
        .args(["tag", id, "landscape", "nature"])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["edit", id, "--rating", "4"])
        .assert()
        .success();

    // Get alternate variant hash
    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", id])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let alt_hash = parsed["variants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["role"].as_str().unwrap() != "original")
        .unwrap()["content_hash"]
        .as_str()
        .unwrap();

    // Split
    let output = maki()
        .current_dir(&root)
        .args(["--json", "split", id, alt_hash])
        .output()
        .unwrap();
    let split_result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let new_id = split_result["new_assets"][0]["asset_id"].as_str().unwrap();

    // Check new asset has inherited metadata
    let output = maki()
        .current_dir(&root)
        .args(["--json", "show", new_id])
        .output()
        .unwrap();
    let new_asset: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(new_asset["rating"].as_u64().unwrap(), 4);
    let tags: Vec<&str> = new_asset["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(tags.contains(&"landscape"));
    assert!(tags.contains(&"nature"));
    // New variant should be role "original"
    assert_eq!(new_asset["variants"][0]["role"].as_str().unwrap(), "original");
}
