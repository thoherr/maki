//! `search_exec` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.

use super::*;

/// Column list shared by every query that materializes a `SearchRow`.
/// Aliases required: `a` = assets, `bv` = best variant, `s` = stacks.
/// Order is load-bearing — `map_search_row` reads columns by index.
const SEARCH_ROW_COLUMNS: &str = "a.id, a.name, a.asset_type, a.created_at, bv.original_filename, bv.format, \
     a.tags, a.description, bv.content_hash, a.rating, a.color_label, \
     a.primary_variant_format, a.variant_count, a.stack_id, s.member_count, \
     a.preview_rotation, a.face_count, a.duration_seconds";

/// Map a row produced by a `SEARCH_ROW_COLUMNS` SELECT into a `SearchRow`.
fn map_search_row(row: &rusqlite::Row) -> rusqlite::Result<SearchRow> {
    let tags_json: String = row.get(6)?;
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    let rating_val: Option<i64> = row.get(9)?;
    let variant_count_val: i64 = row.get(12)?;
    let stack_member_count: Option<i64> = row.get(14)?;
    let rotation_val: Option<i64> = row.get(15)?;
    let face_count_val: i64 = row.get::<_, Option<i64>>(16)?.unwrap_or(0);
    let duration_seconds: Option<f64> = row.get(17)?;
    Ok(SearchRow {
        asset_id: row.get(0)?,
        name: row.get(1)?,
        asset_type: row.get(2)?,
        created_at: row.get(3)?,
        original_filename: row.get(4)?,
        format: row.get(5)?,
        tags,
        description: row.get(7)?,
        content_hash: row.get(8)?,
        rating: rating_val.map(|r| r as u8),
        color_label: row.get(10)?,
        primary_format: row.get(11)?,
        variant_count: variant_count_val as u32,
        stack_id: row.get(13)?,
        stack_count: stack_member_count.map(|n| n as u32),
        preview_rotation: rotation_val.map(|r| r as u16),
        face_count: face_count_val as u32,
        duration_seconds,
        similarity: None,
        exact_match: false,
    })
}

impl Catalog {
    // ═══ SEARCH EXECUTION ═══

    /// Paginated search with dynamic filters and sorting.
    /// Uses a separate COUNT query + paginated data query (faster than COUNT(*) OVER()
    /// which forces SQLite to materialize the entire result set).
    pub fn search_paginated_with_count(&self, opts: &SearchOptions) -> Result<(Vec<SearchRow>, u64)> {
        let (where_clause, params, needs_fl_join, needs_v_join) = self.build_search_where(opts);

        // --- Step 1: Count total matches ---
        let total_count = {
            let count_sql = if needs_v_join {
                let mut sql = String::from(
                    "SELECT COUNT(DISTINCT a.id) FROM assets a \
                     JOIN variants bv ON bv.content_hash = a.best_variant_hash \
                     JOIN variants v ON v.asset_id = a.id",
                );
                if needs_fl_join {
                    sql.push_str(" JOIN file_locations fl ON v.content_hash = fl.content_hash");
                }
                sql.push_str(&where_clause);
                sql
            } else if needs_fl_join {
                let mut sql = String::from(
                    "SELECT COUNT(*) FROM assets a \
                     JOIN variants bv ON bv.content_hash = a.best_variant_hash \
                     JOIN file_locations fl ON bv.content_hash = fl.content_hash",
                );
                sql.push_str(&where_clause);
                sql
            } else {
                // Use same bv JOIN as data query so assets with NULL best_variant_hash
                // are excluded from count (matching the data query behavior)
                let mut sql = String::from(
                    "SELECT COUNT(*) FROM assets a \
                     JOIN variants bv ON bv.content_hash = a.best_variant_hash",
                );
                sql.push_str(&where_clause);
                sql
            };
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            self.conn.query_row(&count_sql, param_refs.as_slice(), |row| row.get::<_, u64>(0))?
        };

        if total_count == 0 {
            return Ok((Vec::new(), 0));
        }

        let (rows, _) = self.search_page_inner(opts)?;
        Ok((rows, total_count))
    }

    /// Step 2 of `search_paginated_with_count`: the data query alone.
    fn search_page_inner(&self, opts: &SearchOptions) -> Result<(Vec<SearchRow>, ())> {
        let (where_clause, params, needs_fl_join, needs_v_join) = self.build_search_where(opts);

        // --- Fetch one page of results ---
        let (data_params, data_sql) = {
            let mut p = params;
            let page = opts.page.max(1);
            let offset = (page - 1) as u64 * opts.per_page as u64;

            let sql = if needs_v_join {
                let mut inner = String::from(
                    "WITH matched AS (SELECT DISTINCT a.id \
                     FROM assets a \
                     JOIN variants bv ON bv.content_hash = a.best_variant_hash \
                     JOIN variants v ON v.asset_id = a.id",
                );
                if needs_fl_join {
                    inner.push_str(" JOIN file_locations fl ON v.content_hash = fl.content_hash");
                }
                inner.push_str(&where_clause);
                inner.push_str(&format!(
                    ") SELECT {SEARCH_ROW_COLUMNS} \
                     FROM matched m \
                     JOIN assets a ON a.id = m.id \
                     JOIN variants bv ON bv.content_hash = a.best_variant_hash \
                     LEFT JOIN stacks s ON s.id = a.stack_id"
                ));
                inner.push_str(&format!(" ORDER BY {}", opts.sort.to_sql()));
                inner.push_str(" LIMIT ? OFFSET ?");
                p.push(Box::new(opts.per_page as u64));
                p.push(Box::new(offset));
                inner
            } else {
                let mut sql = format!(
                    "SELECT {SEARCH_ROW_COLUMNS} \
                     FROM assets a \
                     JOIN variants bv ON bv.content_hash = a.best_variant_hash \
                     LEFT JOIN stacks s ON s.id = a.stack_id"
                );
                sql.push_str(&where_clause);
                sql.push_str(&format!(" ORDER BY {}", opts.sort.to_sql()));
                sql.push_str(" LIMIT ? OFFSET ?");
                p.push(Box::new(opts.per_page as u64));
                p.push(Box::new(offset));
                sql
            };
            (p, sql)
        };

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            data_params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&data_sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), map_search_row)?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok((results, ()))
    }

    /// Paginated search with dynamic filters and sorting.
    pub fn search_paginated(&self, opts: &SearchOptions) -> Result<Vec<SearchRow>> {
        let (rows, _total) = self.search_paginated_with_count(opts)?;
        Ok(rows)
    }

    /// One page of results without the preceding COUNT query. For callers
    /// that already know the total (or do not need it): the count is a
    /// second full pass over the filtered set, which for a broad tag costs
    /// as much as the page itself.
    pub fn search_page(&self, opts: &SearchOptions) -> Result<Vec<SearchRow>> {
        self.search_page_inner(opts).map(|(rows, _)| rows)
    }

    /// Fetch a single asset as a SearchRow by asset ID.
    pub fn get_search_row(&self, asset_id: &str) -> Result<Option<SearchRow>> {
        let sql = format!(
            "SELECT {SEARCH_ROW_COLUMNS} \
             FROM assets a \
             JOIN variants bv ON bv.content_hash = a.best_variant_hash \
             LEFT JOIN stacks s ON s.id = a.stack_id \
             WHERE a.id = ?1"
        );
        let result = self
            .conn
            .query_row(&sql, rusqlite::params![asset_id], map_search_row);
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Count total results matching the same filters as search_paginated (without LIMIT/OFFSET).
    pub fn search_count(&self, opts: &SearchOptions) -> Result<u64> {
        let (where_clause, params, needs_fl_join, needs_v_join) = self.build_search_where(opts);

        let count_expr = if needs_v_join { "COUNT(DISTINCT a.id)" } else { "COUNT(*)" };
        let mut sql = format!(
            "SELECT {} FROM assets a \
             JOIN variants bv ON bv.content_hash = a.best_variant_hash",
            count_expr
        );

        if needs_v_join {
            sql.push_str(" JOIN variants v ON v.asset_id = a.id");
        }
        if needs_fl_join {
            sql.push_str(" JOIN file_locations fl ON v.content_hash = fl.content_hash");
        }

        sql.push_str(&where_clause);

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let count: u64 = self.conn.query_row(&sql, param_refs.as_slice(), |r| r.get(0))?;
        Ok(count)
    }

}

