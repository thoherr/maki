//! `import` and `watch`.

use crate::common::*;

#[test]
fn import_single_file() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"fake jpeg data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));
}

#[test]
fn import_directory() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let sub = root.join("batch");
    std::fs::create_dir_all(&sub).unwrap();
    create_test_file(&sub, "a.jpg", b"aaa");
    create_test_file(&sub, "b.png", b"bbb");

    maki()
        .current_dir(&root)
        .args(["import", sub.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 imported"));
}

#[test]
fn import_xmp_applies_metadata() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    create_test_file(&photos, "DSC_100.nef", b"raw image bytes");

    let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3"
    xmp:Label="Yellow">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>wildlife</rdf:li>
     <rdf:li>birds</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    create_test_file(&photos, "DSC_100.xmp", xmp.as_bytes());

    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1 imported")
                .and(predicate::str::contains("1 recipe")),
        );

    // Get asset ID via search
    let output = maki()
        .current_dir(&root)
        .args(["search", "DSC_100"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    // Verify tags and metadata appear in show output
    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("wildlife")
                .and(predicate::str::contains("birds"))
                .and(predicate::str::contains("rating"))
                .and(predicate::str::contains("3"))
                .and(predicate::str::contains("label"))
                .and(predicate::str::contains("Yellow")),
        );
}

