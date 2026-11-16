//! `lookup` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.

use super::*;

impl Catalog {
    // ═══ ASSET LOOKUPS ═══

    /// Search assets by optional filters. Results join assets with variants.
    pub fn search_assets(
        &self,
        text: Option<&str>,
        asset_type: Option<&str>,
        tag: Option<&str>,
        format: Option<&str>,
        rating_min: Option<u8>,
        rating_exact: Option<u8>,
    ) -> Result<Vec<SearchRow>> {
        let asset_types_vec;
        let tags_vec;
        let formats_vec;
        let opts = SearchOptions {
            text,
            asset_types: if let Some(t) = asset_type {
                asset_types_vec = vec![t.to_string()];
                &asset_types_vec
            } else {
                &[]
            },
            tags: if let Some(t) = tag {
                tags_vec = vec![t.to_string()];
                &tags_vec
            } else {
                &[]
            },
            formats: if let Some(f) = format {
                formats_vec = vec![f.to_string()];
                &formats_vec
            } else {
                &[]
            },
            rating: if let Some(min) = rating_min {
                Some(NumericFilter::Min(min as f64))
            } else { rating_exact.map(|exact| NumericFilter::Exact(exact as f64)) },
            per_page: u32::MAX,
            ..Default::default()
        };
        self.search_paginated(&opts)
    }

    /// Resolve a short asset ID prefix to a full UUID string.
    ///
    /// Returns `Ok(Some(id))` if exactly one match, `Ok(None)` if no match,
    /// or an error if the prefix is ambiguous (multiple matches).
    pub fn resolve_asset_id(&self, prefix: &str) -> Result<Option<String>> {
        let prefix = prefix.trim();
        let pattern = format!("{prefix}%");
        let mut stmt = self.conn.prepare(
            "SELECT id FROM assets WHERE id LIKE ?1",
        )?;
        let mut ids: Vec<String> = stmt
            .query_map(rusqlite::params![pattern], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?;

        match ids.len() {
            0 => Ok(None),
            1 => Ok(ids.pop()),
            n => anyhow::bail!(
                "Ambiguous asset ID prefix '{prefix}': matches {n} assets"
            ),
        }
    }

    /// [`Self::resolve_asset_id`] for callers that need the asset to exist:
    /// `no asset found matching '<prefix>'` when nothing matches (the
    /// ambiguous-prefix error passes through unchanged).
    pub fn resolve_asset_id_required(&self, prefix: &str) -> Result<String> {
        self.resolve_asset_id(prefix)?
            .ok_or_else(|| crate::error::MakiError::not_found("asset", prefix).into())
    }

    /// Load full asset details from the catalog (variants + locations).
    pub fn load_asset_details(&self, asset_id: &str) -> Result<Option<AssetDetails>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, asset_type, created_at, tags, description, rating, color_label, \
             COALESCE(tag_sources, '{}'), best_variant_hash \
             FROM assets WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![asset_id])?;
        let row = match rows.next()? {
            Some(r) => r,
            None => return Ok(None),
        };

        let id: String = row.get(0)?;
        let name: Option<String> = row.get(1)?;
        let asset_type: String = row.get(2)?;
        let created_at: String = row.get(3)?;
        let tags_json: String = row.get(4)?;
        let description: Option<String> = row.get(5)?;
        let rating_val: Option<i64> = row.get(6)?;
        let rating = rating_val.map(|r| r as u8);
        let color_label: Option<String> = row.get(7)?;
        let tag_sources_json: String = row.get(8)?;
        let best_variant_hash: Option<String> = row.get(9)?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        let tag_sources: std::collections::BTreeMap<String, crate::models::TagSource> =
            serde_json::from_str(&tag_sources_json).unwrap_or_default();