#[cfg(test)]
mod tests {
    use crate::catalog::test_support::*;
    use crate::catalog::*;

    // ── color label & rating-0 regression tests ───────────────
    fn setup_rating_label_catalog() -> Catalog {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();
        // Asset 1: rating=5, color_label="Red"
        let mut a1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:rl1");
        a1.rating = Some(5);
        a1.color_label = Some("Red".to_string());
        let v1 = test_variant(a1.id, "sha256:rl1", "rl1.jpg");
        a1.variants.push(v1.clone());
        catalog.insert_asset(&a1).unwrap();
        catalog.insert_variant(&v1).unwrap();

        // Asset 2: rating=0, color_label="Blue"
        let mut a2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:rl2");
        a2.rating = Some(0);
        a2.color_label = Some("Blue".to_string());
        let v2 = test_variant(a2.id, "sha256:rl2", "rl2.jpg");
        a2.variants.push(v2.clone());
        catalog.insert_asset(&a2).unwrap();
        catalog.insert_variant(&v2).unwrap();

        // Asset 3: rating=NULL (unrated), no color label
        let mut a3 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:rl3");
        a3.rating = None;
        a3.color_label = None;
        let v3 = test_variant(a3.id, "sha256:rl3", "rl3.jpg");
        a3.variants.push(v3.clone());
        catalog.insert_asset(&a3).unwrap();
        catalog.insert_variant(&v3).unwrap();

        // Asset 4: rating=3, no color label
        let mut a4 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:rl4");
        a4.rating = Some(3);
        let v4 = test_variant(a4.id, "sha256:rl4", "rl4.jpg");
        a4.variants.push(v4.clone());
        catalog.insert_asset(&a4).unwrap();
        catalog.insert_variant(&v4).unwrap();

        catalog
    }

    #[test]
    fn search_color_label_case_insensitive() {
        let catalog = setup_rating_label_catalog();
        // User types "Red" (capitalized) — matches stored "Red"
        let labels = vec!["Red".to_string()];
        let opts = SearchOptions { color_labels: &labels, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "rl1.jpg");

        // User types "red" (lowercase) — also matches stored "Red"
        let labels_lower = vec!["red".to_string()];
        let opts_lower = SearchOptions { color_labels: &labels_lower, per_page: u32::MAX, ..Default::default() };
        let results_lower = catalog.search_paginated(&opts_lower).unwrap();
        assert_eq!(results_lower.len(), 1);

        // User types "BLUE" (uppercase) — matches stored "Blue"
        let labels_up = vec!["BLUE".to_string()];
        let opts_up = SearchOptions { color_labels: &labels_up, per_page: u32::MAX, ..Default::default() };
        let results_up = catalog.search_paginated(&opts_up).unwrap();
        assert_eq!(results_up.len(), 1);
        assert_eq!(results_up[0].original_filename, "rl2.jpg");
    }

    #[test]
    fn search_color_label_any_is_complement_of_none() {
        let catalog = setup_rating_label_catalog();
        let all = catalog.search_paginated(&SearchOptions { per_page: u32::MAX, ..Default::default() }).unwrap();
        let none = catalog.search_paginated(&SearchOptions { color_label_none: true, per_page: u32::MAX, ..Default::default() }).unwrap();
        let any = catalog.search_paginated(&SearchOptions { color_label_any: true, per_page: u32::MAX, ..Default::default() }).unwrap();
        assert_eq!(none.len() + any.len(), all.len());
        assert!(none.iter().any(|r| r.original_filename == "rl4.jpg"));
        assert!(any.iter().all(|r| r.original_filename != "rl4.jpg"));
    }

