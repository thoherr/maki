//! XMP write-back and `sync-metadata` (Pro).

use crate::common::*;

#[test]
fn sync_metadata_unchanged() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    create_test_file(&root, "photos/SM_001.ARW", b"raw-sm-unchanged");
    create_test_file(
        &root,
        "photos/SM_001.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"3\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    // No changes — should report unchanged
    maki()
        .current_dir(&root)
        .args(["sync-metadata"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 unchanged"));
}

#[test]
fn sync_metadata_inbound_reads_external_changes() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let xmp_path = root.join("photos/SM_002.xmp");
    create_test_file(&root, "photos/SM_002.ARW", b"raw-sm-inbound");
    create_test_file(
        &root,
        "photos/SM_002.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"2\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    // Modify XMP externally (simulates CaptureOne edit)
    std::fs::write(
        &xmp_path,
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"5\"/></rdf:RDF></x:xmpmeta>",
    )
    .unwrap();

    // sync-metadata should read external change
    maki()
        .current_dir(&root)
        .args(["sync-metadata"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 read from disk"));

    // Verify rating was updated
    maki()
        .current_dir(&root)
        .args(["search", "--json", "rating:5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SM_002"));
}

#[test]
fn sync_metadata_outbound_writes_pending() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Enable writeback for this test
    let config_path = root.join("maki.toml");
    std::fs::write(&config_path, "[writeback]\nenabled = true\n").unwrap();

    let xmp_path = root.join("photos/SM_003.xmp");
    create_test_file(&root, "photos/SM_003.ARW", b"raw-sm-outbound");
    create_test_file(
        &root,
        "photos/SM_003.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"1\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    // Get the asset ID
    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "*"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let asset_id = stdout.trim();

    // Edit rating in DAM — this writes back immediately
    maki()
        .current_dir(&root)
        .args(["edit", asset_id, "--rating", "4"])
        .assert()
        .success();

    // sync-metadata should report unchanged (writeback already happened inline)
    maki()
        .current_dir(&root)
        .args(["sync-metadata"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 unchanged"));

    // Verify the XMP was updated by the edit command's write-back
    let xmp_content = std::fs::read_to_string(&xmp_path).unwrap();
    assert!(xmp_content.contains("Rating=\"4\"") || xmp_content.contains("Rating='4'"));
}

/// When `[writeback] enabled = false` (the default), edits should still be
/// tracked: the recipe gets `pending_writeback = true` and the XMP file on
/// disk stays untouched. A subsequent `maki writeback` command then flushes
/// those staged edits — proving the auto-off / manual-on workflow.
#[test]
fn edit_with_writeback_disabled_tracks_pending_then_manual_flush_writes() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // No [writeback] section in maki.toml → enabled defaults to false.

    let xmp_path = root.join("photos/WB_010.xmp");
    create_test_file(&root, "photos/WB_010.ARW", b"raw-wb-disabled");
    create_test_file(
        &root,
        "photos/WB_010.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"1\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["search", "-q", "*"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let asset_id = stdout.trim().to_string();

    // Edit rating with writeback disabled.
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "4"])
        .assert()
        .success();

    // The XMP on disk still shows Rating="1" — auto-flush is off.
    let xmp_after_edit = std::fs::read_to_string(&xmp_path).unwrap();
    assert!(
        xmp_after_edit.contains("Rating=\"1\"") || xmp_after_edit.contains("Rating='1'"),
        "Auto-flush off: XMP must stay at Rating=1 after edit. Got: {xmp_after_edit}"
    );

    // `maki status` should surface the pending writeback (proves tracking
    // ran even though auto-flush was off).
    maki()
        .current_dir(&root)
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pending XMP writeback"));

    // Manual flush works regardless of `[writeback] enabled` — that's the
    // whole point of the split.
    maki()
        .current_dir(&root)
        .args(["writeback"])
        .assert()
        .success();

    // The XMP on disk now reflects the edit.
    let xmp_after_flush = std::fs::read_to_string(&xmp_path).unwrap();
    assert!(
        xmp_after_flush.contains("Rating=\"4\"") || xmp_after_flush.contains("Rating='4'"),
        "Manual flush wrote the rating. Got: {xmp_after_flush}"
    );
}

/// `maki writeback --all --mirror-tags <query>` reconciles XMP keyword
/// lists against the catalog, removing entries the catalog no longer has.
/// Regression for the additive-only flush bug: a tag rename done while
/// auto-writeback was off used to leave the OLD tag stranded in XMP
/// alongside the new one. Mirror mode reads `dc:subject` and
/// `lr:hierarchicalSubject`, diffs against the asset's catalog tags, and
/// removes the stale entries — exactly what's needed after large
/// catalog-only tag restructuring.
#[test]
fn writeback_mirror_tags_removes_stale_xmp_keywords() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Default config: writeback disabled. Stage an XMP that already has
    // a keyword on disk, then import.
    let xmp_path = root.join("photos/MT_001.xmp");
    create_test_file(&root, "photos/MT_001.ARW", b"raw-mirror-tags");
    create_test_file(
        &root,
        "photos/MT_001.xmp",
        b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n  <rdf:RDF>\n    <rdf:Description xmp:Rating=\"3\">\n      <dc:subject>\n        <rdf:Bag>\n          <rdf:li>OldTag</rdf:li>\n        </rdf:Bag>\n      </dc:subject>\n    </rdf:Description>\n  </rdf:RDF>\n</x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    // Catalog now has the asset tagged "OldTag" (absorbed during import).
    // Rename it in MAKI to "NewTag". With auto-flush off, the catalog
    // updates but the .xmp on disk still has "OldTag".
    maki()
        .current_dir(&root)
        .args(["tag", "rename", "OldTag", "NewTag", "--apply"])
        .assert()
        .success();

    let xmp_pre_flush = std::fs::read_to_string(&xmp_path).unwrap();
    assert!(xmp_pre_flush.contains("OldTag"), "Auto-flush off: XMP still has OldTag pre-flush");
    assert!(!xmp_pre_flush.contains("NewTag"), "Auto-flush off: NewTag not yet on disk");

    // Default `maki writeback --all` is additive — would leave OldTag.
    // `--mirror-tags` (requires --all) reconciles the keyword list.
    maki()
        .current_dir(&root)
        .args(["writeback", "--all", "--mirror-tags"])
        .assert()
        .success();

    let xmp_post_flush = std::fs::read_to_string(&xmp_path).unwrap();
    assert!(
        xmp_post_flush.contains("NewTag"),
        "Mirror flush wrote NewTag. Got: {xmp_post_flush}"
    );
    assert!(
        !xmp_post_flush.contains("OldTag"),
        "Mirror flush removed stale OldTag. Got: {xmp_post_flush}"
    );
}

/// `[writeback] mirror_tags = true` in maki.toml turns mirror-tags on
/// by default for every `maki writeback`. The bug this protects
/// against: a user renames a tag in MAKI, runs `maki writeback`
/// without `--mirror-tags` (because they forgot, or because mirror-
/// tags requires `--all` on the CLI), then re-imports — the old
/// keyword still on the XMP file gets absorbed back into the catalog,
/// silently undoing the rename. With the config flag on, every plain
/// `maki writeback` does the keyword reconciliation.
#[test]
fn writeback_config_mirror_tags_default() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Turn on the config flag. `maki init` writes a default
    // `[writeback]` section, so we replace the file rather than append.
    let toml_path = root.join("maki.toml");
    std::fs::write(&toml_path, "[writeback]\nmirror_tags = true\n").unwrap();

    let xmp_path = root.join("photos/MT_CFG.xmp");
    create_test_file(&root, "photos/MT_CFG.ARW", b"raw-mirror-tags-cfg");
    create_test_file(
        &root,
        "photos/MT_CFG.xmp",
        b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n  <rdf:RDF>\n    <rdf:Description xmp:Rating=\"3\">\n      <dc:subject>\n        <rdf:Bag>\n          <rdf:li>OldTag</rdf:li>\n        </rdf:Bag>\n      </dc:subject>\n    </rdf:Description>\n  </rdf:RDF>\n</x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["tag", "rename", "OldTag", "NewTag", "--apply"])
        .assert()
        .success();

    // Plain `maki writeback` — no --mirror-tags flag, no --all. With
    // the config flag on, this should still strip OldTag from the XMP.
    maki()
        .current_dir(&root)
        .args(["writeback"])
        .assert()
        .success();

    let xmp_after = std::fs::read_to_string(&xmp_path).unwrap();
    assert!(
        xmp_after.contains("NewTag"),
        "Plain writeback with config mirror_tags=true wrote NewTag. Got: {xmp_after}"
    );
    assert!(
        !xmp_after.contains("OldTag"),
        "Plain writeback with config mirror_tags=true removed stale OldTag. Got: {xmp_after}"
    );
}

/// `--mirror-tags` requires `--all` (clap-level constraint). Using it
/// alone should be rejected, since mirror mode without --all would only
/// touch pending recipes — the rename/delete scenario it's designed for
/// fundamentally needs the broader sweep that --all enables.
#[test]
fn writeback_mirror_tags_requires_all() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    maki()
        .current_dir(&root)
        .args(["writeback", "--mirror-tags"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--all").or(predicate::str::contains("required")));
}

/// Regression: `maki writeback` must NOT clear `pending_writeback` for
/// recipes it skipped because their volume was offline. The previous
/// implementation force-cleared the flag on every recipe in the asset's
/// process set after the per-recipe loop, regardless of whether each
/// recipe was actually written or skipped — so a multi-variant asset
/// with one offline variant would silently lose its pending state on
/// disk (the YAML sidecar got cleared while the SQLite flag for the
/// skipped recipe stayed correct, an internal inconsistency that
/// `rebuild-catalog` from YAML would then surface as lost edits).
#[test]
fn writeback_preserves_pending_when_volume_offline() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Register a separate "removable" volume in a sibling tempdir so the
    // recipe lives on a volume we can independently take offline (by
    // renaming the mount directory). Keeping it outside the catalog
    // root avoids the auto-added test-vol from init_catalog winning the
    // most-specific-mount-point lookup at import time.
    let removable_dir = tempdir().unwrap();
    let removable = removable_dir.path().canonicalize().unwrap();
    maki()
        .current_dir(&root)
        .args(["volume", "add", "removable", removable.to_str().unwrap()])
        .assert()
        .success();

    let xmp_path = removable.join("WB_OFF.xmp");
    create_test_file(&removable, "WB_OFF.ARW", b"raw-offline-vol");
    create_test_file(
        &removable,
        "WB_OFF.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"1\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", removable.to_str().unwrap()])
        .assert()
        .success();

    // Empty query (NOT "*") matches all assets — `*` is treated as a
    // literal free-text token.
    let out = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        !asset_id.is_empty(),
        "search returned no asset after import"
    );

    // Edit while writeback is auto-off — marks the recipe pending.
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "5"])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["status"])
        .assert()
        .stdout(predicate::str::contains("pending XMP writeback"));

    // Take the removable volume offline by renaming its mount point.
    // The device registry uses `mount_point.exists()` to decide online
    // status, so the directory just has to be unreachable at its
    // registered path.
    let stashed = removable.with_extension("offline");
    std::fs::rename(&removable, &stashed).unwrap();

    // Manual flush. Should report the recipe as skipped (offline) and
    // — crucially — leave the pending flag intact. The stdout summary
    // should also name the offline volume so the user knows which
    // drive to reconnect.
    maki()
        .current_dir(&root)
        .args(["writeback"])
        .assert()
        .success()
        .stdout(predicate::str::contains("offline volumes: removable"));

    // Bring the volume back so `maki status` can see the file again
    // when it walks the catalog.
    std::fs::rename(&stashed, &removable).unwrap();

    // SQLite side: status still announces the pending writeback. The
    // bug clear-path was in `writeback_process`'s after-loop sidecar
    // save; we want to confirm the DB flag also stayed put (the inner
    // loop's `continue` already skipped the DB clear, so this is a
    // belt-and-suspenders check that the skip path wasn't structurally
    // moved into the cleared set by a future refactor).
    maki()
        .current_dir(&root)
        .args(["status"])
        .assert()
        .stdout(predicate::str::contains("pending XMP writeback"));

    // YAML sidecar: confirm the recipe entry there also kept
    // pending_writeback: true. The sidecar is the source of truth, so
    // a cleared YAML would be the actual user-visible regression —
    // a subsequent `rebuild-catalog` would lose the pending state.
    let sidecar = root
        .join("metadata")
        .join(&asset_id[..2])
        .join(format!("{asset_id}.yaml"));
    let yaml = std::fs::read_to_string(&sidecar).unwrap();
    assert!(
        yaml.contains("pending_writeback: true"),
        "Sidecar must still have pending_writeback: true after writeback \
         skipped the recipe for an offline volume. Got:\n{yaml}"
    );

    // The XMP itself on disk should NOT have been written — its Rating
    // stays at 1 (the original) since the volume was offline during the
    // flush attempt.
    let xmp_after = std::fs::read_to_string(&xmp_path).unwrap();
    assert!(
        xmp_after.contains("Rating=\"1\"") || xmp_after.contains("Rating='1'"),
        "Offline-volume XMP must stay at Rating=1. Got: {xmp_after}"
    );
}

/// Regression: when `maki writeback` succeeds at flushing a recipe to
/// disk, the YAML sidecar must be re-saved with `pending_writeback:
/// false`. The earlier post-fix at the per-asset sidecar-save loop
/// gated saves on `r.pending_writeback`, but the file_changed branch
/// inside the per-recipe loop had already pre-cleared the in-memory
/// flag (so the sidecar save would carry the new state). The guard
/// then saw the already-cleared value, decided "nothing to do", and
/// skipped the save — leaving the YAML stuck on `pending_writeback:
/// true` while the SQLite catalog and XMP file on disk were in sync.
/// A subsequent `rebuild-catalog` (which trusts YAML as the source of
/// truth) would reintroduce a phantom pending flag on a recipe whose
/// disk file is already current.
#[test]
fn writeback_clears_yaml_pending_flag_on_success() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    create_test_file(&root, "photos/WB_OK.ARW", b"raw-online-success");
    create_test_file(
        &root,
        "photos/WB_OK.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"1\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    let out = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(!asset_id.is_empty(), "search returned no asset after import");

    // Edit while writeback is auto-off (the default test catalog state).
    // The edit marks the recipe pending_writeback in both SQLite and YAML.
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "5"])
        .assert()
        .success();

    // Sanity check: YAML reports pending before the flush.
    let sidecar = root
        .join("metadata")
        .join(&asset_id[..2])
        .join(format!("{asset_id}.yaml"));
    let yaml_before = std::fs::read_to_string(&sidecar).unwrap();
    assert!(
        yaml_before.contains("pending_writeback: true"),
        "Sidecar must report pending after edit. Got:\n{yaml_before}"
    );

    // Manual flush — the recipe is online and writable, so the XMP file
    // gets the rating bump and the catalog clears the pending flag.
    maki()
        .current_dir(&root)
        .args(["writeback"])
        .assert()
        .success();

    // YAML must reflect the cleared state — this is the regression.
    let yaml_after = std::fs::read_to_string(&sidecar).unwrap();
    assert!(
        !yaml_after.contains("pending_writeback: true"),
        "Sidecar must NOT have pending_writeback: true after a successful \
         writeback. The YAML is the source of truth — a stale flag here \
         would reintroduce a phantom pending state on the next \
         `rebuild-catalog`. Got:\n{yaml_after}"
    );

    // Belt-and-suspenders: SQLite agrees the flag is cleared (status
    // should no longer mention pending writeback).
    maki()
        .current_dir(&root)
        .args(["status"])
        .assert()
        .stdout(predicate::str::contains("pending XMP writeback").not());

    // And the XMP file on disk reflects the new rating.
    let xmp_path = root.join("photos/WB_OK.xmp");
    let xmp_after = std::fs::read_to_string(&xmp_path).unwrap();
    assert!(
        xmp_after.contains("Rating=\"5\"") || xmp_after.contains("Rating='5'"),
        "XMP must have been written with Rating=5. Got: {xmp_after}"
    );
}