#[test]
fn watch_once_imports_new_files() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Cycle 1: empty volume — baseline established, nothing imported.
    maki()
        .current_dir(&root)
        .args(["watch", "--once"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to do"));

    // Drop a new file; cycle 2 finds it stable and imports it.
    create_test_file(&root, "fresh.jpg", b"fresh data");
    maki()
        .current_dir(&root)
        .args(["watch", "--once"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    maki()
        .current_dir(&root)
        .args(["search", "fresh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fresh"));

    // Cycle 3: nothing changed — nothing imported again.
    maki()
        .current_dir(&root)
        .args(["watch", "--once"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to do"));
}

#[test]
fn watch_once_refreshes_changed_recipe() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    create_test_file(&photos, "DSC_200.nef", b"raw bytes for watch test");
    let xmp = |rating: u8| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="{rating}">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#
        )
    };
    create_test_file(&photos, "DSC_200.xmp", xmp(2).as_bytes());

    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    // Externally modify the tracked recipe, then run one watch cycle:
    // the changed .xmp must go through the refresh path, not import.
    create_test_file(&photos, "DSC_200.xmp", xmp(4).as_bytes());
    maki()
        .current_dir(&root)
        .args(["watch", "--once"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 recipe(s) refreshed"));

    maki()
        .current_dir(&root)
        .args(["search", "rating:4"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DSC_200"));
}

#[test]
fn import_skips_captureone_by_default() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    create_test_file(&photos, "DSC_001.nef", b"raw data for cos test");
    create_test_file(&photos, "DSC_001.cos", b"captureone sidecar");

    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    // Show should NOT mention CaptureOne recipe
    let output = maki()
        .current_dir(&root)
        .args(["search", "DSC_001"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Recipes:").not());
}

#[test]
fn import_includes_captureone_with_flag() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    create_test_file(&photos, "DSC_002.nef", b"raw data for cos include test");
    create_test_file(&photos, "DSC_002.cos", b"captureone sidecar data");

    maki()
        .current_dir(&root)
        .args([
            "import",
            "--include",
            "captureone",
            photos.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1 imported")
                .and(predicate::str::contains("1 recipe")),
        );

    // Show should mention CaptureOne recipe
    let output = maki()
        .current_dir(&root)
        .args(["search", "DSC_002"])
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
            predicate::str::contains("Recipes:")
                .and(predicate::str::contains("CaptureOne")),
        );
}

#[test]
fn import_skip_audio_excludes_audio_files() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    create_test_file(&root, "photo.jpg", b"jpeg data");
    create_test_file(&root, "song.mp3", b"audio data");

    // Import with --skip audio
    maki()
        .current_dir(&root)
        .args([
            "import",
            "--skip",
            "audio",
            root.join("photo.jpg").to_str().unwrap(),
            root.join("song.mp3").to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    // Only photo should be searchable
    maki()
        .current_dir(&root)
        .args(["search", "song"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results"));
}

#[test]
fn import_unknown_group_errors() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photo.jpg", b"data");

    maki()
        .current_dir(&root)
        .args([
            "import",
            "--include",
            "bogus",
            root.join("photo.jpg").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown file type group"));
}

#[test]
fn import_conflicting_include_skip_errors() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photo.jpg", b"data");

    maki()
        .current_dir(&root)
        .args([
            "import",
            "--include",
            "audio",
            "--skip",
            "audio",
            root.join("photo.jpg").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be both included and skipped"));
}

#[test]
fn reimport_updated_recipe_updates_in_place() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    create_test_file(&photos, "DSC_200.nef", b"raw image for update test");

    let xmp_v1 = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>original_tag</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">Original description</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    create_test_file(&photos, "DSC_200.xmp", xmp_v1.as_bytes());

    // First import
    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1 imported")
                .and(predicate::str::contains("1 recipe")),
        );

    // Modify the XMP
    let xmp_v2 = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="5">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>original_tag</rdf:li>
     <rdf:li>new_tag</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">Updated description</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    std::fs::write(photos.join("DSC_200.xmp"), xmp_v2).unwrap();

    // Re-import same directory
    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1 recipe(s) updated")
                .and(predicate::str::contains("1 skipped")),
        );

    // Verify metadata was updated
    let output = maki()
        .current_dir(&root)
        .args(["search", "DSC_200"])
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
            predicate::str::contains("new_tag")
                .and(predicate::str::contains("Updated description"))
                .and(predicate::str::contains("5")),
        );
}

#[test]
fn standalone_recipe_attaches_to_parent() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    create_test_file(&photos, "DSC_300.nef", b"raw image for standalone test");

    // Import NEF only
    maki()
        .current_dir(&root)
        .args(["import", photos.join("DSC_300.nef").to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    // Now create XMP and import it separately
    let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>standalone_tag</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    create_test_file(&photos, "DSC_300.xmp", xmp.as_bytes());

    maki()
        .current_dir(&root)
        .args(["import", photos.join("DSC_300.xmp").to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 recipe"));

    // Verify only one asset exists (no standalone Other asset created)
    let output = maki()
        .current_dir(&root)
        .args(["search", "DSC_300"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("1 result"),
        "Expected 1 result, got: {stdout}"
    );

    // Verify the recipe is attached and metadata applied
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");
    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Recipes:")
                .and(predicate::str::contains("standalone_tag"))
                .and(predicate::str::contains("rating")),
        );
}

#[test]
fn import_json_outputs_result() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photo.jpg", b"import-json-test");

    let output = maki()
        .current_dir(&root)
        .args(["--json", "import", root.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed["imported"].is_number());
    assert_eq!(parsed["imported"].as_u64(), Some(1));
}

#[test]
fn import_dry_run_reports_without_changes() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let sub = root.join("batch");
    std::fs::create_dir_all(&sub).unwrap();
    create_test_file(&sub, "a.jpg", b"dry-run-a");
    create_test_file(&sub, "b.png", b"dry-run-b");

    // Dry run should report what would happen
    maki()
        .current_dir(&root)
        .args(["import", "--dry-run", sub.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"))
        .stdout(predicate::str::contains("2 imported"));

    // Search should find nothing — no actual imports happened
    maki()
        .current_dir(&root)
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results found"));

    // No sidecar YAML files should have been created
    let sidecar_count = std::fs::read_dir(root.join("assets"))
        .map(|rd| rd.flatten().count())
        .unwrap_or(0);
    assert_eq!(sidecar_count, 0, "dry run should not create sidecar files");
}

#[test]
fn import_dry_run_json_includes_flag() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photo.jpg", b"dry-run-json-test");

    let output = maki()
        .current_dir(&root)
        .args(["--json", "import", "--dry-run", root.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["dry_run"].as_bool(), Some(true));
    assert_eq!(parsed["imported"].as_u64(), Some(1));
}

#[test]
fn import_with_volume_flag() {
    let dir = tempdir().unwrap();
    let (root, vol1, _vol2) = init_two_volumes(dir.path());

    create_test_file(&vol1, "explicit_vol.jpg", b"explicit volume test data");

    // Import with explicit --volume instead of auto-detect
    maki()
        .current_dir(&root)
        .args([
            "import",
            "--volume",
            "vol1",
            vol1.join("explicit_vol.jpg").to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    // Verify it landed on vol1
    let output = maki()
        .current_dir(&root)
        .args(["search", "explicit_vol"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("vol1"));
}

#[test]
fn import_jpeg_with_embedded_xmp_extracts_metadata() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();

    // Build a minimal JPEG with embedded XMP in APP1
    let xmp_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="5"
    xmp:Label="Green">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>architecture</rdf:li>
     <rdf:li>urban</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">City skyline at dusk</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    let xmp_namespace = b"http://ns.adobe.com/xap/1.0/\0";
    let mut jpeg_data: Vec<u8> = Vec::new();
    // SOI
    jpeg_data.extend_from_slice(&[0xFF, 0xD8]);
    // APP1 with XMP
    jpeg_data.extend_from_slice(&[0xFF, 0xE1]);
    let payload_len = xmp_namespace.len() + xmp_xml.len();
    let segment_len = (payload_len + 2) as u16;
    jpeg_data.extend_from_slice(&segment_len.to_be_bytes());
    jpeg_data.extend_from_slice(xmp_namespace);
    jpeg_data.extend_from_slice(xmp_xml.as_bytes());
    // EOI
    jpeg_data.extend_from_slice(&[0xFF, 0xD9]);

    let jpeg_path = photos.join("skyline.jpg");
    std::fs::write(&jpeg_path, &jpeg_data).unwrap();

    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    // Get asset ID via search
    let output = maki()
        .current_dir(&root)
        .args(["search", "skyline"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    // Verify embedded XMP metadata appears in show output
    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("architecture")
                .and(predicate::str::contains("urban"))
                .and(predicate::str::contains("5"))
                .and(predicate::str::contains("Green"))
                .and(predicate::str::contains("City skyline at dusk")),
        );
}

#[test]
fn import_xmp_hierarchical_subject() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    create_test_file(&photos, "DSC_200.nef", b"raw image for hier test");

    let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>animals</rdf:li>
     <rdf:li>birds</rdf:li>
     <rdf:li>eagles</rdf:li>
     <rdf:li>sunset</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>animals|birds|eagles</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    create_test_file(&photos, "DSC_200.xmp", xmp.as_bytes());

    maki()
        .current_dir(&root)
        .args(["import", photos.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "DSC_200"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().expect("search returned an ID");

    // Verify: flat components (animals, birds, eagles) should be deduplicated
    // into the hierarchical tag, while "sunset" remains
    let show_output = maki()
        .current_dir(&root)
        .args(["show", short_id])
        .output()
        .unwrap();
    let show_stdout = String::from_utf8_lossy(&show_output.stdout);

    assert!(
        show_stdout.contains("animals|birds|eagles"),
        "should contain hierarchical tag: {show_stdout}"
    );
    assert!(
        show_stdout.contains("sunset"),
        "should contain non-component flat tag: {show_stdout}"
    );

    // Hierarchical search should work
    maki()
        .current_dir(&root)
        .args(["search", "tag:animals"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 result"));
}
