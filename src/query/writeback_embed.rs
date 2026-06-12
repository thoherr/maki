//! Embedded XMP write-back — `maki writeback --embed` (Pro).
//!
//! Where the sibling module (`writeback.rs`) flushes catalog metadata
//! into `.xmp` *recipe sidecars*, this one writes it into the JPEG
//! variant files themselves: the APP1 XMP segment is replaced (or
//! inserted) with a packet carrying the catalog's rating, label,
//! description, and tags. `--embed` REPLACES the recipe flush for the
//! run — sidecar `.xmp` recipes are untouched.
//!
//! Scope selection mirrors the sidecar writeback (`--asset`/query/
//! `--volume`/`--all`/`--force` interplay), but the target set is JPEG
//! variants with online file locations, not `.xmp` recipes:
//!
//! - default        — assets owning at least one recipe flagged
//!                    `pending_writeback` (the same "staged changes"
//!                    signal the sidecar flush uses)
//! - `--all`/`--force` — every asset (any caller-supplied scope still
//!                    applies via the in-memory ID set)
//! - `--volume`     — restricts which *file locations* are written
//!                    (only JPEG copies on that volume)
//!
//! Tag semantics reuse the sidecar writeback's conventions verbatim:
//! `dc:subject` gets flat components, `lr:hierarchicalSubject` gets
//! ancestor paths; `--force` rebuilds both blocks from catalog state,
//! `--mirror-tags` removes entries the catalog doesn't have, default
//! is additive. The packet edits go through the same
//! `xmp_reader::update_*_str` locate-and-splice writers as sidecars.
//!
//! There is NO pending-flag machinery for embedded targets (v1):
//! offline volumes and missing files are counted as skipped with the
//! volume named — nothing is queued for later.
//!
//! # Safety ladder (per file, non-negotiable order)
//!
//! 1. copy the original into the catalog trash
//!    ([`Trash::preserve_copy`]; skipped with `--no-trash`)
//! 2. write the rewritten JPEG to a temp file next to the original
//! 3. verify the temp file — its embedded XMP must parse to exactly
//!    the packet we spliced in; on mismatch delete the temp and fail
//!    the file (original untouched)
//! 4. atomic rename over the original
//! 5. re-hash and migrate the variant identity
//!    ([`Catalog::migrate_variant_hash`] + sidecar YAML + preview file
//!    renames + denormalized columns)
//!
//! Idempotence: the new packet is computed first and compared against
//! the existing one — byte-equality counts as `already_in_sync`, no
//! write, no hash churn.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::catalog::Catalog;
use crate::content_store::ContentStore;
use crate::device_registry::DeviceRegistry;
use crate::embedded_xmp_write;
use crate::metadata_store::MetadataStore;
use crate::models::Asset;
use crate::trash::{Disposition, Trash};
use crate::xmp_reader;

use super::{EmbedWritebackResult, QueryEngine};

/// Outcome of one JPEG file rewrite attempt.
enum FileOutcome {
    /// File rewritten (or would be, in dry-run).
    Written,
    /// Embedded XMP already carries the catalog state — no write.
    AlreadyInSync,
    /// Failed; original untouched. Message for the error list.
    Failed(String),
}

