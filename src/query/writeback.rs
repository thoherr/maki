//! XMP write-back — flushing catalog metadata (rating, label, description,
//! tags) to `.xmp` recipe files on disk.
//!
//! Extracted from query.rs (was section 16 of its table of contents).
//! All methods are `impl QueryEngine`.
//!
//! The four field-specific write-back loops (rating, tags, description,
//! label) previously existed as four near-identical ~90-line copies; they
//! now share one generic engine (`write_back_field_to_xmp_inner`) so the
//! pending-flag bookkeeping, offline-volume handling, and re-hash logic
//! exist exactly once. Each field contributes only its `xmp_reader`
//! update call as a closure. The v4.5.14–v4.5.17 escape-handling fixes
//! had to be replicated across the old copies by hand — that hazard is
//! what this consolidation removes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::catalog::Catalog;
use crate::content_store::ContentStore;
use crate::device_registry::DeviceRegistry;
use crate::metadata_store::MetadataStore;
use crate::models::recipe::Recipe;
use crate::models::Asset;
use crate::xmp_reader;

use super::{QueryEngine, WritebackResult};

impl QueryEngine {
    /// Mark a recipe as pending write-back in SQLite and set the flag on the struct.
    /// The caller must save the sidecar YAML.
    fn mark_recipe_pending(recipe: &mut Recipe, catalog: &Catalog) {
        if !recipe.pending_writeback {
            recipe.pending_writeback = true;
            let _ = catalog.mark_pending_writeback(&recipe.id.to_string());
        }
    }

    /// Clear pending write-back flag after successful XMP write.
    /// The caller must save the sidecar YAML.
    fn clear_recipe_pending(recipe: &mut Recipe, catalog: &Catalog) {
        if recipe.pending_writeback {
            recipe.pending_writeback = false;
            let _ = catalog.clear_pending_writeback(&recipe.id.to_string());
        }
    }

    /// Shared engine for the field-specific write-back loops.
    ///
    /// Walks every `.xmp` recipe of `asset` and applies `apply` to the
    /// file on disk. The bookkeeping is identical for every field:
    ///
    /// - auto-writeback disabled → mark recipe pending, skip the write
    ///   (the change record survives; `maki writeback` flushes later)
    /// - volume offline or file missing → mark pending
    /// - `apply` returned `Ok(true)` (file changed) → re-hash, update the
    ///   recipe's content hash in catalog and in-memory, clear pending
    /// - `Ok(false)` (file already in sync) → clear pending if set
    /// - `Err` → warn naming `field_label`; pending state untouched
    ///
    /// Saves the sidecar once at the end if any recipe state changed.
    fn write_back_field_to_xmp_inner(
        &self,
        asset: &mut Asset,
        catalog: &Catalog,
        store: &MetadataStore,
        online: &HashMap<uuid::Uuid, PathBuf>,
        content_store: &ContentStore,
        field_label: &str,
        apply: impl Fn(&Path) -> Result<bool>,
    ) {
        let writeback_active = self.is_writeback_enabled();
        let mut sidecar_dirty = false;

        for recipe in &mut asset.recipes {
            let ext = recipe
                .location
                .relative_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if ext != "xmp" {
                continue;
            }

            if !writeback_active {
                // Auto-flush off: track the change for later, skip the file
                // write. `maki writeback` reads pending markers and flushes.
                Self::mark_recipe_pending(recipe, catalog);
                sidecar_dirty = true;
                continue;
            }

            let mount_point = match online.get(&recipe.location.volume_id) {
                Some(mp) => mp.as_path(),
                None => {
                    Self::mark_recipe_pending(recipe, catalog);
                    sidecar_dirty = true;
                    continue;
                }
            };

            let full_path = mount_point.join(&recipe.location.relative_path);
            if !full_path.exists() {
                Self::mark_recipe_pending(recipe, catalog);
                sidecar_dirty = true;
                continue;
            }

            match apply(&full_path) {
                Ok(true) => match content_store.hash_file(&full_path) {
                    Ok(new_hash) => {
                        if let Err(e) = catalog
                            .update_recipe_content_hash(&recipe.id.to_string(), &new_hash)
                        {
                            eprintln!("Warning: could not update recipe hash in catalog: {e}");
                        }
                        recipe.content_hash = new_hash;
                        if recipe.pending_writeback {
                            Self::clear_recipe_pending(recipe, catalog);
                        }
                        sidecar_dirty = true;
                    }
                    Err(e) => {
                        eprintln!("Warning: could not re-hash XMP file: {e}");
                    }
                },
                Ok(false) => {
                    if recipe.pending_writeback {
                        Self::clear_recipe_pending(recipe, catalog);
                        sidecar_dirty = true;
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Warning: could not write {field_label} to {}: {e}",
                        full_path.display()
                    );
                }
            }
        }

        if sidecar_dirty {
            if let Err(e) = store.save(asset) {
                eprintln!(
                    "Warning: could not save sidecar after XMP {field_label} write-back: {e}"
                );
            }
        }
    }