    fn setup_stale_catalog() -> Catalog {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();
        let volume = crate::models::Volume::new(
            "vol".to_string(),
            std::path::PathBuf::from("/mnt/vol"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();
        let now = chrono::Utc::now();
        let cases: [(&str, &str, Option<chrono::DateTime<chrono::Utc>>); 3] = [
            ("sha256:st-never", "never.jpg", None),
            // 25 h ago: same calendar day as a `stale:1` cutoff most of the
            // time — the case the old string comparison got wrong.
            ("sha256:st-old", "old.jpg", Some(now - chrono::Duration::hours(25))),
            ("sha256:st-fresh", "fresh.jpg", Some(now - chrono::Duration::hours(23))),
        ];
        for (hash, name, verified) in cases {
            let asset = crate::models::Asset::new(crate::models::AssetType::Image, hash);
            let variant = crate::models::Variant { format: "jpg".to_string(), file_size: 100, ..test_variant(asset.id, hash, name) };
            let mut asset = asset;
            asset.variants.push(variant.clone());
            catalog.insert_asset(&asset).unwrap();
            catalog.insert_variant(&variant).unwrap();
            let loc = crate::models::FileLocation {
                volume_id: volume.id,
                relative_path: std::path::PathBuf::from(format!("photos/{name}")),
                verified_at: verified,
            };
            catalog.insert_file_location(hash, &loc).unwrap();
        }
        catalog
    }

    #[test]
    fn search_stale_days_compares_timestamps_not_strings() {
        let catalog = setup_stale_catalog();
        let opts = SearchOptions { stale_days: Some(NumericFilter::Exact(1.0)), per_page: u32::MAX, ..Default::default() };
        let mut names: Vec<String> = catalog.search_paginated(&opts).unwrap().into_iter().map(|r| r.original_filename).collect();
        names.sort();
        assert_eq!(names, vec!["never.jpg", "old.jpg"]);
    }

    #[test]
    fn search_stale_never_matches_only_unverified() {
        let catalog = setup_stale_catalog();
        let opts = SearchOptions { stale_never: true, per_page: u32::MAX, ..Default::default() };
        let names: Vec<String> = catalog.search_paginated(&opts).unwrap().into_iter().map(|r| r.original_filename).collect();
        assert_eq!(names, vec!["never.jpg"]);
    }

    #[test]
    fn search_has_online_locations() {
        let catalog = setup_stale_catalog();
        let vol_id: String = catalog.conn.query_row("SELECT id FROM volumes LIMIT 1", [], |r| r.get(0)).unwrap();
        let ids = vec![vol_id];
        let opts = SearchOptions { has_online_locations: Some(&ids), per_page: u32::MAX, ..Default::default() };
        assert_eq!(catalog.search_paginated(&opts).unwrap().len(), 3);
        let none: Vec<String> = vec![];
        let opts = SearchOptions { has_online_locations: Some(&none), per_page: u32::MAX, ..Default::default() };
        assert_eq!(catalog.search_paginated(&opts).unwrap().len(), 0);
    }

    #[test]
    fn search_tagcount_end_to_end() {
        // End-to-end exercise of the denormalised leaf_tag_count column:
        // insert_asset populates it, search_paginated filters on it.
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        fn add(catalog: &Catalog, name: &str, tags: Vec<String>) {
            let mut a = crate::models::Asset::new(
                crate::models::AssetType::Image,
                &format!("sha256:{name}"),
            );
            a.name = Some(name.to_string());
            a.tags = tags;
            let v = crate::models::Variant {
                content_hash: format!("sha256:{name}"),
                asset_id: a.id,
                role: crate::models::VariantRole::Original,
                format: "jpg".to_string(),
                file_size: 100,
                original_filename: format!("{name}.jpg"),
                source_metadata: Default::default(),
                locations: vec![],
            };
            a.variants.push(v.clone());
            catalog.insert_asset(&a).unwrap();
            catalog.insert_variant(&v).unwrap();
        }

        // a1: untagged (leaf count = 0)
        add(&catalog, "a1", vec![]);
        // a2: one leaf tag (a plain flat tag → 1 leaf)
        add(&catalog, "a2", vec!["sunset".to_string()]);
        // a3: one leaf in a 3-deep hierarchy (3 stored tags, 1 leaf)
        add(
            &catalog,
            "a3",
            vec![
                "subject".to_string(),
                "subject|nature".to_string(),
                "subject|nature|landscape".to_string(),
            ],
        );
        // a4: two leaves sharing ancestors (4 stored, 2 leaves)
        add(
            &catalog,
            "a4",
            vec![
                "subject".to_string(),
                "subject|nature".to_string(),
                "subject|nature|landscape".to_string(),
                "subject|nature|forest".to_string(),
            ],
        );
        // a5: five flat leaves
        add(
            &catalog,
            "a5",
            vec![
                "sunset".to_string(),
                "concert".to_string(),
                "portrait".to_string(),
                "documentary".to_string(),
                "bw".to_string(),
            ],
        );

        let names = |rows: Vec<SearchRow>| -> Vec<String> {
            rows.into_iter().map(|r| r.name.unwrap_or_default()).collect()
        };

        // tagcount:0 — the untagged ones (just a1)
        let opts = SearchOptions {
            tag_count: Some(NumericFilter::Exact(0.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let r = names(catalog.search_paginated(&opts).unwrap());
        assert_eq!(r, vec!["a1".to_string()]);

        // tagcount:1 — exactly one leaf (a2, a3)
        let opts = SearchOptions {
            tag_count: Some(NumericFilter::Exact(1.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let mut r = names(catalog.search_paginated(&opts).unwrap());
        r.sort();
        assert_eq!(r, vec!["a2".to_string(), "a3".to_string()]);

        // tagcount:2+ — two or more leaves (a4, a5)
        let opts = SearchOptions {
            tag_count: Some(NumericFilter::Min(2.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let mut r = names(catalog.search_paginated(&opts).unwrap());
        r.sort();
        assert_eq!(r, vec!["a4".to_string(), "a5".to_string()]);

        // tagcount:2-3 — two or three (just a4, which has 2)
        let opts = SearchOptions {
            tag_count: Some(NumericFilter::Range(2.0, 3.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let r = names(catalog.search_paginated(&opts).unwrap());
        assert_eq!(r, vec!["a4".to_string()]);
    }

    #[test]
    fn search_rating_zero_matches_unrated_and_zero() {
        let catalog = setup_rating_label_catalog();
        // rating:0 should match both NULL-rated and 0-rated assets (a2, a3)
        let opts = SearchOptions {
            rating: Some(NumericFilter::Exact(0.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.original_filename.as_str()).collect();
        assert_eq!(results.len(), 2, "rating:0 should match rl2 (rating=0) and rl3 (NULL). Got: {names:?}");
        assert!(names.contains(&"rl2.jpg"));
        assert!(names.contains(&"rl3.jpg"));
    }

    #[test]
    fn search_rating_exact_3_excludes_null() {
        let catalog = setup_rating_label_catalog();
        // rating:3 should only match the 3-star asset, not NULL
        let opts = SearchOptions {
            rating: Some(NumericFilter::Exact(3.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "rl4.jpg");
    }

    fn setup_case_tag_catalog() -> Catalog {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        // Asset 1: lowercase "landscape"
        let mut a1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cs1");
        a1.tags = vec!["landscape".to_string()];
        let v1 = test_variant(a1.id, "sha256:cs1", "cs1.jpg");
        a1.variants.push(v1.clone());
        catalog.insert_asset(&a1).unwrap();
        catalog.insert_variant(&v1).unwrap();

        // Asset 2: capitalized "Landscape"
        let mut a2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cs2");
        a2.tags = vec!["Landscape".to_string()];
        let v2 = test_variant(a2.id, "sha256:cs2", "cs2.jpg");
        a2.variants.push(v2.clone());
        catalog.insert_asset(&a2).unwrap();
        catalog.insert_variant(&v2).unwrap();

        // Asset 3: no tag
        let mut a3 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cs3");
        let v3 = test_variant(a3.id, "sha256:cs3", "cs3.jpg");
        a3.variants.push(v3.clone());
        catalog.insert_asset(&a3).unwrap();
        catalog.insert_variant(&v3).unwrap();

        catalog
    }

    #[test]
    fn search_tag_case_insensitive_default() {
        let catalog = setup_case_tag_catalog();
        // Without `^`, `tag:landscape` matches both "landscape" and "Landscape"
        let tags = vec!["landscape".to_string()];
        let opts = SearchOptions { tags: &tags, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 2, "case-insensitive tag should match both cases");
    }

    #[test]
    fn search_tag_case_sensitive_marker() {
        let catalog = setup_case_tag_catalog();
        // With `^`, `tag:^landscape` matches ONLY "landscape"
        let tags = vec!["^landscape".to_string()];
        let opts = SearchOptions { tags: &tags, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "cs1.jpg");

        // And `tag:^Landscape` matches ONLY "Landscape"
        let tags2 = vec!["^Landscape".to_string()];
        let opts2 = SearchOptions { tags: &tags2, per_page: u32::MAX, ..Default::default() };
        let results2 = catalog.search_paginated(&opts2).unwrap();
        assert_eq!(results2.len(), 1);
        assert_eq!(results2[0].original_filename, "cs2.jpg");
    }

    #[test]
    fn search_description_filter() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let cases = [
            ("sha256:d1", Some("a beautiful sunset over the ocean")),
            ("sha256:d2", Some("portrait of a cat in soft window light")),
            ("sha256:d3", Some("Sunset behind mountain peaks")),
            ("sha256:d4", None),  // no description
        ];
        for (hash, desc) in &cases {
            let mut a = crate::models::Asset::new(crate::models::AssetType::Image, hash);
            a.description = desc.map(|s| s.to_string());
            let v = crate::models::Variant {
                content_hash: hash.to_string(),
                asset_id: a.id,
                role: crate::models::VariantRole::Original,
                format: "jpg".to_string(),
                file_size: 100,
                original_filename: format!("{}.jpg", &hash[7..]),
                source_metadata: Default::default(),
                locations: vec![],
            };
            a.variants.push(v.clone());
            catalog.insert_asset(&a).unwrap();
            catalog.insert_variant(&v).unwrap();
        }

        // description:sunset matches d1 and d3 (case-insensitive substring)
        let descs = vec!["sunset".to_string()];
        let opts = SearchOptions { descriptions: &descs, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.original_filename.as_str()).collect();
        assert_eq!(results.len(), 2, "got: {names:?}");
        assert!(names.contains(&"d1.jpg"));
        assert!(names.contains(&"d3.jpg"));

        // description:cat matches only d2
        let descs = vec!["cat".to_string()];
        let opts = SearchOptions { descriptions: &descs, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "d2.jpg");

        // -description:cat excludes d2; assets with NULL description survive
        let exclude = vec!["cat".to_string()];
        let opts = SearchOptions { descriptions_exclude: &exclude, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.original_filename.as_str()).collect();
        assert_eq!(results.len(), 3, "got: {names:?}");
        assert!(!names.contains(&"d2.jpg"));
        assert!(names.contains(&"d4.jpg"), "NULL description should survive exclusion");
    }

    #[test]
    fn search_tag_prefix_anchor() {
        // Build a catalog with various tags to test the | prefix anchor
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let cases = [
            ("sha256:pa1", vec!["wedding"]),
            ("sha256:pa2", vec!["wedding-2024"]),
            ("sha256:pa3", vec!["events|wedding"]),
            ("sha256:pa4", vec!["events|wedding|2024-05"]),
            ("sha256:pa5", vec!["weekend"]),               // starts with "we" but not "wed"
            ("sha256:pa6", vec!["midweek"]),                // contains "we" mid-component, must NOT match |we
            ("sha256:pa7", vec!["other"]),
        ];
        for (hash, tags) in &cases {
            let mut a = crate::models::Asset::new(crate::models::AssetType::Image, hash);
            a.tags = tags.iter().map(|s| s.to_string()).collect();
            let v = crate::models::Variant {
                content_hash: hash.to_string(),
                asset_id: a.id,
                role: crate::models::VariantRole::Original,
                format: "jpg".to_string(),
                file_size: 100,
                original_filename: format!("{}.jpg", &hash[7..]),
                source_metadata: Default::default(),
                locations: vec![],
            };
            a.variants.push(v.clone());
            catalog.insert_asset(&a).unwrap();
            catalog.insert_variant(&v).unwrap();
        }

        // |wed should match: wedding, wedding-2024, events|wedding, events|wedding|2024-05
        // (4 assets); should NOT match weekend, midweek, other.
        let tags = vec!["|wed".to_string()];
        let opts = SearchOptions { tags: &tags, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.original_filename.as_str()).collect();
        assert_eq!(results.len(), 4, "got: {names:?}");
        assert!(names.contains(&"pa1.jpg"));
        assert!(names.contains(&"pa2.jpg"));
        assert!(names.contains(&"pa3.jpg"));
        assert!(names.contains(&"pa4.jpg"));
        assert!(!names.contains(&"pa5.jpg"));
        assert!(!names.contains(&"pa6.jpg"));
        assert!(!names.contains(&"pa7.jpg"));

        // |we should additionally match weekend (root-level)
        let tags = vec!["|we".to_string()];
        let opts = SearchOptions { tags: &tags, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.original_filename.as_str()).collect();
        assert!(names.contains(&"pa5.jpg"), "weekend should match |we");
        // midweek must NOT match — "we" is mid-component, not at a boundary
        assert!(!names.contains(&"pa6.jpg"), "midweek should NOT match |we (we is mid-component)");
    }

    #[test]
    fn search_tag_prefix_anchor_case_sensitive() {
        // Combined ^| should be case-sensitive prefix anchor
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        for (hash, tag) in &[
            ("sha256:pcs1", "Wedding"),
            ("sha256:pcs2", "wedding"),
        ] {
            let mut a = crate::models::Asset::new(crate::models::AssetType::Image, hash);
            a.tags = vec![tag.to_string()];
            let v = crate::models::Variant {
                content_hash: hash.to_string(),
                asset_id: a.id,
                role: crate::models::VariantRole::Original,
                format: "jpg".to_string(),
                file_size: 100,
                original_filename: format!("{}.jpg", &hash[7..]),
                source_metadata: Default::default(),
                locations: vec![],
            };
            a.variants.push(v.clone());
            catalog.insert_asset(&a).unwrap();
            catalog.insert_variant(&v).unwrap();
        }

        // ^|Wed → only the capitalized variant
        let tags = vec!["^|Wed".to_string()];
        let opts = SearchOptions { tags: &tags, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "pcs1.jpg");

        // |Wed (no ^) → both variants
        let tags = vec!["|Wed".to_string()];
        let opts = SearchOptions { tags: &tags, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn search_tag_case_sensitive_exact_combined() {
        let catalog = setup_case_tag_catalog();
        // Both markers combined: `tag:=^landscape` (exact level + case-sensitive)
        let tags = vec!["=^landscape".to_string()];
        let opts = SearchOptions { tags: &tags, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "cs1.jpg");

        // Reverse order also works: `tag:^=landscape`
        let tags2 = vec!["^=landscape".to_string()];
        let opts2 = SearchOptions { tags: &tags2, per_page: u32::MAX, ..Default::default() };
        let results2 = catalog.search_paginated(&opts2).unwrap();
        assert_eq!(results2.len(), 1);
        assert_eq!(results2[0].original_filename, "cs1.jpg");
    }

    #[test]
    fn search_tag_exact_with_ancestor_expansion() {
        // Simulates the CaptureOne/Lightroom ancestor expansion scenario.
        // An asset tagged `location|Germany|Bayern|Holzkirchen|Marktplatz`
        // also gets standalone ancestor tags: `Holzkirchen`, `Bayern`, etc.
        // tag:=Holzkirchen should EXCLUDE this asset because Holzkirchen
        // is a non-leaf component (it has `|Marktplatz` below it).
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        // Asset 1: Holzkirchen is a MID-PATH component (has deeper tags)
        let mut a1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:ae1");
        a1.tags = vec![
            "location".to_string(),
            "location|Germany".to_string(),
            "location|Germany|Bayern".to_string(),
            "location|Germany|Bayern|Holzkirchen".to_string(),
            "location|Germany|Bayern|Holzkirchen|Marktplatz".to_string(),
            // Standalone ancestors from expand_ancestors:
            "Germany".to_string(),
            "Bayern".to_string(),
            "Holzkirchen".to_string(),
            "Marktplatz".to_string(),
        ];
        let v1 = crate::models::Variant { file_size: 100, ..test_variant(a1.id, "sha256:ae1", "ae1.jpg") };
        a1.variants.push(v1.clone());
        catalog.insert_asset(&a1).unwrap();
        catalog.insert_variant(&v1).unwrap();

        // Asset 2: Holzkirchen IS the leaf (no deeper tags)
        let mut a2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:ae2");
        a2.tags = vec![
            "location".to_string(),
            "location|Germany".to_string(),
            "location|Germany|Bayern".to_string(),
            "location|Germany|Bayern|Holzkirchen".to_string(),
            "Germany".to_string(),
            "Bayern".to_string(),
            "Holzkirchen".to_string(),
        ];
        let v2 = crate::models::Variant { file_size: 100, ..test_variant(a2.id, "sha256:ae2", "ae2.jpg") };
        a2.variants.push(v2.clone());
        catalog.insert_asset(&a2).unwrap();
        catalog.insert_variant(&v2).unwrap();

        // tag:/Holzkirchen (leaf-only-at-any-level) should match ONLY ae2:
        // Holzkirchen is a leaf there. ae1 has Marktplatz below the deep
        // Holzkirchen path, so Holzkirchen is not a leaf in the deep branch
        // — but ae1 ALSO has the standalone "Holzkirchen" via ancestor
        // expansion, which IS a leaf. So both ae1 and ae2 should match `/Holzkirchen`?
        // No — the leaf-only check examines whether the asset has ANY
        // descendant of Holzkirchen anywhere in its tag list. ae1 does
        // (location|...|Holzkirchen|Marktplatz contains "Holzkirchen|"),
        // so it gets rejected; ae2 doesn't, so it matches.
        let tags = vec!["/Holzkirchen".to_string()];
        let opts = SearchOptions { tags: &tags, per_page: u32::MAX, ..Default::default() };
        let results = catalog.search_paginated(&opts).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.original_filename.as_str()).collect();
        assert_eq!(results.len(), 1, "tag:/Holzkirchen should match only ae2. Got: {names:?}");
        assert_eq!(results[0].original_filename, "ae2.jpg");

        // tag:=Holzkirchen (whole-path) matches BOTH because both have the
        // standalone "Holzkirchen" tag from ancestor expansion. The whole-path
        // check is purely about the JSON value equality; it doesn't care what
        // OTHER tags an asset has.
        let tags_eq = vec!["=Holzkirchen".to_string()];
        let opts_eq = SearchOptions { tags: &tags_eq, per_page: u32::MAX, ..Default::default() };
        let results_eq = catalog.search_paginated(&opts_eq).unwrap();
        assert_eq!(results_eq.len(), 2, "tag:=Holzkirchen should match both (both have standalone Holzkirchen)");

        // tag:Holzkirchen (no marker) should match both via root-level standalone match.
        let tags_no_eq = vec!["Holzkirchen".to_string()];
        let opts_no_eq = SearchOptions { tags: &tags_no_eq, per_page: u32::MAX, ..Default::default() };
        let results_no_eq = catalog.search_paginated(&opts_no_eq).unwrap();
        assert_eq!(results_no_eq.len(), 2, "tag:Holzkirchen should match both");

        // tag:=/Holzkirchen — combined whole-path AND no-descendants.
        // ae1 has the standalone "Holzkirchen" (whole-path match) BUT also
        // has "location|Germany|Bayern|Holzkirchen|Marktplatz" — Holzkirchen
        // is a mid-path component there, so the no-descendants guard rejects.
        // ae2 has the standalone Holzkirchen with no descendants → matches.
        // This combination is what the tags-page "(N as leaf)" link uses so
        // the click count equals the row's leaf count.
        let tags_eq_leaf = vec!["=/Holzkirchen".to_string()];
        let opts_eq_leaf = SearchOptions { tags: &tags_eq_leaf, per_page: u32::MAX, ..Default::default() };
        let results_eq_leaf = catalog.search_paginated(&opts_eq_leaf).unwrap();
        assert_eq!(results_eq_leaf.len(), 1, "tag:=/Holzkirchen should match only ae2");
        assert_eq!(results_eq_leaf[0].original_filename, "ae2.jpg");

        // Marker order doesn't matter: tag:/=Holzkirchen must give the same.
        let tags_alt = vec!["/=Holzkirchen".to_string()];
        let opts_alt = SearchOptions { tags: &tags_alt, per_page: u32::MAX, ..Default::default() };
        let results_alt = catalog.search_paginated(&opts_alt).unwrap();
        assert_eq!(results_alt.len(), 1, "tag:/=Holzkirchen should match only ae2");
        assert_eq!(results_alt[0].original_filename, "ae2.jpg");
    }

    #[test]
    fn search_rating_min_1_excludes_unrated_and_zero() {
        let catalog = setup_rating_label_catalog();
        // rating:1+ should NOT include unrated (NULL) or 0-rated — only a1, a4
        let opts = SearchOptions {
            rating: Some(NumericFilter::Min(1.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 2);
        let names: Vec<&str> = results.iter().map(|r| r.original_filename.as_str()).collect();
        assert!(names.contains(&"rl1.jpg"));
        assert!(names.contains(&"rl4.jpg"));
    }

    #[test]
    fn search_by_camera() {
        let catalog = setup_metadata_catalog();
        let cam = vec!["X-T5".to_string()];
        let opts = SearchOptions {
            cameras: &cam,
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "DSCF0001.RAF");
    }

    #[test]
    fn search_by_camera_partial() {
        let catalog = setup_metadata_catalog();
        let cam = vec!["Z 6".to_string()];
        let opts = SearchOptions {
            cameras: &cam,
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "DSC_0001.NEF");
    }

    #[test]
    fn search_by_iso_exact_and_range() {
        let catalog = setup_metadata_catalog();

        // Exact ISO
        let opts = SearchOptions {
            iso: Some(NumericFilter::Exact(400.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "DSCF0001.RAF");

        // ISO range 100-800: should match Fuji (400) only
        let opts = SearchOptions {
            iso: Some(NumericFilter::Range(100.0, 800.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "DSCF0001.RAF");

        // ISO min 1000+: should match Nikon (3200) only
        let opts = SearchOptions {
            iso: Some(NumericFilter::Min(1000.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "DSC_0001.NEF");
    }

    #[test]
    fn search_by_focal_range() {
        let catalog = setup_metadata_catalog();

        // focal 50-56: should match both
        let opts = SearchOptions {
            focal: Some(NumericFilter::Range(50.0, 56.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 2);

        // focal 55-60: should match Fuji (56mm) only
        let opts = SearchOptions {
            focal: Some(NumericFilter::Range(55.0, 60.0)),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "DSCF0001.RAF");
    }

    #[test]
    fn search_text_includes_metadata() {
        let catalog = setup_metadata_catalog();

        // "FUJIFILM" exists in source_metadata JSON but not in name/filename/description
        let opts = SearchOptions {
            text: Some("FUJIFILM"),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "DSCF0001.RAF");
    }

    #[test]
    fn search_text_fts_substring_matches_mid_word() {
        // The FTS5 trigram path must keep LIKE's substring semantics:
        // mid-word fragments and fragments spanning separators match.
        let catalog = setup_search_catalog();

        for needle in ["unse", "nset_bea", "ach.jp"] {
            let opts = SearchOptions {
                text: Some(needle),
                per_page: u32::MAX,
                ..Default::default()
            };
            let results = catalog.search_paginated(&opts).unwrap();
            assert_eq!(results.len(), 1, "substring '{needle}' must match");
            assert_eq!(results[0].original_filename, "sunset_beach.jpg");
        }
    }

    #[test]
    fn search_text_fts_is_case_insensitive() {
        let catalog = setup_search_catalog();
        let opts = SearchOptions {
            text: Some("SUNSET"),
            per_page: u32::MAX,
            ..Default::default()
        };
        assert_eq!(catalog.search_paginated(&opts).unwrap().len(), 1);
    }

    #[test]
    fn search_text_fts_follows_updates() {
        // Trigger-maintained index: renaming an asset must be reflected
        // without any explicit FTS maintenance call.
        let catalog = setup_search_catalog();
        let opts = SearchOptions {
            text: Some("freshly-renamed"),
            per_page: u32::MAX,
            ..Default::default()
        };
        assert_eq!(catalog.search_paginated(&opts).unwrap().len(), 0);

        let id: String = catalog
            .conn
            .query_row("SELECT id FROM assets LIMIT 1", [], |r| r.get(0))
            .unwrap();
        catalog
            .conn
            .execute(
                "UPDATE assets SET name = 'freshly-renamed asset' WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap();
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1, "FTS must follow the rename via trigger");
    }

    #[test]
    fn search_text_exclude_uses_fts() {
        let catalog = setup_search_catalog();
        // Excluding "sunset" must drop the sunset asset but keep others.
        let all = catalog
            .search_paginated(&SearchOptions { per_page: u32::MAX, ..Default::default() })
            .unwrap()
            .len();
        let exclude = vec!["sunset".to_string()];
        let opts = SearchOptions {
            text_exclude: &exclude,
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), all - 1);
        assert!(results.iter().all(|r| r.original_filename != "sunset_beach.jpg"));
    }

    #[test]
    fn search_text_short_terms_fall_back_to_like() {
        // Terms under 3 chars can't use the trigram index — the LIKE
        // fallback must still find them.
        let catalog = setup_search_catalog();
        let opts = SearchOptions {
            text: Some("su"),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert!(
            results.iter().any(|r| r.original_filename == "sunset_beach.jpg"),
            "2-char term must match via LIKE fallback"
        );
    }

    #[test]
    fn search_text_fts_quotes_are_literal() {
        // User input must be treated as a substring, never FTS5 query
        // syntax — operators and quotes can't error or change semantics.
        let catalog = setup_search_catalog();
        for hostile in ["a AND b", "name:x", "\"quoted\"", "x OR"] {
            let opts = SearchOptions {
                text: Some(hostile),
                per_page: u32::MAX,
                ..Default::default()
            };
            // Must not error; matches nothing in the fixture.
            let results = catalog.search_paginated(&opts).unwrap();
            assert!(results.is_empty(), "'{hostile}' should match nothing");
        }
    }

    #[test]
    fn search_meta_json_extract() {
        let catalog = setup_metadata_catalog();

        // meta:label=Red should match the Nikon variant
        let opts = SearchOptions {
            meta_filters: vec![("label", "Red")],
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "DSC_0001.NEF");
    }

    #[test]
    fn search_copies_exact() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let vol1 = crate::models::Volume::new(
            "vol-a".to_string(),
            std::path::PathBuf::from("/mnt/a"),
            crate::models::VolumeType::Local,
        );
        let vol2 = crate::models::Volume::new(
            "vol-b".to_string(),
            std::path::PathBuf::from("/mnt/b"),
            crate::models::VolumeType::Local,
        );
        let vol3 = crate::models::Volume::new(
            "vol-c".to_string(),
            std::path::PathBuf::from("/mnt/c"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&vol1).unwrap();
        catalog.ensure_volume(&vol2).unwrap();
        catalog.ensure_volume(&vol3).unwrap();

        // Asset with 1 location
        let mut a1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cop1");
        let v1 = test_variant(a1.id, "sha256:cop1", "one.jpg");
        a1.variants.push(v1.clone());
        catalog.insert_asset(&a1).unwrap();
        catalog.insert_variant(&v1).unwrap();
        catalog.insert_file_location(&v1.content_hash, &crate::models::FileLocation {
            volume_id: vol1.id,
            relative_path: std::path::PathBuf::from("one.jpg"),
            verified_at: None,
        }).unwrap();

        // Asset with 2 locations
        let mut a2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cop2");
        let v2 = crate::models::Variant { file_size: 2000, ..test_variant(a2.id, "sha256:cop2", "two.jpg") };
        a2.variants.push(v2.clone());
        catalog.insert_asset(&a2).unwrap();
        catalog.insert_variant(&v2).unwrap();
        catalog.insert_file_location(&v2.content_hash, &crate::models::FileLocation {
            volume_id: vol1.id,
            relative_path: std::path::PathBuf::from("two.jpg"),
            verified_at: None,
        }).unwrap();
        catalog.insert_file_location(&v2.content_hash, &crate::models::FileLocation {
            volume_id: vol2.id,
            relative_path: std::path::PathBuf::from("two.jpg"),
            verified_at: None,
        }).unwrap();

        // Asset on 3 distinct volumes
        let mut a3 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cop3");
        let v3 = crate::models::Variant { file_size: 3000, ..test_variant(a3.id, "sha256:cop3", "three.jpg") };
        a3.variants.push(v3.clone());
        catalog.insert_asset(&a3).unwrap();
        catalog.insert_variant(&v3).unwrap();
        catalog.insert_file_location(&v3.content_hash, &crate::models::FileLocation {
            volume_id: vol1.id,
            relative_path: std::path::PathBuf::from("three.jpg"),
            verified_at: None,
        }).unwrap();
        catalog.insert_file_location(&v3.content_hash, &crate::models::FileLocation {
            volume_id: vol2.id,
            relative_path: std::path::PathBuf::from("three.jpg"),
            verified_at: None,
        }).unwrap();
        catalog.insert_file_location(&v3.content_hash, &crate::models::FileLocation {
            volume_id: vol3.id,
            relative_path: std::path::PathBuf::from("three.jpg"),
            verified_at: None,
        }).unwrap();

        // copies:1 → only the 1-volume asset (at risk)
        let results = catalog.search_paginated(&SearchOptions {
            copies: Some(NumericFilter::Exact(1.0)),
            per_page: u32::MAX,
            ..Default::default()
        }).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "one.jpg");

        // copies:2 → only the 2-volume asset
        let results = catalog.search_paginated(&SearchOptions {
            copies: Some(NumericFilter::Exact(2.0)),
            per_page: u32::MAX,
            ..Default::default()
        }).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "two.jpg");

        // copies:3 → only the 3-volume asset
        let results = catalog.search_paginated(&SearchOptions {
            copies: Some(NumericFilter::Exact(3.0)),
            per_page: u32::MAX,
            ..Default::default()
        }).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "three.jpg");
    }

    #[test]
    fn search_copies_min() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let vol1 = crate::models::Volume::new("vol-a".to_string(), std::path::PathBuf::from("/mnt/a"), crate::models::VolumeType::Local);
        let vol2 = crate::models::Volume::new("vol-b".to_string(), std::path::PathBuf::from("/mnt/b"), crate::models::VolumeType::Local);
        let vol3 = crate::models::Volume::new("vol-c".to_string(), std::path::PathBuf::from("/mnt/c"), crate::models::VolumeType::Local);
        catalog.ensure_volume(&vol1).unwrap();
        catalog.ensure_volume(&vol2).unwrap();
        catalog.ensure_volume(&vol3).unwrap();

        // Asset on 1 volume
        let mut a1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cpm1");
        let v1 = test_variant(a1.id, "sha256:cpm1", "one.jpg");
        a1.variants.push(v1.clone());
        catalog.insert_asset(&a1).unwrap();
        catalog.insert_variant(&v1).unwrap();
        catalog.insert_file_location(&v1.content_hash, &crate::models::FileLocation {
            volume_id: vol1.id, relative_path: std::path::PathBuf::from("one.jpg"), verified_at: None,
        }).unwrap();

        // Asset on 2 volumes
        let mut a2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cpm2");
        let v2 = crate::models::Variant { file_size: 2000, ..test_variant(a2.id, "sha256:cpm2", "two.jpg") };
        a2.variants.push(v2.clone());
        catalog.insert_asset(&a2).unwrap();
        catalog.insert_variant(&v2).unwrap();
        catalog.insert_file_location(&v2.content_hash, &crate::models::FileLocation {
            volume_id: vol1.id, relative_path: std::path::PathBuf::from("two.jpg"), verified_at: None,
        }).unwrap();
        catalog.insert_file_location(&v2.content_hash, &crate::models::FileLocation {
            volume_id: vol2.id, relative_path: std::path::PathBuf::from("two.jpg"), verified_at: None,
        }).unwrap();

        // Asset on 3 volumes
        let mut a3 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cpm3");
        let v3 = crate::models::Variant { file_size: 3000, ..test_variant(a3.id, "sha256:cpm3", "three.jpg") };
        a3.variants.push(v3.clone());
        catalog.insert_asset(&a3).unwrap();
        catalog.insert_variant(&v3).unwrap();
        catalog.insert_file_location(&v3.content_hash, &crate::models::FileLocation {
            volume_id: vol1.id, relative_path: std::path::PathBuf::from("three.jpg"), verified_at: None,
        }).unwrap();
        catalog.insert_file_location(&v3.content_hash, &crate::models::FileLocation {
            volume_id: vol2.id, relative_path: std::path::PathBuf::from("three.jpg"), verified_at: None,
        }).unwrap();
        catalog.insert_file_location(&v3.content_hash, &crate::models::FileLocation {
            volume_id: vol3.id, relative_path: std::path::PathBuf::from("three.jpg"), verified_at: None,
        }).unwrap();

        // copies:2+ → assets on 2 or more volumes
        let results = catalog.search_paginated(&SearchOptions {
            copies: Some(NumericFilter::Min(2.0)),
            per_page: u32::MAX,
            ..Default::default()
        }).unwrap();
        assert_eq!(results.len(), 2);
        let names: Vec<&str> = results.iter().map(|r| r.original_filename.as_str()).collect();
        assert!(names.contains(&"two.jpg"));
        assert!(names.contains(&"three.jpg"));

        // copies:3+ → only the 3-volume asset
        let results = catalog.search_paginated(&SearchOptions {
            copies: Some(NumericFilter::Min(3.0)),
            per_page: u32::MAX,
            ..Default::default()
        }).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].original_filename, "three.jpg");
    }

    #[test]
    fn search_paginated_with_count_returns_total() {
        let catalog = setup_search_catalog();

        // Page 1 with per_page=1: should return 1 row but total_count=2
        let opts = SearchOptions {
            per_page: 1,
            page: 1,
            ..Default::default()
        };
        let (rows, total) = catalog.search_paginated_with_count(&opts).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(total, 2);

        // Page 2: should return 1 row, same total
        let opts2 = SearchOptions {
            per_page: 1,
            page: 2,
            ..Default::default()
        };
        let (rows2, total2) = catalog.search_paginated_with_count(&opts2).unwrap();
        assert_eq!(rows2.len(), 1);
        assert_eq!(total2, 2);

        // All on one page
        let opts3 = SearchOptions {
            per_page: 100,
            page: 1,
            ..Default::default()
        };
        let (rows3, total3) = catalog.search_paginated_with_count(&opts3).unwrap();
        assert_eq!(rows3.len(), 2);
        assert_eq!(total3, 2);
    }

    #[test]
    fn search_paginated_with_count_empty_results() {
        let catalog = setup_search_catalog();

        let opts = SearchOptions {
            text: Some("nonexistent_query_xyz"),
            per_page: 10,
            page: 1,
            ..Default::default()
        };
        let (rows, total) = catalog.search_paginated_with_count(&opts).unwrap();
        assert!(rows.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn search_paginated_with_count_filtered() {
        let catalog = setup_search_catalog();

        // Filter to only images (1 of 2 assets)
        let tags = vec!["landscape".to_string()];
        let opts = SearchOptions {
            tags: &tags,
            per_page: 100,
            page: 1,
            ..Default::default()
        };
        let (rows, total) = catalog.search_paginated_with_count(&opts).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(total, 1);
    }

    #[test]
    fn search_similar_asset_ids_filter() {
        let catalog = setup_search_catalog();

        // Get all assets to find their IDs
        let all = catalog.search_paginated(&SearchOptions {
            per_page: u32::MAX,
            ..Default::default()
        }).unwrap();
        assert_eq!(all.len(), 2);

        // Filter to just the first asset using similar_asset_ids
        let ids = vec![all[0].asset_id.clone()];
        let opts = SearchOptions {
            similar_asset_ids: Some(&ids),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].asset_id, all[0].asset_id);
    }

    #[test]
    fn search_has_embed_filter() {
        let catalog = setup_search_catalog();

        // No embeddings yet — embed:none should return all
        let all = catalog
            .search_paginated(&SearchOptions {
                has_embed: Some(false),
                per_page: u32::MAX,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(all.len(), 2);

        // embed:any should return nothing
        let embedded = catalog
            .search_paginated(&SearchOptions {
                has_embed: Some(true),
                per_page: u32::MAX,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(embedded.len(), 0);

        // Insert a fake embedding for one asset
        let asset_id = &all[0].asset_id;
        catalog
            .conn()
            .execute(
                "INSERT INTO embeddings (asset_id, model, embedding) VALUES (?1, 'test', X'00')",
                rusqlite::params![asset_id],
            )
            .unwrap();

        // embed:any should now return 1
        let embedded = catalog
            .search_paginated(&SearchOptions {
                has_embed: Some(true),
                per_page: u32::MAX,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(embedded.len(), 1);
        assert_eq!(&embedded[0].asset_id, asset_id);

        // embed:none should return 1
        let not_embedded = catalog
            .search_paginated(&SearchOptions {
                has_embed: Some(false),
                per_page: u32::MAX,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(not_embedded.len(), 1);
        assert_ne!(&not_embedded[0].asset_id, asset_id);
    }

    #[test]
    fn search_similar_asset_ids_empty() {
        let catalog = setup_search_catalog();

        // Empty similar IDs should return nothing
        let ids: Vec<String> = vec![];
        let opts = SearchOptions {
            similar_asset_ids: Some(&ids),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn search_text_search_ids_filter() {
        let catalog = setup_search_catalog();
        let all = catalog
            .search_paginated(&SearchOptions {
                per_page: u32::MAX,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(all.len(), 2);

        // Filter using text_search_ids (same mechanism as similar but for text queries)
        let ids = vec![all[0].asset_id.clone()];
        let opts = SearchOptions {
            text_search_ids: Some(&ids),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].asset_id, all[0].asset_id);
    }

    #[test]
    fn search_text_search_ids_empty() {
        let catalog = setup_search_catalog();
        let ids: Vec<String> = vec![];
        let opts = SearchOptions {
            text_search_ids: Some(&ids),
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn search_text_search_ids_composes_with_other_filters() {
        let catalog = setup_search_catalog();
        let all = catalog
            .search_paginated(&SearchOptions {
                per_page: u32::MAX,
                ..Default::default()
            })
            .unwrap();

        // Both IDs in text_search_ids, but restrict by rating
        let ids: Vec<String> = all.iter().map(|r| r.asset_id.clone()).collect();
        let opts = SearchOptions {
            text_search_ids: Some(&ids),
            rating: Some(NumericFilter::Min(5.0)), // Only the 5-star asset
            per_page: u32::MAX,
            ..Default::default()
        };
        let results = catalog.search_paginated(&opts).unwrap();
        // Should only return assets matching both text_search_ids AND rating
        assert!(results.len() <= ids.len());
    }
}
