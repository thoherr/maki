//! `dedup` section of `AssetService` — methods extracted from the original
//! 8.9-kLOC asset_service.rs into a multi-file `impl AssetService` block.
//!
//! Free functions and result types live in the parent `asset_service` module.

use super::*;

/// Options of one same-volume dedup run.
#[derive(Debug, Clone, Default)]
pub struct DedupOptions {
    /// Only this volume (label).
    pub volume: Option<String>,
    /// Only variants of this format (extension).
    pub format: Option<String>,
    /// Only locations under this volume-relative path prefix.
    pub path_prefix: Option<String>,
    /// Keep the location whose path contains this substring.
    pub prefer: Option<String>,
    /// Never remove a location when fewer than this many would remain
    /// across all volumes.
    pub min_copies: usize,
    /// Remove files and records; otherwise report only.
    pub apply: bool,
    /// Delete removed files permanently instead of moving them to the trash.
    pub no_trash: bool,
}

impl AssetService {
    // ═══ DEDUP ═══

    /// Remove same-volume duplicate file locations.
    ///
    /// For each variant with 2+ locations on the same volume, keeps the "best"
    /// location and removes the rest. In apply mode, deletes physical files and
    /// removes catalog/sidecar location records.
    /// `on_entry` receives `(original filename, relative path, status,
    /// volume label)` for every location of every duplicate group.
    pub fn dedup_with(
        &self,
        options: &DedupOptions,
        on_entry: impl Fn(&str, &str, DedupStatus, &str),
    ) -> Result<DedupResult> {
        let registry = DeviceRegistry::new(&self.catalog_root);
        let catalog = Catalog::open(&self.catalog_root)?;

        let filter_volume_id = if let Some(label) = &options.volume {
            let vol = registry.resolve_volume(label)?;
            Some(vol.id.to_string())
        } else {
            None
        };

        let entries = if options.format.is_some() || options.path_prefix.is_some() || filter_volume_id.is_some() {
            catalog.find_duplicates_filtered(
                "same",
                filter_volume_id.as_deref(),
                options.format.as_deref(),
                options.path_prefix.as_deref(),
            )?
        } else {
            catalog.find_duplicates_same_volume()?
        };

        let mut run = DedupRun {
            service: self,
            catalog,
            metadata_store: MetadataStore::new(&self.catalog_root),
            trash: crate::trash::Trash::new_with(&self.catalog_root, &self.config.trash, options.no_trash),
            volumes: registry.list()?,
            options,
            result: DedupResult {
                duplicates_found: 0,
                locations_to_remove: 0,
                locations_removed: 0,
                files_deleted: 0,
                files_trashed: 0,
                recipes_removed: 0,
                bytes_freed: 0,
                dry_run: !options.apply,
                errors: Vec::new(),
            },
            on_entry,
        };

        for entry in &entries {
            run.process_entry(entry, filter_volume_id.as_deref());
        }

        Ok(run.result)
    }
}

