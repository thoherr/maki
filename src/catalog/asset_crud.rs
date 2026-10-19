//! `asset_crud` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.

use super::*;

/// The media columns on `assets` that are derived purely from the variant
/// list (`source_metadata`). Computed by [`derive_media_columns`] and
/// written by both `insert_asset` and `update_denormalized_variant_columns`
/// so the two write paths cannot drift.
///
/// `audio_key` / `audio_bpm` belong here too: `maki audio analyze` stores
/// its results in the variant's `source_metadata` (sidecar = source of
/// truth) and re-saves the asset, so the columns are always a projection of
/// the variant metadata, never analyzer-only state.
struct DerivedMediaColumns {
    duration_seconds: Option<f64>,
    video_codec: Option<String>,
    audio_sample_rate: Option<i64>,
    audio_channels: Option<i64>,
    audio_bitrate: Option<i64>,
    audio_key: Option<String>,
    audio_bpm: Option<f64>,
}

fn derive_media_columns(variants: &[Variant]) -> DerivedMediaColumns {
    let first_str = |key: &str| variants.iter().find_map(|v| v.source_metadata.get(key).cloned());
    let first_f64 = |key: &str| variants.iter().find_map(|v| v.source_metadata.get(key)?.parse::<f64>().ok());
    let first_i64 = |key: &str| variants.iter().find_map(|v| v.source_metadata.get(key)?.parse::<i64>().ok());
    DerivedMediaColumns {
        // Duration is shared across media types: video variants carry
        // `video_duration`, audio variants `audio_duration` (both seconds).
        duration_seconds: first_f64("video_duration").or_else(|| first_f64("audio_duration")),
        video_codec: first_str("video_codec"),
        audio_sample_rate: first_i64("audio_sample_rate"),
        audio_channels: first_i64("audio_channels"),
        audio_bitrate: first_i64("audio_bitrate"),
        audio_key: first_str("audio_key"),
        audio_bpm: first_f64("audio_bpm"),
    }
}

impl Catalog {
    // ═══ ASSET CRUD ═══