/// Regression: when an XMP file on disk was already externally
/// modified to match the catalog's pending values (typical case:
/// user rsyncs a primary → backup volume between curating and
/// running `maki writeback`), the writeback path must:
///
/// 1. NOT issue file writes (no churn, no timestamp changes)
/// 2. Clear `pending_writeback` in both SQLite and YAML
/// 3. Reconcile the recipe's stored `content_hash` with the
///    now-different on-disk hash so a subsequent `maki verify`
///    doesn't flag every synced recipe as a mismatch
/// 4. Count under `already_in_sync` in the summary, not `written`
#[test]
fn writeback_reconciles_when_disk_already_matches_catalog() {
    use sha2::Digest;

    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Removable volume so we can simulate offline-during-edit, then
    // rsync-while-offline, then bring back online before writeback.
    let removable_dir = tempdir().unwrap();
    let removable = removable_dir.path().canonicalize().unwrap();
    maki()
        .current_dir(&root)
        .args(["volume", "add", "removable", removable.to_str().unwrap()])
        .assert()
        .success();

    let xmp_path = removable.join("WB_SYNC.xmp");
    create_test_file(&removable, "WB_SYNC.ARW", b"raw-pre-sync");
    create_test_file(
        &removable,
        "WB_SYNC.xmp",
        b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
          <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
          <rdf:Description rdf:about=\"\" \
            xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" \
            xmp:Rating=\"1\"/>\
          </rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", removable.to_str().unwrap()])
        .assert()
        .success();

    let asset_id = String::from_utf8_lossy(
        &maki()
            .current_dir(&root)
            .args(["search", "-q", ""])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert!(!asset_id.is_empty());

    // Take volume offline, edit the rating. Pending flag set; XMP on disk
    // still has rating=1.
    let stashed = removable.with_extension("offline");
    std::fs::rename(&removable, &stashed).unwrap();
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "5"])
        .assert()
        .success();

    // Simulate external rsync: bring the volume "back online" with an XMP
    // that already contains rating=5 (as if the user rsync'd it from a
    // parallel master that had been written to). The pre-rsync hash the
    // catalog still remembers is the rating=1 file; the post-rsync hash
    // is whatever rating=5 hashes to — different.
    std::fs::rename(&stashed, &removable).unwrap();
    std::fs::write(
        &xmp_path,
        b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
          <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
          <rdf:Description rdf:about=\"\" \
            xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" \
            xmp:Rating=\"5\"/>\
          </rdf:RDF></x:xmpmeta>",
    )
    .unwrap();

    let rsynced_bytes = std::fs::read(&xmp_path).unwrap();
    let mut hasher = sha2::Sha256::new();
    hasher.update(&rsynced_bytes);
    let post_rsync_hash = format!("sha256:{}", sha256_hex_of(&hasher.finalize()));
    let mtime_before = std::fs::metadata(&xmp_path).unwrap().modified().unwrap();

    // Run writeback. Should report "1 already in sync, 0 written" and
    // touch nothing on disk.
    maki()
        .current_dir(&root)
        .args(["writeback"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1 already in sync")
                .and(predicate::str::contains("0 written")),
        );

    let mtime_after = std::fs::metadata(&xmp_path).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "writeback must not write the file when disk already matches catalog"
    );

    // Pending flag cleared both in DB (status no longer reports it) and
    // in the YAML sidecar (which would otherwise reintroduce it on a
    // subsequent rebuild-catalog).
    maki()
        .current_dir(&root)
        .args(["status"])
        .assert()
        .stdout(predicate::str::contains("pending XMP writeback").not());

    let sidecar = root
        .join("metadata")
        .join(&asset_id[..2])
        .join(format!("{asset_id}.yaml"));
    let yaml = std::fs::read_to_string(&sidecar).unwrap();
    assert!(
        !yaml.contains("pending_writeback: true"),
        "Sidecar must not have stale pending_writeback after no-op writeback. Got:\n{yaml}"
    );

    // Catalog's stored content_hash for the recipe must now match the
    // post-rsync file — otherwise `maki verify` flags it.
    assert!(
        yaml.contains(&post_rsync_hash),
        "Sidecar must carry the post-rsync content_hash. Expected {post_rsync_hash}. Got:\n{yaml}"
    );
}

