//! Structural operations of `QueryEngine`: `group` / `group_by_asset_ids`
//! / `split` (variant ownership), `auto_group` (filename-stem grouping
//! within a session root) and `stack_from_tag`. These rebuild child rows
//! and keep their own transactional bodies.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::catalog::Catalog;
use crate::error::MakiError;
use crate::metadata_store::MetadataStore;

use super::{
    find_session_root, AutoGroupResult, FromTagDetail, FromTagResult, GroupResult, NewSplitAsset,
    QueryEngine, SplitResult, StemGroupEntry,
};

/// Check if `short` is a prefix-match for `long` with a separator boundary.
///
/// Returns true if `short == long` (exact match) or if `long` starts with `short`
/// and the character immediately following in `long` is non-alphanumeric.
/// This prevents `DSC_001` from matching `DSC_0010` while allowing
/// `DSC_001` to match `DSC_001-Edit` or `DSC_001_v2`.
pub(super) fn stem_prefix_matches(short: &str, long: &str) -> bool {
    if short == long {
        return true;
    }
    if !long.starts_with(short) {
        return false;
    }
    // The character right after the prefix must be a non-alphanumeric separator
    match long[short.len()..].chars().next() {
        Some(c) => !c.is_alphanumeric(),
        None => true,
    }
}

/// How [`QueryEngine::auto_group`] partitions its candidates before
/// comparing filename stems.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoGroupScope {
    /// Partition by session root (`[group] session_root_pattern`, else the
    /// parent directory) — the default for `maki auto-group`.
    SessionRoots,
    /// Like `SessionRoots`, with the import directories as the fallback
    /// boundary for paths without a dated component (`import --auto-group`).
    ImportRoots(Vec<String>),
    /// No partitioning: stems are compared across every directory.
    /// DANGEROUS — use only with a carefully scoped asset-id list.
    Global,
}

/// Progress of [`QueryEngine::stack_from_tag`], one per matched tag.
/// `Display` renders the CLI's stderr line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromTagEvent<'a> {
    /// Fewer than two unstacked assets carry the tag — no stack created.
    Skipped {
        tag: &'a str,
        unstacked: usize,
        already_stacked: u32,
        /// `Some(n)` when `--remove-tags` applied: the tag was removed
        /// from the `n` assets that carried it.
        tag_removed_from: Option<u32>,
    },
    /// A stack was created (or would be, when `applied` is false).
    Stacked { tag: &'a str, applied: bool, stacked: u32, skipped: u32 },
}

impl std::fmt::Display for FromTagEvent<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FromTagEvent::Skipped { tag, unstacked, already_stacked, tag_removed_from } => {
                write!(f, "{tag} — skipped ({unstacked} unstacked, {already_stacked} already stacked)")?;
                if let Some(n) = tag_removed_from.filter(|n| *n > 0) {
                    write!(f, " (removed tag from {n} asset(s))")?;
                }
                Ok(())
            }
            FromTagEvent::Stacked { tag, applied, stacked, skipped } => {
                let action = if *applied { "stacked" } else { "would stack" };
                write!(f, "{tag} — {action} {stacked} assets (skipped {skipped})")
            }
        }
    }
}

impl QueryEngine {
    // ═══ GROUP & SPLIT ═══

    /// Group variants (identified by content hashes) into a single asset.
    ///
    /// Picks the oldest asset as the target, moves all other variants into it,
    /// merges tags, and deletes donor assets.
    pub fn group(&self, variant_hashes: &[String]) -> Result<GroupResult> {
        if variant_hashes.is_empty() {
            return Err(MakiError::InvalidInput("no variant hashes provided".into()).into());
        }

        let catalog = self.catalog()?;
        let store = &self.meta_store;

        // Step 1: Look up owning asset for each hash
        let mut asset_ids = Vec::new();
        for hash in variant_hashes {
            let asset_id = catalog
                .find_asset_id_by_variant(hash)?
                .ok_or_else(|| MakiError::not_found("variant", hash.as_str()))?;
            asset_ids.push(asset_id);
        }

        // Step 2: Collect unique asset IDs
        let unique_ids: Vec<String> = {
            let mut seen = HashSet::new();
            asset_ids
                .iter()
                .filter(|id| seen.insert((*id).clone()))
                .cloned()
                .collect()
        };

        if unique_ids.len() == 1 {
            return Ok(GroupResult {
                target_id: unique_ids.into_iter().next().unwrap(),
                variants_moved: 0,
                donors_removed: 0,
            });
        }

        // Step 3: Load all assets from sidecar, pick oldest as target
        let mut assets: Vec<crate::models::Asset> = unique_ids
            .iter()
            .map(|id| {
                let uuid: uuid::Uuid = id.parse()?;
                store.load(uuid)
            })
            .collect::<Result<_>>()?;

        assets.sort_by_key(|a| a.created_at);
        let target = assets.remove(0);
        let donors = assets; // remaining are donors

        self.merge_assets(catalog, store, target, donors)
    }

    /// Shared tail of [`Self::group`] and [`Self::group_by_asset_ids`]:
    /// fold `donors` into `target` in memory (variants, tags with their
    /// provenance, recipes, best rating, first label / description), then
    /// persist in the documented order — sidecars first (save the target,
    /// delete the donors), then every SQLite row in ONE transaction.
    ///
    /// Sidecars first is what makes a failure recoverable: if the
    /// transaction fails, the sidecars already describe the merged state
    /// and `maki doctor --repair` converges SQLite to it (the target's rows
    /// are rebuilt from its sidecar, the donors' rows are phantoms and
    /// removed). With the reverse order a committed catalog would face
    /// still-existing donor sidecars, and doctor would move the variants
    /// back and forth between target and donors.
    pub(super) fn merge_assets(
        &self,
        catalog: &Catalog,
        store: &MetadataStore,
        mut target: crate::models::Asset,
        donors: Vec<crate::models::Asset>,
    ) -> Result<GroupResult> {
        let target_id = target.id;
        let mut all_tags: HashSet<String> = target.tags.iter().cloned().collect();

        // Check if the target asset has RAW variants (for smart role assignment)
        let target_has_raw = target.variants.iter().any(|v| {
            crate::models::format::is_raw_extension(&v.format)
        });

        // (content_hash, new role) of every variant that changes owner —
        // the catalog rows are updated from this list so SQLite carries
        // the same role as the sidecar.
        let mut moved: Vec<(String, crate::models::VariantRole)> = Vec::new();

        for donor in &donors {
            for variant in &donor.variants {
                let mut moved_variant = variant.clone();
                moved_variant.asset_id = target_id;
                // Smart role assignment: in RAW+non-RAW groups, non-RAW donors
                // become Export (processed output). Otherwise Alternate.
                if moved_variant.role == crate::models::VariantRole::Original {
                    if target_has_raw && !crate::models::format::is_raw_extension(&moved_variant.format) {
                        moved_variant.role = crate::models::VariantRole::Export;
                    } else {
                        moved_variant.role = crate::models::VariantRole::Alternate;
                    }
                }
                moved.push((moved_variant.content_hash.clone(), moved_variant.role.clone()));
                target.variants.push(moved_variant);
            }
            for tag in &donor.tags {
                if all_tags.insert(tag.clone()) {
                    // Carry the donor's provenance for tags new to the target.
                    target.add_tags_with_source(
                        std::slice::from_ref(tag),
                        donor.tag_source(tag),
                    );
                }
            }
            for recipe in &donor.recipes {
                target.recipes.push(recipe.clone());
            }
            // Keep the highest rating across target and donors
            if let Some(donor_rating) = donor.rating {
                target.rating = Some(target.rating.map_or(donor_rating, |r| r.max(donor_rating)));
            }
            // Keep first non-None color label
            if target.color_label.is_none() && donor.color_label.is_some() {
                target.color_label.clone_from(&donor.color_label);
            }
            // Keep first non-None description
            if target.description.is_none() && donor.description.is_some() {
                target.description.clone_from(&donor.description);
            }
        }

        // Sidecars (source of truth) first.
        store.save(&target)?;
        for donor in &donors {
            store.delete(donor.id)?;
        }

        // Then every catalog row, atomically. Reassigning the variants
        // before the donors' rows go means the donors' recipes and
        // locations (which hang off the variants) survive the merge.
        let target_id_str = target_id.to_string();
        catalog.with_tx(|c| {
            c.insert_asset(&target)?;
            for (hash, role) in &moved {
                c.update_variant_asset_id(hash, &target_id_str)?;
                c.update_variant_role(hash, role.as_str())?;
            }
            for donor in &donors {
                c.delete_asset_with_collections(&self.catalog_root, &donor.id.to_string())?;
            }
            Ok(())
        })?;

        Ok(GroupResult {
            target_id: target_id_str,
            variants_moved: moved.len(),
            donors_removed: donors.len(),
        })
    }