/// Order duplicate locations best-first (the first one is kept):
/// `--prefer` matches, then most recently verified, then shortest path,
/// then alphabetical.
fn compare_locations(
    a: &crate::catalog::LocationDetails,
    b: &crate::catalog::LocationDetails,
    prefer: Option<&str>,
) -> std::cmp::Ordering {
    // 1. Prefer locations matching --prefer substring
    if let Some(prefix) = prefer {
        let a_match = a.relative_path.contains(prefix);
        let b_match = b.relative_path.contains(prefix);
        if a_match != b_match {
            return if a_match {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
    }

    // 2. Prefer more recently verified (NULL = oldest)
    let a_ver = a.verified_at.as_deref().unwrap_or("");
    let b_ver = b.verified_at.as_deref().unwrap_or("");
    match b_ver.cmp(a_ver) {
        std::cmp::Ordering::Equal => {}
        other => return other,
    }

    // 3. Prefer shorter relative paths
    match a.relative_path.len().cmp(&b.relative_path.len()) {
        std::cmp::Ordering::Equal => {}
        other => return other,
    }

    // 4. Tiebreak: alphabetical
    a.relative_path.cmp(&b.relative_path)
}

/// One dedup run in progress.
struct DedupRun<'a, F> {
    service: &'a AssetService,
    catalog: Catalog,
    metadata_store: MetadataStore,
    trash: crate::trash::Trash,
    volumes: Vec<Volume>,
    options: &'a DedupOptions,
    result: DedupResult,
    on_entry: F,
}

impl<F: Fn(&str, &str, DedupStatus, &str)> DedupRun<'_, F> {
    /// One duplicate variant: per volume with 2+ locations, keep the best
    /// and remove (or report) the rest, honoring `min_copies`.
    fn process_entry(&mut self, entry: &crate::catalog::DuplicateEntry, filter_volume_id: Option<&str>) {
        // Group locations by volume_id
        let mut by_volume: BTreeMap<String, Vec<&crate::catalog::LocationDetails>> =
            BTreeMap::new();
        for loc in &entry.locations {
            by_volume
                .entry(loc.volume_id.clone())
                .or_default()
                .push(loc);
        }

        // Track how many locations we're removing for this variant (for min-copies)
        let mut entry_removals = 0usize;

        for (vol_id, mut locs) in by_volume {
            if locs.len() < 2 {
                continue;
            }

            // If volume filter set, skip other volumes
            if let Some(fid) = filter_volume_id {
                if vol_id != fid {
                    continue;
                }
            }

            self.result.duplicates_found += 1;

            // Sort by resolution heuristic (best first = keep)
            locs.sort_by(|a, b| compare_locations(a, b, self.options.prefer.as_deref()));

            let vol_label = locs
                .first()
                .map(|l| l.volume_label.as_str())
                .unwrap_or("?");

            // Keep the first, mark the rest for removal
            (self.on_entry)(
                &entry.original_filename,
                &locs[0].relative_path,
                DedupStatus::Keep,
                vol_label,
            );

            for loc in &locs[1..] {
                // Check min-copies constraint: total locations across all volumes
                let remaining = entry.locations.len() - entry_removals;
                if remaining <= self.options.min_copies {
                    (self.on_entry)(
                        &entry.original_filename,
                        &loc.relative_path,
                        DedupStatus::Skipped,
                        vol_label,
                    );
                    continue;
                }

                entry_removals += 1;
                self.result.locations_to_remove += 1;
                self.result.bytes_freed += entry.file_size;

                (self.on_entry)(
                    &entry.original_filename,
                    &loc.relative_path,
                    DedupStatus::Remove,
                    vol_label,
                );

                let colocated_recipes = self.colocated_recipes(&entry.content_hash, &vol_id, &loc.relative_path);
                if self.options.apply {
                    self.remove_location(&entry.content_hash, &vol_id, loc, &colocated_recipes);
                } else {
                    // Dry-run: just count recipes
                    self.result.recipes_removed += colocated_recipes.len();
                }
            }
        }
    }

    /// Recipes of the variant on the same volume in the same directory as
    /// a location: `(recipe id, content hash, relative path)`.
    fn colocated_recipes(&self, content_hash: &str, vol_id: &str, relative_path: &str) -> Vec<(String, String, String)> {
        let loc_dir = std::path::Path::new(relative_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        self.catalog
            .list_recipes_for_variant_on_volume(content_hash, vol_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_id, _hash, rpath)| {
                let rdir = std::path::Path::new(rpath)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                rdir == loc_dir
            })
            .collect()
    }

    /// Apply mode: dispose of the file (trash unless disabled), drop the
    /// location from sidecar and catalog, then the same for its co-located
    /// recipes.
    fn remove_location(
        &mut self,
        content_hash: &str,
        vol_id: &str,
        loc: &crate::catalog::LocationDetails,
        colocated_recipes: &[(String, String, String)],
    ) {
        let volume_uuid: Uuid = vol_id.parse().unwrap_or_default();
        let online_vol = self.volumes.iter().find(|v| v.id.to_string() == vol_id && v.is_online);

        // Remove the physical file (into the trash unless disabled)
        if let Some(vol) = online_vol {
            let full_path = vol.mount_point.join(&loc.relative_path);
            match self.trash.dispose(&full_path, &vol.label, &loc.relative_path) {
                Ok(crate::trash::Disposition::Trashed(_)) => {
                    self.result.files_deleted += 1;
                    self.result.files_trashed += 1;
                }
                Ok(crate::trash::Disposition::Deleted) => {
                    self.result.files_deleted += 1;
                }
                Err(e) => {
                    self.result.errors.push(format!(
                        "Failed to delete {}: {e}",
                        full_path.display()
                    ));
                    return;
                }
            }
        }

        // Sidecar (source of truth) first, then the catalog row
        if let Err(e) = self.service.remove_sidecar_file_location(
            &self.metadata_store,
            &self.catalog,
            content_hash,
            volume_uuid,
            &loc.relative_path,
        ) {
            self.result.errors.push(format!(
                "Failed to update sidecar for {}: {e}",
                loc.relative_path
            ));
        } else if let Err(e) = self.catalog.delete_file_location(
            content_hash,
            vol_id,
            &loc.relative_path,
        ) {
            self.result.errors.push(format!(
                "Failed to remove catalog location {}: {e}",
                loc.relative_path
            ));
        } else {
            self.result.locations_removed += 1;
        }

        // Clean up co-located recipe files
        for (recipe_id, _recipe_hash, recipe_path) in colocated_recipes {
            if let Some(vol) = online_vol {
                let recipe_full = vol.mount_point.join(recipe_path);
                if recipe_full.exists() {
                    if let Ok(crate::trash::Disposition::Trashed(_)) =
                        self.trash.dispose(&recipe_full, &vol.label, recipe_path)
                    {
                        self.result.files_trashed += 1;
                    }
                }
            }
            if let Err(e) = self.service.remove_sidecar_recipe(
                &self.metadata_store,
                &self.catalog,
                content_hash,
                volume_uuid,
                recipe_path,
            ) {
                self.result.errors.push(format!(
                    "Failed to update sidecar for recipe {recipe_path}: {e}"
                ));
            } else if let Err(e) = self.catalog.delete_recipe(recipe_id) {
                self.result.errors.push(format!(
                    "Failed to remove recipe {recipe_path}: {e}"
                ));
            } else {
                self.result.recipes_removed += 1;
            }
        }
    }
}