impl QueryEngine {
    /// Write catalog metadata (rating, label, description, tags) into
    /// the embedded XMP of every JPEG variant in scope. See the module
    /// docs for semantics; see [`EmbedWritebackResult`] for reporting.
    #[allow(clippy::too_many_arguments)]
    pub fn writeback_embedded(
        &self,
        volume_filter: Option<&str>,
        asset_id_set: Option<&HashSet<String>>,
        all: bool,
        force: bool,
        mirror_tags: bool,
        no_trash: bool,
        dry_run: bool,
        log: bool,
        callback: Option<&dyn Fn(&str, &str)>,
    ) -> Result<EmbedWritebackResult> {
        let catalog = Catalog::open(&self.catalog_root)?;
        let store = MetadataStore::new(&self.catalog_root);
        let registry = DeviceRegistry::new(&self.catalog_root);
        let volumes = registry.list()?;
        let online: HashMap<uuid::Uuid, PathBuf> = volumes
            .iter()
            .filter(|v| v.is_online)
            .map(|v| (v.id, v.mount_point.clone()))
            .collect();
        let volume_labels: HashMap<uuid::Uuid, String> =
            volumes.iter().map(|v| (v.id, v.label.clone())).collect();
        let content_store = ContentStore::new(&self.catalog_root);
        let trash = Trash::from_config(&self.catalog_root, no_trash);
        let config = crate::config::CatalogConfig::load(&self.catalog_root).unwrap_or_default();
        let previewer = crate::preview::PreviewGenerator::new(
            &self.catalog_root,
            crate::Verbosity::quiet(),
            &config.preview,
        );

        // Resolve volume filter to a volume ID (restricts which file
        // locations get written).
        let volume_id_filter: Option<uuid::Uuid> = if let Some(label) = volume_filter {
            let vol = volumes
                .iter()
                .find(|v| v.label == label)
                .ok_or_else(|| anyhow::anyhow!("unknown volume: {label}"))?;
            Some(vol.id)
        } else {
            None
        };

        // Target assets. Default mirrors the sidecar flush: only assets
        // with staged (pending) recipe changes. `--all`/`--force` widen
        // to every asset; any explicit scope (query/--asset) still
        // applies through `asset_id_set`.
        let mut asset_ids: Vec<String> = if all || force {
            let mut stmt = catalog.conn().prepare("SELECT id FROM assets")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        } else {
            let mut stmt = catalog.conn().prepare(
                "SELECT DISTINCT v.asset_id FROM recipes r \
                 JOIN variants v ON r.variant_hash = v.content_hash \
                 WHERE r.pending_writeback = 1",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        if let Some(set) = asset_id_set {
            asset_ids.retain(|id| set.contains(id));
        }
        asset_ids.sort();

        let mut result = EmbedWritebackResult {
            dry_run,
            ..Default::default()
        };

        for asset_id in &asset_ids {
            let asset_uuid: uuid::Uuid = match asset_id.parse() {
                Ok(u) => u,
                Err(_) => {
                    result.errors.push(format!("Invalid asset ID: {asset_id}"));
                    result.failed += 1;
                    continue;
                }
            };
            let mut asset = match store.load(asset_uuid) {
                Ok(a) => a,
                Err(e) => {
                    result
                        .errors
                        .push(format!("Could not load asset {asset_id}: {e}"));
                    result.failed += 1;
                    continue;
                }
            };

            // Catalog tag material, same conventions as the sidecar
            // flush: dc:subject gets flat components, lr:hierarchical-
            // Subject gets ancestor paths. Sorted for deterministic
            // rendering order.
            let mut dc_tags: Vec<String> = asset
                .tags
                .iter()
                .flat_map(|t| t.split('|').map(|s| s.to_string()))
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            dc_tags.sort();
            let lr_tags = crate::tag_util::expand_all_ancestors(&asset.tags);

            // (variant index, old hash, new hash) collected during the
            // write loop; applied to catalog + sidecar afterwards so we
            // don't mutate `asset.variants` while iterating it.
            let mut hash_migrations: Vec<(usize, String, String)> = Vec::new();

            for (vi, variant) in asset.variants.iter().enumerate() {
                let fmt = variant.format.to_lowercase();
                if fmt == "tif" || fmt == "tiff" {
                    // Explicitly deferred: TIFF metadata lives in the
                    // IFD chain — real binary surgery, not a segment
                    // splice. Counted per file so the summary is honest.
                    for loc in &variant.locations {
                        if let Some(vf) = volume_id_filter {
                            if loc.volume_id != vf {
                                continue;
                            }
                        }
                        result.skipped += 1;
                        let rel = loc.relative_path_str();
                        if log {
                            eprintln!(
                                "{rel} — skipped (embedded write-back not supported for TIFF yet)"
                            );
                        }
                        if let Some(cb) = callback {
                            cb(&rel, "skipped (TIFF not supported)");
                        }
                    }
                    continue;
                }
                if fmt != "jpg" && fmt != "jpeg" {
                    continue;
                }

                let mut written_paths: Vec<PathBuf> = Vec::new();
                for loc in &variant.locations {
                    if let Some(vf) = volume_id_filter {
                        if loc.volume_id != vf {
                            continue;
                        }
                    }
                    let rel = loc.relative_path_str();
                    let mount_point = match online.get(&loc.volume_id) {
                        Some(mp) => mp.as_path(),
                        None => {
                            result.skipped += 1;
                            let label = volume_labels
                                .get(&loc.volume_id)
                                .cloned()
                                .unwrap_or_else(|| loc.volume_id.to_string());
                            if log {
                                eprintln!("{rel} — skipped (volume offline: {label})");
                            }
                            result.skipped_offline_volumes.insert(label);
                            continue;
                        }
                    };
                    let full_path = mount_point.join(&loc.relative_path);
                    if !full_path.exists() {
                        result.skipped += 1;
                        if log {
                            eprintln!("{rel} — skipped (file missing)");
                        }
                        continue;
                    }

                    let volume_label = volume_labels
                        .get(&loc.volume_id)
                        .cloned()
                        .unwrap_or_else(|| loc.volume_id.to_string());
                    match embed_one_jpeg(
                        &full_path,
                        &rel,
                        &volume_label,
                        &asset,
                        &dc_tags,
                        &lr_tags,
                        force,
                        mirror_tags,
                        &trash,
                        dry_run,
                        &mut result.trashed_originals,
                    ) {
                        FileOutcome::Written => {
                            result.written += 1;
                            let status = if dry_run {
                                "would write (embedded)"
                            } else {
                                "written (embedded)"
                            };
                            if log {
                                eprintln!("{rel} — {status}");
                            }
                            if let Some(cb) = callback {
                                cb(&rel, status);
                            }
                            if !dry_run {
                                written_paths.push(full_path);
                            }
                        }
                        FileOutcome::AlreadyInSync => {
                            result.already_in_sync += 1;
                            if log {
                                eprintln!("{rel} — already in sync (no write)");
                            }
                            if let Some(cb) = callback {
                                cb(&rel, "already in sync");
                            }
                        }
                        FileOutcome::Failed(msg) => {
                            result.failed += 1;
                            if log {
                                eprintln!("{rel} — FAILED");
                            }
                            if let Some(cb) = callback {
                                cb(&rel, "failed");
                            }
                            result.errors.push(msg);
                        }
                    }
                }

                // The file bytes changed, so the content hash — the
                // variant's identity — changed with them. All copies of
                // the variant started identical and the rewrite is
                // deterministic, so hashing the first rewritten copy
                // covers them all.
                if let Some(first) = written_paths.first() {
                    match content_store.hash_file(first) {
                        Ok(new_hash) => {
                            if new_hash != variant.content_hash {
                                hash_migrations.push((
                                    vi,
                                    variant.content_hash.clone(),
                                    new_hash,
                                ));
                            }
                        }
                        Err(e) => {
                            result.errors.push(format!(
                                "Could not re-hash {} after write: {e}",
                                first.display()
                            ));
                        }
                    }
                }
            }

            // Variant identity migration: SQLite cascade, then the YAML
            // sidecar (source of truth), preview files, and the
            // denormalized columns.
            if !hash_migrations.is_empty() {
                for (vi, old_hash, new_hash) in &hash_migrations {
                    if let Err(e) = catalog.migrate_variant_hash(old_hash, new_hash) {
                        result.errors.push(format!(
                            "Could not migrate variant hash for {asset_id}: {e}"
                        ));
                        continue;
                    }
                    asset.variants[*vi].content_hash = new_hash.clone();
                    for r in &mut asset.recipes {
                        if r.variant_hash == *old_hash {
                            r.variant_hash = new_hash.clone();
                        }
                    }
                    if asset.preview_variant.as_deref() == Some(old_hash.as_str()) {
                        asset.preview_variant = Some(new_hash.clone());
                    }
                    rename_preview_files(&previewer, old_hash, new_hash);
                }
                if let Err(e) = store.save(&asset) {
                    result
                        .errors
                        .push(format!("Could not save sidecar for {asset_id}: {e}"));
                }
                if let Err(e) = catalog.update_denormalized_variant_columns(&asset) {
                    result.errors.push(format!(
                        "Could not update denormalized columns for {asset_id}: {e}"
                    ));
                }
            }
        }

        Ok(result)
    }
}

/// Rewrite one JPEG file's embedded XMP to carry the catalog state,
/// following the safety ladder (trash copy → temp file → verify →
/// atomic rename). Pure per-file step: hashing and identity migration
/// are the caller's job.
#[allow(clippy::too_many_arguments)]
fn embed_one_jpeg(
    full_path: &Path,
    rel_path: &str,
    volume_label: &str,
    asset: &Asset,
    dc_tags: &[String],
    lr_tags: &[String],
    force: bool,
    mirror_tags: bool,
    trash: &Trash,
    dry_run: bool,
    trashed_originals: &mut u32,
) -> FileOutcome {
    let data = match std::fs::read(full_path) {
        Ok(d) => d,
        Err(e) => return FileOutcome::Failed(format!("{rel_path}: could not read: {e}")),
    };

    let existing = embedded_xmp_write::read_xmp_packet(&data);
    let new_xml = match &existing {
        Some(xml) => {
            // Same three remove-set modes as the sidecar flush
            // (`writeback_process`): `force` rebuilds the keyword
            // blocks from scratch, `mirror_tags` drops entries the
            // catalog doesn't have, default is additive.
            let (dc_remove, lr_remove): (Vec<String>, Vec<String>) = if force || mirror_tags {
                let parsed = xmp_reader::parse_xmp(xml);
                if force {
                    (parsed.keywords, parsed.hierarchical_keywords)
                } else {
                    let dc_set: HashSet<&str> = dc_tags.iter().map(|s| s.as_str()).collect();
                    let lr_set: HashSet<&str> = lr_tags.iter().map(|s| s.as_str()).collect();
                    (
                        parsed
                            .keywords
                            .iter()
                            .filter(|t| !dc_set.contains(t.as_str()))
                            .cloned()
                            .collect(),
                        parsed
                            .hierarchical_keywords
                            .iter()
                            .filter(|t| !lr_set.contains(t.as_str()))
                            .cloned()
                            .collect(),
                    )
                }
            } else {
                (Vec::new(), Vec::new())
            };

            // Same field order as the sidecar flush.
            let mut s = xmp_reader::update_rating_str(xml, asset.rating);
            s = xmp_reader::update_label_str(&s, asset.color_label.as_deref());
            s = xmp_reader::update_description_str(&s, asset.description.as_deref());
            if !dc_tags.is_empty() || !dc_remove.is_empty() {
                s = xmp_reader::update_tags_str(&s, dc_tags, &dc_remove);
                s = xmp_reader::update_hierarchical_subjects_str(&s, lr_tags, &lr_remove);
            }
            s
        }
        None => {
            // No XMP segment yet. Only insert one if the catalog has
            // something to say — an all-empty packet would be churn.
            let has_rating = asset.rating.is_some_and(|r| r > 0);
            let has_desc = asset.description.as_deref().is_some_and(|d| !d.is_empty());
            if !has_rating && asset.color_label.is_none() && !has_desc && asset.tags.is_empty() {
                return FileOutcome::AlreadyInSync;
            }
            embedded_xmp_write::wrap_xmp_packet(&xmp_reader::create_xmp(
                &asset.tags,
                asset.rating,
                asset.color_label.as_deref(),
                asset.description.as_deref(),
            ))
        }
    };

    // Idempotence: byte-identical packet means the file already carries
    // the catalog state (the update_*_str writers are byte-stable on
    // no-ops).
    if existing.as_deref() == Some(new_xml.as_str()) {
        return FileOutcome::AlreadyInSync;
    }

    let new_data = match embedded_xmp_write::splice_xmp_into_jpeg(&data, &new_xml) {
        Ok(d) => d,
        Err(e) => return FileOutcome::Failed(format!("{rel_path}: {e}")),
    };
    if new_data == data {
        return FileOutcome::AlreadyInSync;
    }

    if dry_run {
        return FileOutcome::Written;
    }

    // Ladder step 1: preserve the original in the catalog trash. A
    // failed copy aborts the file — we never rewrite without the
    // safety copy (unless the trash is explicitly disabled).
    match trash.preserve_copy(full_path, volume_label, rel_path) {
        Ok(Disposition::Trashed(_)) => *trashed_originals += 1,
        Ok(Disposition::Deleted) => {} // trash disabled — nothing preserved
        Err(e) => {
            return FileOutcome::Failed(format!(
                "{rel_path}: could not preserve original in trash: {e}"
            ));
        }
    }

    // Ladder step 2: temp file next to the original (same directory so
    // the final rename is atomic on the same filesystem). The `.jpg`
    // suffix lets `extract_embedded_xmp` verify it like any JPEG.
    let file_name = full_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let temp = full_path.with_file_name(format!(".{file_name}.maki-embed-tmp.jpg"));
    if let Err(e) = std::fs::write(&temp, &new_data) {
        let _ = std::fs::remove_file(&temp);
        return FileOutcome::Failed(format!("{rel_path}: could not write temp file: {e}"));
    }

    // Ladder steps 3 + 4: verify, then atomic rename.
    let expected = xmp_reader::parse_xmp(&new_xml);
    match embedded_xmp_write::verify_and_swap(full_path, &temp, &expected) {
        Ok(()) => FileOutcome::Written,
        Err(msg) => FileOutcome::Failed(format!("{rel_path}: {msg}")),
    }
}

/// Move preview + smart-preview files from the old hash path to the
/// new one. Renames whatever exists; silently ignores missing files
/// (previews are regenerable).
fn rename_preview_files(
    previewer: &crate::preview::PreviewGenerator,
    old_hash: &str,
    new_hash: &str,
) {
    let pairs = [
        (
            previewer.preview_path(old_hash),
            previewer.preview_path(new_hash),
        ),
        (
            previewer.smart_preview_path(old_hash),
            previewer.smart_preview_path(new_hash),
        ),
    ];
    for (old_p, new_p) in pairs {
        if old_p.exists() {
            if let Some(parent) = new_p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::rename(&old_p, &new_p);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use crate::catalog::Catalog;
    use crate::content_store::ContentStore;
    use crate::device_registry::DeviceRegistry;
    use crate::metadata_store::MetadataStore;
    use crate::models::volume::FileLocation;
    use crate::models::{Asset, AssetType, Variant, VariantRole};
    use crate::query::QueryEngine;

    /// Minimal JPEG with an embedded XMP packet carrying Rating=1.
    fn build_jpeg(xmp_xml: &str) -> Vec<u8> {
        crate::embedded_xmp::test_builders::build_jpeg_with_xmp(xmp_xml)
    }

    fn initial_xmp() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="1">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#
    }

    /// Build a catalog root with one registered (online) volume, one
    /// asset whose single variant is a real JPEG on disk. Returns
    /// (tempdir guard, root, asset, jpeg path).
    fn setup_catalog_with_jpeg() -> (tempfile::TempDir, PathBuf, Asset, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();

        let catalog = Catalog::open(&root).unwrap();
        catalog.initialize().unwrap();
        DeviceRegistry::init(&root).unwrap();
        let registry = DeviceRegistry::new(&root);
        let volume = registry
            .register("TESTVOL", &root, crate::models::VolumeType::External, None)
            .unwrap();
        catalog.ensure_volume(&volume).unwrap();

        // Real JPEG with embedded XMP on disk.
        let jpeg_rel = "photos/IMG_1.jpg";
        let jpeg_path = root.join(jpeg_rel);
        std::fs::create_dir_all(jpeg_path.parent().unwrap()).unwrap();
        std::fs::write(&jpeg_path, build_jpeg(initial_xmp())).unwrap();

        let content_store = ContentStore::new(&root);
        let hash = content_store.hash_file(&jpeg_path).unwrap();

        let mut asset = Asset::new(AssetType::Image, &hash);
        asset.rating = Some(5);
        asset.tags = vec!["subject|nature|sunset".to_string()];
        asset.preview_variant = Some(hash.clone());
        let variant = Variant {
            content_hash: hash.clone(),
            asset_id: asset.id,
            role: VariantRole::Original,
            format: "jpg".to_string(),
            file_size: std::fs::metadata(&jpeg_path).unwrap().len(),
            original_filename: "IMG_1.jpg".to_string(),
            source_metadata: Default::default(),
            locations: vec![FileLocation {
                volume_id: volume.id,
                relative_path: PathBuf::from(jpeg_rel),
                verified_at: None,
            }],
        };
        asset.variants.push(variant.clone());

        let store = MetadataStore::new(&root);
        store.save(&asset).unwrap();
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&variant).unwrap();
        catalog
            .insert_file_location(&hash, &variant.locations[0])
            .unwrap();
        catalog.update_denormalized_variant_columns(&asset).unwrap();

        (dir, root, asset, jpeg_path)
    }

    fn run_embed(root: &Path, asset_id: &str, dry_run: bool) -> crate::query::EmbedWritebackResult {
        let engine = QueryEngine::new(root);
        let scope: HashSet<String> = HashSet::from([asset_id.to_string()]);
        engine
            .writeback_embedded(
                None,
                Some(&scope),
                false,
                true,  // force — the fixture has no pending recipes
                false, // mirror_tags
                false, // no_trash (trash stays on)
                dry_run,
                false,
                None,
            )
            .unwrap()
    }

    #[test]
    fn embedded_writeback_migrates_hash_and_stays_doctor_clean() {
        let (_dir, root, asset, jpeg_path) = setup_catalog_with_jpeg();
        let old_hash = asset.variants[0].content_hash.clone();

        // Plant a fake preview + smart preview for the old hash so the
        // rename step has something to move.
        let config = crate::config::CatalogConfig::default();
        let previewer = crate::preview::PreviewGenerator::new(
            &root,
            crate::Verbosity::quiet(),
            &config.preview,
        );
        for p in [
            previewer.preview_path(&old_hash),
            previewer.smart_preview_path(&old_hash),
        ] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"preview-bytes").unwrap();
        }

        let result = run_embed(&root, &asset.id.to_string(), false);
        assert_eq!(result.written, 1, "errors: {:?}", result.errors);
        assert_eq!(result.failed, 0, "errors: {:?}", result.errors);
        assert_eq!(result.trashed_originals, 1);

        // The file now carries the catalog metadata.
        let xmp = crate::embedded_xmp::extract_embedded_xmp(&jpeg_path);
        assert_eq!(
            xmp.source_metadata.get("rating").map(String::as_str),
            Some("5")
        );
        assert!(xmp
            .hierarchical_keywords
            .contains(&"subject|nature|sunset".to_string()));

        // Variant identity migrated everywhere.
        let content_store = ContentStore::new(&root);
        let new_hash = content_store.hash_file(&jpeg_path).unwrap();
        assert_ne!(new_hash, old_hash);

        let store = MetadataStore::new(&root);
        let reloaded = store.load(asset.id).unwrap();
        assert_eq!(reloaded.variants[0].content_hash, new_hash);
        assert_eq!(reloaded.preview_variant.as_deref(), Some(new_hash.as_str()));

        let catalog = Catalog::open(&root).unwrap();
        let stored_best: Option<String> = catalog
            .conn()
            .query_row(
                "SELECT best_variant_hash FROM assets WHERE id = ?1",
                rusqlite::params![asset.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored_best.as_deref(), Some(new_hash.as_str()));
        let loc_count: i64 = catalog
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM file_locations WHERE content_hash = ?1",
                rusqlite::params![new_hash],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(loc_count, 1);

        // Preview files renamed to the new hash path.
        assert!(previewer.preview_path(&new_hash).exists());
        assert!(previewer.smart_preview_path(&new_hash).exists());
        assert!(!previewer.preview_path(&old_hash).exists());

        // Original preserved in the trash.
        let trash_entries = crate::trash::Trash::list(&root).unwrap();
        assert_eq!(trash_entries.len(), 1);
        assert_eq!(trash_entries[0].relative_path, "photos/IMG_1.jpg");

        // The migration must leave both stores consistent: doctor's
        // field-by-field compare reports zero mismatches.
        let report = crate::doctor::run_doctor(&root, None, false, |_, _| {}).unwrap();
        assert!(
            report.healthy(),
            "doctor found issues after hash migration: mismatched={:?} \
             missing_in_catalog={:?} missing_sidecar={:?}",
            report.mismatched,
            report.missing_in_catalog,
            report.missing_sidecar
        );

        // Idempotence: a second run is a no-op.
        let again = run_embed(&root, &asset.id.to_string(), false);
        assert_eq!(again.written, 0);
        assert_eq!(again.already_in_sync, 1);
        assert_eq!(again.trashed_originals, 0);
        assert_eq!(content_store.hash_file(&jpeg_path).unwrap(), new_hash);
    }

    #[test]
    fn embedded_writeback_dry_run_touches_nothing() {
        let (_dir, root, asset, jpeg_path) = setup_catalog_with_jpeg();
        let before = std::fs::read(&jpeg_path).unwrap();

        let result = run_embed(&root, &asset.id.to_string(), true);
        assert!(result.dry_run);
        assert_eq!(result.written, 1);
        assert_eq!(result.trashed_originals, 0);

        assert_eq!(std::fs::read(&jpeg_path).unwrap(), before);
        assert!(crate::trash::Trash::list(&root).unwrap().is_empty());
        // Catalog hash unchanged.
        let store = MetadataStore::new(&root);
        let reloaded = store.load(asset.id).unwrap();
        assert_eq!(
            reloaded.variants[0].content_hash,
            asset.variants[0].content_hash
        );
    }

    #[test]
    fn embedded_writeback_no_trash_skips_preservation() {
        let (_dir, root, asset, _jpeg_path) = setup_catalog_with_jpeg();
        let engine = QueryEngine::new(&root);
        let scope: HashSet<String> = HashSet::from([asset.id.to_string()]);
        let result = engine
            .writeback_embedded(
                None,
                Some(&scope),
                false,
                true,
                false,
                true, // no_trash
                false,
                false,
                None,
            )
            .unwrap();
        assert_eq!(result.written, 1, "errors: {:?}", result.errors);
        assert_eq!(result.trashed_originals, 0);
        assert!(crate::trash::Trash::list(&root).unwrap().is_empty());
    }

    #[test]
    fn embedded_writeback_skips_tiff_variants() {
        let (_dir, root, mut asset, _jpeg) = setup_catalog_with_jpeg();

        // Add a TIFF variant with a location.
        let tiff_rel = "photos/IMG_1.tif";
        let tiff_path = root.join(tiff_rel);
        std::fs::write(&tiff_path, b"II*\0fake-tiff").unwrap();
        let content_store = ContentStore::new(&root);
        let tiff_hash = content_store.hash_file(&tiff_path).unwrap();
        let registry = DeviceRegistry::new(&root);
        let volume_id = registry.list().unwrap()[0].id;
        let tiff_variant = Variant {
            content_hash: tiff_hash.clone(),
            asset_id: asset.id,
            role: VariantRole::Processed,
            format: "tif".to_string(),
            file_size: 12,
            original_filename: "IMG_1.tif".to_string(),
            source_metadata: Default::default(),
            locations: vec![FileLocation {
                volume_id,
                relative_path: PathBuf::from(tiff_rel),
                verified_at: None,
            }],
        };
        asset.variants.push(tiff_variant.clone());
        let store = MetadataStore::new(&root);
        store.save(&asset).unwrap();
        let catalog = Catalog::open(&root).unwrap();
        catalog.insert_variant(&tiff_variant).unwrap();
        catalog
            .insert_file_location(&tiff_hash, &tiff_variant.locations[0])
            .unwrap();
        catalog.update_denormalized_variant_columns(&asset).unwrap();

        let result = run_embed(&root, &asset.id.to_string(), false);
        // JPEG written, TIFF skipped, nothing failed.
        assert_eq!(result.written, 1, "errors: {:?}", result.errors);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.failed, 0);
        // TIFF bytes untouched.
        assert_eq!(std::fs::read(&tiff_path).unwrap(), b"II*\0fake-tiff");
    }