/// Regression: `dc:subject` must contain the flat components of EVERY
/// hierarchical tag, not just the subject facet. CaptureOne / Lightroom
/// users with multi-facet vocabularies (person, location, event,
/// project, …) expect every component leaf to land in dc:subject so
/// downstream tools can search them as plain keywords. The hierarchical
/// `lr:hierarchicalSubject` list always carries the full ancestor
/// paths regardless of facet.
#[test]
fn writeback_flat_tags_cover_all_facets() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let xmp_path = root.join("photos/FACETS.xmp");
    create_test_file(&root, "photos/FACETS.ARW", b"raw-facets");
    create_test_file(
        &root,
        "photos/FACETS.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"1\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    let out = maki()
        .current_dir(&root)
        .args(["search", "-q", ""])
        .output()
        .unwrap();
    let asset_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(!asset_id.is_empty(), "search returned no asset after import");

    // Tag across four distinct facets. Each tag is hierarchical so the
    // writeback flattener has something to split per facet.
    maki()
        .current_dir(&root)
        .args([
            "tag",
            &asset_id,
            "subject|nature|tree",
            "person|family|alice",
            "event|wedding|2024",
            "location|france|paris",
        ])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["writeback"])
        .assert()
        .success();

    let xmp = std::fs::read_to_string(&xmp_path).unwrap();

    // Every facet's leaf component (and the intermediate / root
    // components, since the tag command auto-expands ancestors) should
    // land in dc:subject as a separate <rdf:li>.
    let expected_flat = [
        "subject", "nature", "tree",
        "person", "family", "alice",
        "event", "wedding", "2024",
        "location", "france", "paris",
    ];
    for tag in expected_flat {
        let needle = format!("<rdf:li>{tag}</rdf:li>");
        assert!(
            xmp.contains(&needle),
            "dc:subject missing flat component {tag}. \
             Cross-facet flat-tag writeback must cover every facet, not \
             just subject. Got:\n{xmp}"
        );
    }

    // Hierarchical block must carry the full ancestor path for each
    // facet — this is what Lightroom uses to render the tag tree.
    let expected_hier = [
        "subject|nature|tree",
        "person|family|alice",
        "event|wedding|2024",
        "location|france|paris",
    ];
    for path in expected_hier {
        let needle = format!("<rdf:li>{path}</rdf:li>");
        assert!(
            xmp.contains(&needle),
            "lr:hierarchicalSubject missing path {path}. Got:\n{xmp}"
        );
    }
}

