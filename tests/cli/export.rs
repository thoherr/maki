//! `export` and `contact-sheet`.

use crate::common::*;

#[test]
fn export_flat_copies_best_variant() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "export_flat.jpg", b"flat export content");
    maki()
        .current_dir(&root)
        .args(["import", root.join("export_flat.jpg").to_str().unwrap()])
        .assert()
        .success();

    let export_dir = dir.path().join("exported");
    maki()
        .current_dir(&root)
        .args(["export", "export_flat", export_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 files"));

    assert!(export_dir.join("export_flat.jpg").exists());
    assert_eq!(
        std::fs::read(export_dir.join("export_flat.jpg")).unwrap(),
        b"flat export content"
    );
}

#[test]
fn export_mirror_preserves_paths() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "subdir/mirror_test.jpg", b"mirror content");
    maki()
        .current_dir(&root)
        .args(["import", root.join("subdir/mirror_test.jpg").to_str().unwrap()])
        .assert()
        .success();

    let export_dir = dir.path().join("mirror_out");
    maki()
        .current_dir(&root)
        .args([
            "export",
            "mirror_test",
            export_dir.to_str().unwrap(),
            "--layout",
            "mirror",
        ])
        .assert()
        .success();

    assert!(export_dir.join("subdir/mirror_test.jpg").exists());
}

#[test]
fn export_dry_run_no_files() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "dry_run_export.jpg", b"dry run content");
    maki()
        .current_dir(&root)
        .args(["import", root.join("dry_run_export.jpg").to_str().unwrap()])
        .assert()
        .success();

    let export_dir = dir.path().join("dry_out");
    maki()
        .current_dir(&root)
        .args([
            "export",
            "dry_run_export",
            export_dir.to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("dry run"));

    // Directory should not be created in dry-run mode
    assert!(!export_dir.exists());
}