    #[test]
    fn embedded_writeback_inserts_fresh_packet_when_jpeg_has_no_xmp() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();

        let catalog = Catalog::open(&root).unwrap();
        catalog.initialize().unwrap();
        DeviceRegistry::init(&root).unwrap();
        let registry = DeviceRegistry::new(&root);
        let volume = registry
            .register("TESTVOL", &root, crate::models::VolumeType::External, None)
            .unwrap();
        catalog.ensure_volume(&volume).unwrap();

        // JPEG with an Exif APP1 but no XMP segment.
        let jpeg_rel = "noxmp.jpg";
        let jpeg_path = root.join(jpeg_rel);
        let mut data = Vec::new();
        data.extend_from_slice(&[0xFF, 0xD8]);
        data.extend_from_slice(&[0xFF, 0xE1, 0x00, 0x08]);
        data.extend_from_slice(b"Exif\0\0");
        data.extend_from_slice(&[0xFF, 0xD9]);
        std::fs::write(&jpeg_path, &data).unwrap();

        let content_store = ContentStore::new(&root);
        let hash = content_store.hash_file(&jpeg_path).unwrap();
        let mut asset = Asset::new(AssetType::Image, &hash);
        asset.rating = Some(3);
        asset.color_label = Some("Red".to_string());
        let variant = Variant {
            content_hash: hash.clone(),
            asset_id: asset.id,
            role: VariantRole::Original,
            format: "jpg".to_string(),
            file_size: data.len() as u64,
            original_filename: "noxmp.jpg".to_string(),
            source_metadata: Default::default(),
            locations: vec![FileLocation {
                volume_id: volume.id,
                relative_path: PathBuf::from(jpeg_rel),
                verified_at: None,
            }],
        };
        asset.variants.push(variant.clone());
        let store = MetadataStore::new(&root);
        store.save(&asset).unwrap();
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&variant).unwrap();
        catalog.insert_file_location(&hash, &variant.locations[0]).unwrap();
        catalog.update_denormalized_variant_columns(&asset).unwrap();

        let result = run_embed(&root, &asset.id.to_string(), false);
        assert_eq!(result.written, 1, "errors: {:?}", result.errors);

        let xmp = crate::embedded_xmp::extract_embedded_xmp(&jpeg_path);
        assert_eq!(xmp.source_metadata.get("rating").map(String::as_str), Some("3"));
        assert_eq!(xmp.source_metadata.get("label").map(String::as_str), Some("Red"));
        // Exif segment survived the insertion.
        let bytes = std::fs::read(&jpeg_path).unwrap();
        assert!(bytes.windows(6).any(|w| w == b"Exif\0\0"));

        // Doctor stays clean after this migration too.
        let report = crate::doctor::run_doctor(&root, None, false, |_, _| {}).unwrap();
        assert!(report.healthy(), "{:?}", report.mismatched);
    }
}
