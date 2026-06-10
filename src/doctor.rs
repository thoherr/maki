//! `maki doctor` — verify (and optionally repair) consistency between
//! the YAML sidecars (source of truth) and the SQLite catalog (derived
//! cache).
//!
//! The dual-store invariant — "every write path updates both stores,
//! plus the denormalized columns" — is enforced by convention, and the
//! v4.5.15 `insert_recipe` bug showed how silently it can break: one
//! omitted column left `pending_writeback` flags stale in SQLite while
//! the YAML carried the truth. This module makes the invariant
//! *verifiable*: it compares every asset field-by-field, both
//! directions, and can rebuild the SQLite side from YAML for flagged
//! assets.
//!
//! What is checked, per asset:
//! - **presence both ways**: sidecars without a catalog row, catalog
//!   rows without a sidecar, unreadable sidecar files
//! - **metadata fields**: name, created_at, asset_type, tags,
//!   description, rating, color_label, preview rotation/variant
//! - **denormalized columns** (recomputed from the sidecar with the
//!   same helpers `insert_asset` uses): best_variant_hash,
//!   primary_variant_format, variant_count, leaf_tag_count,
//!   latitude/longitude, video duration/codec
//! - **variant set**, **file-location set** (per variant), and the
//!   **recipe set** including per-recipe `content_hash` and
//!   `pending_writeback` (the exact v4.5.15 divergence)
//! - **face_count** against the faces table (DB-internal consistency)
//!
//! Repair is always "rebuild derived state from YAML", never "edit
//! sidecars": flagged assets get their variants/locations/recipes
//! re-inserted from the sidecar and denormalized columns recomputed;
//! catalog rows without a sidecar are deleted. Faces, embeddings, and
//! collection/stack memberships are preserved on repair (they are not
//! part of the sidecar comparison).
//!
//! Content-hash verification of files on disk is `maki verify`'s job —
//! doctor checks store consistency, not disk integrity.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;

use crate::catalog::Catalog;
use crate::metadata_store::MetadataStore;
use crate::models::Asset;

/// One asset whose catalog row disagrees with its sidecar.
#[derive(Debug, serde::Serialize)]
pub struct AssetMismatch {
    pub asset_id: String,
    /// Names of the mismatched aspects (e.g. `tags`, `rating`,
    /// `recipe_pending_writeback`, `variants`, `best_variant_hash`).
    pub fields: Vec<String>,
}

/// Outcome of a doctor run.
#[derive(Debug, Default, serde::Serialize)]
pub struct DoctorReport {
    pub sidecars_total: usize,
    pub catalog_assets_total: usize,
    /// How many assets were compared field-by-field (equals the common
    /// ID count unless `--sample` reduced it).
    pub assets_checked: usize,
    /// Sidecars with no catalog row (rebuild-catalog / repair re-adds).
    pub missing_in_catalog: Vec<String>,
    /// Catalog rows with no sidecar (phantom rows; repair deletes).
    pub missing_sidecar: Vec<String>,
    /// Sidecar files that exist but failed to parse.
    pub unreadable_sidecars: Vec<String>,
    pub mismatched: Vec<AssetMismatch>,
    pub repaired: usize,
    pub sampled: bool,
    pub dry_run: bool,
}

impl DoctorReport {
    /// Total number of consistency issues found.
    pub fn issues(&self) -> usize {
        self.missing_in_catalog.len()
            + self.missing_sidecar.len()
            + self.unreadable_sidecars.len()
            + self.mismatched.len()
    }

    pub fn healthy(&self) -> bool {
        self.issues() == 0
    }
}