#[test]
fn sync_metadata_dry_run() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let xmp_path = root.join("photos/SM_004.xmp");
    create_test_file(&root, "photos/SM_004.ARW", b"raw-sm-dryrun");
    create_test_file(
        &root,
        "photos/SM_004.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"2\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    // Modify XMP externally
    std::fs::write(
        &xmp_path,
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"5\"/></rdf:RDF></x:xmpmeta>",
    )
    .unwrap();

    // Dry run should report but not apply
    maki()
        .current_dir(&root)
        .args(["sync-metadata", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 read from disk"));

    // Rating should still be 2 (unchanged)
    maki()
        .current_dir(&root)
        .args(["search", "--json", "rating:2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SM_004"));
}

#[test]
fn sync_metadata_json_output() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    create_test_file(&root, "photos/SM_005.ARW", b"raw-sm-json");
    create_test_file(
        &root,
        "photos/SM_005.xmp",
        b"<x:xmpmeta><rdf:RDF><rdf:Description xmp:Rating=\"3\"/></rdf:RDF></x:xmpmeta>",
    );
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
        .assert()
        .success();

    let output = maki()
        .current_dir(&root)
        .args(["sync-metadata", "--json"])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["unchanged"], 1);
    assert_eq!(parsed["inbound"], 0);
    assert_eq!(parsed["outbound"], 0);
    assert_eq!(parsed["conflicts"], 0);
    assert_eq!(parsed["dry_run"], false);
}