    /// Insert (or upsert) an asset into the catalog.
    ///
    /// **Invariant: `asset.variants` must be populated before calling.**
    /// The denormalized columns (`best_variant_hash`,
    /// `primary_variant_format`, `variant_count`, `latitude`/`longitude`,
    /// `duration_seconds`/`video_codec`/`audio_*`) are computed here from the variant
    /// list — inserting an asset and filling its variants in afterwards
    /// leaves them empty/stale. This is the known trap in test setup:
    /// `setup_search_catalog()` / `setup_metadata_catalog()` populate
    /// `asset.variants` first for exactly this reason.
    pub fn insert_asset(&self, asset: &Asset) -> Result<()> {
        let tags_json = serde_json::to_string(&asset.tags)?;
        let tag_sources_json = serde_json::to_string(&asset.tag_sources)?;
        let best_hash = crate::models::variant::compute_best_variant_hash_with_override(
            &asset.variants,
            asset.preview_variant.as_deref(),
        );
        let primary_format = crate::models::variant::compute_primary_format(&asset.variants);
        let variant_count = asset.variants.len() as i64;
        let (latitude, longitude) = crate::models::variant::compute_gps_from_variants(&asset.variants);
        let DerivedMediaColumns {
            duration_seconds,
            video_codec,
            audio_sample_rate,
            audio_channels,
            audio_bitrate,
            audio_key,
            audio_bpm,
        } = derive_media_columns(&asset.variants);
        // Leaf tag count — denormalised for the `tagcount:` search filter.
        // Computed from the current tags list, so any call that saves the
        // asset (tag add/remove/rename/split/clear/reimport) picks up the
        // new count for free.
        let leaf_tag_count = crate::tag_util::leaf_tag_count(&asset.tags) as i64;
        // Use ON CONFLICT UPDATE instead of INSERT OR REPLACE to avoid
        // intermediate DELETE that triggers FK constraint violations on
        // variants/faces/collection_assets referencing this asset.
        self.conn.execute(
            "INSERT INTO assets (id, name, created_at, asset_type, tags, description, rating, color_label, best_variant_hash, primary_variant_format, variant_count, latitude, longitude, preview_rotation, preview_variant, duration_seconds, video_codec, audio_sample_rate, audio_channels, audio_bitrate, audio_key, audio_bpm, face_scan_status, leaf_tag_count, tag_sources) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25) \
             ON CONFLICT(id) DO UPDATE SET \
               name = excluded.name, \
               created_at = excluded.created_at, \
               asset_type = excluded.asset_type, \
               tags = excluded.tags, \
               tag_sources = excluded.tag_sources, \
               description = excluded.description, \
               rating = excluded.rating, \
               color_label = excluded.color_label, \
               best_variant_hash = excluded.best_variant_hash, \
               primary_variant_format = excluded.primary_variant_format, \
               variant_count = excluded.variant_count, \
               latitude = excluded.latitude, \
               longitude = excluded.longitude, \
               preview_rotation = excluded.preview_rotation, \
               preview_variant = excluded.preview_variant, \
               duration_seconds = excluded.duration_seconds, \
               video_codec = excluded.video_codec, \
               audio_sample_rate = excluded.audio_sample_rate, \
               audio_channels = excluded.audio_channels, \
               audio_bitrate = excluded.audio_bitrate, \
               audio_key = excluded.audio_key, \
               audio_bpm = excluded.audio_bpm, \
               face_scan_status = excluded.face_scan_status, \
               leaf_tag_count = excluded.leaf_tag_count",
            rusqlite::params![
                asset.id.to_string(),
                asset.name,
                asset.created_at.to_rfc3339(),
                format!("{:?}", asset.asset_type).to_lowercase(),
                tags_json,
                asset.description,
                asset.rating.map(|r| r as i64),
                asset.color_label,
                best_hash,
                primary_format,
                variant_count,
                latitude,
                longitude,
                asset.preview_rotation.map(|r| r as i64),
                asset.preview_variant,
                duration_seconds,
                video_codec,
                audio_sample_rate,
                audio_channels,
                audio_bitrate,
                audio_key,
                audio_bpm,
                asset.face_scan_status.as_deref(),
                leaf_tag_count,
                tag_sources_json,
            ],
        )?;
        Ok(())
    }

