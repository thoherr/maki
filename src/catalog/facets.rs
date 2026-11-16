//! `facets` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.

use super::*;

impl Catalog {
    // ═══ CALENDAR & FACETS ═══

    /// Get asset counts per day for a given year, respecting search filters.
    ///
    /// Returns a map of `"YYYY-MM-DD"` → count. Reuses `build_search_where()`
    /// for filter consistency, then adds a year constraint and groups by day.
    pub fn calendar_counts(&self, year: i32, opts: &SearchOptions) -> Result<HashMap<String, u64>> {
        let (where_clause, mut params, needs_fl_join, needs_v_join) = self.build_search_where(opts);

        let mut sql = String::from(
            "SELECT substr(a.created_at, 1, 10) as day, COUNT(DISTINCT a.id) \
             FROM assets a \
             JOIN variants bv ON bv.content_hash = a.best_variant_hash",
        );

        if needs_v_join {
            sql.push_str(" JOIN variants v ON v.asset_id = a.id");
        }
        if needs_fl_join {
            sql.push_str(" JOIN file_locations fl ON v.content_hash = fl.content_hash");
        }

        sql.push_str(&where_clause);

        // Add year constraint
        sql.push_str(" AND a.created_at >= ? AND a.created_at < ?");
        params.push(Box::new(format!("{year:04}-01-01")));
        params.push(Box::new(format!("{:04}-01-01", year + 1)));

        sql.push_str(" GROUP BY day");

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?;

        let mut counts = HashMap::new();
        for row in rows {
            let (day, count) = row?;
            counts.insert(day, count);
        }
        Ok(counts)
    }