/// `maki writeback --force` opts into the writeback path for recipes
/// in the explicit scope even when SQLite reports `pending_writeback
/// = 0`. With no XMP tag blocks present (this fixture's case), the
/// run completes as "already in sync" because there's nothing to
/// rebuild.
#[test]
fn writeback_force_rewrites_non_pending_recipe() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    create_test_file(&root, "WB_FORCE.ARW", b"raw-force-test");
    create_test_file(
        &root,
        "WB_FORCE.xmp",
        b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
          <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
          <rdf:Description rdf:about=\"\" \
            xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" \
            xmp:Rating=\"1\"/>\
          </rdf:RDF></x:xmpmeta>",
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

    // Plain `writeback --asset <id>` finds nothing pending and is a
    // no-op — confirms the test fixture has pending=0 in SQLite.
    let plain = maki()
        .current_dir(&root)
        .args(["writeback", "--asset", &asset_id])
        .output()
        .unwrap();
    let plain_out = String::from_utf8_lossy(&plain.stdout);
    assert!(
        plain_out.contains("0 written"),
        "Plain writeback should report 0 written when no recipe is \
         pending. Got:\n{plain_out}"
    );

    // With --force, the same scope picks up the recipe and processes
    // it. The XMP already carries Rating=1 (which is what the catalog
    // has — import absorbed it), so the result is `already in sync`
    // not `written` — that's still the right behaviour: --force opts
    // into the writeback path, the catalog/disk reconciliation runs,
    // and the test asserts that opt-in happened.
    let forced = maki()
        .current_dir(&root)
        .args(["writeback", "--asset", &asset_id, "--force"])
        .output()
        .unwrap();
    let forced_out = String::from_utf8_lossy(&forced.stdout);
    assert!(
        forced_out.contains("already in sync"),
        "writeback --force must process the recipe even when not \
         pending. Expected 'already in sync' in output. Got:\n{forced_out}"
    );
}