    /// Registry prologue shared by the auto-writeback entry points that
    /// only have catalog + store at hand (the `edit` path): load online
    /// volumes, build the content store, delegate to the shared loop.
    ///
    /// Note: no `if !is_writeback_enabled() return` short-circuit here.
    /// Tracking is always on; the inner loop skips the actual XMP file
    /// write when disabled and marks the recipe pending for later flush
    /// via `maki writeback`. This lets users keep auto-write off as a
    /// safety net without losing the change record.
    fn write_back_field_to_xmp_auto(
        &self,
        asset: &mut Asset,
        catalog: &Catalog,
        store: &MetadataStore,
        field_label: &str,
        apply: impl Fn(&Path) -> Result<bool>,
    ) {
        let registry = DeviceRegistry::new(&self.catalog_root);
        let volumes = match registry.list() {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "Warning: could not load volumes for XMP {field_label} write-back: {e}"
                );
                return;
            }
        };
        let online: HashMap<uuid::Uuid, PathBuf> = volumes
            .iter()
            .filter(|v| v.is_online)
            .map(|v| (v.id, v.mount_point.clone()))
            .collect();
        let content_store = ContentStore::new(&self.catalog_root);
        self.write_back_field_to_xmp_inner(
            asset,
            catalog,
            store,
            &online,
            &content_store,
            field_label,
            apply,
        );
    }

    /// Write back a rating change to `.xmp` recipe files on disk
    /// (`xmp:Rating`). Silently skips offline volumes and missing files.
    pub(super) fn write_back_rating_to_xmp(
        &self,
        asset: &mut Asset,
        rating: Option<u8>,
        catalog: &Catalog,
        store: &MetadataStore,
    ) {
        self.write_back_field_to_xmp_auto(asset, catalog, store, "rating", |p| {
            xmp_reader::update_rating(p, rating)
        });
    }

    /// Like [`Self::write_back_rating_to_xmp`] but with the online-volume
    /// map and content store supplied by the caller (batch context).
    pub(super) fn write_back_rating_to_xmp_inner(
        &self,
        asset: &mut Asset,
        rating: Option<u8>,
        catalog: &Catalog,
        store: &MetadataStore,
        online: &HashMap<uuid::Uuid, PathBuf>,
        content_store: &ContentStore,
    ) {
        self.write_back_field_to_xmp_inner(asset, catalog, store, online, content_store, "rating", |p| {
            xmp_reader::update_rating(p, rating)
        });
    }

    /// Write back a description change to `.xmp` recipe files on disk
    /// (`dc:description`). Silently skips offline volumes and missing files.
    pub(super) fn write_back_description_to_xmp(
        &self,
        asset: &mut Asset,
        description: Option<&str>,
        catalog: &Catalog,
        store: &MetadataStore,
    ) {
        self.write_back_field_to_xmp_auto(asset, catalog, store, "description", |p| {
            xmp_reader::update_description(p, description)
        });
    }

    /// Write back a color label change to `.xmp` recipe files on disk
    /// (`xmp:Label`). Silently skips offline volumes and missing files.
    pub(super) fn write_back_label_to_xmp(
        &self,
        asset: &mut Asset,
        label: Option<&str>,
        catalog: &Catalog,
        store: &MetadataStore,
    ) {
        self.write_back_field_to_xmp_auto(asset, catalog, store, "label", |p| {
            xmp_reader::update_label(p, label)
        });
    }

    /// Like [`Self::write_back_label_to_xmp`] but with the online-volume
    /// map and content store supplied by the caller (batch context).
    pub(super) fn write_back_label_to_xmp_inner(
        &self,
        asset: &mut Asset,
        label: Option<&str>,
        catalog: &Catalog,
        store: &MetadataStore,
        online: &HashMap<uuid::Uuid, PathBuf>,
        content_store: &ContentStore,
    ) {
        self.write_back_field_to_xmp_inner(asset, catalog, store, online, content_store, "label", |p| {
            xmp_reader::update_label(p, label)
        });
    }

    /// Write back tag add/remove operations to `.xmp` recipe files on disk.
    ///
    /// Applies the same delta (add/remove) to both keyword conventions:
    /// `dc:subject` gets flat individual component names (CaptureOne
    /// convention — for "person|artist|musician", the entries "person",
    /// "artist", "musician"), `lr:hierarchicalSubject` gets all ancestor
    /// paths ("person", "person|artist", "person|artist|musician").
    /// Silently skips offline volumes and missing files.
    pub(super) fn write_back_tags_to_xmp_inner(
        &self,
        asset: &mut Asset,
        tags_to_add: &[String],
        tags_to_remove: &[String],
        catalog: &Catalog,
        store: &MetadataStore,
        online: &HashMap<uuid::Uuid, PathBuf>,
        content_store: &ContentStore,
    ) {
        let dc_add: Vec<String> = tags_to_add
            .iter()
            .flat_map(|t| t.split('|').map(|s| s.to_string()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let dc_remove: Vec<String> = tags_to_remove
            .iter()
            .flat_map(|t| t.split('|').map(|s| s.to_string()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let lr_add = crate::tag_util::expand_all_ancestors(tags_to_add);
        let lr_remove = crate::tag_util::expand_all_ancestors(tags_to_remove);

        // Tag write errors are reported per sub-field and treated as "no
        // change" (matching the historical behavior) rather than aborting
        // the recipe — a dc:subject failure must not block the
        // lr:hierarchicalSubject update.
        self.write_back_field_to_xmp_inner(
            asset,
            catalog,
            store,
            online,
            content_store,
            "tags",
            |full_path| {
                let changed_dc = match xmp_reader::update_tags(full_path, &dc_add, &dc_remove) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!(
                            "Warning: could not write tags to {}: {e}",
                            full_path.display()
                        );
                        false
                    }
                };
                let changed_lr = match xmp_reader::update_hierarchical_subjects(
                    full_path, &lr_add, &lr_remove,
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!(
                            "Warning: could not write hierarchical subjects to {}: {e}",
                            full_path.display()
                        );
                        false
                    }
                };
                Ok(changed_dc || changed_lr)
            },
        );
    }

    /// Write back pending metadata changes to XMP recipe files.
    ///
    /// For each recipe with `pending_writeback=1`, reads the current asset metadata
    /// (rating, label, tags, description) and writes all four fields to the XMP file.
    /// Clears the pending flag on success.
    ///
    /// `all=true` writes back all XMP recipes regardless of pending flag.
    #[allow(clippy::too_many_arguments)]
    pub fn writeback(
        &self,
        volume_filter: Option<&str>,
        asset_filter: Option<&str>,
        asset_id_set: Option<&HashSet<String>>,
        all: bool,
        force: bool,
        mirror_tags: bool,
        dry_run: bool,
        log: bool,
        callback: Option<&dyn Fn(&str, &str)>,
    ) -> Result<WritebackResult> {
        // `maki writeback` is the explicit manual flush. It runs whether or
        // not `[writeback] enabled` is set — the config flag governs only
        // *automatic* writeback on every edit. Users who keep auto-flush
        // off as a safety net still want this command to work; that's the
        // whole point of the split.
        //
        // `force` extends the scope to recipes whose pending flag is
        // false. `all` extends the scope to every recipe in the catalog
        // (and implies pending-filter off). The two compose: `force`
        // alone keeps any caller-supplied scope (asset/query/volume) and
        // re-writes everything in it; `all` ignores scope entirely.
        let catalog = Catalog::open(&self.catalog_root)?;
        let store = MetadataStore::new(&self.catalog_root);
        let registry = DeviceRegistry::new(&self.catalog_root);
        let volumes = registry.list()?;
        let online: HashMap<uuid::Uuid, PathBuf> = volumes
            .iter()
            .filter(|v| v.is_online)
            .map(|v| (v.id, v.mount_point.clone()))
            .collect();
        // Map of UUID → label for ALL volumes (including offline ones)
        // so the writeback summary can name offline drives the user
        // needs to reconnect.
        let volume_labels: HashMap<uuid::Uuid, String> = volumes
            .iter()
            .map(|v| (v.id, v.label.clone()))
            .collect();
        let content_store = ContentStore::new(&self.catalog_root);

        // Resolve volume filter to volume ID
        let volume_id_filter: Option<String> = if let Some(label) = volume_filter {
            let vol = volumes.iter().find(|v| v.label == label)
                .ok_or_else(|| anyhow::anyhow!("unknown volume: {label}"))?;
            Some(vol.id.to_string())
        } else {
            None
        };

        // Collect recipes to process. Two modes:
        //   `all` / `force` — every xmp recipe in the catalog, optionally
        //               volume-filtered at SQL level. For `all`, the
        //               asset/query scope (if any) is applied later by
        //               `writeback_process`; for `force`, the caller's
        //               scope is likewise re-applied there against the
        //               in-memory `asset_id_set` (the membership test is
        //               O(1) per row, so we don't push it into SQL).
        //   default   — only recipes with `pending_writeback = 1`.
        let pending_recipes: Vec<(String, String, String, String)> = if all || force {
            let sql = if volume_id_filter.is_some() {
                "SELECT r.id, v.asset_id, r.volume_id, r.relative_path \
                 FROM recipes r \
                 JOIN variants v ON r.variant_hash = v.content_hash \
                 WHERE r.volume_id = ?1 AND LOWER(r.relative_path) LIKE '%.xmp'"
            } else {
                "SELECT r.id, v.asset_id, r.volume_id, r.relative_path \
                 FROM recipes r \
                 JOIN variants v ON r.variant_hash = v.content_hash \
                 WHERE LOWER(r.relative_path) LIKE '%.xmp'"
            };
            let mut stmt = catalog.conn().prepare(sql)?;
            let params: Vec<Box<dyn rusqlite::types::ToSql>> = if let Some(ref vid) = volume_id_filter {
                vec![Box::new(vid.clone())]
            } else {
                vec![]
            };
            let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            let mut result = Vec::new();
            for row in rows { result.push(row?); }
            result
        } else {
            catalog.list_pending_writeback_recipes(volume_id_filter.as_deref())?
        };

        self.writeback_process(pending_recipes, &catalog, &store, &online, &volume_labels, &content_store, asset_filter, asset_id_set, mirror_tags, force, dry_run, log, callback)
    }

    /// Process a list of recipes for writeback. Each tuple is (recipe_id, asset_id, volume_id, relative_path).
    ///
    /// `volume_labels` maps volume UUID → user-facing label across ALL
    /// volumes (online + offline). Used to surface offline-volume names
    /// in the result summary so users know which drives to reconnect
    /// before re-running. Pass an empty map to fall back to UUIDs.
    #[allow(clippy::too_many_arguments)]
    pub fn writeback_process(
        &self,
        recipes: Vec<(String, String, String, String)>,
        catalog: &Catalog,
        store: &MetadataStore,
        online: &HashMap<uuid::Uuid, PathBuf>,
        volume_labels: &HashMap<uuid::Uuid, String>,
        content_store: &ContentStore,
        asset_filter: Option<&str>,
        asset_id_set: Option<&HashSet<String>>,
        mirror_tags: bool,
        force: bool,
        dry_run: bool,
        log: bool,
        callback: Option<&dyn Fn(&str, &str)>,
    ) -> Result<WritebackResult> {
        // No `is_writeback_enabled()` gate: explicit manual flush always
        // runs (see comment on `writeback`).
        let mut result = WritebackResult::default();
        result.dry_run = dry_run;

        // Group by asset_id
        let mut by_asset: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
        for (recipe_id, asset_id, volume_id, rel_path) in recipes {
            if let Some(prefix) = asset_filter {
                if !asset_id.starts_with(prefix) {
                    continue;
                }
            }
            if let Some(set) = asset_id_set {
                if !set.contains(&asset_id) {
                    continue;
                }
            }
            by_asset.entry(asset_id).or_default().push((recipe_id, volume_id, rel_path));
        }

        for (asset_id, recipe_entries) in &by_asset {
            let asset_uuid: uuid::Uuid = match asset_id.parse() {
                Ok(u) => u,
                Err(_) => {
                    result.errors.push(format!("Invalid asset ID: {asset_id}"));
                    result.failed += recipe_entries.len() as u32;
                    continue;
                }
            };
            let mut asset = match store.load(asset_uuid) {
                Ok(a) => a,
                Err(e) => {
                    result.errors.push(format!("Could not load asset {asset_id}: {e}"));
                    result.failed += recipe_entries.len() as u32;
                    continue;
                }
            };

            // Track only the recipes whose write actually succeeded (or
            // had nothing to do because the XMP was already in sync). The
            // sidecar pending-flag clear below mirrors this set so a
            // recipe that skipped — offline volume or missing file —
            // keeps its `pending_writeback = true` and gets picked up by
            // the next `maki writeback` once the volume comes back online.
            let mut cleared_recipe_ids: HashSet<String> = HashSet::new();

            for (recipe_id, volume_id, rel_path) in recipe_entries {
                let vol_uuid: uuid::Uuid = match volume_id.parse() {
                    Ok(u) => u,
                    Err(_) => {
                        result.errors.push(format!("Invalid volume ID: {volume_id}"));
                        result.failed += 1;
                        continue;
                    }
                };

                let mount_point = match online.get(&vol_uuid) {
                    Some(mp) => mp.as_path(),
                    None => {
                        result.skipped += 1;
                        let label = volume_labels
                            .get(&vol_uuid)
                            .cloned()
                            .unwrap_or_else(|| vol_uuid.to_string());
                        if log {
                            eprintln!("{rel_path} — skipped (volume offline: {label})");
                        }
                        result.skipped_offline_volumes.insert(label);
                        continue;
                    }
                };

                let full_path = mount_point.join(rel_path);
                if !full_path.exists() {
                    result.skipped += 1;
                    if log {
                        eprintln!("{rel_path} — skipped (file missing)");
                    }
                    continue;
                }

                if dry_run {
                    result.written += 1;
                    if log {
                        eprintln!("{rel_path} — would write back");
                    }
                    if let Some(cb) = callback {
                        cb(rel_path, "would write back");
                    }
                    continue;
                }

                // Write all four metadata fields
                let mut file_changed = false;

                if let Ok(true) = xmp_reader::update_rating(&full_path, asset.rating) {
                    file_changed = true;
                }
                if let Ok(true) = xmp_reader::update_label(
                    &full_path,
                    asset.color_label.as_deref(),
                ) {
                    file_changed = true;
                }
                if let Ok(true) = xmp_reader::update_description(
                    &full_path,
                    asset.description.as_deref(),
                ) {
                    file_changed = true;
                }
                // Tags: write flat components to dc:subject, ancestor paths to lr:hierarchicalSubject
                let dc_tags: Vec<String> = asset.tags.iter()
                    .flat_map(|t| t.split('|').map(|s| s.to_string()))
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                let lr_tags = crate::tag_util::expand_all_ancestors(&asset.tags);

                // Three remove-set modes:
                //
                //   `force`        — take EVERY existing XMP entry into the
                //                    remove list. Combined with the catalog
                //                    tags going into `tags_to_add`, this
                //                    rebuilds the dc:subject and
                //                    lr:hierarchicalSubject blocks from
                //                    scratch — useful when stale entries
                //                    (e.g. multi-layer escape leftovers)
                //                    are stuck in a file that mirror-tags
                //                    somehow misses, or just as an escape
                //                    hatch when the user wants a guaranteed
                //                    clean rewrite.
                //   `mirror_tags`  — remove any XMP entry not in the
                //                    catalog's current set. Reconciles
                //                    renames, splits, deletes, unicode-
                //                    fixes that accumulated while inline
                //                    writeback was off.
                //   neither        — additive only: keep all existing
                //                    entries; add catalog tags missing
                //                    from the file.
                let (dc_remove, lr_remove): (Vec<String>, Vec<String>) = if force || mirror_tags {
                    let xmp = xmp_reader::extract(&full_path);
                    if force {
                        (xmp.keywords.clone(), xmp.hierarchical_keywords.clone())
                    } else {
                        let dc_set: std::collections::HashSet<&str> =
                            dc_tags.iter().map(|s| s.as_str()).collect();
                        let lr_set: std::collections::HashSet<&str> =
                            lr_tags.iter().map(|s| s.as_str()).collect();
                        let dc_rm: Vec<String> = xmp.keywords.iter()
                            .filter(|t| !dc_set.contains(t.as_str()))
                            .cloned()
                            .collect();
                        let lr_rm: Vec<String> = xmp.hierarchical_keywords.iter()
                            .filter(|t| !lr_set.contains(t.as_str()))
                            .cloned()
                            .collect();
                        (dc_rm, lr_rm)
                    }
                } else {
                    (Vec::new(), Vec::new())
                };

                if !dc_tags.is_empty() || !dc_remove.is_empty() {
                    if let Ok(true) = xmp_reader::update_tags(&full_path, &dc_tags, &dc_remove) {
                        file_changed = true;
                    }
                    if let Ok(true) = xmp_reader::update_hierarchical_subjects(&full_path, &lr_tags, &lr_remove) {
                        file_changed = true;
                    }
                }

                if file_changed {
                    match content_store.hash_file(&full_path) {
                        Ok(new_hash) => {
                            let _ = catalog.update_recipe_content_hash(recipe_id, &new_hash);
                            // Update the in-memory recipe too
                            if let Some(r) = asset.recipes.iter_mut().find(|r| r.id.to_string() == *recipe_id) {
                                r.content_hash = new_hash;
                                r.pending_writeback = false;
                            }
                        }
                        Err(e) => {
                            eprintln!("Warning: could not re-hash {}: {e}", full_path.display());
                        }
                    }
                    result.written += 1;
                    if log {
                        eprintln!("{rel_path} — written");
                    }
                    if let Some(cb) = callback {
                        cb(rel_path, "written");
                    }
                } else {
                    // file_changed=false reached this point only because every
                    // xmp_reader::update_* returned Ok(false) — the XMP on
                    // disk already carries the values we would have written.
                    // This is the "external sync got here first" path: a
                    // common case after the user rsyncs ARCHIVE → BACKUP
                    // outside of MAKI between curating and running
                    // `maki writeback`. The pending flag clears below (no
                    // action needed here), but the catalog's stored
                    // content_hash for this recipe may be stale because
                    // rsync replaced the file on disk while we still
                    // remembered the pre-rsync hash. Re-hash and reconcile
                    // so a subsequent `maki verify` doesn't flag every
                    // synced recipe as a mismatch.
                    match content_store.hash_file(&full_path) {
                        Ok(disk_hash) => {
                            let stored_hash = asset
                                .recipes
                                .iter()
                                .find(|r| r.id.to_string() == *recipe_id)
                                .map(|r| r.content_hash.clone());
                            if stored_hash.as_deref() != Some(disk_hash.as_str()) {
                                let _ = catalog
                                    .update_recipe_content_hash(recipe_id, &disk_hash);
                                if let Some(r) = asset.recipes.iter_mut().find(|r| r.id.to_string() == *recipe_id) {
                                    r.content_hash = disk_hash;
                                }
                            }
                        }
                        Err(e) => {
                            // Hash failure here doesn't block the writeback — we
                            // still clear pending below. Log so operators can see.
                            eprintln!("Warning: could not re-hash {}: {e}", full_path.display());
                        }
                    }
                    result.already_in_sync += 1;
                    if log {
                        eprintln!("{rel_path} — already in sync (no write)");
                    }
                    if let Some(cb) = callback {
                        cb(rel_path, "already in sync");
                    }
                }

                let _ = catalog.clear_pending_writeback(recipe_id);
                cleared_recipe_ids.insert(recipe_id.clone());
            }

            // Save sidecar for recipes that actually completed. A
            // multi-variant asset with one offline variant must keep its
            // overall pending state so the offline recipe still gets
            // flushed later — `cleared_recipe_ids` is exactly that set
            // (offline/missing recipes hit the `continue` paths above
            // and never landed in it).
            //
            // We must NOT gate on `r.pending_writeback` here. The
            // file_changed branch above already pre-cleared the
            // in-memory flag (so the sidecar save would carry the new
            // state); a `&& r.pending_writeback` guard at this point
            // sees the already-cleared value, decides "nothing to do",
            // and skips `store.save()` — leaving the YAML stuck on
            // `pending_writeback: true` while SQLite + XMP are in sync.
            // Source of truth (YAML) diverges silently, and a later
            // `rebuild-catalog` would reintroduce a phantom pending
            // flag on a recipe whose disk file is already current.
            if !dry_run && !cleared_recipe_ids.is_empty() {
                let mut any_changed = false;
                for r in &mut asset.recipes {
                    if cleared_recipe_ids.contains(&r.id.to_string()) {
                        if r.pending_writeback {
                            r.pending_writeback = false;
                        }
                        any_changed = true;
                    }
                }
                if any_changed {
                    if let Err(e) = store.save(&asset) {
                        eprintln!("Warning: could not save sidecar for {asset_id}: {e}");
                    }
                }
            }
        }

        Ok(result)
    }
}
