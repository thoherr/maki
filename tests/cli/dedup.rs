//! `duplicates`, `dedup` and `backup-status`.

use crate::common::*;

#[test]
fn duplicates_shows_multi_location_files() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let content = b"identical content";
    let file1 = create_test_file(&root, "copy1.jpg", content);
    let file2 = create_test_file(&root, "subdir/copy2.jpg", content);

    maki()
        .current_dir(&root)
        .args(["import", file1.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["import", file2.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .arg("duplicates")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("copy1.jpg")
                .or(predicate::str::contains("copy2.jpg")),
        );
}

#[test]
fn duplicates_empty_when_no_duplicates() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "unique.jpg", b"unique bytes");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .arg("duplicates")
        .assert()
        .success()
        .stdout(predicate::str::contains("No duplicates found"));
}

#[test]
fn duplicates_format_json() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let output = maki()
        .current_dir(&root)
        .args(["duplicates", "--format=json"])
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
fn duplicates_same_volume_flag() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Import the same content twice on the same volume (different paths)
    let content = b"same volume dup content";
    let file1 = create_test_file(&root, "copy_a.jpg", content);
    let file2 = create_test_file(&root, "subdir/copy_b.jpg", content);

    maki()
        .current_dir(&root)
        .args(["import", file1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", file2.to_str().unwrap()])
        .assert()
        .success();

    // --same-volume should find the duplicate
    maki()
        .current_dir(&root)
        .args(["duplicates", "--same-volume"])
        .assert()
        .success()
        .stdout(predicate::str::contains("same-volume duplicate"));

    // --cross-volume should NOT find anything (both on same volume)
    maki()
        .current_dir(&root)
        .args(["duplicates", "--cross-volume"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No cross-volume copies found"));
}

#[test]
fn duplicates_cross_volume_flag() {
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

    // Import the same content on two different volumes
    let content = b"cross volume copy content";
    let file1 = create_test_file(&root, "original.jpg", content);
    let file2 = create_test_file(&vol2_path, "backup.jpg", content);

    maki()
        .current_dir(&root)
        .args(["import", file1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", "--volume", "vol2", file2.to_str().unwrap()])
        .assert()
        .success();

    // --cross-volume should find the cross-volume copy
    maki()
        .current_dir(&root)
        .args(["duplicates", "--cross-volume"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 volumes"));

    // --same-volume should NOT find it (each volume has only 1 copy)
    maki()
        .current_dir(&root)
        .args(["duplicates", "--same-volume"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No same-volume duplicates found"));
}

#[test]
fn duplicates_volume_filter() {
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

    // Import same content on both volumes
    let content = b"volume filter dup content";
    let file1 = create_test_file(&root, "vf_orig.jpg", content);
    let file2 = create_test_file(&vol2_path, "vf_backup.jpg", content);

    maki()
        .current_dir(&root)
        .args(["import", file1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", "--volume", "vol2", file2.to_str().unwrap()])
        .assert()
        .success();

    // --volume test-vol should show the duplicate
    maki()
        .current_dir(&root)
        .args(["duplicates", "--volume", "test-vol"])
        .assert()
        .success()
        .stdout(predicate::str::contains("duplicate locations"));

    // --volume nonexistent should find nothing
    maki()
        .current_dir(&root)
        .args(["duplicates", "--volume", "nonexistent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No duplicates found"));
}

#[test]
fn duplicates_mutually_exclusive_flags() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["duplicates", "--same-volume", "--cross-volume"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("mutually exclusive"));
}

#[test]
fn dedup_report_mode() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let content = b"dedup report content";
    let file1 = create_test_file(&root, "original.jpg", content);
    let file2 = create_test_file(&root, "subdir/copy.jpg", content);

    maki()
        .current_dir(&root)
        .args(["import", file1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", file2.to_str().unwrap()])
        .assert()
        .success();

    // Report mode (no --apply)
    maki()
        .current_dir(&root)
        .args(["dedup"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 duplicate groups"))
        .stdout(predicate::str::contains("1 redundant locations"))
        .stdout(predicate::str::contains("reclaimable"))
        .stdout(predicate::str::contains("Run with --apply"));

    // Both files should still exist
    assert!(file1.exists());
    assert!(file2.exists());
}

#[test]
fn dedup_apply() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let content = b"dedup apply content";
    let file1 = create_test_file(&root, "keep.jpg", content);
    let file2 = create_test_file(&root, "subdir/remove.jpg", content);

    maki()
        .current_dir(&root)
        .args(["import", file1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", file2.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["dedup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 duplicate groups"))
        .stdout(predicate::str::contains("1 locations removed"))
        .stdout(predicate::str::contains("1 files deleted"));

    // One file should remain, one should be gone
    let remaining = file1.exists() as usize + file2.exists() as usize;
    assert_eq!(remaining, 1, "exactly one copy should remain");

    // Duplicates should now be empty
    maki()
        .current_dir(&root)
        .args(["duplicates", "--same-volume"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No same-volume duplicates found"));
}

#[test]
fn dedup_prefer_flag() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let content = b"dedup prefer content";
    let file1 = create_test_file(&root, "Originals/photo.jpg", content);
    let file2 = create_test_file(&root, "Selects/photo.jpg", content);

    maki()
        .current_dir(&root)
        .args(["import", file1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", file2.to_str().unwrap()])
        .assert()
        .success();

    // Prefer Selects path — so the Selects copy should be kept
    maki()
        .current_dir(&root)
        .args(["dedup", "--prefer", "Selects", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 files deleted"));

    // Selects copy should remain, Originals should be deleted
    assert!(file2.exists(), "preferred location should be kept");
    assert!(!file1.exists(), "non-preferred location should be removed");
}

#[test]
fn dedup_min_copies() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let content = b"dedup min copies content";
    let file1 = create_test_file(&root, "copy_a.jpg", content);
    let file2 = create_test_file(&root, "copy_b.jpg", content);

    maki()
        .current_dir(&root)
        .args(["import", file1.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", file2.to_str().unwrap()])
        .assert()
        .success();

    // With min-copies=2, nothing should be removed (only 2 locations total)
    maki()
        .current_dir(&root)
        .args(["dedup", "--min-copies", "2", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 files deleted"));

    // Both files should still exist
    assert!(file1.exists());
    assert!(file2.exists());
}

#[test]
fn dedup_volume_filter() {
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

    // Create same-volume dups on vol2 only
    let content = b"dedup volume filter content";
    let file_v2a = create_test_file(&vol2_path, "a.jpg", content);
    let file_v2b = create_test_file(&vol2_path, "subdir/b.jpg", content);

    maki()
        .current_dir(&root)
        .args(["import", "--volume", "vol2", file_v2a.to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["import", "--volume", "vol2", file_v2b.to_str().unwrap()])
        .assert()
        .success();

    // Dedup only test-vol — should find nothing (dups are on vol2)
    maki()
        .current_dir(&root)
        .args(["dedup", "--volume", "test-vol"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 duplicate groups"));

    // Dedup vol2 — should find the duplicates
    maki()
        .current_dir(&root)
        .args(["dedup", "--volume", "vol2", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 duplicate groups"))
        .stdout(predicate::str::contains("1 files deleted"));
}

#[test]
fn backup_status_overview() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let file1 = create_test_file(&root, "photo1.jpg", b"backup status photo 1");
    let file2 = create_test_file(&root, "photo2.jpg", b"backup status photo 2");

    maki().current_dir(&root).args(["import", file1.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", file2.to_str().unwrap()]).assert().success();

    maki()
        .current_dir(&root)
        .args(["backup-status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Total assets:"))
        .stdout(predicate::str::contains("1 volume only:"))
        .stdout(predicate::str::contains("AT RISK"));
}

#[test]
fn backup_status_at_risk() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let file1 = create_test_file(&root, "risk1.jpg", b"at risk content 1");
    let file2 = create_test_file(&root, "risk2.jpg", b"at risk content 2");

    maki().current_dir(&root).args(["import", file1.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", file2.to_str().unwrap()]).assert().success();

    // --at-risk -q should output 2 asset IDs (one per line)
    let output = maki()
        .current_dir(&root)
        .args(["backup-status", "--at-risk", "-q"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let lines: Vec<&str> = std::str::from_utf8(&output)
        .unwrap()
        .trim()
        .lines()
        .collect();
    assert_eq!(lines.len(), 2, "expected 2 at-risk asset IDs, got: {:?}", lines);
    // Each line should look like a UUID
    for line in &lines {
        assert!(line.len() >= 36, "expected UUID, got: {}", line);
    }
}

#[test]
fn backup_status_min_copies() {
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

    // Import same file on both volumes (2 distinct volumes)
    let content = b"min copies content";
    let file1 = create_test_file(&root, "orig.jpg", content);
    let file2 = create_test_file(&vol2_path, "copy.jpg", content);

    maki().current_dir(&root).args(["import", file1.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", "--volume", "vol2", file2.to_str().unwrap()]).assert().success();

    // With --min-copies 1, the asset is on 2 volumes so it's not at risk
    maki()
        .current_dir(&root)
        .args(["backup-status", "--min-copies", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No at-risk assets"));

    // With --min-copies 3, the asset is on only 2 volumes so it IS at risk
    maki()
        .current_dir(&root)
        .args(["backup-status", "--min-copies", "3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("AT RISK").or(predicate::str::contains("at-risk")));
}

#[test]
fn backup_status_with_query() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let img = create_test_file(&root, "scene.jpg", b"backup query image");
    let aud = create_test_file(&root, "track.mp3", b"backup query audio");

    maki().current_dir(&root).args(["import", "--include", "audio", img.to_str().unwrap()]).assert().success();
    maki().current_dir(&root).args(["import", "--include", "audio", aud.to_str().unwrap()]).assert().success();

    // Scope to images only — should show 1 total asset
    maki()
        .current_dir(&root)
        .args(["backup-status", "type:image"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Total assets:").and(predicate::str::contains("1")));
}

#[test]
fn backup_status_volume_gap() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Second volume
    let dir2 = tempdir().unwrap();
    let vol2_path = dir2.path().canonicalize().unwrap();
    maki()
        .current_dir(&root)
        .args(["volume", "add", "vol2", vol2_path.to_str().unwrap()])
        .assert()
        .success();

    // Import file on test-vol only
    let file1 = create_test_file(&root, "gap_test.jpg", b"volume gap content");
    maki().current_dir(&root).args(["import", file1.to_str().unwrap()]).assert().success();

    // --volume vol2 --at-risk -q should list the asset (missing from vol2)
    let output = maki()
        .current_dir(&root)
        .args(["backup-status", "--volume", "vol2", "--at-risk", "-q"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let lines: Vec<&str> = std::str::from_utf8(&output)
        .unwrap()
        .trim()
        .lines()
        .collect();
    assert_eq!(lines.len(), 1, "expected 1 asset missing from vol2, got: {:?}", lines);
}

#[test]
fn backup_status_json() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let file1 = create_test_file(&root, "json_test.jpg", b"backup json content");
    maki().current_dir(&root).args(["import", file1.to_str().unwrap()]).assert().success();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "backup-status"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON output");
    assert!(json.get("total_assets").is_some());
    assert!(json.get("at_risk_count").is_some());
    assert!(json.get("location_distribution").is_some());
    assert!(json.get("purpose_coverage").is_some());
    assert!(json.get("volume_gaps").is_some());
    assert!(json.get("min_copies").is_some());
    // Verify distribution uses volume_count not location_count
    let dist = json["location_distribution"].as_array().unwrap();
    assert!(dist[0].get("volume_count").is_some());
}

#[test]
fn backup_status_purpose_coverage() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Re-register volume with a purpose
    // First we need to add a volume with --purpose
    let dir2 = tempdir().unwrap();
    let vol2_path = dir2.path().canonicalize().unwrap();
    maki()
        .current_dir(&root)
        .args(["volume", "add", "working-vol", vol2_path.to_str().unwrap(), "--purpose", "working"])
        .assert()
        .success();

    let file1 = create_test_file(&vol2_path, "purpose_test.jpg", b"purpose coverage content");
    maki()
        .current_dir(&root)
        .args(["import", "--volume", "working-vol", file1.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["backup-status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Coverage by volume purpose:"))
        .stdout(predicate::str::contains("Working"));
}