/// `--force` rebuilds the dc:subject + lr:hierarchicalSubject blocks
/// from catalog state, dropping any pre-existing XMP entry that isn't
/// in the catalog. Reproduces the "stale runaway-escape entry stuck in
/// a file that --mirror-tags didn't catch" scenario the user hit on
/// Z91_4714.xmp.
///
/// Fixture setup is non-trivial: we want a clean *catalog* state plus
/// a *damaged* on-disk XMP, but `maki import` reads the XMP we hand
/// it. So we import a clean file first, set tags via CLI, then
/// overwrite the on-disk XMP with the damaged content.
#[test]
fn writeback_force_drops_stale_xmp_entry() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    // Step 1: import a RAW with a clean XMP so the catalog learns
    // only what we want it to know.
    create_test_file(&root, "WB_FORCE_STALE.ARW", b"raw");
    let xmp_path = root.join("WB_FORCE_STALE.xmp");
    std::fs::write(
        &xmp_path,
        br#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>person|ensemble|band|Bobby &amp; the BigTones</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#,
    )
    .unwrap();

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

    // Step 2: overwrite the XMP with damaged content the catalog
    // does NOT know about — multi-layer escape leftovers in
    // dc:subject AND leaf-only legacy entries in lr:hierarchicalSubject.
    std::fs::write(
        &xmp_path,
        br#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="0">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>Bobby &amp; the BigTones</rdf:li>
     <rdf:li>Bobby &amp;amp;amp;amp;amp;amp;amp;amp; the BigTones</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>person|ensemble|band|Bobby &amp; the BigTones</rdf:li>
     <rdf:li>Bavaria</rdf:li>
     <rdf:li>Bobby &amp;amp; the BigTones</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#,
    )
    .unwrap();

    // Step 3: `--force` rewrites both tag blocks from catalog state.
    // The catalog only knows `person|ensemble|band|Bobby & the BigTones`;
    // the leaf-only entries and multi-escape leftovers have no
    // catalog home and get dropped.
    maki()
        .current_dir(&root)
        .args(["writeback", "--asset", &asset_id, "--force"])
        .assert()
        .success();

    let after = std::fs::read_to_string(&xmp_path).unwrap();
    assert!(
        !after.contains("&amp;amp;"),
        "stale multi-layer-escape entries must be removed from BOTH \
         dc:subject and lr:hierarchicalSubject:\n{after}"
    );
    assert!(
        !after.contains("<rdf:li>Bavaria</rdf:li>"),
        "leaf-only legacy entries in lr:hierarchicalSubject must be \
         removed:\n{after}"
    );
    assert!(
        after.contains("Bobby &amp; the BigTones"),
        "correctly-escaped entry must survive:\n{after}"
    );
    assert!(
        after.contains("<rdf:li>person|ensemble|band|Bobby &amp; the BigTones</rdf:li>"),
        "correctly-pipe-pathed hierarchical entry must survive:\n{after}"
    );
}