        // Load variants
        let mut vstmt = self.conn.prepare(
            "SELECT content_hash, role, format, file_size, original_filename, source_metadata \
             FROM variants WHERE asset_id = ?1",
        )?;
        let variants: Vec<VariantDetails> = vstmt
            .query_map(rusqlite::params![asset_id], |vrow| {
                let meta_json: String = vrow.get(5)?;
                let source_metadata: std::collections::HashMap<String, String> =
                    serde_json::from_str(&meta_json).unwrap_or_default();
                Ok(VariantDetails {
                    content_hash: vrow.get(0)?,
                    role: vrow.get(1)?,
                    format: vrow.get(2)?,
                    file_size: vrow.get(3)?,
                    original_filename: vrow.get(4)?,
                    source_metadata,
                    locations: Vec::new(), // filled below
                })
            })?
            .collect::<std::result::Result<_, _>>()?;

        // Load locations for each variant
        let mut lstmt = self.conn.prepare(
            "SELECT fl.relative_path, vol.label, vol.id, vol.purpose, fl.verified_at \
             FROM file_locations fl \
             JOIN volumes vol ON fl.volume_id = vol.id \
             WHERE fl.content_hash = ?1",
        )?;

        let mut variants = variants;
        for v in &mut variants {
            v.locations = Self::load_locations_for_hash_checked(&mut lstmt, &v.content_hash)?;
        }

        // Load recipes linked to any variant of this asset
        let mut rstmt = self.conn.prepare(
            "SELECT r.variant_hash, r.software, r.recipe_type, r.content_hash, r.volume_id, \
                    vol.label, r.relative_path, r.pending_writeback \
             FROM recipes r \
             JOIN variants v ON r.variant_hash = v.content_hash \
             LEFT JOIN volumes vol ON r.volume_id = vol.id \
             WHERE v.asset_id = ?1",
        )?;
        let recipes: Vec<RecipeDetails> = rstmt
            .query_map(rusqlite::params![asset_id], |rrow| {
                Ok(RecipeDetails {
                    variant_hash: rrow.get(0)?,
                    software: rrow.get(1)?,
                    recipe_type: rrow.get(2)?,
                    content_hash: rrow.get(3)?,
                    volume_id: rrow.get(4)?,
                    volume_label: rrow.get(5)?,
                    relative_path: rrow.get(6)?,
                    pending_writeback: rrow.get::<_, i32>(7).unwrap_or(0) != 0,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;

        Ok(Some(AssetDetails {
            id,
            name,
            asset_type,
            created_at,
            tags,
            tag_sources,
            description,
            rating,
            color_label,
            variants,
            recipes,
            best_variant_hash,
        }))
    }

    /// Find which asset owns a variant by its content hash.
    pub fn find_asset_id_by_variant(&self, content_hash: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT asset_id FROM variants WHERE content_hash = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![content_hash])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Get an asset's name by ID.
    pub fn get_asset_name(&self, asset_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(a.name, bv.original_filename) FROM assets a \
             LEFT JOIN variants bv ON bv.content_hash = a.best_variant_hash \
             WHERE a.id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![asset_id])?;
        match rows.next()? {
            Some(row) => Ok(row.get(0)?),
            None => Ok(None),
        }
    }

    /// Get an asset's best_variant_hash by ID.
    pub fn get_asset_best_variant_hash(&self, asset_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT best_variant_hash FROM assets WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![asset_id])?;
        match rows.next()? {
            Some(row) => Ok(row.get(0)?),
            None => Ok(None),
        }
    }

    /// Reassign a variant to a different asset in the catalog.
    pub fn update_variant_asset_id(&self, content_hash: &str, new_asset_id: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE variants SET asset_id = ?1 WHERE content_hash = ?2",
            rusqlite::params![new_asset_id, content_hash],
        )?;
        if changed == 0 {
            anyhow::bail!("no variant found with hash '{content_hash}'");
        }
        Ok(())
    }

