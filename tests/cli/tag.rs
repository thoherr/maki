//! `tag` and its subcommands.

use crate::common::*;

#[test]
fn tag_add_and_remove() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "bird.jpg", b"bird data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "bird"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().unwrap();

    // Add tag
    maki()
        .current_dir(&root)
        .args(["tag", short_id, "nature", "wildlife"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Added tags:")
                .and(predicate::str::contains("nature")),
        );

    // Remove tag
    maki()
        .current_dir(&root)
        .args(["tag", short_id, "--remove", "wildlife"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed tags:").and(predicate::str::contains("wildlife")));

    // Verify remaining tags via show
    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("nature")
                .and(predicate::str::contains("wildlife").not()),
        );
}

#[test]
fn tag_clear_source_removes_only_matching_provenance() {
    // `tag clear --source xmp-import` must drop the tags that came in via
    // XMP keyword import while leaving user tags alone; `maki show` must
    // annotate them as `(xmp)` while they exist.
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_file(&root, "gig.jpg", b"gig data");
    create_test_file(
        &root,
        "gig.xmp",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>ImportedKeyword</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#,
    );

    maki()
        .current_dir(&root)
        .args(["import", root.to_str().unwrap()])
        .assert()
        .success();

    let asset_id = String::from_utf8_lossy(
        &maki()
            .current_dir(&root)
            .args(["search", "-q", "", "--format", "ids"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert!(!asset_id.is_empty());

    // Add a user tag alongside the imported keyword.
    maki()
        .current_dir(&root)
        .args(["tag", &asset_id, "usertag"])
        .assert()
        .success();

    // Human show output annotates the imported keyword's provenance.
    maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("ImportedKeyword (xmp)")
                .and(predicate::str::contains("usertag (").not()),
        );

    // JSON show output carries the tag_sources object.
    let show_json = String::from_utf8_lossy(
        &maki()
            .current_dir(&root)
            .args(["show", &asset_id, "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .to_string();
    let parsed: serde_json::Value = serde_json::from_str(&show_json).unwrap();
    assert_eq!(
        parsed["tag_sources"]["ImportedKeyword"],
        serde_json::json!("xmp-import"),
        "{show_json}"
    );

    // Bad source value is rejected.
    maki()
        .current_dir(&root)
        .args(["tag", "clear", &asset_id, "--source", "bogus"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown tag source"));

    // Clear only the xmp-import tags.
    maki()
        .current_dir(&root)
        .args(["tag", "clear", &asset_id, "--source", "xmp-import"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("ImportedKeyword")
                .and(predicate::str::contains("source: xmp-import")),
        );

    maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("usertag")
                .and(predicate::str::contains("ImportedKeyword").not()),
        );

    // A second source-filtered clear finds nothing to do.
    maki()
        .current_dir(&root)
        .args(["tag", "clear", &asset_id, "--source", "xmp-import"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No tags with source 'xmp-import'"));

    // Unfiltered clear still removes everything.
    maki()
        .current_dir(&root)
        .args(["tag", "clear", &asset_id])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["show", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("usertag").not());
}

#[test]
fn tag_split_replaces_one_tag_with_multiple() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "concert.jpg", b"concert data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "concert"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().unwrap();

    // Seed with one tag we'll split.
    maki()
        .current_dir(&root)
        .args(["tag", short_id, "concert-jane-2024"])
        .assert()
        .success();

    // Split it into the canonical pair.
    maki()
        .current_dir(&root)
        .args([
            "tag", "split", "concert-jane-2024",
            "subject|performing arts|concert",
            "event|concert-jane-2024",
            "--apply",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 split"));

    // Verify the split landed: source gone, both targets present, ancestors expanded.
    // Tags are rendered comma-separated on a "Tags:" line; bare source tag would
    // appear as either `: concert-jane-2024,` or `, concert-jane-2024,` / EOL.
    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("subject|performing arts|concert")
                .and(predicate::str::contains("event|concert-jane-2024"))
                .and(predicate::str::contains("subject|performing arts"))
                .and(predicate::str::contains(": concert-jane-2024,").not())
                .and(predicate::str::contains(", concert-jane-2024,").not()),
        );
}

#[test]
fn tag_split_keep_preserves_source() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "sunset.jpg", b"sunset data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "sunset"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().unwrap();

    maki()
        .current_dir(&root)
        .args(["tag", short_id, "sunset"])
        .assert()
        .success();

    // --keep: source stays, target added.
    maki()
        .current_dir(&root)
        .args(["tag", "split", "sunset", "color|warm", "--keep", "--apply"])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("sunset")
                .and(predicate::str::contains("color|warm"))
                .and(predicate::str::contains("color")),
        );
}

#[test]
fn tag_split_dry_run_does_not_mutate() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "photo.jpg", b"photo data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "photo"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().unwrap();

    maki()
        .current_dir(&root)
        .args(["tag", short_id, "foo"])
        .assert()
        .success();

    // No --apply: should report what would happen but leave state alone.
    // Stdout carries the "Run with --apply" hint; "Dry run —" prefix goes to stderr
    // (matches tag rename's convention).
    maki()
        .current_dir(&root)
        .args(["tag", "split", "foo", "bar", "baz"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--apply"))
        .stderr(predicate::str::contains("Dry run"));

    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("foo")
                .and(predicate::str::contains("bar").not())
                .and(predicate::str::contains("baz").not()),
        );
}

#[test]
fn tag_hierarchy_search() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "eagle.jpg", b"eagle data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "eagle"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().unwrap();

    // Add hierarchical tag (using | as hierarchy separator)
    maki()
        .current_dir(&root)
        .args(["tag", short_id, "animals|birds|eagles"])
        .assert()
        .success();

    // Search for parent tag should match
    maki()
        .current_dir(&root)
        .args(["search", "tag:animals"])
        .assert()
        .success()
        .stdout(predicate::str::contains("eagle"));

    // Search for intermediate tag should match
    maki()
        .current_dir(&root)
        .args(["search", "tag:animals|birds"])
        .assert()
        .success()
        .stdout(predicate::str::contains("eagle"));

    // Search for exact tag should match
    maki()
        .current_dir(&root)
        .args(["search", "tag:animals|birds|eagles"])
        .assert()
        .success()
        .stdout(predicate::str::contains("eagle"));

    // Search for unrelated tag should not match
    maki()
        .current_dir(&root)
        .args(["search", "tag:cats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results"));
}

#[test]
fn tag_hierarchy_add_remove() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let file = create_test_file(&root, "hawk.jpg", b"hawk data");

    maki()
        .current_dir(&root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "hawk"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = stdout.split_whitespace().next().unwrap();

    // Add hierarchical tag
    maki()
        .current_dir(&root)
        .args(["tag", short_id, "animals|birds|hawks"])
        .assert()
        .success()
        .stdout(predicate::str::contains("animals|birds|hawks"));

    // Verify in show
    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("animals|birds|hawks"));

    // Remove hierarchical tag
    maki()
        .current_dir(&root)
        .args(["tag", short_id, "--remove", "animals|birds|hawks"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed tags:"));

    // Verify removal
    maki()
        .current_dir(&root)
        .args(["show", short_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("animals|birds|hawks").not());
}

/// Append a raw `tags:` block to an asset's sidecar, bypassing every
/// normalisation the CLI applies at write time. Only valid for assets
/// imported without tags (the serializer omits an empty `tags:` key).
fn append_raw_tags_to_sidecar(root: &Path, id: &str, raw_tags: &[&str]) {
    let path = sidecar_path(root, id);
    let mut yaml = std::fs::read_to_string(&path).unwrap();
    assert!(!yaml.contains("\ntags:"), "sidecar already has a tags block:\n{yaml}");
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }
    yaml.push_str("tags:\n");
    for t in raw_tags {
        // JSON string escaping is valid YAML double-quoted scalar syntax.
        yaml.push_str(&format!("  - {}\n", serde_json::to_string(t).unwrap()));
    }
    std::fs::write(&path, yaml).unwrap();
}

#[test]
fn tag_scan_reports_malformed_tags() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["scan_clean.jpg", "scan_bad.jpg"]);

    maki()
        .current_dir(&root)
        .args(["tag", &ids[0], "fine|tag"])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["tag", "scan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tag scan: 2 asset(s) scanned, no malformed tags."));

    // Inject a tag the normaliser would rewrite (leading space + stray
    // separator) straight into the sidecar.
    append_raw_tags_to_sidecar(&root, &ids[1], &[" |bad"]);

    maki()
        .current_dir(&root)
        .args(["tag", "scan"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Tag scan: 2 scanned, 1 with malformed tag(s).")
                .and(predicate::str::contains(&ids[1]))
                .and(predicate::str::contains("raw:")),
        );

    let out = maki()
        .current_dir(&root)
        .args(["tag", "scan", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(parsed["scanned"], 2);
    assert_eq!(parsed["malformed"], 1);
    let assets = parsed["assets"].as_array().unwrap();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0]["asset_id"].as_str(), Some(ids[1].as_str()));
    assert_eq!(assets[0]["tags"][0]["raw"].as_str(), Some(" |bad"));
    assert_eq!(assets[0]["tags"][0]["normalised"][0].as_str(), Some("bad"));
    assert!(assets[0]["tags"][0]["raw_bytes_hex"].as_str().unwrap().starts_with("20"));
}

#[test]
fn tag_fix_unicode_normalises_nfd_tags() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["nfd.jpg", "nfc.jpg"]);

    const NFD: &str = "Mu\u{0308}nchen"; // u + combining diaeresis
    const NFC: &str = "M\u{00fc}nchen";
    append_raw_tags_to_sidecar(&root, &ids[0], &[NFD]);

    let out = maki()
        .current_dir(&root)
        .args(["--json", "tag", "fix-unicode"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(parsed["dry_run"], true);
    assert_eq!(parsed["scanned"], 2);
    assert_eq!(parsed["fixed"], 1);
    assert_eq!(parsed["tags_normalized"], 1);
    assert_eq!(parsed["merged"], 0);

    // Dry run leaves the NFD bytes in the sidecar.
    assert!(std::fs::read_to_string(sidecar_path(&root, &ids[0])).unwrap().contains(NFD));

    maki()
        .current_dir(&root)
        .args(["tag", "fix-unicode", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tag fix-unicode: 2 scanned, 1 fixed, 1 tag value(s) normalised"));

    let yaml = std::fs::read_to_string(sidecar_path(&root, &ids[0])).unwrap();
    assert!(yaml.contains(NFC) && !yaml.contains(NFD), "sidecar must be NFC:\n{yaml}");
    let tags = show_json(&root, &ids[0])["tags"].clone();
    assert_eq!(tags.as_array().unwrap().len(), 1, "{tags}");
    assert_eq!(tags[0].as_str(), Some(NFC), "catalog must carry the NFC form");

    maki()
        .current_dir(&root)
        .args(["tag", "fix-unicode"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tag fix-unicode: 2 asset(s) scanned, all already NFC."));
}

#[test]
fn tag_expand_ancestors_adds_missing_paths() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["leaf_only.jpg", "complete.jpg"]);

    // The CLI add path already stores ancestors, so write a leaf-only
    // hierarchy into the sidecar directly (what an external tool or an
    // older catalog would leave behind).
    append_raw_tags_to_sidecar(&root, &ids[0], &["place|europe|berlin"]);
    maki()
        .current_dir(&root)
        .args(["tag", &ids[1], "animal|cat"])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["tag", "expand-ancestors"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Expand ancestors: 2 checked, 1 assets with missing ancestors, 2 tags would add")
                .and(predicate::str::contains("Run with --apply")),
        );

    let out = maki()
        .current_dir(&root)
        .args(["--json", "tag", "expand-ancestors", "--asset", &ids[0]])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(parsed["dry_run"], true);
    assert_eq!(parsed["checked"], 1);
    assert_eq!(parsed["expanded"], 1);
    assert_eq!(parsed["tags_added"], 2);

    maki()
        .current_dir(&root)
        .args(["tag", "expand-ancestors", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Expand ancestors: 2 checked, 1 assets with missing ancestors, 2 tags added"));

    let tags: Vec<String> = show_json(&root, &ids[0])["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    for expected in ["place", "place|europe", "place|europe|berlin"] {
        assert!(tags.contains(&expected.to_string()), "missing {expected} in {tags:?}");
    }

    maki()
        .current_dir(&root)
        .args(["tag", "expand-ancestors"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Expand ancestors: 2 checked, 0 assets with missing ancestors, 0 tags would add"));
}

#[test]
fn tag_delete_cascades_to_descendants() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["del_a.jpg", "del_b.jpg", "del_c.jpg"]);

    maki().current_dir(&root).args(["tag", &ids[0], "event|concert|2024"]).assert().success();
    maki().current_dir(&root).args(["tag", &ids[1], "event|party"]).assert().success();
    maki().current_dir(&root).args(["tag", &ids[2], "landscape"]).assert().success();

    // Unknown tag: nothing matched, still a clean exit.
    maki()
        .current_dir(&root)
        .args(["tag", "delete", "nosuchtag"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No assets found with tag \"nosuchtag\"."));

    maki()
        .current_dir(&root)
        .args(["--log", "tag", "delete", "event"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Tag delete \"event\": 2 matched, 2 would remove")
                .and(predicate::str::contains("Run with --apply")),
        )
        .stderr(predicate::str::contains("would remove"));

    // Dry run leaves tags in place.
    assert!(show_json(&root, &ids[0])["tags"].as_array().unwrap().iter().any(|t| t == "event|concert|2024"));

    let out = maki()
        .current_dir(&root)
        .args(["--json", "tag", "delete", "event", "--apply"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(parsed["dry_run"], false);
    assert_eq!(parsed["matched"], 2);
    assert_eq!(parsed["removed"], 2);
    assert_eq!(parsed["skipped"], 0);

    for id in &ids[..2] {
        let tags = show_json(&root, id)["tags"].clone();
        assert!(
            !tags.as_array().unwrap().iter().any(|t| t.as_str().unwrap().starts_with("event")),
            "event hierarchy must be gone from {id}: {tags}"
        );
    }
    assert!(show_json(&root, &ids[2])["tags"].as_array().unwrap().iter().any(|t| t == "landscape"));
}

#[test]
fn tag_export_vocabulary_yaml_json_and_text_formats() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    let ids = import_and_get_ids(&root, &["vocab_a.jpg", "vocab_b.jpg"]);
    maki().current_dir(&root).args(["tag", &ids[0], "nature|birds"]).assert().success();
    maki().current_dir(&root).args(["tag", &ids[1], "nature|birds", "travel"]).assert().success();

    // Default: YAML into the catalog root.
    maki()
        .current_dir(&root)
        .args(["tag", "export-vocabulary"])
        .assert()
        .success()
        .stdout(predicate::str::contains("(3 used, ").and(predicate::str::contains("vocabulary.yaml")));
    // Without --prune the built-in default vocabulary is merged in as
    // "planned" entries; the three catalog tags must be among them.
    let yaml = std::fs::read_to_string(root.join("vocabulary.yaml")).unwrap();
    assert!(yaml.contains("nature") && yaml.contains("birds") && yaml.contains("travel"), "{yaml}");

    // YAML with counts: trailing comments, still parseable YAML.
    let counted = root.join("vocab_counts.yaml");
    maki()
        .current_dir(&root)
        .args(["tag", "export-vocabulary", "--counts", "--prune", "--output", counted.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Exported 3 tags to").and(predicate::str::contains("pruned")));
    // MAKI's vocabulary format is a hand-editable mix of nested maps
    // (branches) and lists (leaves); assert the shape textually.
    let yaml = std::fs::read_to_string(&counted).unwrap();
    assert!(yaml.contains("nature:  # 2 assets"), "branch with count:\n{yaml}");
    assert!(yaml.contains("  - birds  # 2 assets"), "nested leaf with count:\n{yaml}");
    // `travel` shares the top level with the `nature` branch, so it is
    // emitted as an empty key (strict YAML), not a list item.
    assert!(yaml.contains("travel:  # 1 asset\n"), "top-level leaf with count:\n{yaml}");

    // JSON: nested object with counts.
    let json_path = root.join("vocab.json");
    maki()
        .current_dir(&root)
        .args(["tag", "export-vocabulary", "--format", "json", "--prune", "--output", json_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Exported 3 tags to"));
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).expect("valid JSON file");
    assert!(parsed.get("nature").is_some(), "{parsed}");
    assert!(parsed.get("travel").is_some(), "{parsed}");
    assert_eq!(parsed["travel"]["count"], 1, "{parsed}");

    // Text: tab-indented keyword list, no comments.
    let txt_path = root.join("vocab.txt");
    maki()
        .current_dir(&root)
        .args(["tag", "export-vocabulary", "--format", "text", "--counts", "--prune", "--output", txt_path.to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicate::str::contains("--counts is ignored for text format"));
    let txt = std::fs::read_to_string(&txt_path).unwrap();
    assert!(txt.contains("nature\n\tbirds"), "hierarchy must be tab-indented:\n{txt}");
    assert!(!txt.contains('#'));

    maki()
        .current_dir(&root)
        .args(["tag", "export-vocabulary", "--format", "xml"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown --format"));
}
