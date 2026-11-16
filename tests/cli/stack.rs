//! `stack` subcommands.

use crate::common::*;

#[test]
fn stack_create_and_list() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["stack_a.jpg", "stack_b.jpg", "stack_c.jpg"]);

    maki()
        .current_dir(&root)
        .args(["stack", "create", &ids[0], &ids[1], &ids[2]])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created stack"))
        .stdout(predicate::str::contains("3 assets"));

    maki()
        .current_dir(&root)
        .args(["stack", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("3 assets"))
        .stdout(predicate::str::contains("pick:"));
}

#[test]
fn stack_alias_works() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["st_a.jpg", "st_b.jpg"]);

    maki()
        .current_dir(&root)
        .args(["st", "create", &ids[0], &ids[1]])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created stack"));
}

#[test]
fn stack_show_members() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["show_a.jpg", "show_b.jpg"]);

    maki()
        .current_dir(&root)
        .args(["stack", "create", &ids[0], &ids[1]])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["stack", "show", &ids[0]])
        .assert()
        .success()
        .stdout(predicate::str::contains(&ids[0]))
        .stdout(predicate::str::contains(&ids[1]))
        .stdout(predicate::str::contains("[pick]"));
}

#[test]
fn stack_set_pick() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["pick_a.jpg", "pick_b.jpg"]);

    maki()
        .current_dir(&root)
        .args(["stack", "create", &ids[0], &ids[1]])
        .assert()
        .success();

    // Initially ids[0] is the pick
    maki()
        .current_dir(&root)
        .args(["stack", "show", &ids[0]])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{} [pick]", ids[0])));

    // Change pick to ids[1]
    maki()
        .current_dir(&root)
        .args(["stack", "pick", &ids[1]])
        .assert()
        .success()
        .stdout(predicate::str::contains("as stack pick"));

    // Verify ids[1] is now pick
    maki()
        .current_dir(&root)
        .args(["stack", "show", &ids[1]])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{} [pick]", ids[1])));
}

#[test]
fn stack_remove_members() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["rm_a.jpg", "rm_b.jpg", "rm_c.jpg"]);

    maki()
        .current_dir(&root)
        .args(["stack", "create", &ids[0], &ids[1], &ids[2]])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["stack", "remove", &ids[2]])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed 1 asset"));

    // Stack should now have 2 members
    maki()
        .current_dir(&root)
        .args(["stack", "show", &ids[0]])
        .assert()
        .success()
        .stdout(predicate::str::contains(&ids[0]))
        .stdout(predicate::str::contains(&ids[1]));
}

#[test]
fn stack_dissolve() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["diss_a.jpg", "diss_b.jpg"]);

    maki()
        .current_dir(&root)
        .args(["stack", "create", &ids[0], &ids[1]])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["stack", "dissolve", &ids[0]])
        .assert()
        .success()
        .stdout(predicate::str::contains("Stack dissolved"));

    // No stacks should remain
    maki()
        .current_dir(&root)
        .args(["stack", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No stacks"));
}

#[test]
fn stack_add_to_existing() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["add_a.jpg", "add_b.jpg", "add_c.jpg"]);

    // Create stack with first two
    maki()
        .current_dir(&root)
        .args(["stack", "create", &ids[0], &ids[1]])
        .assert()
        .success();

    // Add third to existing stack (reference: ids[0])
    maki()
        .current_dir(&root)
        .args(["stack", "add", &ids[0], &ids[1], &ids[2]])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added"));

    // Show should include all three
    maki()
        .current_dir(&root)
        .args(["stack", "show", &ids[0]])
        .assert()
        .success()
        .stdout(predicate::str::contains(&ids[2]));
}

#[test]
fn stack_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["json_a.jpg", "json_b.jpg"]);

    maki()
        .current_dir(&root)
        .args(["stack", "create", "--json", &ids[0], &ids[1]])
        .assert()
        .success()
        .stdout(predicate::str::contains("member_count"))
        .stdout(predicate::str::contains("2"));

    maki()
        .current_dir(&root)
        .args(["stack", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("member_count"))
        .stdout(predicate::str::contains("2"));
}