    /// Group assets by their IDs into a single asset.
    ///
    /// If `target_id` is provided, that asset becomes the merge target
    /// (must be one of the `asset_ids`). Otherwise the oldest asset wins.
    pub fn group_by_asset_ids(
        &self,
        asset_ids: &[String],
        target_id: Option<&str>,
    ) -> Result<GroupResult> {
        if asset_ids.len() < 2 {
            return Err(MakiError::InvalidInput("need at least 2 assets to group".into()).into());
        }

        if let Some(tid) = target_id {
            if !asset_ids.iter().any(|id| id == tid) {
                return Err(MakiError::InvalidInput(format!(
                    "target asset '{tid}' is not in the selected assets"
                ))
                .into());
            }
        }

        let catalog = self.catalog()?;
        let store = &self.meta_store;

        // Deduplicate
        let unique_ids: Vec<String> = {
            let mut seen = HashSet::new();
            asset_ids
                .iter()
                .filter(|id| seen.insert((*id).clone()))
                .cloned()
                .collect()
        };

        if unique_ids.len() < 2 {
            return Ok(GroupResult {
                target_id: unique_ids.into_iter().next().unwrap(),
                variants_moved: 0,
                donors_removed: 0,
            });
        }

        // Load all assets from sidecar
        let mut assets: Vec<crate::models::Asset> = unique_ids
            .iter()
            .map(|id| {
                let uuid: uuid::Uuid = id.parse()?;
                store.load(uuid)
            })
            .collect::<Result<_>>()?;

        // Pick target: explicit or oldest
        let target_idx = if let Some(tid) = target_id {
            assets
                .iter()
                .position(|a| a.id.to_string() == tid)
                .unwrap()
        } else {
            assets.sort_by_key(|a| a.created_at);
            0
        };

        let target = assets.remove(target_idx);
        let donors = assets;

        self.merge_assets(catalog, store, target, donors)
    }

    /// Split variants out of an asset into new standalone assets.
    ///
    /// Each extracted variant becomes a separate asset with role `Original`.
    /// Tags, rating, color_label, and description are inherited from the source.
    /// Recipes attached to extracted variants move with them.
    pub fn split(&self, asset_id: &str, variant_hashes: &[String]) -> Result<SplitResult> {
        if variant_hashes.is_empty() {
            return Err(MakiError::InvalidInput("no variant hashes provided".into()).into());
        }

        let catalog = self.catalog()?;
        let store = &self.meta_store;

        // Resolve asset ID (supports prefix matching)
        let full_id = catalog.resolve_asset_id_required(asset_id)?;
        let source_uuid: uuid::Uuid = full_id.parse()?;
        let mut source = store.load(source_uuid)?;

        // Validate: all hashes belong to this asset
        let source_hashes: HashSet<&str> =
            source.variants.iter().map(|v| v.content_hash.as_str()).collect();
        for hash in variant_hashes {
            if !source_hashes.contains(hash.as_str()) {
                return Err(MakiError::InvalidInput(format!(
                    "Variant '{}' does not belong to asset '{}'",
                    hash,
                    &full_id[..8]
                ))
                .into());
            }
        }

        // Refuse to extract all variants — at least one must remain
        let extract_set: HashSet<&str> = variant_hashes.iter().map(|h| h.as_str()).collect();
        if extract_set.len() >= source.variants.len() {
            return Err(MakiError::InvalidInput(
                "cannot extract all variants — at least one must remain in the source asset".into(),
            )
            .into());
        }

        // Check if the identity variant (the one that generated the asset UUID) is being split off
        let identity_hash = source.variants.iter()
            .find(|v| crate::models::Asset::id_for_hash(&v.content_hash) == source_uuid)
            .map(|v| v.content_hash.clone());
        if let Some(ref ih) = identity_hash {
            if extract_set.contains(ih.as_str()) {
                return Err(MakiError::InvalidInput(format!(
                    "Cannot split off variant '{}' — it is the identity variant that generated this asset's ID. \
                     Split the other variants instead, or use 'maki group' to reorganize.",
                    &ih[..20.min(ih.len())]
                ))
                .into());
            }
        }

        // Phase 1: compute every new asset and the trimmed source in memory.
        let mut new_assets: Vec<crate::models::Asset> = Vec::new();
        for hash in variant_hashes {
            // Find and remove the variant from source
            let idx = source
                .variants
                .iter()
                .position(|v| v.content_hash == *hash)
                .ok_or_else(|| MakiError::not_found("variant", hash.as_str()))?;
            let mut variant = source.variants.remove(idx);

            // Create new asset ID deterministically from variant hash (using the same
            // namespace as Asset::new to ensure consistency if the file is reimported)
            let new_uuid = crate::models::Asset::id_for_hash(hash);
            variant.asset_id = new_uuid;
            variant.role = crate::models::VariantRole::Original;

            // Move recipes that belong to this variant
            let mut moved_recipes = Vec::new();
            source.recipes.retain(|r| {
                if r.variant_hash == *hash {
                    moved_recipes.push(r.clone());
                    false
                } else {
                    true
                }
            });

            // Determine asset type from the variant's format
            let asset_type = crate::models::format::determine_asset_type(&variant.format);

            new_assets.push(crate::models::Asset {
                id: new_uuid,
                name: None,
                created_at: source.created_at,
                asset_type,
                tags: source.tags.clone(),
                tag_sources: source.tag_sources.clone(),
                description: source.description.clone(),
                rating: source.rating,
                color_label: source.color_label.clone(),
                // New split-off asset starts unscanned — its own image may have
                // faces that the source asset's scan never saw (different crop,
                // different variant, etc.)
                face_scan_status: None,
                preview_rotation: None,
                preview_variant: None,
                variants: vec![variant],
                recipes: moved_recipes,
            });
        }

        // Phase 2: sidecars (source of truth) first — the new assets, then
        // the trimmed source.
        for new_asset in &new_assets {
            store.save(new_asset)?;
        }
        store.save(&source)?;

        // Phase 3: every catalog row in one transaction. The extracted
        // variants change owner and role; `insert_asset` derives the new
        // assets' denormalized columns, the source's are recomputed.
        catalog.with_tx(|c| {
            for new_asset in &new_assets {
                c.insert_asset(new_asset)?;
                let variant = &new_asset.variants[0];
                c.update_variant_asset_id(&variant.content_hash, &new_asset.id.to_string())?;
                c.update_variant_role(&variant.content_hash, "original")?;
            }
            c.insert_asset(&source)?;
            c.update_denormalized_variant_columns(&source)?;
            Ok(())
        })?;

        let new_assets_info = new_assets
            .iter()
            .map(|a| NewSplitAsset {
                asset_id: a.id.to_string(),
                variant_hash: a.variants[0].content_hash.clone(),
                original_filename: a.variants[0].original_filename.clone(),
            })
            .collect();

        Ok(SplitResult {
            source_id: full_id,
            new_assets: new_assets_info,
        })
    }

