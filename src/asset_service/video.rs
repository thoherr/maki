//! `video` section of `AssetService` — methods extracted from the original
//! 8.9-kLOC asset_service.rs into a multi-file `impl AssetService` block.
//!
//! Free functions and result types live in the parent `asset_service` module.

use super::*;

impl AssetService {
    // ═══ VIDEO METADATA ═══

    /// Backfill video metadata (duration, codec, resolution, framerate) via ffprobe.
    ///
    /// Updates variant source_metadata in both the YAML sidecar and SQLite
    /// catalog. `Ok(true)` when metadata was added, `Ok(false)` when there
    /// was nothing to add, the error when a store write fails (audit F8:
    /// callers decide how to report it).
    pub fn backfill_video_metadata(
        &self,
        asset_id: &str,
        variant_hash: &str,
        source_path: &std::path::Path,
    ) -> Result<bool> {
        let video_meta = crate::preview::extract_video_metadata(source_path);
        self.merge_variant_metadata(asset_id, variant_hash, video_meta)
    }

    /// Backfill audio metadata (duration, bitrate, sample rate, channels,
    /// embedded title/artist/album) via lofty. Audio analog of
    /// [`Self::backfill_video_metadata`] — same sidecar + catalog update.
    pub fn backfill_audio_metadata(
        &self,
        asset_id: &str,
        variant_hash: &str,
        source_path: &std::path::Path,
    ) -> Result<bool> {
        let audio_meta = crate::preview::extract_audio_source_metadata(source_path);
        self.merge_variant_metadata(asset_id, variant_hash, audio_meta)
    }

    /// Merge extracted metadata into a variant's source_metadata, updating
    /// sidecar (source of truth) first and then the catalog rows in one
    /// transaction. Returns `Ok(true)` when something was written, `Ok(false)`
    /// when there was nothing to merge or the variant is not on this asset,
    /// and the error when either store write fails (audit F8: formerly
    /// swallowed with `.ok()`).
    pub(crate) fn merge_variant_metadata(
        &self,
        asset_id: &str,
        variant_hash: &str,
        meta: std::collections::HashMap<String, String>,
    ) -> Result<bool> {
        if meta.is_empty() {
            return Ok(false);
        }
        let store = MetadataStore::new(&self.catalog_root);
        let uuid: uuid::Uuid = asset_id
            .parse()
            .with_context(|| format!("invalid asset id '{asset_id}'"))?;
        let mut asset = store.load(uuid)?;
        let Some(v) = asset.variants.iter_mut().find(|v| v.content_hash == variant_hash) else {
            return Ok(false);
        };
        v.source_metadata.extend(meta);
        let variant = v.clone();

        store.save(&asset)?;
        let catalog = crate::catalog::Catalog::open(&self.catalog_root)?;
        catalog.with_tx(|c| {
            c.insert_variant(&variant)?;
            c.insert_asset(&asset)?;
            Ok(())
        })?;
        Ok(true)
    }
}