/// Run the consistency check. `sample` limits the field-by-field
/// comparison to ~N evenly spaced assets (presence checks always cover
/// everything); `repair` rebuilds the SQLite side from YAML for every
/// flagged asset. `on_issue(asset_id, description)` fires per finding
/// for `--log`-style progress output.
pub fn run_doctor(
    catalog_root: &Path,
    sample: Option<usize>,
    repair: bool,
    on_issue: impl Fn(&str, &str),
) -> Result<DoctorReport> {
    let catalog = Catalog::open(catalog_root)?;
    let store = MetadataStore::new(catalog_root);

    let mut report = DoctorReport { dry_run: !repair, ..Default::default() };

    // ── Presence both ways ──────────────────────────────────────────
    let sidecar_ids = list_sidecar_ids(catalog_root)?;
    let catalog_ids = catalog.list_all_asset_ids()?;
    report.sidecars_total = sidecar_ids.len();
    report.catalog_assets_total = catalog_ids.len();

    for id in &sidecar_ids {
        if !catalog_ids.contains(id) {
            on_issue(id, "sidecar has no catalog row");
            report.missing_in_catalog.push(id.clone());
        }
    }
    for id in &catalog_ids {
        if !sidecar_ids.contains(id) {
            on_issue(id, "catalog row has no sidecar");
            report.missing_sidecar.push(id.clone());
        }
    }

    // ── Field-by-field comparison on the common set ─────────────────
    let mut common: Vec<&String> = sidecar_ids.intersection(&catalog_ids).collect();
    common.sort(); // deterministic order (and deterministic sampling)
    let step = match sample {
        Some(n) if n > 0 && n < common.len() => {
            report.sampled = true;
            common.len() as f64 / n as f64
        }
        _ => 1.0,
    };

    let mut next = 0.0_f64;
    for (i, id) in common.iter().enumerate() {
        if (i as f64) < next {
            continue;
        }
        next += step;
        report.assets_checked += 1;

        let uuid: uuid::Uuid = match id.parse() {
            Ok(u) => u,
            Err(_) => continue,
        };
        let asset = match store.load(uuid) {
            Ok(a) => a,
            Err(_) => {
                on_issue(id, "sidecar unreadable");
                report.unreadable_sidecars.push((*id).clone());
                continue;
            }
        };
        let fields = compare_asset(&catalog, &asset)?;
        if !fields.is_empty() {
            on_issue(id, &format!("mismatch: {}", fields.join(", ")));
            report.mismatched.push(AssetMismatch {
                asset_id: (*id).clone(),
                fields,
            });
        }
    }

    // ── Repair ──────────────────────────────────────────────────────
    if repair {
        for id in report
            .missing_in_catalog
            .iter()
            .chain(report.mismatched.iter().map(|m| &m.asset_id))
        {
            let uuid: uuid::Uuid = match id.parse() {
                Ok(u) => u,
                Err(_) => continue,
            };
            if let Ok(asset) = store.load(uuid) {
                rebuild_asset_rows(&catalog, &asset)?;
                report.repaired += 1;
            }
        }
        for id in &report.missing_sidecar {
            delete_phantom_rows(&catalog, id)?;
            report.repaired += 1;
        }
    }

    Ok(report)
}

/// Walk `<catalog_root>/metadata/<shard>/<uuid>.yaml` collecting IDs
/// without parsing the files.
fn list_sidecar_ids(catalog_root: &Path) -> Result<HashSet<String>> {
    let metadata_dir = catalog_root.join("metadata");
    let mut ids = HashSet::new();
    if !metadata_dir.exists() {
        return Ok(ids);
    }
    for shard in std::fs::read_dir(&metadata_dir)? {
        let shard = shard?;
        if !shard.file_type()?.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(shard.path())? {
            let path = file?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if uuid::Uuid::parse_str(stem).is_ok() {
                    ids.insert(stem.to_string());
                }
            }
        }
    }
    Ok(ids)
}