    /// Auto-group assets by filename stem using fuzzy prefix matching.
    ///
    /// Two stems match if the shorter is a prefix of the longer and the next
    /// character in the longer string is non-alphanumeric (a separator like
    /// `-`, `_`, ` `, `(`, etc.). This handles the common case where export
    /// tools append suffixes to the original filename:
    /// `Z91_8561.ARW` → `Z91_8561-1-HighRes-(c)_2025_Name.tif`.
    ///
    /// Picks the best target per group (RAW preferred, then oldest) and
    /// merges. `scope` decides how candidates are partitioned before stems
    /// are compared (see [`AutoGroupScope`]); `on_group` is called once per
    /// group found with the root stem and the number of assets in it.
    pub fn auto_group(
        &self,
        asset_ids: &[String],
        dry_run: bool,
        scope: AutoGroupScope,
        on_group: impl FnMut(&str, usize),
    ) -> Result<AutoGroupResult> {
        let (global_scope, import_roots) = match &scope {
            AutoGroupScope::SessionRoots => (false, &[][..]),
            AutoGroupScope::ImportRoots(roots) => (false, roots.as_slice()),
            AutoGroupScope::Global => (true, &[][..]),
        };
        self.auto_group_inner(asset_ids, dry_run, global_scope, import_roots, on_group)
    }

    pub(super) fn auto_group_inner(&self, asset_ids: &[String], dry_run: bool, global_scope: bool, import_roots: &[String], mut on_group: impl FnMut(&str, usize)) -> Result<AutoGroupResult> {
        let catalog = self.catalog()?;

        // Deduplicate input IDs
        let unique_ids: Vec<String> = {
            let mut seen = HashSet::new();
            asset_ids
                .iter()
                .filter(|id| seen.insert((*id).clone()))
                .cloned()
                .collect()
        };

        // Load details for each asset and extract stem + directory
        struct StemEntry {
            stem: String,
            dir: String, // parent directory of primary variant
            asset_id: String,
            details: crate::catalog::AssetDetails,
        }
        let mut entries: Vec<StemEntry> = Vec::new();
        for id in &unique_ids {
            let details = match catalog.load_asset_details(id)? {
                Some(d) => d,
                None => continue,
            };
            let (stem, dir) = if let Some(v) = details.variants.first() {
                let s = std::path::Path::new(&v.original_filename)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_uppercase())
                    .unwrap_or_default();
                // Extract directory from the first file location of the first variant
                let d = if let Some(loc) = v.locations.first() {
                    let p = std::path::Path::new(&loc.relative_path);
                    p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default()
                } else {
                    String::new()
                };
                (s, d)
            } else {
                continue;
            };
            if stem.is_empty() {
                continue;
            }
            entries.push(StemEntry { stem, dir, asset_id: id.clone(), details });
        }

        // Partition by directory neighborhood: group entries whose files share
        // a common shoot/session root. This prevents DSC_0001 from a 2019 shoot
        // being grouped with DSC_0001 from a 2024 shoot.
        //
        // The session root is detected by finding the deepest directory component
        // matching `[group] session_root_pattern` (by default a date, YYYY-MM).
        // Everything below that level (Capture/, Selects/, Output/ and their
        // subdirectories) belongs to the same session. Without a match the
        // import directory (if any) or the parent directory is the boundary —
        // the same rule `import --auto-group` uses to pick its candidates.
        let neighborhoods: Vec<Vec<StemEntry>> = if global_scope {
            vec![entries]
        } else {
            let pattern = &self.config.group.session_root_pattern;
            let mut dir_groups: HashMap<String, Vec<StemEntry>> = HashMap::new();
            for entry in entries {
                let session_root = find_session_root(&entry.dir, pattern, import_roots);
                dir_groups.entry(session_root).or_default().push(entry);
            }
            dir_groups.into_values().collect()
        };

        // Process each neighborhood independently
        let mut all_groups = Vec::new();
        let mut total_donors_merged = 0;
        let mut total_variants_moved = 0;

        for neighborhood in neighborhoods {
            let mut entries = neighborhood;

        // Sort by stem length (shortest first) for prefix resolution
        entries.sort_by_key(|e| e.stem.len());

        // Resolve each stem to its root (shortest valid prefix-match)
        let mut roots: Vec<String> = Vec::new();
        let mut stem_to_root: HashMap<String, String> = HashMap::new();

        for entry in &entries {
            let stem = &entry.stem;
            if stem_to_root.contains_key(stem) {
                continue;
            }
            let mut found_root = None;
            for root in &roots {
                if stem_prefix_matches(root, stem) {
                    found_root = Some(root.clone());
                    break;
                }
            }
            match found_root {
                Some(root) => {
                    stem_to_root.insert(stem.clone(), root);
                }
                None => {
                    roots.push(stem.clone());
                    stem_to_root.insert(stem.clone(), stem.clone());
                }
            }
        }

        // Group assets by resolved root stem
        let mut group_map: HashMap<String, Vec<(String, crate::catalog::AssetDetails)>> =
            HashMap::new();
        for entry in entries {
            let root = stem_to_root.get(&entry.stem).unwrap();
            group_map
                .entry(root.clone())
                .or_default()
                .push((entry.asset_id, entry.details));
        }

        // Filter to groups with >1 distinct asset and merge
        for (root_stem, mut entries) in group_map {
            if entries.len() < 2 {
                continue;
            }

            // Sort: prefer asset with RAW variant, then oldest by created_at
            entries.sort_by(|a, b| {
                let a_raw = a.1.variants.iter().any(|v| {
                    crate::models::format::is_raw_extension(&v.format)
                });
                let b_raw = b.1.variants.iter().any(|v| {
                    crate::models::format::is_raw_extension(&v.format)
                });
                b_raw.cmp(&a_raw).then_with(|| a.1.created_at.cmp(&b.1.created_at))
            });

            let target_id = entries[0].0.clone();
            let all_ids: Vec<String> = entries.iter().map(|e| e.0.clone()).collect();
            let donor_count = entries.len() - 1;

            if !dry_run {
                let all_hashes: Vec<String> = entries
                    .iter()
                    .flat_map(|e| e.1.variants.iter().map(|v| v.content_hash.clone()))
                    .collect();
                let result = self.group(&all_hashes)?;
                total_variants_moved += result.variants_moved;
                total_donors_merged += result.donors_removed;
            } else {
                let donor_variants: usize = entries[1..]
                    .iter()
                    .map(|e| e.1.variants.len())
                    .sum();
                total_variants_moved += donor_variants;
                total_donors_merged += donor_count;
            }

            on_group(&root_stem, entries.len());
            all_groups.push(StemGroupEntry {
                stem: root_stem,
                target_id,
                asset_ids: all_ids,
                donor_count,
            });
        }

        } // end neighborhood loop