#[test]
fn stack_search_stacked_filter() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["sf_a.jpg", "sf_b.jpg", "sf_solo.jpg"]);

    // Stack first two, leave third solo
    maki()
        .current_dir(&root)
        .args(["stack", "create", &ids[0], &ids[1]])
        .assert()
        .success();

    // stacked:true should find 2 assets
    maki()
        .current_dir(&root)
        .args(["search", "stacked:true"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 result"));

    // stacked:false should find 1 asset
    maki()
        .current_dir(&root)
        .args(["search", "stacked:false"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 result"));
}

#[test]
fn stack_rebuild_catalog_preserves_stacks() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["rb_a.jpg", "rb_b.jpg"]);

    maki()
        .current_dir(&root)
        .args(["stack", "create", &ids[0], &ids[1]])
        .assert()
        .success();

    // Rebuild catalog
    maki()
        .current_dir(&root)
        .args(["rebuild-catalog"])
        .assert()
        .success()
        .stdout(predicate::str::contains("stack"));

    // Stacks should survive
    maki()
        .current_dir(&root)
        .args(["stack", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 assets"));
}

#[test]
fn stack_remove_dissolves_when_one_left() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["auto_a.jpg", "auto_b.jpg"]);

    maki()
        .current_dir(&root)
        .args(["stack", "create", &ids[0], &ids[1]])
        .assert()
        .success();

    // Remove one member — stack should auto-dissolve since only 1 would remain
    maki()
        .current_dir(&root)
        .args(["stack", "remove", &ids[1]])
        .assert()
        .success();

    // No stacks should remain
    maki()
        .current_dir(&root)
        .args(["stack", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No stacks"));
}

#[test]
fn stack_from_tag_dry_run() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["ft_a.jpg", "ft_b.jpg", "ft_c.jpg", "ft_d.jpg", "ft_e.jpg"]);

    // Tag 3 assets with "Group A"
    for id in &ids[0..3] {
        maki().current_dir(&root).args(["tag", id, "Group A"]).assert().success();
    }
    // Tag 2 assets with "Group B"
    for id in &ids[3..5] {
        maki().current_dir(&root).args(["tag", id, "Group B"]).assert().success();
    }

    // Dry run
    maki()
        .current_dir(&root)
        .args(["stack", "from-tag", "Group {}"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tags matched: 2"))
        .stdout(predicate::str::contains("Stacks created: 2"))
        .stdout(predicate::str::contains("dry run"));

    // Verify no stacks were actually created
    maki()
        .current_dir(&root)
        .args(["stack", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No stacks"));
}

#[test]
fn stack_from_tag_apply() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["fta_a.jpg", "fta_b.jpg", "fta_c.jpg"]);

    for id in &ids {
        maki().current_dir(&root).args(["tag", id, "MyGroup X"]).assert().success();
    }

    maki()
        .current_dir(&root)
        .args(["stack", "from-tag", "MyGroup {}", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Stacks created: 1"))
        .stdout(predicate::str::contains("Assets stacked: 3"));

    // Verify stack was created
    maki()
        .current_dir(&root)
        .args(["stack", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("3 assets"));
}

#[test]
fn stack_from_tag_remove_tags() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["ftr_a.jpg", "ftr_b.jpg"]);

    for id in &ids {
        maki().current_dir(&root).args(["tag", id, "Stack 99"]).assert().success();
        maki().current_dir(&root).args(["tag", id, "keeper"]).assert().success();
    }

    maki()
        .current_dir(&root)
        .args(["stack", "from-tag", "Stack {}", "--apply", "--remove-tags"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tags removed: 2"));

    // Verify "Stack 99" tag is gone but "keeper" remains
    maki()
        .current_dir(&root)
        .args(["show", &ids[0]])
        .assert()
        .success()
        .stdout(predicate::str::contains("keeper"))
        .stdout(predicate::str::contains("Stack 99").not());
}

#[test]
fn stack_from_tag_skips_already_stacked() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["fts_a.jpg", "fts_b.jpg", "fts_c.jpg"]);

    // Tag all three with same tag FIRST (tagging does insert_asset which resets stack_id)
    for id in &ids {
        maki().current_dir(&root).args(["tag", id, "Overlap X"]).assert().success();
    }

    // Then stack first two
    maki()
        .current_dir(&root)
        .args(["stack", "create", &ids[0], &ids[1]])
        .assert()
        .success();

    // Only 1 unstacked asset — too few to create a stack
    maki()
        .current_dir(&root)
        .args(["stack", "from-tag", "Overlap {}", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tags skipped: 1"))
        .stdout(predicate::str::contains("Stacks created: 0"));
}

#[test]
fn stack_from_tag_single_asset_skipped() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["fts1_a.jpg"]);

    maki().current_dir(&root).args(["tag", &ids[0], "Solo 1"]).assert().success();

    maki()
        .current_dir(&root)
        .args(["stack", "from-tag", "Solo {}", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tags skipped: 1"))
        .stdout(predicate::str::contains("Stacks created: 0"));
}

#[test]
fn stack_from_tag_remove_tags_cleans_up_orphans() {
    // --remove-tags should remove the tag even when no stack is created
    // (orphan tag on single asset or already-stacked assets).
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["fto_a.jpg", "fto_b.jpg", "fto_c.jpg"]);

    // Asset A: orphan (only one asset with "Aperture Stack 1")
    maki().current_dir(&root).args(["tag", &ids[0], "Aperture Stack 1"]).assert().success();

    // Assets B and C: already stacked with "Aperture Stack 2" tag still present
    for id in &ids[1..] {
        maki().current_dir(&root).args(["tag", id, "Aperture Stack 2"]).assert().success();
    }
    maki().current_dir(&root).args(["stack", "create", &ids[1], &ids[2]]).assert().success();

    // Run stack from-tag with --remove-tags — both tags should be cleaned up
    // even though no new stacks are created.
    maki()
        .current_dir(&root)
        .args(["stack", "from-tag", "Aperture Stack {}", "--apply", "--remove-tags"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tags skipped: 2"))
        .stdout(predicate::str::contains("Tags removed: 3"));

    // Verify all three assets no longer carry the Aperture Stack tag
    for id in &ids {
        maki()
            .current_dir(&root)
            .args(["show", id])
            .assert()
            .success()
            .stdout(predicate::str::contains("Aperture Stack").not());
    }
}

#[test]
fn stack_from_tag_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let ids = import_and_get_ids(&root, &["ftj_a.jpg", "ftj_b.jpg"]);

    for id in &ids {
        maki().current_dir(&root).args(["tag", id, "Batch 42"]).assert().success();
    }

    let output = maki()
        .current_dir(&root)
        .args(["--json", "stack", "from-tag", "Batch {}", "--apply"])
        .output()
        .unwrap();

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON output");
    assert_eq!(json["tags_matched"], 1);
    assert_eq!(json["stacks_created"], 1);
    assert_eq!(json["assets_stacked"], 2);
    assert_eq!(json["dry_run"], false);
    assert!(json["details"][0]["stack_id"].is_string());
}

#[test]
fn stack_from_tag_no_wildcard_errors() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    maki()
        .current_dir(&root)
        .args(["stack", "from-tag", "no wildcard"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("{}"));
}