/// End-to-end embedded write-back: import a real JPEG with embedded
/// XMP, change rating + tags in the catalog, run
/// `maki writeback --embed --force`, and verify the JPEG file itself
/// now carries the new metadata, the original is preserved in the
/// trash, the variant's content hash migrated, and `maki verify`
/// confirms the stores are hash-consistent with the disk.
#[test]
fn writeback_embed_writes_jpeg_and_migrates_hash() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let jpeg_path = root.join("photos/EMB_001.jpg");
    let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="1">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    create_test_file(&root, "photos/EMB_001.jpg", &build_jpeg_with_xmp(xmp));
    maki()
        .current_dir(&root)
        .args(["import", root.join("photos").to_str().unwrap()])
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

    // Catalog-side edits (auto-writeback is off by default; with no
    // .xmp recipe there's nothing to stage anyway).
    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "4"])
        .assert()
        .success();
    maki()
        .current_dir(&root)
        .args(["tag", &asset_id, "subject|nature|sunset"])
        .assert()
        .success();

    let before = std::fs::read(&jpeg_path).unwrap();

    // `--force`: a JPEG-only asset has no pending recipes, so the
    // default pending-driven scope would find nothing.
    maki()
        .current_dir(&root)
        .args(["writeback", "--embed", "--force", "--asset", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Writeback (embedded): 1 written"))
        .stdout(predicate::str::contains("1 original(s) preserved in trash"));

    // The JPEG file itself changed and carries the new metadata.
    let after = std::fs::read(&jpeg_path).unwrap();
    assert_ne!(after, before, "JPEG bytes must change");
    let after_str = String::from_utf8_lossy(&after);
    assert!(
        after_str.contains("xmp:Rating=\"4\""),
        "embedded XMP must carry the new rating:\n{after_str}"
    );
    assert!(
        after_str.contains("<rdf:li>subject|nature|sunset</rdf:li>"),
        "embedded XMP must carry the hierarchical tag:\n{after_str}"
    );
    assert!(
        after_str.contains("<rdf:li>sunset</rdf:li>"),
        "embedded XMP must carry the flat dc:subject component:\n{after_str}"
    );

    // Original preserved in the catalog trash.
    maki()
        .current_dir(&root)
        .args(["trash", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("EMB_001.jpg"));

    // Variant identity migrated: `maki verify --force` re-hashes the
    // file on disk against the catalog's stored content hash. A stale
    // (un-migrated) hash would report a FAILED mismatch and exit
    // non-zero.
    maki()
        .current_dir(&root)
        .args(["verify", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 verified"));

    // Second run is a no-op: already in sync, no new trash copy, no
    // further byte changes.
    maki()
        .current_dir(&root)
        .args(["writeback", "--embed", "--force", "--asset", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 written, 1 already in sync"));
    assert_eq!(std::fs::read(&jpeg_path).unwrap(), after);
}

/// `--embed --dry-run` reports what would be written without touching
/// the file, the trash, or the catalog hashes.
#[test]
fn writeback_embed_dry_run_touches_nothing() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let jpeg_path = root.join("EMB_DRY.jpg");
    let xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
               xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
               <rdf:Description rdf:about=\"\" \
               xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"1\"/>\
               </rdf:RDF></x:xmpmeta>";
    create_test_file(&root, "EMB_DRY.jpg", &build_jpeg_with_xmp(xmp));
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

    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "5"])
        .assert()
        .success();

    let before = std::fs::read(&jpeg_path).unwrap();
    maki()
        .current_dir(&root)
        .args(["writeback", "--embed", "--force", "--asset", &asset_id, "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Dry run"))
        .stdout(predicate::str::contains("1 written"));

    assert_eq!(std::fs::read(&jpeg_path).unwrap(), before, "dry-run must not modify the JPEG");
    assert!(!root.join(".trash").exists(), "dry-run must not create trash copies");
}

/// `--embed --no-trash` rewrites the JPEG without preserving the
/// original in the catalog trash.
#[test]
fn writeback_embed_no_trash_skips_trash_copy() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
               xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
               <rdf:Description rdf:about=\"\" \
               xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"1\"/>\
               </rdf:RDF></x:xmpmeta>";
    create_test_file(&root, "EMB_NT.jpg", &build_jpeg_with_xmp(xmp));
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

    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "2"])
        .assert()
        .success();

    maki()
        .current_dir(&root)
        .args(["writeback", "--embed", "--force", "--no-trash", "--asset", &asset_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 written"))
        .stdout(predicate::str::contains("preserved in trash").not());

    assert!(!root.join(".trash").exists(), "--no-trash must not create trash copies");
}

/// TIFF variants are skipped with a per-file status — embedded
/// write-back is JPEG-only in v1 (IFD rewriting deferred).
#[test]
fn writeback_embed_skips_tiff_variant() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());

    let tiff_path = root.join("EMB_T.tif");
    create_test_file(&root, "EMB_T.tif", b"II*\0fake-tiff-bytes");
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

    maki()
        .current_dir(&root)
        .args(["edit", &asset_id, "--rating", "3"])
        .assert()
        .success();

    let before = std::fs::read(&tiff_path).unwrap();
    maki()
        .current_dir(&root)
        .args(["writeback", "--embed", "--force", "--asset", &asset_id, "--log"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 skipped"))
        .stderr(predicate::str::contains("not supported for TIFF yet"));
    assert_eq!(std::fs::read(&tiff_path).unwrap(), before, "TIFF must be untouched");
}