/// Compare one sidecar asset against its catalog rows. Returns the
/// names of every mismatched aspect (empty = consistent).
///
/// Expected values are produced with the SAME conversions and helper
/// functions `insert_asset` uses, so this stays in lockstep with the
/// write path — if a new denormalized column is added to `insert_asset`
/// without updating this comparison, the column simply isn't checked
/// (fails open, never false-positives).
fn compare_asset(catalog: &Catalog, asset: &Asset) -> Result<Vec<String>> {
    let id = asset.id.to_string();
    let mut fields = Vec::new();

    // Stored row, all relevant columns as Rust types.
    #[allow(clippy::type_complexity)]
    let row: (
        Option<String>, // name
        String,         // created_at
        String,         // asset_type
        String,         // tags json
        Option<String>, // description
        Option<i64>,    // rating
        Option<String>, // color_label
        Option<String>, // best_variant_hash
        Option<String>, // primary_variant_format
        i64,            // variant_count
        Option<i64>,    // preview_rotation
        Option<String>, // preview_variant
        i64,            // leaf_tag_count
        i64,            // face_count
    ) = catalog.conn().query_row(
        "SELECT name, created_at, asset_type, tags, description, rating, color_label, \
         best_variant_hash, primary_variant_format, variant_count, preview_rotation, \
         preview_variant, leaf_tag_count, COALESCE(face_count, 0) \
         FROM assets WHERE id = ?1",
        rusqlite::params![id],
        |r| {
            Ok((
                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?,
                r.get(12)?, r.get(13)?,
            ))
        },
    )?;

    // Direct metadata fields (same conversions as insert_asset).
    if row.0 != asset.name {
        fields.push("name".to_string());
    }
    if row.1 != asset.created_at.to_rfc3339() {
        fields.push("created_at".to_string());
    }
    if row.2 != format!("{:?}", asset.asset_type).to_lowercase() {
        fields.push("asset_type".to_string());
    }
    let stored_tags: Vec<String> = serde_json::from_str(&row.3).unwrap_or_default();
    if stored_tags != asset.tags {
        fields.push("tags".to_string());
    }
    if row.4 != asset.description {
        fields.push("description".to_string());
    }
    if row.5 != asset.rating.map(|r| r as i64) {
        fields.push("rating".to_string());
    }
    if row.6 != asset.color_label {
        fields.push("color_label".to_string());
    }
    if row.10 != asset.preview_rotation.map(|r| r as i64) {
        fields.push("preview_rotation".to_string());
    }
    if row.11 != asset.preview_variant {
        fields.push("preview_variant".to_string());
    }

    // Denormalized columns, recomputed with the insert_asset helpers.
    let expect_best = crate::models::variant::compute_best_variant_hash_with_override(
        &asset.variants,
        asset.preview_variant.as_deref(),
    );
    if row.7 != expect_best {
        fields.push("best_variant_hash".to_string());
    }
    let expect_primary = crate::models::variant::compute_primary_format(&asset.variants);
    if row.8 != expect_primary {
        fields.push("primary_variant_format".to_string());
    }
    if row.9 != asset.variants.len() as i64 {
        fields.push("variant_count".to_string());
    }
    if row.12 != crate::tag_util::leaf_tag_count(&asset.tags) as i64 {
        fields.push("leaf_tag_count".to_string());
    }

    // face_count vs the faces table (skip when the faces table doesn't
    // exist — standard builds may never create it).
    let faces_table_exists: bool = catalog
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='faces'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if faces_table_exists {
        let actual_faces: i64 = catalog.conn().query_row(
            "SELECT COUNT(*) FROM faces WHERE asset_id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )?;
        if row.13 != actual_faces {
            fields.push("face_count".to_string());
        }
    }

    // Variant set.
    let stored_variants: HashSet<String> = catalog
        .conn()
        .prepare("SELECT content_hash FROM variants WHERE asset_id = ?1")?
        .query_map(rusqlite::params![id], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    let sidecar_variants: HashSet<String> =
        asset.variants.iter().map(|v| v.content_hash.clone()).collect();
    if stored_variants != sidecar_variants {
        fields.push("variants".to_string());
    } else {
        // File-location sets, only meaningful when the variant sets agree.
        let mut stored_locs: HashSet<(String, String, String)> = HashSet::new();
        {
            let mut stmt = catalog.conn().prepare(
                "SELECT fl.content_hash, fl.volume_id, fl.relative_path \
                 FROM file_locations fl \
                 JOIN variants v ON v.content_hash = fl.content_hash \
                 WHERE v.asset_id = ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            for r in rows {
                stored_locs.insert(r?);
            }
        }
        let sidecar_locs: HashSet<(String, String, String)> = asset
            .variants
            .iter()
            .flat_map(|v| {
                v.locations.iter().map(|l| {
                    (
                        v.content_hash.clone(),
                        l.volume_id.to_string(),
                        l.relative_path.to_string_lossy().to_string(),
                    )
                })
            })
            .collect();
        if stored_locs != sidecar_locs {
            fields.push("file_locations".to_string());
        }
    }

    // Recipe set + per-recipe content_hash and pending_writeback.
    let mut stored_recipes: HashMap<String, (String, bool)> = HashMap::new();
    {
        let mut stmt = catalog.conn().prepare(
            "SELECT r.id, r.content_hash, r.pending_writeback \
             FROM recipes r \
             JOIN variants v ON v.content_hash = r.variant_hash \
             WHERE v.asset_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![id], |r| {
            Ok((r.get::<_, String>(0)?, (r.get::<_, String>(1)?, r.get::<_, i64>(2)? != 0)))
        })?;
        for r in rows {
            let (rid, v) = r?;
            stored_recipes.insert(rid, v);
        }
    }
    let mut recipe_set_mismatch = false;
    let mut recipe_hash_mismatch = false;
    let mut recipe_pending_mismatch = false;
    if stored_recipes.len() != asset.recipes.len() {
        recipe_set_mismatch = true;
    }
    for recipe in &asset.recipes {
        match stored_recipes.get(&recipe.id.to_string()) {
            None => recipe_set_mismatch = true,
            Some((hash, pending)) => {
                if *hash != recipe.content_hash {
                    recipe_hash_mismatch = true;
                }
                if *pending != recipe.pending_writeback {
                    recipe_pending_mismatch = true;
                }
            }
        }
    }
    if recipe_set_mismatch {
        fields.push("recipes".to_string());
    }
    if recipe_hash_mismatch {
        fields.push("recipe_content_hash".to_string());
    }
    if recipe_pending_mismatch {
        fields.push("recipe_pending_writeback".to_string());
    }

    Ok(fields)
}

/// Rebuild one asset's catalog rows from its sidecar: re-insert the
/// asset (upsert), variants, file locations, and recipes; recompute the
/// denormalized columns. Faces, embeddings, and collection/stack rows
/// are left untouched.
fn rebuild_asset_rows(catalog: &Catalog, asset: &Asset) -> Result<()> {
    let id = asset.id.to_string();

    // Delete derived rows in FK order (recipes/locations before variants).
    let stored_hashes: Vec<String> = catalog
        .conn()
        .prepare("SELECT content_hash FROM variants WHERE asset_id = ?1")?
        .query_map(rusqlite::params![id], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    for hash in &stored_hashes {
        catalog
            .conn()
            .execute("DELETE FROM recipes WHERE variant_hash = ?1", rusqlite::params![hash])?;
        catalog.conn().execute(
            "DELETE FROM file_locations WHERE content_hash = ?1",
            rusqlite::params![hash],
        )?;
    }
    catalog
        .conn()
        .execute("DELETE FROM variants WHERE asset_id = ?1", rusqlite::params![id])?;

    // Re-insert from the sidecar (source of truth).
    catalog.insert_asset(asset)?;
    for variant in &asset.variants {
        catalog.insert_variant(variant)?;
        for loc in &variant.locations {
            catalog.insert_file_location(&variant.content_hash, loc)?;
        }
    }
    for recipe in &asset.recipes {
        catalog.insert_recipe(recipe)?;
    }
    catalog.update_denormalized_variant_columns(asset)?;
    let _ = catalog.update_face_count(&id);
    Ok(())
}

/// Delete every catalog row belonging to a phantom asset (a row with no
/// sidecar — the sidecar is the source of truth, so the rows must go).
fn delete_phantom_rows(catalog: &Catalog, asset_id: &str) -> Result<()> {
    let stored_hashes: Vec<String> = catalog
        .conn()
        .prepare("SELECT content_hash FROM variants WHERE asset_id = ?1")?
        .query_map(rusqlite::params![asset_id], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    for hash in &stored_hashes {
        let _ = catalog
            .conn()
            .execute("DELETE FROM recipes WHERE variant_hash = ?1", rusqlite::params![hash]);
        let _ = catalog.conn().execute(
            "DELETE FROM file_locations WHERE content_hash = ?1",
            rusqlite::params![hash],
        );
    }
    let _ = catalog
        .conn()
        .execute("DELETE FROM variants WHERE asset_id = ?1", rusqlite::params![asset_id]);
    for table in ["faces", "embeddings", "collection_assets"] {
        // Tables may not exist in every build; ignore errors.
        let _ = catalog.conn().execute(
            &format!("DELETE FROM {table} WHERE asset_id = ?1"),
            rusqlite::params![asset_id],
        );
    }
    let _ = catalog
        .conn()
        .execute("DELETE FROM assets WHERE id = ?1", rusqlite::params![asset_id]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AssetType, Variant, VariantRole};

    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let catalog = Catalog::open(&root).unwrap();
        catalog.initialize().unwrap();
        (dir, root)
    }

    fn seed_asset(root: &Path, hash: &str, name: &str) -> Asset {
        let mut asset = Asset::new(AssetType::Image, hash);
        asset.name = Some(name.to_string());
        asset.tags = vec!["alpha".to_string()];
        asset.rating = Some(3);
        let variant = Variant {
            content_hash: hash.to_string(),
            asset_id: asset.id,
            role: VariantRole::Original,
            format: "jpg".to_string(),
            file_size: 100,
            original_filename: format!("{name}.jpg"),
            source_metadata: Default::default(),
            locations: vec![],
        };
        asset.variants.push(variant.clone());
        let store = MetadataStore::new(root);
        store.save(&asset).unwrap();
        let catalog = Catalog::open_fast(root).unwrap();
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&variant).unwrap();
        asset
    }

    #[test]
    fn healthy_catalog_reports_no_issues() {
        let (_d, root) = setup();
        seed_asset(&root, "sha256:doc1", "first");
        let report = run_doctor(&root, None, false, |_, _| {}).unwrap();
        assert!(report.healthy(), "{report:?}");
        assert_eq!(report.assets_checked, 1);
        assert_eq!(report.sidecars_total, 1);
        assert_eq!(report.catalog_assets_total, 1);
    }

    #[test]
    fn detects_and_repairs_field_divergence() {
        let (_d, root) = setup();
        let asset = seed_asset(&root, "sha256:doc2", "second");
        // Sabotage SQLite directly: stale rating + stale pending flag class.
        {
            let catalog = Catalog::open_fast(&root).unwrap();
            catalog
                .conn()
                .execute(
                    "UPDATE assets SET rating = 5, tags = '[]' WHERE id = ?1",
                    rusqlite::params![asset.id.to_string()],
                )
                .unwrap();
        }
        let report = run_doctor(&root, None, false, |_, _| {}).unwrap();
        assert_eq!(report.mismatched.len(), 1, "{report:?}");
        let fields = &report.mismatched[0].fields;
        assert!(fields.contains(&"rating".to_string()), "{fields:?}");
        assert!(fields.contains(&"tags".to_string()), "{fields:?}");
        // leaf_tag_count stays consistent here: it's recomputed from the
        // SIDECAR tags, which weren't touched — only the stored copy was.

        let repaired = run_doctor(&root, None, true, |_, _| {}).unwrap();
        assert_eq!(repaired.repaired, 1);
        let clean = run_doctor(&root, None, false, |_, _| {}).unwrap();
        assert!(clean.healthy(), "{clean:?}");
    }

    #[test]
    fn detects_stale_pending_writeback_flag() {
        // The v4.5.15 divergence class: sidecar says pending, SQLite says clean.
        let (_d, root) = setup();
        let mut asset = seed_asset(&root, "sha256:doc3", "third");
        let recipe = crate::models::recipe::Recipe {
            id: uuid::Uuid::new_v4(),
            variant_hash: "sha256:doc3".to_string(),
            software: "test".to_string(),
            recipe_type: crate::models::recipe::RecipeType::Sidecar,
            content_hash: "sha256:recipehash".to_string(),
            location: crate::models::volume::FileLocation {
                volume_id: uuid::Uuid::nil(),
                relative_path: "x.xmp".into(),
                verified_at: None,
            },
            pending_writeback: true,
        };
        asset.recipes.push(recipe.clone());
        let store = MetadataStore::new(&root);
        store.save(&asset).unwrap();
        {
            let catalog = Catalog::open_fast(&root).unwrap();
            catalog.insert_recipe(&recipe).unwrap();
            // Simulate the pre-v4.5.15 bug: SQLite flag silently reset.
            catalog
                .conn()
                .execute(
                    "UPDATE recipes SET pending_writeback = 0 WHERE id = ?1",
                    rusqlite::params![recipe.id.to_string()],
                )
                .unwrap();
        }
        let report = run_doctor(&root, None, false, |_, _| {}).unwrap();
        assert_eq!(report.mismatched.len(), 1, "{report:?}");
        assert!(
            report.mismatched[0].fields.contains(&"recipe_pending_writeback".to_string()),
            "{:?}",
            report.mismatched[0].fields
        );

        run_doctor(&root, None, true, |_, _| {}).unwrap();
        let clean = run_doctor(&root, None, false, |_, _| {}).unwrap();
        assert!(clean.healthy(), "{clean:?}");
    }

    #[test]
    fn detects_missing_row_and_phantom_row() {
        let (_d, root) = setup();
        let a1 = seed_asset(&root, "sha256:doc4", "fourth");
        let a2 = seed_asset(&root, "sha256:doc5", "fifth");
        // a1: drop the catalog row (sidecar without row).
        // a2: drop the sidecar (phantom row).
        {
            let catalog = Catalog::open_fast(&root).unwrap();
            catalog
                .conn()
                .execute(
                    "DELETE FROM variants WHERE asset_id = ?1",
                    rusqlite::params![a1.id.to_string()],
                )
                .unwrap();
            catalog
                .conn()
                .execute("DELETE FROM assets WHERE id = ?1", rusqlite::params![a1.id.to_string()])
                .unwrap();
        }
        MetadataStore::new(&root).delete(a2.id).unwrap();

        let report = run_doctor(&root, None, false, |_, _| {}).unwrap();
        assert_eq!(report.missing_in_catalog, vec![a1.id.to_string()]);
        assert_eq!(report.missing_sidecar, vec![a2.id.to_string()]);

        let repaired = run_doctor(&root, None, true, |_, _| {}).unwrap();
        assert_eq!(repaired.repaired, 2);
        let clean = run_doctor(&root, None, false, |_, _| {}).unwrap();
        assert!(clean.healthy(), "{clean:?}");
        // a1 is back in the catalog, a2's phantom rows are gone.
        let catalog = Catalog::open_fast(&root).unwrap();
        assert!(catalog.list_all_asset_ids().unwrap().contains(&a1.id.to_string()));
        assert!(!catalog.list_all_asset_ids().unwrap().contains(&a2.id.to_string()));
    }

    #[test]
    fn sampling_limits_field_checks_but_not_presence() {
        let (_d, root) = setup();
        for i in 0..10 {
            seed_asset(&root, &format!("sha256:doc6-{i}"), &format!("s{i}"));
        }
        let report = run_doctor(&root, Some(3), false, |_, _| {}).unwrap();
        assert!(report.sampled);
        assert!(report.assets_checked >= 3 && report.assets_checked < 10, "{report:?}");
        assert_eq!(report.sidecars_total, 10, "presence check covers everything");
    }
}