        // Sort groups by stem for deterministic output
        all_groups.sort_by(|a, b| a.stem.cmp(&b.stem));

        Ok(AutoGroupResult {
            groups: all_groups,
            total_donors_merged,
            total_variants_moved,
            dry_run,
        })
    }

    // ═══ STACK FROM TAG ═══

    /// Convert tags matching a pattern into stacks.
    /// Pattern uses `{}` as a wildcard placeholder (e.g. `"Aperture Stack {}"`).
    /// Default is report-only (dry run); pass `apply=true` to execute.
    /// `on_event` receives one [`FromTagEvent`] per matched tag; the CLI
    /// prints `event.to_string()` under `--log`.
    pub fn stack_from_tag(
        &self,
        pattern: &str,
        remove_tags: bool,
        apply: bool,
        on_event: &mut dyn FnMut(FromTagEvent<'_>),
    ) -> Result<FromTagResult> {
        if !pattern.contains("{}") {
            anyhow::bail!("pattern must contain '{{}}' as a wildcard placeholder");
        }

        // Build regex: escape metacharacters, replace {} with (.+), anchor
        let escaped = regex::escape(pattern).replace("\\{\\}", "(.+)");
        let re = regex::Regex::new(&format!("^{escaped}$"))?;

        let catalog = self.catalog()?;
        let all_tags = catalog.list_all_tags()?;

        let mut matching_tags: Vec<String> = all_tags
            .iter()
            .filter(|(tag, _)| re.is_match(tag))
            .map(|(tag, _)| tag.clone())
            .collect();
        matching_tags.sort();

        let mut result = FromTagResult {
            tags_matched: matching_tags.len() as u32,
            tags_skipped: 0,
            stacks_created: 0,
            assets_stacked: 0,
            assets_skipped: 0,
            tags_removed: 0,
            dry_run: !apply,
            details: Vec::new(),
            errors: Vec::new(),
        };

        // `create` persists stacks.yaml itself (StackStore::open).
        let store = crate::stack::StackStore::open(catalog.conn(), &self.catalog_root);

        for tag in &matching_tags {
            let assets = catalog.assets_with_exact_tag(tag)?;
            let total_found = assets.len() as u32;
            let all_asset_ids: Vec<String> = assets.iter().map(|(id, _)| id.clone()).collect();

            // Partition into stacked and unstacked
            let (already_stacked, unstacked): (Vec<_>, Vec<_>) =
                assets.into_iter().partition(|(_, stack_id)| stack_id.is_some());

            let skipped = already_stacked.len() as u32;

            // Helper: when --remove-tags is set, remove the tag from every asset
            // that carries it — regardless of whether a stack was created. This
            // handles orphan tags (only 1 asset) and tags left over from earlier
            // runs where stacks were already created but tag removal was incomplete.
            let remove_all_tags = |ids: &[String], result: &mut FromTagResult| {
                if remove_tags && apply {
                    for id in ids {
                        match self.tag(id, std::slice::from_ref(tag), true) {
                            Ok(_) => result.tags_removed += 1,
                            Err(e) => result.errors.push(format!(
                                "{}: could not remove tag '{tag}': {e:#}",
                                &id[..8.min(id.len())]
                            )),
                        }
                    }
                }
            };

            if unstacked.len() < 2 {
                result.tags_skipped += 1;
                on_event(FromTagEvent::Skipped {
                    tag,
                    unstacked: unstacked.len(),
                    already_stacked: skipped,
                    tag_removed_from: if remove_tags && apply { Some(total_found) } else { None },
                });
                // Still remove the tag when requested, even though no stack is created.
                remove_all_tags(&all_asset_ids, &mut result);
                result.details.push(FromTagDetail {
                    tag: tag.clone(),
                    assets_found: total_found,
                    assets_stacked: 0,
                    assets_skipped: skipped,
                    stack_id: None,
                });
                continue;
            }

            let unstacked_ids: Vec<String> = unstacked.into_iter().map(|(id, _)| id).collect();
            let stacked_count = unstacked_ids.len() as u32;
            let mut stack_id_str = None;

            if apply {
                let stack = store.create(&unstacked_ids)?;
                stack_id_str = Some(stack.id.to_string());
            }
            // Remove tag from ALL assets with it (newly-stacked + already-stacked).
            remove_all_tags(&all_asset_ids, &mut result);

            result.stacks_created += 1;
            result.assets_stacked += stacked_count;
            result.assets_skipped += skipped;

            on_event(FromTagEvent::Stacked { tag, applied: apply, stacked: stacked_count, skipped });

            result.details.push(FromTagDetail {
                tag: tag.clone(),
                assets_found: total_found,
                assets_stacked: stacked_count,
                assets_skipped: skipped,
                stack_id: stack_id_str,
            });
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::metadata_store::MetadataStore;
    use crate::models::{Asset, AssetType, Variant};
    use crate::query::test_support::*;

    #[test]
    fn group_two_variants_from_two_assets() {
        let (dir, hash1, hash2, id1, id2) = setup_group_env();
        let engine = QueryEngine::new(dir.path());

        let result = engine.group(&[hash1, hash2]).unwrap();

        // Target should be the older asset (asset1)
        assert_eq!(result.target_id, id1);
        assert_eq!(result.variants_moved, 1);
        assert_eq!(result.donors_removed, 1);

        // Target should now have both variants
        let details = engine.show(&id1).unwrap();
        assert_eq!(details.variants.len(), 2);

        // Original variant keeps its role; the JPEG donor joining a RAW
        // target becomes an export. The catalog row carries the same role
        // as the sidecar (until the F2 refactor the row said "alternate"
        // while the sidecar said "export").
        let original = details.variants.iter().find(|v| v.content_hash == "sha256:hash1").unwrap();
        assert_eq!(original.role, "original");
        let moved = details.variants.iter().find(|v| v.content_hash == "sha256:hash2").unwrap();
        assert_eq!(moved.role, "export");
        let sidecar = MetadataStore::new(dir.path()).load(id1.parse().unwrap()).unwrap();
        let moved_in_sidecar = sidecar.variants.iter().find(|v| v.content_hash == "sha256:hash2").unwrap();
        assert_eq!(moved_in_sidecar.role.as_str(), "export");

        // Donor should be gone
        assert!(engine.show(&id2).is_err());
    }

    #[test]
    fn group_already_same_asset_is_noop() {
        let (dir, hash1, _, id1, _) = setup_group_env();
        let engine = QueryEngine::new(dir.path());

        let result = engine.group(&[hash1.clone(), hash1]).unwrap();

        assert_eq!(result.target_id, id1);
        assert_eq!(result.variants_moved, 0);
        assert_eq!(result.donors_removed, 0);
    }

    #[test]
    fn group_nonexistent_hash_errors() {
        let (dir, _, _, _, _) = setup_group_env();
        let engine = QueryEngine::new(dir.path());

        let result = engine.group(&["sha256:bogus".to_string()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no variant found"));
    }

    /// A statement failing mid-merge (here: the donor's variant row is
    /// missing from SQLite, so the ownership update affects no row) must
    /// roll back every catalog write of the merge — the target keeps its
    /// old tags, the donor's asset row is still there.
    #[test]
    fn group_rolls_back_catalog_rows_when_a_statement_fails() {
        let (dir, _hash1, hash2, id1, id2) = setup_group_env();
        let catalog = Catalog::open(dir.path()).unwrap();
        catalog
            .conn()
            .execute("DELETE FROM variants WHERE content_hash = ?1", rusqlite::params![hash2])
            .unwrap();

        let engine = QueryEngine::new(dir.path());
        let err = engine
            .group_by_asset_ids(&[id1.clone(), id2.clone()], None)
            .unwrap_err();
        assert!(err.to_string().contains("no variant found"), "{err}");

        let asset_row = "SELECT COUNT(*) FROM assets WHERE id = ?1";
        assert_eq!(count(&catalog, asset_row, &id2), 1, "donor row survives the rollback");
        let tags: String = catalog
            .conn()
            .query_row("SELECT tags FROM assets WHERE id = ?1", rusqlite::params![id1], |r| r.get(0))
            .unwrap();
        assert_eq!(tags, r#"["landscape"]"#, "target row untouched");
        assert_eq!(count(&catalog, "SELECT COUNT(*) FROM variants WHERE asset_id = ?1", &id1), 1);
        // The sidecars were written first (source of truth): the target
        // describes the merged state, the donor sidecar is gone — exactly
        // the state `doctor --repair` converges the catalog to.
        let store = MetadataStore::new(dir.path());
        assert!(store.load(id2.parse().unwrap()).is_err());
        assert_eq!(store.load(id1.parse().unwrap()).unwrap().variants.len(), 2);
    }

    /// Same contract for split: when the extracted variant's row is
    /// missing from SQLite, no new asset row and no change to the source
    /// row may remain.
    #[test]
    fn split_rolls_back_catalog_rows_when_a_statement_fails() {
        let (dir, hash1, hash2, id1, _id2) = setup_group_env();
        let engine = QueryEngine::new(dir.path());
        engine.group(&[hash1.clone(), hash2.clone()]).unwrap();

        let catalog = Catalog::open(dir.path()).unwrap();
        catalog
            .conn()
            .execute("DELETE FROM variants WHERE content_hash = ?1", rusqlite::params![hash2])
            .unwrap();
        let before: (i64, String) = catalog
            .conn()
            .query_row(
                "SELECT variant_count, best_variant_hash FROM assets WHERE id = ?1",
                rusqlite::params![id1],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();

        let err = engine.split(&id1, std::slice::from_ref(&hash2)).unwrap_err();
        assert!(err.to_string().contains("no variant found"), "{err}");

        let new_id = Asset::id_for_hash(&hash2).to_string();
        assert_eq!(count(&catalog, "SELECT COUNT(*) FROM assets WHERE id = ?1", &new_id), 0);
        let after: (i64, String) = catalog
            .conn()
            .query_row(
                "SELECT variant_count, best_variant_hash FROM assets WHERE id = ?1",
                rusqlite::params![id1],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(after, before, "source row untouched");
    }

    #[test]
    fn group_merges_tags() {
        let (dir, hash1, hash2, id1, _) = setup_group_env();
        let engine = QueryEngine::new(dir.path());

        engine.group(&[hash1, hash2]).unwrap();

        let details = engine.show(&id1).unwrap();
        assert!(details.tags.contains(&"landscape".to_string()));
        assert!(details.tags.contains(&"nature".to_string()));
    }

    #[test]
    fn group_merges_best_rating() {
        let dir = tempfile::tempdir().unwrap();
        let catalog_root = dir.path();
        let catalog = Catalog::open(catalog_root).unwrap();
        catalog.initialize().unwrap();
        let store = MetadataStore::new(catalog_root);

        // Asset 1: rating 3, no label, no description
        let mut asset1 = Asset::new(AssetType::Image, "sha256:rate1");
        asset1.created_at = chrono::Utc::now() - chrono::Duration::hours(2);
        asset1.rating = Some(3);
        let v1 = Variant { file_size: 25_000_000, ..test_variant(asset1.id, "sha256:rate1", "DSC_001.ARW") };
        asset1.variants.push(v1.clone());
        catalog.insert_asset(&asset1).unwrap();
        catalog.insert_variant(&v1).unwrap();
        store.save(&asset1).unwrap();

        // Asset 2: rating 5, color label Red, description "Great shot"
        let mut asset2 = Asset::new(AssetType::Image, "sha256:rate2");
        asset2.rating = Some(5);
        asset2.color_label = Some("Red".to_string());
        asset2.description = Some("Great shot".to_string());
        let v2 = Variant { file_size: 5_000_000, ..test_variant(asset2.id, "sha256:rate2", "DSC_001.JPG") };
        asset2.variants.push(v2.clone());
        catalog.insert_asset(&asset2).unwrap();
        catalog.insert_variant(&v2).unwrap();
        store.save(&asset2).unwrap();

        let id1 = asset1.id.to_string();
        let engine = QueryEngine::new(dir.path());
        engine.group(&["sha256:rate1".to_string(), "sha256:rate2".to_string()]).unwrap();

        let details = engine.show(&id1).unwrap();
        // Highest rating wins
        assert_eq!(details.rating, Some(5));
        // First non-None color label
        assert_eq!(details.color_label.as_deref(), Some("Red"));
        // First non-None description
        assert_eq!(details.description.as_deref(), Some("Great shot"));
    }

    #[test]
    fn group_keeps_target_metadata_when_higher() {
        let dir = tempfile::tempdir().unwrap();
        let catalog_root = dir.path();
        let catalog = Catalog::open(catalog_root).unwrap();
        catalog.initialize().unwrap();
        let store = MetadataStore::new(catalog_root);

        // Asset 1 (target, older): rating 5, label Blue, description "Target desc"
        let mut asset1 = Asset::new(AssetType::Image, "sha256:keep1");
        asset1.created_at = chrono::Utc::now() - chrono::Duration::hours(2);
        asset1.rating = Some(5);
        asset1.color_label = Some("Blue".to_string());
        asset1.description = Some("Target desc".to_string());
        let v1 = Variant { file_size: 25_000_000, ..test_variant(asset1.id, "sha256:keep1", "IMG_001.ARW") };
        asset1.variants.push(v1.clone());
        catalog.insert_asset(&asset1).unwrap();
        catalog.insert_variant(&v1).unwrap();
        store.save(&asset1).unwrap();

        // Asset 2 (donor): rating 2, label Red, description "Donor desc"
        let mut asset2 = Asset::new(AssetType::Image, "sha256:keep2");
        asset2.rating = Some(2);
        asset2.color_label = Some("Red".to_string());
        asset2.description = Some("Donor desc".to_string());
        let v2 = Variant { file_size: 5_000_000, ..test_variant(asset2.id, "sha256:keep2", "IMG_001.JPG") };
        asset2.variants.push(v2.clone());
        catalog.insert_asset(&asset2).unwrap();
        catalog.insert_variant(&v2).unwrap();
        store.save(&asset2).unwrap();

        let id1 = asset1.id.to_string();
        let engine = QueryEngine::new(dir.path());
        engine.group(&["sha256:keep1".to_string(), "sha256:keep2".to_string()]).unwrap();

        let details = engine.show(&id1).unwrap();
        // Target had higher rating — keep it
        assert_eq!(details.rating, Some(5));
        // Target already had label — keep it
        assert_eq!(details.color_label.as_deref(), Some("Blue"));
        // Target already had description — keep it
        assert_eq!(details.description.as_deref(), Some("Target desc"));
    }

    // ── group recipe preservation tests ──────────────────────────────

    #[test]
    fn group_preserves_recipes() {
        use crate::models::{Recipe, RecipeType};
        use crate::models::volume::FileLocation;

        let (dir, hash1, hash2, id1, id2) = setup_group_env();

        // Add a recipe to the donor (asset2)
        let store = MetadataStore::new(dir.path());
        let uuid2: uuid::Uuid = id2.parse().unwrap();
        let mut asset2 = store.load(uuid2).unwrap();
        asset2.recipes.push(Recipe {
            id: uuid::Uuid::new_v4(),
            variant_hash: "sha256:hash2".to_string(),
            software: "Adobe/CaptureOne".to_string(),
            recipe_type: RecipeType::Sidecar,
            content_hash: "sha256:recipe_hash".to_string(),
            location: FileLocation {
                volume_id: uuid::Uuid::nil(),
                relative_path: "DSC_001.xmp".into(),
                verified_at: None,
            },
            pending_writeback: false,
        });
        store.save(&asset2).unwrap();

        let engine = QueryEngine::new(dir.path());
        engine.group(&[hash1, hash2]).unwrap();

        // Verify recipe is on the target sidecar
        let uuid1: uuid::Uuid = id1.parse().unwrap();
        let target = store.load(uuid1).unwrap();
        assert_eq!(target.recipes.len(), 1);
        assert_eq!(target.recipes[0].variant_hash, "sha256:hash2");
    }

    // ── auto_group tests ─────────────────────────────────────────────

    #[test]
    fn group_merge_removes_donor_from_collections_yaml() {
        let (dir, _, _, id1, id2) = setup_group_env();
        let root = dir.path();
        {
            let catalog = Catalog::open(root).unwrap();
            let collections = crate::collection::CollectionStore::open(catalog.conn(), root);
            collections.create("keepers", None).unwrap();
            collections.add_assets("keepers", &[id1.clone(), id2.clone()]).unwrap();
        }
        let before = crate::collection::load_yaml(root).unwrap();
        assert!(before.collections[0].asset_ids.contains(&id2));

        // id2 (the JPG) is the donor of the merge into id1 (the RAW).
        let engine = QueryEngine::new(root);
        let result = engine
            .auto_group(&[id1.clone(), id2.clone()], false, AutoGroupScope::SessionRoots, |_, _| {})
            .unwrap();
        assert_eq!(result.total_donors_merged, 1);

        let after = crate::collection::load_yaml(root).unwrap();
        let members = &after.collections[0].asset_ids;
        assert!(members.contains(&id1), "target keeps its membership: {members:?}");
        assert!(!members.contains(&id2), "donor removed from collections.yaml: {members:?}");
        let catalog = Catalog::open(root).unwrap();
        assert_eq!(count(&catalog, "SELECT COUNT(*) FROM collection_assets WHERE asset_id = ?1", &id2), 0);
    }


    #[test]
    fn auto_group_merges_same_stem() {
        let (dir, _, _, id1, id2) = setup_group_env();
        // Both assets have variants with stem DSC_001 (ARW and JPG)
        let engine = QueryEngine::new(dir.path());

        let result = engine
            .auto_group(&[id1.clone(), id2.clone()], false, AutoGroupScope::SessionRoots, |_, _| {})
            .unwrap();

        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.total_donors_merged, 1);
        assert!(!result.dry_run);

        // RAW asset (id1) should be the target
        assert_eq!(result.groups[0].target_id, id1);

        // Only one asset should remain
        let details = engine.show(&id1).unwrap();
        assert_eq!(details.variants.len(), 2);
        assert!(engine.show(&id2).is_err());
    }

    #[test]
    fn auto_group_dry_run_does_not_modify() {
        let (dir, _, _, id1, id2) = setup_group_env();
        let engine = QueryEngine::new(dir.path());

        let result = engine
            .auto_group(&[id1.clone(), id2.clone()], true, AutoGroupScope::SessionRoots, |_, _| {})
            .unwrap();

        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.total_donors_merged, 1);
        assert!(result.dry_run);

        // Both assets should still exist
        assert!(engine.show(&id1).is_ok());
        assert!(engine.show(&id2).is_ok());
    }

    #[test]
    fn auto_group_different_stems_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let catalog_root = dir.path();
        let catalog = Catalog::open(catalog_root).unwrap();
        catalog.initialize().unwrap();
        let store = MetadataStore::new(catalog_root);

        let mut asset1 = Asset::new(AssetType::Image, "sha256:aaa");
        let v1 = test_variant(asset1.id, "sha256:aaa", "IMG_001.JPG");
        asset1.variants.push(v1.clone());
        catalog.insert_asset(&asset1).unwrap();
        catalog.insert_variant(&v1).unwrap();
        store.save(&asset1).unwrap();

        let mut asset2 = Asset::new(AssetType::Image, "sha256:bbb");
        let v2 = Variant { file_size: 2000, ..test_variant(asset2.id, "sha256:bbb", "IMG_002.JPG") };
        asset2.variants.push(v2.clone());
        catalog.insert_asset(&asset2).unwrap();
        catalog.insert_variant(&v2).unwrap();
        store.save(&asset2).unwrap();

        let engine = QueryEngine::new(catalog_root);
        let result = engine
            .auto_group(
                &[asset1.id.to_string(), asset2.id.to_string()],
                false,
                AutoGroupScope::SessionRoots,
                |_, _| {},
            )
            .unwrap();

        assert!(result.groups.is_empty());
        assert_eq!(result.total_donors_merged, 0);
    }

    // ── stem_prefix_matches tests ────────────────────────────────────

    #[test]
    fn stem_prefix_exact_match() {
        assert!(stem_prefix_matches("DSC_001", "DSC_001"));
    }

    #[test]
    fn stem_prefix_separator_dash() {
        assert!(stem_prefix_matches("Z91_8561", "Z91_8561-1-HIGHRES"));
    }

    #[test]
    fn stem_prefix_separator_underscore() {
        assert!(stem_prefix_matches("DSC_001", "DSC_001_V2"));
    }

    #[test]
    fn stem_prefix_separator_space() {
        assert!(stem_prefix_matches("DSC_001", "DSC_001 (1)"));
    }

    #[test]
    fn stem_prefix_separator_paren() {
        assert!(stem_prefix_matches("IMG_1234", "IMG_1234(EDIT)"));
    }

    #[test]
    fn stem_prefix_rejects_digit_continuation() {
        // DSC_001 should NOT match DSC_0010 (different shot number)
        assert!(!stem_prefix_matches("DSC_001", "DSC_0010"));
    }

    #[test]
    fn stem_prefix_rejects_letter_continuation() {
        assert!(!stem_prefix_matches("IMG", "IMAGES"));
    }

    #[test]
    fn stem_prefix_no_match() {
        assert!(!stem_prefix_matches("DSC_001", "IMG_001"));
    }

    // ── fuzzy auto_group tests ───────────────────────────────────────

    /// Helper: create a single-variant asset in the catalog/sidecar.
    fn create_asset_with_filename(
        catalog: &Catalog,
        store: &MetadataStore,
        hash: &str,
        filename: &str,
        format: &str,
    ) -> String {
        let mut asset = test_asset(hash).file(filename).build();
        asset.variants[0].format = format.to_string();
        insert_rows(catalog, &asset);
        store.save(&asset).unwrap();
        asset.id.to_string()
    }

    #[test]
    fn auto_group_fuzzy_prefix_match() {
        let dir = tempfile::tempdir().unwrap();
        let catalog_root = dir.path();
        let catalog = Catalog::open(catalog_root).unwrap();
        catalog.initialize().unwrap();
        let store = MetadataStore::new(catalog_root);

        let id_raw = create_asset_with_filename(
            &catalog, &store, "sha256:raw1", "Z91_8561.ARW", "arw",
        );
        let id_export = create_asset_with_filename(
            &catalog, &store, "sha256:exp1",
            "Z91_8561-1-HighRes-(c)_2025_Thomas Herrmann.TIF", "tif",
        );

        let engine = QueryEngine::new(catalog_root);
        let result = engine
            .auto_group(&[id_raw.clone(), id_export.clone()], false, AutoGroupScope::SessionRoots, |_, _| {})
            .unwrap();

        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.total_donors_merged, 1);
        // RAW asset should be the target
        assert_eq!(result.groups[0].target_id, id_raw);
    }

    #[test]
    fn auto_group_fuzzy_rejects_numeric_continuation() {
        let dir = tempfile::tempdir().unwrap();
        let catalog_root = dir.path();
        let catalog = Catalog::open(catalog_root).unwrap();
        catalog.initialize().unwrap();
        let store = MetadataStore::new(catalog_root);

        let id1 = create_asset_with_filename(
            &catalog, &store, "sha256:f1", "DSC_001.ARW", "arw",
        );
        let id2 = create_asset_with_filename(
            &catalog, &store, "sha256:f2", "DSC_0010.JPG", "jpg",
        );

        let engine = QueryEngine::new(catalog_root);
        let result = engine
            .auto_group(&[id1, id2], false, AutoGroupScope::SessionRoots, |_, _| {})
            .unwrap();

        // Should NOT match — these are different shots
        assert!(result.groups.is_empty());
    }

    #[test]
    fn auto_group_fuzzy_chain_resolves_to_shortest_root() {
        let dir = tempfile::tempdir().unwrap();
        let catalog_root = dir.path();
        let catalog = Catalog::open(catalog_root).unwrap();
        catalog.initialize().unwrap();
        let store = MetadataStore::new(catalog_root);

        let id_raw = create_asset_with_filename(
            &catalog, &store, "sha256:c1", "Z91_8561.ARW", "arw",
        );
        let id_v1 = create_asset_with_filename(
            &catalog, &store, "sha256:c2", "Z91_8561-1.JPG", "jpg",
        );
        let id_v2 = create_asset_with_filename(
            &catalog, &store, "sha256:c3",
            "Z91_8561-1-HighRes.TIF", "tif",
        );

        let engine = QueryEngine::new(catalog_root);
        let result = engine
            .auto_group(&[id_raw.clone(), id_v1.clone(), id_v2.clone()], false, AutoGroupScope::SessionRoots, |_, _| {})
            .unwrap();

        // All three should be in one group
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].asset_ids.len(), 3);
        assert_eq!(result.total_donors_merged, 2);
        // RAW asset should be the target
        assert_eq!(result.groups[0].target_id, id_raw);
    }

    // -- from-tag pattern matching tests --

    fn build_from_tag_regex(pattern: &str) -> regex::Regex {
        let escaped = regex::escape(pattern).replace("\\{\\}", "(.+)");
        regex::Regex::new(&format!("^{escaped}$")).unwrap()
    }

    #[test]
    fn from_tag_pattern_matches_aperture_stack() {
        let re = build_from_tag_regex("Aperture Stack {}");
        assert!(re.is_match("Aperture Stack 1"));
        assert!(re.is_match("Aperture Stack 1234"));
        assert!(re.is_match("Aperture Stack abc"));
        assert!(re.is_match("Aperture Stack 1 extra")); // wildcard captures "1 extra"
        assert!(!re.is_match("Aperture Stack")); // empty wildcard = no match
        assert!(!re.is_match("Other Tag"));
    }

    #[test]
    fn from_tag_pattern_matches_prefix_wildcard() {
        let re = build_from_tag_regex("shoot-{}");
        assert!(re.is_match("shoot-A"));
        assert!(re.is_match("shoot-paris-01"));
        assert!(!re.is_match("shoot-")); // empty wildcard
        assert!(!re.is_match("xshoot-A")); // prefix mismatch
    }

    #[test]
    fn from_tag_pattern_escapes_regex_metacharacters() {
        let re = build_from_tag_regex("Group (A) {}");
        assert!(re.is_match("Group (A) 1"));
        assert!(!re.is_match("Group A 1")); // literal parens required
    }

    #[test]
    fn from_tag_pattern_middle_wildcard() {
        let re = build_from_tag_regex("pre-{}-post");
        assert!(re.is_match("pre-X-post"));
        assert!(re.is_match("pre-hello world-post"));
        assert!(!re.is_match("pre--post")); // empty wildcard
    }

    // ── split tests ────────────────────────────────────────

    fn setup_split_catalog(variant_hashes: &[&str]) -> (TempCatalog, String) {
        let cat = TempCatalog::new();
        // The first hash determines the asset ID.
        let mut builder = test_asset(&format!("sha256:{}", variant_hashes[0])).tags(&["landscape"]).rating(4);
        for hash in &variant_hashes[1..] {
            builder = builder.variant(&format!("sha256:{hash}"), &format!("{hash}.jpg"));
        }
        let seeded = cat.seed(builder);
        (cat, seeded.id.to_string())
    }

    #[test]
    fn split_extracts_variant_into_new_asset() {
        let (dir, asset_id) = setup_split_catalog(&["aaa", "bbb", "ccc"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.split(&asset_id, &["sha256:bbb".to_string()]).unwrap();

        assert_eq!(result.new_assets.len(), 1);
        assert_eq!(result.new_assets[0].variant_hash, "sha256:bbb");

        // Source should have 2 variants remaining
        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let source: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert_eq!(source.variants.len(), 2);

        // New asset should have 1 variant with inherited metadata
        let new_id: uuid::Uuid = result.new_assets[0].asset_id.parse().unwrap();
        let new_asset = store.load(new_id).unwrap();
        assert_eq!(new_asset.variants.len(), 1);
        assert_eq!(new_asset.tags, vec!["landscape".to_string()]);
        assert_eq!(new_asset.rating, Some(4));
    }

    #[test]
    fn split_refuses_all_variants() {
        let (dir, asset_id) = setup_split_catalog(&["aaa", "bbb"]);
        let engine = QueryEngine::new(dir.path());
        let err = engine.split(&asset_id, &[
            "sha256:aaa".to_string(),
            "sha256:bbb".to_string(),
        ]).unwrap_err();
        assert!(err.to_string().contains("at least one must remain"));
    }

    #[test]
    fn split_refuses_identity_variant() {
        let (dir, asset_id) = setup_split_catalog(&["aaa", "bbb", "ccc"]);
        let engine = QueryEngine::new(dir.path());

        // "aaa" is the identity variant (Asset::new uses it for the UUID)
        let err = engine.split(&asset_id, &["sha256:aaa".to_string()]).unwrap_err();
        assert!(err.to_string().contains("identity variant"), "error: {}", err);
    }

    #[test]
    fn split_refuses_unknown_variant() {
        let (dir, asset_id) = setup_split_catalog(&["aaa", "bbb"]);
        let engine = QueryEngine::new(dir.path());
        let err = engine.split(&asset_id, &["sha256:zzz".to_string()]).unwrap_err();
        assert!(err.to_string().contains("does not belong"));
    }

    #[test]
    fn split_new_asset_id_matches_reimport() {
        // Verify that the split-created asset ID matches what Asset::new would produce
        let (dir, asset_id) = setup_split_catalog(&["aaa", "bbb"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.split(&asset_id, &["sha256:bbb".to_string()]).unwrap();

        let expected_id = crate::models::Asset::id_for_hash("sha256:bbb");
        assert_eq!(result.new_assets[0].asset_id, expected_id.to_string());
    }


    // ── find_session_root tests ─────────────────────────

    const DEFAULT_SESSION_PATTERN: &str = r"^\d{4}-\d{2}";

    #[test]
    fn session_root_date_dir() {
        assert_eq!(
            find_session_root("Pictures/Masters/2024/2024-10/2024-10-05-jazz-band/Capture", DEFAULT_SESSION_PATTERN, &[]),
            "Pictures/Masters/2024/2024-10/2024-10-05-jazz-band"
        );
    }

    #[test]
    fn session_root_deep_output() {
        assert_eq!(
            find_session_root("Pictures/Masters/2025/2025-05/2025-05-09-wedding/Output/Final/Web", DEFAULT_SESSION_PATTERN, &[]),
            "Pictures/Masters/2025/2025-05/2025-05-09-wedding"
        );
    }

    #[test]
    fn session_root_selects_subdir() {
        assert_eq!(
            find_session_root("Pictures/Masters/2025/2025-05/2025-05-09-wedding/Selects/Goettweig", DEFAULT_SESSION_PATTERN, &[]),
            "Pictures/Masters/2025/2025-05/2025-05-09-wedding"
        );
    }

    #[test]
    fn session_root_no_date() {
        // Falls back to parent directory
        assert_eq!(
            find_session_root("Unsorted/photos/batch1", DEFAULT_SESSION_PATTERN, &[]),
            "Unsorted/photos"
        );
    }

    #[test]
    fn session_root_different_shoots_same_camera() {
        // Different dates produce different session roots
        let root_2023 = find_session_root("Pictures/Masters/2023/2023-10/2023-10-26-red-bird/Capture", DEFAULT_SESSION_PATTERN, &[]);
        let root_2024 = find_session_root("Pictures/Masters/2024/2024-10/2024-10-05-jazz-band/Capture", DEFAULT_SESSION_PATTERN, &[]);
        assert_ne!(root_2023, root_2024);
    }

    #[test]
    fn session_root_custom_pattern() {
        // Custom pattern matching "shoot-" prefix
        assert_eq!(
            find_session_root("archive/shoot-001/RAW", r"^shoot-", &[]),
            "archive/shoot-001"
        );
    }

    #[test]
    fn session_root_empty_pattern_falls_back() {
        // Empty pattern = no session root detection, falls back to parent dir
        assert_eq!(
            find_session_root("Pictures/Masters/2024/2024-10/2024-10-05-jazz-band/Capture", "", &[]),
            "Pictures/Masters/2024/2024-10/2024-10-05-jazz-band"
        );
    }

    #[test]
    fn session_root_import_root_bounds_undated_tree() {
        // No date anywhere; the imported tree is the session, however deep
        // the file sits below it.
        let roots = vec!["Clients/Wedding-Smith".to_string()];
        assert_eq!(
            find_session_root("Clients/Wedding-Smith/Output/Final/Web", DEFAULT_SESSION_PATTERN, &roots),
            "Clients/Wedding-Smith"
        );
        assert_eq!(
            find_session_root("Clients/Wedding-Smith/Capture", DEFAULT_SESSION_PATTERN, &roots),
            "Clients/Wedding-Smith"
        );
    }

    #[test]
    fn session_root_import_root_itself_goes_one_level_up() {
        // Importing Output/ alone: the import root is the file's own
        // directory, so the parent is used to reach the sibling Capture/.
        let roots = vec!["Clients/Wedding-Smith/Output".to_string()];
        assert_eq!(
            find_session_root("Clients/Wedding-Smith/Output", DEFAULT_SESSION_PATTERN, &roots),
            "Clients/Wedding-Smith"
        );
    }

    #[test]
    fn session_root_pattern_wins_over_import_root() {
        // A dated component overrides the import root in both directions:
        // deeper than the import root …
        let roots = vec!["Archive".to_string()];
        assert_eq!(
            find_session_root("Archive/2024-10-05-jazz/Output/Web", DEFAULT_SESSION_PATTERN, &roots),
            "Archive/2024-10-05-jazz"
        );
        // … and above it.
        let roots = vec!["2024-10-05-jazz/Output/Web".to_string()];
        assert_eq!(
            find_session_root("2024-10-05-jazz/Output/Web", DEFAULT_SESSION_PATTERN, &roots),
            "2024-10-05-jazz"
        );
    }

    #[test]
    fn session_root_import_root_prefix_must_be_component() {
        // "Clients/Wedding" must not capture "Clients/Wedding-Smith/…".
        let roots = vec!["Clients/Wedding".to_string()];
        assert_eq!(
            find_session_root("Clients/Wedding-Smith/Output/Web", DEFAULT_SESSION_PATTERN, &roots),
            "Clients/Wedding-Smith/Output"
        );
    }

    #[test]
    fn session_root_deepest_import_root_wins() {
        let roots = vec!["Clients".to_string(), "Clients/Wedding-Smith".to_string()];
        assert_eq!(
            find_session_root("Clients/Wedding-Smith/Output/Web", DEFAULT_SESSION_PATTERN, &roots),
            "Clients/Wedding-Smith"
        );
    }

    #[test]
    fn session_root_volume_root_import_contains_everything() {
        // Importing the volume root ("" after stripping the mount point)
        // makes the whole volume one undated session.
        let roots = vec![String::new()];
        assert_eq!(
            find_session_root("Unsorted/photos/batch1", DEFAULT_SESSION_PATTERN, &roots),
            ""
        );
    }
}