    /// Get all distinct years that have assets.
    pub fn calendar_years(&self) -> Result<Vec<i32>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT CAST(substr(created_at, 1, 4) AS INTEGER) \
             FROM assets \
             WHERE created_at IS NOT NULL \
             ORDER BY 1",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, i32>(0))?;
        let mut years = Vec::new();
        for row in rows {
            years.push(row?);
        }
        Ok(years)
    }

    /// Get facet counts for the browse sidebar, respecting search filters.
    ///
    /// The filtered set is resolved **once** into a per-connection TEMP
    /// table (`facet_match`: id plus the handful of asset columns the
    /// facets group by), and the aggregates run against that. Before this
    /// each of the eight facets re-ran the full WHERE — eight scans of the
    /// catalog for a broad filter (~7 s for a 77k-asset tag on a 270k
    /// catalog; now one scan plus cheap aggregates). The tags facet still
    /// joins `assets` for the JSON column (not copied — it is the bulk of
    /// the row), and the volumes facet joins locations as before. Counts
    /// are identical: the table holds each matching asset exactly once.
    pub fn facet_counts(&self, opts: &SearchOptions) -> Result<FacetCounts> {
        let (where_clause, params, needs_fl_join, needs_v_join) = self.build_search_where(opts);

        // Helper: build the FROM/JOIN prefix used by the materialising query
        let mut base_from = String::from(
            "FROM assets a \
             JOIN variants bv ON bv.content_hash = a.best_variant_hash",
        );
        if needs_v_join {
            base_from.push_str(" JOIN variants v ON v.asset_id = a.id");
        }
        if needs_fl_join {
            base_from.push_str(" JOIN file_locations fl ON v.content_hash = fl.content_hash");
        }

        // 0. Materialise the matching ids with their facet columns.
        // TEMP tables live on this connection only (pooled connections
        // keep it between requests; `temp_store = MEMORY`), so create
        // once and clear per call.
        self.conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS facet_match ( \
               id TEXT PRIMARY KEY, rating INTEGER, color_label TEXT, \
               fmt TEXT, year TEXT, geo INTEGER \
             ) WITHOUT ROWID; \
             DELETE FROM facet_match;",
        )?;
        {
            let sql = format!(
                "INSERT OR IGNORE INTO facet_match (id, rating, color_label, fmt, year, geo) \
                 SELECT a.id, a.rating, a.color_label, \
                        COALESCE(a.primary_variant_format, 'unknown'), \
                        substr(a.created_at, 1, 4), \
                        a.latitude IS NOT NULL \
                 {base_from}{where_clause}"
            );
            let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
            self.conn.execute(&sql, refs.as_slice())?;
        }

        // 1. Total count
        let total: u64 = self.conn.query_row("SELECT COUNT(*) FROM facet_match", [], |r| r.get(0))?;

        // 2. Rating distribution
        let mut ratings = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT rating, COUNT(*) AS cnt FROM facet_match GROUP BY rating ORDER BY rating",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, Option<u8>>(0)?, row.get::<_, u64>(1)?))
            })?;
            for row in rows {
                ratings.push(row?);
            }
        }

        // 3. Label distribution
        let mut labels = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT color_label, COUNT(*) AS cnt FROM facet_match GROUP BY color_label ORDER BY color_label",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, Option<String>>(0)?, row.get::<_, u64>(1)?))
            })?;
            for row in rows {
                labels.push(row?);
            }
        }

        // 4. Format distribution (uses denormalized primary_variant_format)
        let mut formats = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT fmt, COUNT(*) AS cnt FROM facet_match GROUP BY fmt ORDER BY cnt DESC LIMIT 30",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?;
            for row in rows {
                formats.push(row?);
            }
        }

        // 5. Volume distribution — needs every location of every variant.
        // CROSS JOIN pins the join order: the TEMP table has no statistics,
        // and left to itself the planner scanned all file_locations and
        // probed the (small) set per row — 1.4 s for an 1100-asset filter.
        let mut volumes = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT fl.volume_id, vol.label, COUNT(DISTINCT m.id) AS cnt \
                 FROM facet_match m \
                 CROSS JOIN variants v ON v.asset_id = m.id \
                 CROSS JOIN file_locations fl ON fl.content_hash = v.content_hash \
                 CROSS JOIN volumes vol ON vol.id = fl.volume_id \
                 GROUP BY fl.volume_id ORDER BY cnt DESC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, u64>(2)?))
            })?;
            for row in rows {
                volumes.push(row?);
            }
        }

        // 6. Tag distribution (JSON expansion of a.tags). Returns every tag
        // present in the matching set, not just a top-N: the facet sidebar
        // renders these as a hierarchy, and an arbitrary cap chops off
        // lower-frequency siblings — with a knock-on effect of leaving
        // descendants without their parent rows (the JS tree-build then
        // synthesises a count=0 parent, which is visually wrong). The data
        // volume is bounded by the user's actual tag vocabulary; the 5000-
        // row cap is generous (real catalogues we've seen mid-restructure
        // were at ~4500 distinct tags total — far more than would appear
        // in any single filtered result set), but cheap: short strings,
        // SQLite GROUP BY on a denormalised JSON expansion.
        let mut tags = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT je.value, COUNT(DISTINCT m.id) AS cnt \
                 FROM facet_match m CROSS JOIN assets a ON a.id = m.id, json_each(a.tags) AS je \
                 GROUP BY je.value ORDER BY cnt DESC LIMIT 5000",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?;
            for row in rows {
                let (tag_name, count) = row?;
                tags.push((tag_name, count));
            }
        }

        // 7. Year distribution
        let mut years = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT year, COUNT(*) AS cnt FROM facet_match \
                 WHERE year IS NOT NULL GROUP BY year ORDER BY year DESC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?;
            for row in rows {
                years.push(row?);
            }
        }

        // 8. Geotagged count
        let geotagged: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM facet_match WHERE geo = 1",
            [],
            |r| r.get(0),
        )?;

        // Release the memory held by the copy until the next call.
        let _ = self.conn.execute_batch("DELETE FROM facet_match;");

        Ok(FacetCounts {
            total,
            ratings,
            labels,
            formats,
            volumes,
            tags,
            years,
            geotagged,
        })
    }

    /// Get map markers (geotagged assets) matching search filters.
    pub fn map_markers(&self, opts: &SearchOptions, limit: u32) -> Result<(Vec<MapMarker>, u64)> {
        let (where_clause, mut params, needs_fl_join, needs_v_join) = self.build_search_where(opts);

        // Count total geotagged assets matching filters
        let mut count_sql = String::from(
            "SELECT COUNT(DISTINCT a.id) FROM assets a \
             JOIN variants bv ON bv.content_hash = a.best_variant_hash",
        );
        if needs_v_join {
            count_sql.push_str(" JOIN variants v ON v.asset_id = a.id");
        }
        if needs_fl_join {
            count_sql.push_str(" JOIN file_locations fl ON v.content_hash = fl.content_hash");
        }
        count_sql.push_str(&where_clause);
        count_sql.push_str(" AND a.latitude IS NOT NULL AND a.longitude IS NOT NULL");

        let count_param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let total: u64 = self.conn.query_row(&count_sql, count_param_refs.as_slice(), |r| r.get(0))?;

        // Fetch markers
        let mut sql = String::from(
            "SELECT DISTINCT a.id, a.latitude, a.longitude, a.best_variant_hash, \
             COALESCE(a.name, bv.original_filename) as display_name, a.rating, a.color_label \
             FROM assets a \
             JOIN variants bv ON bv.content_hash = a.best_variant_hash",
        );
        if needs_v_join {
            sql.push_str(" JOIN variants v ON v.asset_id = a.id");
        }
        if needs_fl_join {
            sql.push_str(" JOIN file_locations fl ON v.content_hash = fl.content_hash");
        }
        sql.push_str(&where_clause);
        sql.push_str(" AND a.latitude IS NOT NULL AND a.longitude IS NOT NULL");
        sql.push_str(" LIMIT ?");
        params.push(Box::new(limit as u64));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(MapMarker {
                id: row.get(0)?,
                lat: row.get(1)?,
                lng: row.get(2)?,
                preview: row.get(3)?,
                name: row.get(4)?,
                rating: row.get::<_, Option<i64>>(5)?.map(|r| r as u8),
                label: row.get(6)?,
            })
        })?;

        let mut markers = Vec::new();
        for row in rows {
            markers.push(row?);
        }
        Ok((markers, total))
    }

}