    /// Update a variant's role in the catalog.
    pub fn update_variant_role(&self, content_hash: &str, role: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE variants SET role = ?1 WHERE content_hash = ?2",
            rusqlite::params![role, content_hash],
        )?;
        Ok(())
    }

    /// Delete an asset row from the catalog.
    pub fn delete_asset(&self, asset_id: &str) -> Result<()> {
        let changed = self.conn.execute(
            "DELETE FROM assets WHERE id = ?1",
            rusqlite::params![asset_id],
        )?;
        if changed == 0 {
            anyhow::bail!("no asset found with id '{asset_id}'");
        }
        Ok(())
    }

    /// Load enriched location details for a variant hash. `lstmt` is the
    /// prepared `SELECT fl.relative_path, vol.label, vol.id, vol.purpose,
    /// fl.verified_at … WHERE fl.content_hash = ?1` statement. Query and
    /// row-decoding errors propagate — a malformed row is a catalog
    /// problem to report, not a location to drop silently.
    pub(super) fn load_locations_for_hash_checked(
        lstmt: &mut rusqlite::Statement,
        content_hash: &str,
    ) -> Result<Vec<LocationDetails>> {
        let locs = lstmt
            .query_map(rusqlite::params![content_hash], |lrow| {
                Ok(LocationDetails {
                    relative_path: lrow.get(0)?,
                    volume_label: lrow.get(1)?,
                    volume_id: lrow.get(2)?,
                    volume_purpose: lrow.get(3)?,
                    verified_at: lrow.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(locs)
    }

    /// Compute `volume_count` and `same_volume_groups` from locations.
    pub(super) fn compute_duplicate_stats(entry: &mut DuplicateEntry) {
        let mut vol_counts: HashMap<String, usize> = HashMap::new();
        for loc in &entry.locations {
            *vol_counts.entry(loc.volume_id.clone()).or_insert(0) += 1;
        }
        entry.volume_count = vol_counts.len();
        // Find volume labels where the same volume has 2+ locations
        let mut same_vol: Vec<String> = Vec::new();
        for loc in &entry.locations {
            let count = vol_counts.get(&loc.volume_id).copied().unwrap_or(0);
            if count > 1 && !same_vol.contains(&loc.volume_label) {
                same_vol.push(loc.volume_label.clone());
            }
        }
        entry.same_volume_groups = same_vol;
    }

    /// Load duplicate entries from a variant query and enrich with locations.
    pub(super) fn load_duplicate_entries(
        &self,
        variant_query: &str,
    ) -> Result<Vec<DuplicateEntry>> {
        self.load_duplicate_entries_filtered(variant_query, &[])
    }
}

#[cfg(test)]
mod tests {
    use crate::catalog::test_support::*;
    use crate::catalog::*;

    #[test]
    fn search_by_text() {
        let catalog = setup_search_catalog();
        let results = catalog.search_assets(Some("sunset"), None, None, None, None, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name.as_deref(), Some("sunset photo"));
    }

    #[test]
    fn search_by_type() {
        let catalog = setup_search_catalog();
        let results = catalog.search_assets(None, Some("video"), None, None, None, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].format, "mp4");
    }

    #[test]
    fn search_by_tag() {
        let catalog = setup_search_catalog();
        let results = catalog.search_assets(None, None, Some("landscape"), None, None, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name.as_deref(), Some("sunset photo"));
    }

    #[test]
    fn search_tag_whole_path_match() {
        // `=` prefix means "match the tag path exactly and only". Distinguishes
        // a root-level tag from same-named leaves elsewhere in the hierarchy —
        // the case that the default level-sliding match and `/` (leaf-only at
        // any level) can't disambiguate.
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        // Asset 1: root-level Legoland only
        let mut a1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:wp1");
        a1.tags = vec!["Legoland".to_string()];
        let v1 = crate::models::Variant { file_size: 100, ..test_variant(a1.id, "sha256:wp1", "a.jpg") };
        a1.variants.push(v1.clone());
        catalog.insert_asset(&a1).unwrap();
        catalog.insert_variant(&v1).unwrap();

        // Asset 2: Legoland as a leaf under location|Denmark
        let mut a2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:wp2");
        a2.tags = vec!["location|Denmark|Legoland".to_string()];
        let v2 = crate::models::Variant { file_size: 100, ..test_variant(a2.id, "sha256:wp2", "b.jpg") };
        a2.variants.push(v2.clone());
        catalog.insert_asset(&a2).unwrap();
        catalog.insert_variant(&v2).unwrap();

        // Asset 3: Legoland under a different country
        let mut a3 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:wp3");
        a3.tags = vec!["location|Germany|Legoland".to_string()];
        let v3 = crate::models::Variant { file_size: 100, ..test_variant(a3.id, "sha256:wp3", "c.jpg") };
        a3.variants.push(v3.clone());
        catalog.insert_asset(&a3).unwrap();
        catalog.insert_variant(&v3).unwrap();

        // Baseline: without any marker, all three match (Legoland appears at
        // some hierarchy level in each).
        let results = catalog.search_assets(None, None, Some("Legoland"), None, None, None).unwrap();
        assert_eq!(results.len(), 3, "plain tag:Legoland matches at any level");

        // `/` leaf-only-at-any-level: still matches all three (each Legoland
        // is a leaf in its own branch — none has a Legoland|child).
        let results = catalog.search_assets(None, None, Some("/Legoland"), None, None, None).unwrap();
        assert_eq!(results.len(), 3, "tag:/Legoland matches all leaf occurrences");

        // `=` whole-path: matches only the root-level Legoland.
        let results = catalog.search_assets(None, None, Some("=Legoland"), None, None, None).unwrap();
        assert_eq!(results.len(), 1, "tag:=Legoland matches only standalone root-level tag");
        assert_eq!(results[0].content_hash, "sha256:wp1");

        // Whole-path match at depth: selects exactly one asset.
        let results = catalog.search_assets(None, None, Some("=location|Denmark|Legoland"), None, None, None).unwrap();
        assert_eq!(results.len(), 1, "tag:=location|Denmark|Legoland matches that exact path");
        assert_eq!(results[0].content_hash, "sha256:wp2");

        // Whole-path match with a prefix-of-a-path: no results (there's no
        // asset where the full tag is 'location|Denmark').
        let results = catalog.search_assets(None, None, Some("=location|Denmark"), None, None, None).unwrap();
        assert!(results.is_empty(), "tag:=location|Denmark matches no full paths exactly");

        // Whole-path match for a non-existent path: no results.
        let results = catalog.search_assets(None, None, Some("=nosuchtag"), None, None, None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_by_tag_hierarchical() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:hier1");
        asset.tags = vec!["animals|birds|eagles".to_string(), "sunset".to_string()];
        let variant = test_variant(asset.id, "sha256:hier1", "eagle.jpg");
        asset.variants.push(variant.clone());
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&variant).unwrap();

        // Search for parent tag should find child
        let results = catalog.search_assets(None, None, Some("animals"), None, None, None).unwrap();
        assert_eq!(results.len(), 1, "tag:animals should match animals|birds|eagles");

        // Search for intermediate tag (user types `|` or `>` for hierarchy)
        let results = catalog.search_assets(None, None, Some("animals|birds"), None, None, None).unwrap();
        assert_eq!(results.len(), 1, "tag:animals|birds should match animals|birds|eagles");

        // Search for exact tag
        let results = catalog.search_assets(None, None, Some("animals|birds|eagles"), None, None, None).unwrap();
        assert_eq!(results.len(), 1, "tag:animals|birds|eagles should match exactly");

        // Search for leaf component by name (child matching)
        let results = catalog.search_assets(None, None, Some("eagles"), None, None, None).unwrap();
        assert_eq!(results.len(), 1, "tag:eagles should match animals|birds|eagles");

        // Search for mid-path component by name
        let results = catalog.search_assets(None, None, Some("birds"), None, None, None).unwrap();
        assert_eq!(results.len(), 1, "tag:birds should match animals|birds|eagles");

        // Partial name must NOT match (no substring matching)
        let results = catalog.search_assets(None, None, Some("eagle"), None, None, None).unwrap();
        assert!(results.is_empty(), "tag:eagle should NOT match eagles (no substring)");

        // Search for non-matching parent
        let results = catalog.search_assets(None, None, Some("cats"), None, None, None).unwrap();
        assert!(results.is_empty(), "tag:cats should not match");

        // Search for child when only parent is tagged — should NOT match
        let mut asset2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:hier2");
        asset2.tags = vec!["animals".to_string()];
        let variant2 = test_variant(asset2.id, "sha256:hier2", "cat.jpg");
        asset2.variants.push(variant2.clone());
        catalog.insert_asset(&asset2).unwrap();
        catalog.insert_variant(&variant2).unwrap();

        let results = catalog.search_assets(None, None, Some("animals|birds"), None, None, None).unwrap();
        assert_eq!(results.len(), 1, "tag:animals|birds should not match plain 'animals'");

        // Search tag:animals should match both
        let results = catalog.search_assets(None, None, Some("animals"), None, None, None).unwrap();
        assert_eq!(results.len(), 2, "tag:animals should match both 'animals' and 'animals|birds|eagles'");
    }

    #[test]
    fn search_by_tag_literal_slash() {
        // Tags like "AF Nikkor 85mm f/1.4 D" contain literal `/` — not hierarchy.
        // Searching `tag:AF Nikkor 85mm f/1.4` should match via the raw fallback.
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:slash1");
        asset.tags = vec!["AF Nikkor 85mm f/1.4 D".to_string()];
        let variant = test_variant(asset.id, "sha256:slash1", "portrait.jpg");
        asset.variants.push(variant.clone());
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&variant).unwrap();

        // Exact search should match via raw fallback (since `/` gets converted to `|`)
        let results = catalog
            .search_assets(None, None, Some("AF Nikkor 85mm f/1.4 D"), None, None, None)
            .unwrap();
        assert_eq!(results.len(), 1, "tag with literal slash should be found");

        // Non-matching tag should not match
        let results = catalog
            .search_assets(None, None, Some("AF Nikkor 85mm f/2.8"), None, None, None)
            .unwrap();
        assert!(results.is_empty(), "wrong tag should not match");
    }

    #[test]
    fn search_by_tag_no_false_positive_prefix() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:fp1");
        asset.tags = vec!["other-animals".to_string()];
        let variant = test_variant(asset.id, "sha256:fp1", "other.jpg");
        asset.variants.push(variant.clone());
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&variant).unwrap();

        // "animals" should NOT match "other-animals"
        let results = catalog.search_assets(None, None, Some("animals"), None, None, None).unwrap();
        assert!(results.is_empty(), "tag:animals should not match other-animals");
    }

    #[test]
    fn details_best_variant_index_reads_stored_hash_and_override() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();
        let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:bvi-raw");
        let raw = crate::models::Variant { file_size: 50_000_000, ..test_variant(asset.id, "sha256:bvi-raw", "x.nef") };
        let jpg = crate::models::Variant {
            content_hash: "sha256:bvi-jpg".to_string(),
            role: crate::models::VariantRole::Alternate,
            format: "jpg".to_string(),
            file_size: 400_000,
            original_filename: "x.jpg".to_string(),
            ..raw.clone()
        };
        asset.variants = vec![raw.clone(), jpg.clone()];
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&raw).unwrap();
        catalog.insert_variant(&jpg).unwrap();

        // Rule: the developed JPEG represents the asset.
        let d = catalog.load_asset_details(&asset.id.to_string()).unwrap().unwrap();
        assert_eq!(d.best_variant_hash.as_deref(), Some("sha256:bvi-jpg"));
        let i = d.best_variant_index().unwrap();
        assert_eq!(d.variants[i].content_hash, "sha256:bvi-jpg");

        // User override to the RAW: stored column follows it, so does the accessor.
        asset.preview_variant = Some("sha256:bvi-raw".to_string());
        catalog.insert_asset(&asset).unwrap();
        let d = catalog.load_asset_details(&asset.id.to_string()).unwrap().unwrap();
        let i = d.best_variant_index().unwrap();
        assert_eq!(d.variants[i].content_hash, "sha256:bvi-raw");

        // Stored hash that no longer belongs to the asset: fall back to the rule.
        catalog.conn.execute(
            "UPDATE assets SET best_variant_hash = 'sha256:gone' WHERE id = ?1",
            [asset.id.to_string()],
        ).unwrap();
        let d = catalog.load_asset_details(&asset.id.to_string()).unwrap().unwrap();
        let i = d.best_variant_index().unwrap();
        assert_eq!(d.variants[i].content_hash, "sha256:bvi-jpg");
    }

    #[test]
    fn search_tag_with_double_quotes_unescaped() {
        // Tags stored as raw JSON (unescaped quotes): ["\"Sir\" Oliver Mally"]
        // This is how serde_json serializes tags with quotes
        let catalog = catalog_with_tags(
            vec!["\"Sir\" Oliver Mally".to_string()],
            "quotes1",
        );
        let results = catalog.search_assets(None, None, Some("\"Sir\" Oliver Mally"), None, None, None).unwrap();
        assert_eq!(results.len(), 1, "Should find tag with double quotes");
    }

    #[test]
    fn search_tag_with_double_quotes_no_false_positive() {
        let catalog = catalog_with_tags(
            vec!["Sir Oliver Mally".to_string()],
            "quotes2",
        );
        // Searching for the quoted version should NOT match the unquoted tag
        let results = catalog.search_assets(None, None, Some("\"Sir\" Oliver Mally"), None, None, None).unwrap();
        assert!(results.is_empty(), "Quoted search should not match unquoted tag");
    }

    #[test]
    fn search_tag_with_apostrophe() {
        let catalog = catalog_with_tags(
            vec!["it's a test".to_string()],
            "apos1",
        );
        let results = catalog.search_assets(None, None, Some("it's a test"), None, None, None).unwrap();
        assert_eq!(results.len(), 1, "Should find tag with apostrophe");
    }

    #[test]
    fn search_tag_with_ampersand() {
        let catalog = catalog_with_tags(
            vec!["rock & roll".to_string()],
            "amp1",
        );
        let results = catalog.search_assets(None, None, Some("rock & roll"), None, None, None).unwrap();
        assert_eq!(results.len(), 1, "Should find tag with ampersand");
    }

    #[test]
    fn search_by_format() {
        let catalog = setup_search_catalog();
        let results = catalog.search_assets(None, None, None, Some("jpg"), None, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "sunset_beach.jpg");
    }

    #[test]
    fn search_no_results() {
        let catalog = setup_search_catalog();
        let results = catalog.search_assets(Some("nonexistent"), None, None, None, None, None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_combined_filters() {
        let catalog = setup_search_catalog();
        let results = catalog
            .search_assets(Some("sunset"), Some("image"), Some("landscape"), Some("jpg"), None, None)
            .unwrap();
        assert_eq!(results.len(), 1);
        // Combining mismatched filters yields nothing
        let results = catalog
            .search_assets(Some("sunset"), Some("video"), None, None, None, None)
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn resolve_asset_id_full_match() {
        let catalog = setup_search_catalog();
        let results = catalog.search_assets(None, None, None, None, None, None).unwrap();
        let full_id = &results[0].asset_id;
        let resolved = catalog.resolve_asset_id(full_id).unwrap();
        assert_eq!(resolved.as_deref(), Some(full_id.as_str()));
    }

    #[test]
    fn resolve_asset_id_prefix_match() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:test3");
        let full_id = asset.id.to_string();
        catalog.insert_asset(&asset).unwrap();

        let prefix = &full_id[..8];
        let resolved = catalog.resolve_asset_id(prefix).unwrap();
        assert_eq!(resolved.as_deref(), Some(full_id.as_str()));
    }

    #[test]
    fn resolve_asset_id_no_match() {
        let catalog = setup_search_catalog();
        let resolved = catalog.resolve_asset_id("zzzzzzzz").unwrap();
        assert!(resolved.is_none());
    }

    #[test]
    fn resolve_asset_id_ambiguous() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        // Insert two assets and use an empty prefix to match both
        let a1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:ambig1");
        let a2 = crate::models::Asset::new(crate::models::AssetType::Video, "sha256:ambig2");
        catalog.insert_asset(&a1).unwrap();
        catalog.insert_asset(&a2).unwrap();

        let result = catalog.resolve_asset_id("");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Ambiguous"), "expected ambiguous error, got: {msg}");
    }

    #[test]
    fn load_asset_details_returns_none_for_missing() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();
        let details = catalog.load_asset_details("nonexistent-id").unwrap();
        assert!(details.is_none());
    }

    #[test]
    fn load_asset_details_returns_full_info() {
        let catalog = setup_search_catalog();
        let results = catalog.search_assets(Some("sunset"), None, None, None, None, None).unwrap();
        let asset_id = &results[0].asset_id;

        let details = catalog.load_asset_details(asset_id).unwrap().unwrap();
        assert_eq!(details.id, *asset_id);
        assert_eq!(details.name.as_deref(), Some("sunset photo"));
        assert_eq!(details.asset_type, "image");
        assert_eq!(details.tags, vec!["landscape", "nature"]);
        assert_eq!(details.description.as_deref(), Some("A beautiful sunset over the ocean"));
        assert_eq!(details.variants.len(), 1);
        assert_eq!(details.variants[0].role, "original");
        assert_eq!(details.variants[0].format, "jpg");
        assert_eq!(details.variants[0].file_size, 5000);
        assert_eq!(details.variants[0].original_filename, "sunset_beach.jpg");
    }

    #[test]
    fn load_asset_details_includes_locations() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:loc1");
        asset.name = Some("located asset".to_string());
        catalog.insert_asset(&asset).unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".to_string(),
            std::path::PathBuf::from("/mnt/test"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();

        let variant = crate::models::Variant { file_size: 2048, ..test_variant(asset.id, "sha256:loc1", "photo.png") };
        catalog.insert_variant(&variant).unwrap();

        let loc = crate::models::FileLocation {
            volume_id: volume.id,
            relative_path: std::path::PathBuf::from("photos/photo.png"),
            verified_at: None,
        };
        catalog.insert_file_location(&variant.content_hash, &loc).unwrap();

        let details = catalog.load_asset_details(&asset.id.to_string()).unwrap().unwrap();
        assert_eq!(details.variants.len(), 1);
        assert_eq!(details.variants[0].locations.len(), 1);
        assert_eq!(details.variants[0].locations[0].volume_label, "test-vol");
        assert_eq!(details.variants[0].locations[0].relative_path, "photos/photo.png");
    }

    #[test]
    fn find_asset_id_by_variant_returns_correct_asset() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:findme");
        let asset_id = asset.id.to_string();
        catalog.insert_asset(&asset).unwrap();

        let variant = test_variant(asset.id, "sha256:findme", "test.jpg");
        catalog.insert_variant(&variant).unwrap();

        let found = catalog.find_asset_id_by_variant("sha256:findme").unwrap();
        assert_eq!(found, Some(asset_id));

        let missing = catalog.find_asset_id_by_variant("sha256:nope").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn delete_asset_removes_row() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:delete1");
        let asset_id = asset.id.to_string();
        catalog.insert_asset(&asset).unwrap();

        catalog.delete_asset(&asset_id).unwrap();

        let count: i64 = catalog
            .conn
            .query_row("SELECT COUNT(*) FROM assets WHERE id = ?1", rusqlite::params![asset_id], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn delete_asset_errors_on_missing() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();
        assert!(catalog.delete_asset("nonexistent").is_err());
    }

    #[test]
    fn update_variant_asset_id_changes_fk() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let asset1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:move1");
        let asset2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:move2");
        catalog.insert_asset(&asset1).unwrap();
        catalog.insert_asset(&asset2).unwrap();

        let variant = crate::models::Variant { file_size: 500, ..test_variant(asset1.id, "sha256:moveme", "move.jpg") };
        catalog.insert_variant(&variant).unwrap();

        catalog
            .update_variant_asset_id("sha256:moveme", &asset2.id.to_string())
            .unwrap();

        let new_owner = catalog.find_asset_id_by_variant("sha256:moveme").unwrap();
        assert_eq!(new_owner, Some(asset2.id.to_string()));
    }
}