#[test]
fn export_skip_existing_matching_hash() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "skip_test.jpg", b"skip content");
    maki()
        .current_dir(&root)
        .args(["import", root.join("skip_test.jpg").to_str().unwrap()])
        .assert()
        .success();

    let export_dir = dir.path().join("skip_out");
    // First export
    maki()
        .current_dir(&root)
        .args(["export", "skip_test", export_dir.to_str().unwrap()])
        .assert()
        .success();
    assert!(export_dir.join("skip_test.jpg").exists());

    // Second export — should skip
    maki()
        .current_dir(&root)
        .args(["export", "skip_test", export_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 skipped"));
}

#[test]
fn export_overwrite() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "overwrite_test.jpg", b"overwrite content");
    maki()
        .current_dir(&root)
        .args(["import", root.join("overwrite_test.jpg").to_str().unwrap()])
        .assert()
        .success();

    let export_dir = dir.path().join("overwrite_out");
    // First export
    maki()
        .current_dir(&root)
        .args(["export", "overwrite_test", export_dir.to_str().unwrap()])
        .assert()
        .success();

    // Second export with --overwrite
    maki()
        .current_dir(&root)
        .args([
            "export",
            "overwrite_test",
            export_dir.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 files"));
}

#[test]
fn export_include_sidecars() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "sidecar_test.jpg", b"photo with sidecar");
    // Create an XMP sidecar
    create_test_file(
        &root,
        "sidecar_test.xmp",
        b"<?xml version='1.0'?><x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args([
            "import",
            root.join("sidecar_test.jpg").to_str().unwrap(),
            root.join("sidecar_test.xmp").to_str().unwrap(),
        ])
        .assert()
        .success();

    let export_dir = dir.path().join("sidecar_out");
    maki()
        .current_dir(&root)
        .args([
            "export",
            "sidecar_test",
            export_dir.to_str().unwrap(),
            "--include-sidecars",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 sidecars"));

    assert!(export_dir.join("sidecar_test.jpg").exists());
    assert!(export_dir.join("sidecar_test.xmp").exists());
}

#[cfg(unix)]
#[test]
fn export_symlink() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "link_test.jpg", b"symlink content");
    maki()
        .current_dir(&root)
        .args(["import", root.join("link_test.jpg").to_str().unwrap()])
        .assert()
        .success();

    let export_dir = dir.path().join("link_out");
    maki()
        .current_dir(&root)
        .args([
            "export",
            "link_test",
            export_dir.to_str().unwrap(),
            "--symlink",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 files linked"));

    let target = export_dir.join("link_test.jpg");
    assert!(target.exists());
    assert!(target.symlink_metadata().unwrap().file_type().is_symlink());
}

#[test]
fn export_all_variants() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    // Create two files with the same stem (auto-grouped into one asset)
    create_test_file(&root, "multi.jpg", b"jpeg variant");
    create_test_file(&root, "multi.tif", b"tiff variant");
    maki()
        .current_dir(&root)
        .args([
            "import",
            root.join("multi.jpg").to_str().unwrap(),
            root.join("multi.tif").to_str().unwrap(),
        ])
        .assert()
        .success();

    let export_dir = dir.path().join("all_variants_out");
    maki()
        .current_dir(&root)
        .args([
            "export",
            "multi",
            export_dir.to_str().unwrap(),
            "--all-variants",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 files"));

    assert!(export_dir.join("multi.jpg").exists());
    assert!(export_dir.join("multi.tif").exists());
}

#[test]
fn export_best_variant_only() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "best.jpg", b"jpeg best variant");
    create_test_file(&root, "best.tif", b"tiff extra variant data");
    maki()
        .current_dir(&root)
        .args([
            "import",
            root.join("best.jpg").to_str().unwrap(),
            root.join("best.tif").to_str().unwrap(),
        ])
        .assert()
        .success();

    let export_dir = dir.path().join("best_only_out");
    maki()
        .current_dir(&root)
        .args(["export", "best", export_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 files"));

    // Only one file should be exported (best variant)
    let files: Vec<_> = std::fs::read_dir(&export_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();
    assert_eq!(files.len(), 1);
}

#[test]
fn export_flat_filename_collision() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    // Two files with the same name in different directories
    create_test_file(&root, "subA/collision.jpg", b"content A");
    create_test_file(&root, "subB/collision.jpg", b"content B");
    maki()
        .current_dir(&root)
        .args([
            "import",
            root.join("subA/collision.jpg").to_str().unwrap(),
        ])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args([
            "import",
            root.join("subB/collision.jpg").to_str().unwrap(),
        ])
        .assert()
        .success();

    let export_dir = dir.path().join("collision_out");
    maki()
        .current_dir(&root)
        .args(["export", "collision", export_dir.to_str().unwrap()])
        .assert()
        .success();

    // Should have 2 files (one with hash suffix)
    let files: Vec<_> = std::fs::read_dir(&export_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();
    assert_eq!(files.len(), 2, "should have 2 files, one with hash suffix");
}

#[test]
fn export_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "json_export.jpg", b"json test");
    maki()
        .current_dir(&root)
        .args(["import", root.join("json_export.jpg").to_str().unwrap()])
        .assert()
        .success();

    let export_dir = dir.path().join("json_out");
    let output = maki()
        .current_dir(&root)
        .args([
            "--json",
            "export",
            "json_export",
            export_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).expect("valid JSON");
    assert_eq!(json["assets_matched"].as_u64().unwrap(), 1);
    assert_eq!(json["files_exported"].as_u64().unwrap(), 1);
    assert!(!json["dry_run"].as_bool().unwrap());
}

#[test]
fn export_no_results() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let export_dir = dir.path().join("empty_out");
    maki()
        .current_dir(&root)
        .args([
            "export",
            "nonexistent_query_xyz",
            export_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("No assets matched"));
}

#[test]
fn export_previews_exports_preview_file_with_swapped_extension() {
    use image::{ImageBuffer, Rgb};

    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Real image so import generates a preview
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(1, 1, Rgb([10, 20, 30]));
    let img_path = root.join("pv_export.png");
    img.save(&img_path).unwrap();
    maki()
        .current_dir(&root)
        .args(["import", img_path.to_str().unwrap()])
        .assert()
        .success();

    let export_dir = dir.path().join("pv_out");
    maki()
        .current_dir(&root)
        .args([
            "export",
            "pv_export",
            export_dir.to_str().unwrap(),
            "--previews",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 files"));

    // Original name, preview extension (default preview format is jpg)
    let exported = export_dir.join("pv_export.jpg");
    assert!(exported.exists(), "preview export should keep the original stem with the preview extension");
    // Exported bytes are the preview, not the original PNG
    assert_ne!(
        std::fs::read(&exported).unwrap(),
        std::fs::read(&img_path).unwrap()
    );
}

#[test]
fn export_previews_works_when_volume_offline() {
    use image::{ImageBuffer, Rgb};

    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Asset lives on a removable volume we take offline after import;
    // its preview stays in the catalog directory.
    let removable_dir = tempdir().unwrap();
    let removable = removable_dir.path().canonicalize().unwrap();
    maki()
        .current_dir(&root)
        .args(["volume", "add", "removable", removable.to_str().unwrap()])
        .assert()
        .success();

    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(1, 1, Rgb([200, 100, 50]));
    let img_path = removable.join("pv_offline.png");
    img.save(&img_path).unwrap();
    maki()
        .current_dir(&root)
        .args(["import", removable.to_str().unwrap()])
        .assert()
        .success();

    let stashed = removable.with_extension("offline");
    std::fs::rename(&removable, &stashed).unwrap();

    // Originals export finds nothing — all locations offline
    let orig_dir = dir.path().join("orig_out");
    maki()
        .current_dir(&root)
        .args(["export", "pv_offline", orig_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 files"))
        .stderr(predicate::str::contains("all locations offline"));

    // Preview export still delivers the file
    let pv_dir = dir.path().join("pv_out");
    maki()
        .current_dir(&root)
        .args(["export", "pv_offline", pv_dir.to_str().unwrap(), "--previews"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 files"));
    assert!(pv_dir.join("pv_offline.jpg").exists());
}

#[test]
fn export_smart_previews_missing_reports_skip() {
    use image::{ImageBuffer, Rgb};

    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Import without smart previews — smart export has nothing to ship
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(1, 1, Rgb([1, 2, 3]));
    let img_path = root.join("smart_missing.png");
    img.save(&img_path).unwrap();
    maki()
        .current_dir(&root)
        .args(["import", img_path.to_str().unwrap()])
        .assert()
        .success();

    let export_dir = dir.path().join("smart_out");
    maki()
        .current_dir(&root)
        .args([
            "export",
            "smart_missing",
            export_dir.to_str().unwrap(),
            "--smart-previews",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 files"))
        .stderr(predicate::str::contains("no smart preview on disk"));
}

#[test]
fn contact_sheet_generates_pdf() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "cs_test.jpg", b"contact sheet image");
    maki()
        .current_dir(&root)
        .args(["import", root.join("cs_test.jpg").to_str().unwrap()])
        .assert()
        .success();

    let output = dir.path().join("output.pdf");
    maki()
        .current_dir(&root)
        .args([
            "contact-sheet",
            "cs_test",
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 assets"));

    assert!(output.exists());
    let data = std::fs::read(&output).unwrap();
    assert!(data.starts_with(b"%PDF"), "Output should be a valid PDF");
}

#[test]
fn contact_sheet_dry_run() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "cs_dry.jpg", b"dry run image");
    maki()
        .current_dir(&root)
        .args(["import", root.join("cs_dry.jpg").to_str().unwrap()])
        .assert()
        .success();

    let output = dir.path().join("dry.pdf");
    maki()
        .current_dir(&root)
        .args([
            "contact-sheet",
            "cs_dry",
            output.to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success();

    assert!(!output.exists(), "Dry run should not create the PDF");
}

#[test]
fn contact_sheet_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "cs_json.jpg", b"json image");
    maki()
        .current_dir(&root)
        .args(["import", root.join("cs_json.jpg").to_str().unwrap()])
        .assert()
        .success();

    let output = dir.path().join("json.pdf");
    let cmd = maki()
        .current_dir(&root)
        .args([
            "contact-sheet",
            "cs_json",
            output.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&cmd.get_output().stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["assets"], 1);
    assert_eq!(parsed["pages"], 1);
    assert_eq!(parsed["layout"], "standard");
    assert_eq!(parsed["paper"], "a4");
    assert_eq!(parsed["dry_run"], false);
}

#[test]
fn contact_sheet_zero_results_errors() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let output = dir.path().join("empty.pdf");
    maki()
        .current_dir(&root)
        .args([
            "contact-sheet",
            "nonexistent:true",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure();

    assert!(!output.exists());
}

#[test]
fn contact_sheet_layout_options() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "cs_opts.jpg", b"layout test");
    maki()
        .current_dir(&root)
        .args(["import", root.join("cs_opts.jpg").to_str().unwrap()])
        .assert()
        .success();

    let output = dir.path().join("opts.pdf");
    maki()
        .current_dir(&root)
        .args([
            "contact-sheet",
            "cs_opts",
            output.to_str().unwrap(),
            "--layout", "dense",
            "--landscape",
            "--paper", "a3",
            "--title", "Test Sheet",
            "--fields", "filename,rating",
            "--label-style", "dot",
        ])
        .assert()
        .success();

    assert!(output.exists());
}

#[test]
fn contact_sheet_dry_run_json() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "cs_drj.jpg", b"dry run json");
    maki()
        .current_dir(&root)
        .args(["import", root.join("cs_drj.jpg").to_str().unwrap()])
        .assert()
        .success();

    let output = dir.path().join("drj.pdf");
    let cmd = maki()
        .current_dir(&root)
        .args([
            "contact-sheet",
            "cs_drj",
            output.to_str().unwrap(),
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&cmd.get_output().stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["dry_run"], true);
    assert_eq!(parsed["assets"], 1);
    assert!(!output.exists());
}