#[cfg(test)]
mod tests {
    use crate::catalog::test_support::*;
    use crate::catalog::*;

    #[test]
    fn facet_counts_match_direct_counts_and_survive_reuse() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();
        // (hash, rating, label, format, created_at, tags, lat)
        let rows: Vec<(&str, Option<u8>, Option<&str>, &str, &str, Vec<&str>, Option<f64>)> = vec![
            ("fc1", Some(5), Some("Red"), "nef", "2024-05-01T10:00:00Z", vec!["event|jazz", "pick"], Some(48.1)),
            ("fc2", Some(3), None, "jpg", "2024-06-01T10:00:00Z", vec!["event|jazz"], None),
            ("fc3", None, Some("Red"), "jpg", "2025-01-01T10:00:00Z", vec!["event|rock", "pick"], Some(47.0)),
            ("fc4", Some(3), Some("Blue"), "nef", "2025-02-01T10:00:00Z", vec!["other"], None),
        ];
        for (h, rating, label, fmt, created, tags, lat) in &rows {
            let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, &format!("sha256:{h}"));
            asset.rating = *rating;
            asset.color_label = label.map(|l| l.to_string());
            asset.created_at = chrono::DateTime::parse_from_rfc3339(created).unwrap().with_timezone(&chrono::Utc);
            asset.tags = tags.iter().map(|t| t.to_string()).collect();
            let mut meta = std::collections::BTreeMap::new();
            if let Some(lat) = lat {
                meta.insert("gps_latitude_decimal".to_string(), lat.to_string());
                meta.insert("gps_longitude_decimal".to_string(), "11.5".to_string());
            }
            let variant = crate::models::Variant {
                content_hash: format!("sha256:{h}"),
                asset_id: asset.id,
                role: crate::models::VariantRole::Original,
                format: fmt.to_string(),
                file_size: 1,
                original_filename: format!("{h}.{fmt}"),
                source_metadata: meta,
                locations: vec![],
            };
            asset.variants.push(variant.clone());
            catalog.insert_asset(&asset).unwrap();
            catalog.insert_variant(&variant).unwrap();
        }

        // Unfiltered.
        let f = catalog.facet_counts(&SearchOptions::default()).unwrap();
        assert_eq!(f.total, 4);
        assert_eq!(f.ratings, vec![(None, 1), (Some(3), 2), (Some(5), 1)]);
        assert_eq!(f.labels, vec![(None, 1), (Some("Blue".to_string()), 1), (Some("Red".to_string()), 2)]);
        let fmts: std::collections::HashMap<_, _> = f.formats.iter().cloned().collect();
        assert_eq!(fmts["jpg"], 2);
        assert_eq!(fmts["nef"], 2);
        let tags: std::collections::HashMap<_, _> = f.tags.iter().cloned().collect();
        assert_eq!(tags["event|jazz"], 2);
        assert_eq!(tags["pick"], 2);
        assert_eq!(tags["other"], 1);
        assert_eq!(f.years, vec![("2025".to_string(), 2), ("2024".to_string(), 2)]);
        assert_eq!(f.geotagged, 2);
        assert!(f.volumes.is_empty(), "no file locations registered");

        // Filtered by tag, on the same connection (temp table reused).
        let tags_filter = vec!["event".to_string()];
        let opts = SearchOptions { tags: &tags_filter, ..Default::default() };
        let f = catalog.facet_counts(&opts).unwrap();
        assert_eq!(f.total, 3);
        assert_eq!(f.ratings, vec![(None, 1), (Some(3), 1), (Some(5), 1)]);
        let tags: std::collections::HashMap<_, _> = f.tags.iter().cloned().collect();
        assert_eq!(tags.get("other"), None);
        assert_eq!(tags["event|jazz"], 2);
        assert_eq!(f.geotagged, 2);
        assert_eq!(f.years, vec![("2025".to_string(), 1), ("2024".to_string(), 2)]);

        // And a third call must not see leftovers from the previous set.
        let tags_filter = vec!["other".to_string()];
        let opts = SearchOptions { tags: &tags_filter, ..Default::default() };
        let f = catalog.facet_counts(&opts).unwrap();
        assert_eq!(f.total, 1);
        assert_eq!(f.geotagged, 0);
    }

    #[test]
    fn calendar_years_returns_distinct_years() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let mut a1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cal1");
        a1.created_at = chrono::NaiveDate::from_ymd_opt(2024, 6, 15).unwrap().and_hms_opt(12, 0, 0).unwrap().and_utc();
        let v1 = test_variant(a1.id, "sha256:cal1", "a.jpg");
        a1.variants.push(v1.clone());
        catalog.insert_asset(&a1).unwrap();
        catalog.insert_variant(&v1).unwrap();

        let mut a2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cal2");
        a2.created_at = chrono::NaiveDate::from_ymd_opt(2026, 1, 10).unwrap().and_hms_opt(12, 0, 0).unwrap().and_utc();
        let v2 = test_variant(a2.id, "sha256:cal2", "b.jpg");
        a2.variants.push(v2.clone());
        catalog.insert_asset(&a2).unwrap();
        catalog.insert_variant(&v2).unwrap();

        let years = catalog.calendar_years().unwrap();
        assert_eq!(years, vec![2024, 2026]);
    }

    #[test]
    fn calendar_counts_groups_by_day() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        // Two assets on the same day
        for (hash, hr) in [("sha256:cc1", 10), ("sha256:cc2", 14)] {
            let mut a = crate::models::Asset::new(crate::models::AssetType::Image, hash);
            a.created_at = chrono::NaiveDate::from_ymd_opt(2026, 3, 5).unwrap().and_hms_opt(hr, 0, 0).unwrap().and_utc();
            let v = test_variant(a.id, hash, "x.jpg");
            a.variants.push(v.clone());
            catalog.insert_asset(&a).unwrap();
            catalog.insert_variant(&v).unwrap();
        }

        let opts = SearchOptions::default();
        let counts = catalog.calendar_counts(2026, &opts).unwrap();
        assert_eq!(counts.get("2026-03-05"), Some(&2));
        assert_eq!(counts.len(), 1);

        // Different year returns empty
        let counts2 = catalog.calendar_counts(2025, &opts).unwrap();
        assert!(counts2.is_empty());
    }
}
