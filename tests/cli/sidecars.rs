//! XMP sidecar creation and naming, `create-sidecars`, `fix-recipes`.

use crate::common::*;

#[test]
fn fix_recipes_reattaches_xmp() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();

    // Create an XMP file and import it first (becomes standalone since no media yet)
    let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    create_test_file(&photos, "DSC_001.xmp", xmp.as_bytes());
    maki()
        .current_dir(&root)
        .args(["import", photos.join("DSC_001.xmp").to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    // Now create and import the NRW media file
    create_test_file(&photos, "DSC_001.NRW", b"raw-nrw-fix-recipes-test");
    maki()
        .current_dir(&root)
        .args(["import", photos.join("DSC_001.NRW").to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    // Verify we have 2 assets (standalone XMP + NRW)
    maki()
        .current_dir(&root)
        .args(["stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Assets:    2"));

    // Run fix-recipes --apply
    maki()
        .current_dir(&root)
        .args(["fix-recipes", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 reattached"));

    // Should be down to 1 asset now
    maki()
        .current_dir(&root)
        .args(["stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Assets:    1"));

    // Get the NRW asset's ID via search --format ids
    let output = maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "type:image"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let asset_id = stdout.trim();
    assert!(!asset_id.is_empty(), "should have the NRW image asset");

    // The remaining asset should have the recipe attached and XMP metadata applied
    maki()
        .current_dir(&root)
        .args(["show", asset_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Recipe")
                .and(predicate::str::contains("Rating:"))
                .and(predicate::str::contains("landscape")),
        );
}

#[test]
fn fix_recipes_dry_run() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();

    create_test_file(&photos, "DSC_002.xmp", b"xmp-dry-run-test");
    maki()
        .current_dir(&root)
        .args(["import", photos.join("DSC_002.xmp").to_str().unwrap()])
        .assert()
        .success();

    create_test_file(&photos, "DSC_002.NRW", b"nrw-dry-run-test");
    maki()
        .current_dir(&root)
        .args(["import", photos.join("DSC_002.NRW").to_str().unwrap()])
        .assert()
        .success();

    // Dry run (no --apply) — reports what would happen
    maki()
        .current_dir(&root)
        .args(["fix-recipes"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1 reattached")
                .and(predicate::str::contains("--apply")),
        );

    // Still 2 assets — nothing changed
    maki()
        .current_dir(&root)
        .args(["stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Assets:    2"));
}

#[test]
fn fix_recipes_compound_extension() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();

    // Create DSC_003.NRW.xmp (compound extension) and import as standalone
    create_test_file(&photos, "DSC_003.NRW.xmp", b"xmp-compound-test");
    maki()
        .current_dir(&root)
        .args(["import", photos.join("DSC_003.NRW.xmp").to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 imported"));

    // Import the NRW media file
    create_test_file(&photos, "DSC_003.NRW", b"nrw-compound-test");
    maki()
        .current_dir(&root)
        .args(["import", photos.join("DSC_003.NRW").to_str().unwrap()])
        .assert()
        .success();

    // fix-recipes should match via compound stem stripping
    maki()
        .current_dir(&root)
        .args(["fix-recipes", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 reattached"));

    // Only 1 asset remains
    maki()
        .current_dir(&root)
        .args(["stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Assets:    1"));
}

#[test]
fn fix_recipes_no_parent() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();

    // Import an XMP with no matching media file
    create_test_file(&photos, "ORPHAN.xmp", b"xmp-orphan-test");
    maki()
        .current_dir(&root)
        .args(["import", photos.join("ORPHAN.xmp").to_str().unwrap()])
        .assert()
        .success();

    // fix-recipes reports no parent found
    maki()
        .current_dir(&root)
        .args(["fix-recipes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 no parent found"));
}

#[test]
fn fix_recipes_json() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let photos = root.join("photos");
    std::fs::create_dir_all(&photos).unwrap();

    create_test_file(&photos, "DSC_004.xmp", b"xmp-json-test");
    maki()
        .current_dir(&root)
        .args(["import", photos.join("DSC_004.xmp").to_str().unwrap()])
        .assert()
        .success();

    create_test_file(&photos, "DSC_004.NRW", b"nrw-json-test");
    maki()
        .current_dir(&root)
        .args(["import", photos.join("DSC_004.NRW").to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["--json", "fix-recipes"])
        .output()
        .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(parsed["checked"].as_u64(), Some(1));
    assert_eq!(parsed["reattached"].as_u64(), Some(1));
    assert_eq!(parsed["no_parent"].as_u64(), Some(0));
    assert_eq!(parsed["skipped"].as_u64(), Some(0));
    assert_eq!(parsed["dry_run"].as_bool(), Some(true));
}

/// Import a single RAW into `root/photos/<name>` and tag it; return the asset id.
fn import_tagged_raw(root: &Path, name: &str, tag: &str) -> String {
    create_test_file(root, &format!("photos/{name}"), format!("raw-{name}").as_bytes());
    maki()
        .current_dir(root)
        .args(["import", "--add-tag", tag, root.join("photos").to_str().unwrap()])
        .assert()
        .success();
    let output = maki()
        .current_dir(root)
        .args(["search", "--format", "ids", &format!("tag:{tag}")])
        .assert()
        .success();
    String::from_utf8_lossy(&output.get_output().stdout).trim().to_string()
}

#[test]
fn create_sidecars_writes_stem_named_xmp_and_registers_recipe() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let asset_id = import_tagged_raw(&root, "CS_001.ARW", "cs-stem");

    // Report-only first: nothing on disk.
    maki()
        .current_dir(&root)
        .args(["create-sidecars"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 created"))
        .stdout(predicate::str::contains("--apply"));
    assert!(!root.join("photos/CS_001.xmp").exists());

    maki()
        .current_dir(&root)
        .args(["create-sidecars", "--apply", "--log"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 created"))
        .stderr(predicate::str::contains("CS_001.xmp — created"));

    // Default naming replaces the media extension (Capture One / Lightroom).
    let xmp = root.join("photos/CS_001.xmp");
    assert!(xmp.exists(), "stem-named sidecar expected");
    assert!(!root.join("photos/CS_001.ARW.xmp").exists());
    let content = std::fs::read_to_string(&xmp).unwrap();
    assert!(content.contains("cs-stem"), "tag missing in sidecar: {content}");

    // Registered as a recipe of the asset.
    maki()
        .current_dir(&root)
        .args(["show", &asset_id, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("photos/CS_001.xmp"));

    // Second run: nothing left to create.
    maki()
        .current_dir(&root)
        .args(["create-sidecars", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 created"));
}

#[test]
fn create_sidecars_full_naming_from_config() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    std::fs::write(root.join("maki.toml"), "[writeback]\nsidecar_naming = \"full\"\n").unwrap();
    import_tagged_raw(&root, "CS_002.NEF", "cs-full");

    maki()
        .current_dir(&root)
        .args(["create-sidecars", "--apply", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"sidecar_naming\": \"full\""));

    assert!(root.join("photos/CS_002.NEF.xmp").exists(), "full-named sidecar expected");
    assert!(!root.join("photos/CS_002.xmp").exists());
}

#[test]
fn create_sidecars_leaves_unregistered_file_alone() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    import_tagged_raw(&root, "CS_003.ARW", "cs-exists");

    // Capture One wrote a sidecar after the import; MAKI hasn't imported it.
    let foreign = b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"2\"/></rdf:RDF></x:xmpmeta>";
    let xmp = create_test_file(&root, "photos/CS_003.xmp", foreign);

    maki()
        .current_dir(&root)
        .args(["create-sidecars", "--apply", "--log"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 created"))
        .stdout(predicate::str::contains("1 existing on disk but not imported"))
        .stderr(predicate::str::contains("not imported"));

    assert_eq!(std::fs::read(&xmp).unwrap(), foreign, "foreign sidecar must not be overwritten");
}

#[test]
fn create_sidecars_raw_jpeg_pair_shares_one_sidecar() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photos/CS_004.ARW", b"raw pair");
    create_test_file(&root, "photos/CS_004.JPG", b"jpeg pair");
    maki()
        .current_dir(&root)
        .args(["import", "--add-tag", "cs-pair", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["create-sidecars", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 checked, 1 created"));

    assert!(root.join("photos/CS_004.xmp").exists());
    let xmps: Vec<_> = std::fs::read_dir(root.join("photos"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "xmp"))
        .collect();
    assert_eq!(xmps.len(), 1, "RAW+JPEG pair gets exactly one DSC.xmp");
}

#[test]
fn edit_auto_creates_sidecar_when_configured() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    // Import first with defaults (no sidecar), then switch auto-create on:
    // this exercises the edit path, not the import-time creation.
    let asset_id = import_tagged_raw(&root, "CS_005.ARW", "cs-auto");
    assert!(!root.join("photos/CS_005.xmp").exists(), "default import creates no sidecar");
    std::fs::write(
        root.join("maki.toml"),
        "[writeback]\nenabled = true\ncreate_sidecars = true\n",
    )
    .unwrap();

    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "4"])
        .assert()
        .success();

    let xmp = root.join("photos/CS_005.xmp");
    assert!(xmp.exists(), "edit with create_sidecars should materialise the sidecar");
    let content = std::fs::read_to_string(&xmp).unwrap();
    assert!(content.contains("Rating=\"4\"") || content.contains("Rating='4'"), "{content}");
    assert!(content.contains("cs-auto"), "existing tag must be in the new sidecar: {content}");

    // Registered as a recipe, so a later edit flows into the same file.
    maki()
        .current_dir(&root)
        .args(["show", &asset_id, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("photos/CS_005.xmp"));
    maki()
        .current_dir(&root)
        .args(["tag", &asset_id, "cs-later"])
        .assert()
        .success();
    let content = std::fs::read_to_string(&xmp).unwrap();
    assert!(content.contains("cs-later"), "{content}");
}

#[test]
fn edit_does_not_create_sidecar_without_auto_writeback() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    // create_sidecars without enabled is inert: there is nothing to write.
    std::fs::write(root.join("maki.toml"), "[writeback]\ncreate_sidecars = true\n").unwrap();
    let asset_id = import_tagged_raw(&root, "CS_006.ARW", "cs-off");

    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "3"])
        .assert()
        .success();
    assert!(!root.join("photos/CS_006.xmp").exists());
}

#[test]
fn import_attaches_double_extension_sidecar() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let rating = |r: u8| {
        format!("<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"{r}\"/></rdf:RDF></x:xmpmeta>")
    };

    // Together: DSC.NEF + DSC.NEF.xmp (darktable / full naming) in one import.
    create_test_file(&root, "photos/DE_001.NEF", b"raw de1");
    create_test_file(&root, "photos/DE_001.NEF.xmp", rating(3).as_bytes());
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "rating:3"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());

    // Standalone later: the RAW is known, the sidecar arrives afterwards.
    create_test_file(&root, "photos/DE_002.NEF", b"raw de2");
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos/DE_002.NEF").to_str().unwrap()])
        .assert()
        .success();
    let xmp = create_test_file(&root, "photos/DE_002.NEF.xmp", rating(5).as_bytes());
    maki()
        .current_dir(&root)
        .args(["import", xmp.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 recipe"));
    maki()
        .current_dir(&root)
        .args(["search", "--format", "ids", "rating:5"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn relocate_create_sidecars_uses_stem_naming() {
    let dir = tempdir().unwrap();
    let (root, vol1, vol2) = init_two_volumes(dir.path());
    create_test_file(&vol1, "RL_001.ARW", b"relocate raw");
    maki()
        .current_dir(&root)
        .args(["import", "--add-tag", "rl-sidecar", vol1.to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["relocate", "--query", "tag:rl-sidecar", "--target", "vol2", "--create-sidecars"])
        .assert()
        .success();

    assert!(vol2.join("RL_001.ARW").exists());
    assert!(vol2.join("RL_001.xmp").exists(), "stem-named sidecar on target");
    assert!(!vol2.join("RL_001.ARW.xmp").exists());
    assert!(!vol1.join("RL_001.xmp").exists(), "source volume untouched");
}

#[test]
fn import_with_add_tag_auto_creates_sidecar_when_configured() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    std::fs::write(
        root.join("maki.toml"),
        "[writeback]\nenabled = true\ncreate_sidecars = true\n",
    )
    .unwrap();
    create_test_file(&root, "photos/IA_001.ARW", b"raw ia1");
    create_test_file(&root, "photos/IA_002.ARW", b"raw ia2");
    // IA_002 already has a Capture One sidecar in the same directory: it is
    // imported as a recipe, so no second file must be created for it.
    create_test_file(
        &root,
        "photos/IA_002.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"2\"/></rdf:RDF></x:xmpmeta>",
    );

    maki()
        .current_dir(&root)
        .args(["import", "--add-tag", "ia-import", root.join("photos").to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 sidecar(s) created"));

    let xmp = root.join("photos/IA_001.xmp");
    assert!(xmp.exists(), "import with --add-tag should create the sidecar");
    let content = std::fs::read_to_string(&xmp).unwrap();
    assert!(content.contains("ia-import"), "{content}");

    // The pre-existing sidecar stayed as it was.
    let kept = std::fs::read_to_string(root.join("photos/IA_002.xmp")).unwrap();
    assert!(kept.contains("Rating=\"2\"") && !kept.contains("ia-import"), "{kept}");

    // Nothing left for the explicit command.
    maki()
        .current_dir(&root)
        .args(["create-sidecars", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 created"));
}

#[test]
fn import_with_add_tag_creates_no_sidecar_by_default() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "photos/IB_001.ARW", b"raw ib1");
    maki()
        .current_dir(&root)
        .args(["import", "--add-tag", "ib-import", root.join("photos").to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("sidecar").not());
    assert!(!root.join("photos/IB_001.xmp").exists());
}