    /// Update just the rating for an asset in the catalog.
    pub fn update_asset_rating(&self, asset_id: &str, rating: Option<u8>) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET rating = ?1 WHERE id = ?2",
            rusqlite::params![rating.map(|r| r as i64), asset_id],
        )?;
        Ok(())
    }

    /// Update just the color label for an asset in the catalog.
    pub fn update_asset_color_label(&self, asset_id: &str, color_label: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET color_label = ?1 WHERE id = ?2",
            rusqlite::params![color_label, asset_id],
        )?;
        Ok(())
    }

    /// Update just the preview rotation for an asset in the catalog.
    pub fn update_asset_preview_rotation(&self, asset_id: &str, rotation: Option<u16>) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET preview_rotation = ?1 WHERE id = ?2",
            rusqlite::params![rotation.map(|r| r as i64), asset_id],
        )?;
        Ok(())
    }

    /// Update the preview variant override and recompute best_variant_hash.
    pub fn update_asset_preview_variant(
        &self,
        asset_id: &str,
        preview_variant: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET preview_variant = ?1 WHERE id = ?2",
            rusqlite::params![preview_variant, asset_id],
        )?;
        // Recompute best_variant_hash: if override set, use it; else fall back to scoring
        if let Some(hash) = preview_variant {
            self.conn.execute(
                "UPDATE assets SET best_variant_hash = ?1 WHERE id = ?2 AND EXISTS (SELECT 1 FROM variants WHERE content_hash = ?1 AND asset_id = ?2)",
                rusqlite::params![hash, asset_id],
            )?;
        } else {
            // Clear override — recompute from scoring via SQL
            self.conn.execute(
                "UPDATE assets SET best_variant_hash = (
                    SELECT content_hash FROM variants WHERE asset_id = ?1
                    ORDER BY
                        CASE role WHEN 'export' THEN 300 WHEN 'processed' THEN 200
                            WHEN 'original' THEN 100 ELSE 0 END +
                        CASE WHEN LOWER(format) IN ('jpg','jpeg','png','tiff','tif','webp')
                            THEN 50 ELSE 0 END +
                        MIN(file_size / 1000000, 49)
                    DESC LIMIT 1
                ) WHERE id = ?1",
                rusqlite::params![asset_id],
            )?;
        }
        Ok(())
    }

    /// Update the denormalized face_count for an asset.
    /// Recomputes from the faces table (requires faces table to exist).
    pub fn update_face_count(&self, asset_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET face_count = (SELECT COUNT(*) FROM faces WHERE asset_id = ?1) WHERE id = ?1",
            rusqlite::params![asset_id],
        )?;
        Ok(())
    }

    /// Mark a single asset's face-scan status as done if it carries face
    /// rows but its sidecar predates the `face_scan_status` field (legacy
    /// upgrade fallback — pre-v4.4.3 sidecars). No-op otherwise.
    pub fn backfill_face_scan_status(&self, asset_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET face_scan_status = 'done' \
             WHERE id = ?1 AND face_scan_status IS NULL AND face_count > 0",
            rusqlite::params![asset_id],
        )?;
        Ok(())
    }

    /// Bulk variant of [`Self::update_face_count`] +
    /// [`Self::backfill_face_scan_status`]: recompute `face_count` for
    /// every asset that has face rows, then mark legacy assets (sidecars
    /// predating `face_scan_status`) as scanned. Used by rebuild-catalog
    /// after restoring faces from YAML.
    pub fn backfill_face_denormalization(&self) -> Result<()> {
        self.conn.execute_batch(
            "UPDATE assets SET face_count = (
                SELECT COUNT(*) FROM faces WHERE faces.asset_id = assets.id
            ) WHERE id IN (SELECT DISTINCT asset_id FROM faces)",
        )?;
        self.conn.execute_batch(
            "UPDATE assets SET face_scan_status = 'done' \
             WHERE face_scan_status IS NULL AND face_count > 0",
        )?;
        Ok(())
    }

    /// Mark an asset as having been scanned for faces (regardless of face count).
    /// Used by `maki faces detect` to avoid re-scanning zero-face assets on every run.
    pub fn mark_face_scan_done(&self, asset_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET face_scan_status = 'done' WHERE id = ?1",
            rusqlite::params![asset_id],
        )?;
        Ok(())
    }

    /// Clear the face-scan-done flag for an asset. Used by `--force` to ensure
    /// the asset gets re-scanned even if it had been marked done previously.
    pub fn clear_face_scan_status(&self, asset_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET face_scan_status = NULL WHERE id = ?1",
            rusqlite::params![asset_id],
        )?;
        Ok(())
    }

    /// Check whether an asset has been scanned for faces (regardless of whether
    /// any were found). Returns true if `face_scan_status = 'done'`.
    pub fn is_face_scan_done(&self, asset_id: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM assets WHERE id = ?1 AND face_scan_status = 'done'",
                rusqlite::params![asset_id],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// Update just the created_at date for an asset in the catalog.
    pub fn update_asset_created_at(&self, asset_id: &str, created_at: &chrono::DateTime<chrono::Utc>) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![created_at.to_rfc3339(), asset_id],
        )?;
        Ok(())
    }

    /// Update denormalized variant columns for an asset.
    pub fn update_best_variant_hash(&self, asset_id: &str, hash: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET best_variant_hash = ?1 WHERE id = ?2",
            rusqlite::params![hash, asset_id],
        )?;
        Ok(())
    }

    /// Update all denormalized variant columns from an asset's variants:
    /// `best_variant_hash`, `primary_variant_format`, `variant_count`,
    /// `latitude`/`longitude`, and the media columns (`duration_seconds`,
    /// `video_codec`, `audio_*`) — the same derivation as `insert_asset`.
    pub fn update_denormalized_variant_columns(&self, asset: &Asset) -> Result<()> {
        let best_hash = crate::models::variant::compute_best_variant_hash_with_override(
            &asset.variants,
            asset.preview_variant.as_deref(),
        );
        let primary_format = crate::models::variant::compute_primary_format(&asset.variants);
        let variant_count = asset.variants.len() as i64;
        let (latitude, longitude) = crate::models::variant::compute_gps_from_variants(&asset.variants);
        let media = derive_media_columns(&asset.variants);
        self.conn.execute(
            "UPDATE assets SET best_variant_hash = ?1, primary_variant_format = ?2, variant_count = ?3, \
             latitude = ?4, longitude = ?5, duration_seconds = ?6, video_codec = ?7, \
             audio_sample_rate = ?8, audio_channels = ?9, audio_bitrate = ?10, audio_key = ?11, audio_bpm = ?12 \
             WHERE id = ?13",
            rusqlite::params![
                best_hash,
                primary_format,
                variant_count,
                latitude,
                longitude,
                media.duration_seconds,
                media.video_codec,
                media.audio_sample_rate,
                media.audio_channels,
                media.audio_bitrate,
                media.audio_key,
                media.audio_bpm,
                asset.id.to_string(),
            ],
        )?;
        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AssetType, VariantRole};

    fn variant(hash: &str, asset_id: uuid::Uuid, meta: &[(&str, &str)]) -> Variant {
        Variant {
            content_hash: hash.to_string(),
            asset_id,
            role: VariantRole::Original,
            format: "mp4".to_string(),
            file_size: 100,
            original_filename: "clip.mp4".to_string(),
            source_metadata: meta.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            locations: vec![],
        }
    }

    type MediaRow = (Option<f64>, Option<String>, Option<i64>, Option<i64>, Option<i64>, Option<String>, Option<f64>);

    fn media_row(catalog: &Catalog, id: &str) -> MediaRow {
        catalog.conn.query_row(
            "SELECT duration_seconds, video_codec, audio_sample_rate, audio_channels, audio_bitrate, audio_key, audio_bpm \
             FROM assets WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        ).unwrap()
    }

    /// Regression: the helper refreshed only hash/format/count/GPS, leaving
    /// the media columns stale after a variant's metadata changed.
    #[test]
    fn update_denormalized_variant_columns_refreshes_media_columns() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let mut asset = Asset::new(AssetType::Video, "sha256:media1");
        asset.variants.push(variant("sha256:media1", asset.id, &[
            ("video_duration", "12.5"),
            ("video_codec", "h264"),
            ("audio_sample_rate", "44100"),
            ("audio_channels", "2"),
            ("audio_bitrate", "128"),
            ("audio_key", "Am"),
            ("audio_bpm", "120.0"),
        ]));
        catalog.insert_asset(&asset).unwrap();
        for v in &asset.variants {
            catalog.insert_variant(v).unwrap();
        }
        let id = asset.id.to_string();
        assert_eq!(
            media_row(&catalog, &id),
            (Some(12.5), Some("h264".into()), Some(44100), Some(2), Some(128), Some("Am".into()), Some(120.0))
        );

        // Change every media value on the variant and refresh.
        asset.variants[0].source_metadata = [
            ("video_duration", "30.0"),
            ("video_codec", "hevc"),
            ("audio_sample_rate", "48000"),
            ("audio_channels", "6"),
            ("audio_bitrate", "320"),
            ("audio_key", "C"),
            ("audio_bpm", "90.5"),
        ].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        catalog.update_denormalized_variant_columns(&asset).unwrap();
        assert_eq!(
            media_row(&catalog, &id),
            (Some(30.0), Some("hevc".into()), Some(48000), Some(6), Some(320), Some("C".into()), Some(90.5))
        );

        // Metadata removed entirely → columns cleared, exactly like a fresh insert_asset.
        asset.variants[0].source_metadata.clear();
        catalog.update_denormalized_variant_columns(&asset).unwrap();
        assert_eq!(media_row(&catalog, &id), (None, None, None, None, None, None, None));

        // Audio-only variant: duration comes from `audio_duration`.
        asset.variants[0].source_metadata.insert("audio_duration".into(), "200".into());
        catalog.update_denormalized_variant_columns(&asset).unwrap();
        assert_eq!(media_row(&catalog, &id).0, Some(200.0));
    }
}
