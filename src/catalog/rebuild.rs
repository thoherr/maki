//! `rebuild` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.

use super::*;

impl Catalog {
    // ═══ REBUILD ═══

    /// Drop and recreate data tables (assets, variants, file_locations, recipes).
    /// Keeps the volumes table intact. Ensures the schema is up to date.
    pub fn rebuild(&self) -> Result<()> {
        self.conn.execute_batch(
            "DROP TABLE IF EXISTS schema_version;
             DROP TABLE IF EXISTS faces;
             DROP TABLE IF EXISTS people;
             DROP TABLE IF EXISTS embeddings;
             DROP TABLE IF EXISTS collection_assets;
             DROP TABLE IF EXISTS collections;
             DROP TABLE IF EXISTS stacks;
             DROP TABLE IF EXISTS file_locations;
             DROP TABLE IF EXISTS recipes;
             DROP TABLE IF EXISTS variants;
             DROP TABLE IF EXISTS assets;",
        )?;
        self.initialize()?;
        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use crate::catalog::test_support::*;
    use crate::catalog::*;

    #[test]
    fn rebuild_clears_data_rows() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        // Insert asset + variant + location
        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:rebuild1");
        catalog.insert_asset(&asset).unwrap();

        let volume = crate::models::Volume::new(
            "vol".to_string(),
            std::path::PathBuf::from("/mnt/vol"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();

        let variant = crate::models::Variant { file_size: 100, ..test_variant(asset.id, "sha256:rebuild1", "test.png") };
        catalog.insert_variant(&variant).unwrap();

        let loc = crate::models::FileLocation {
            volume_id: volume.id,
            relative_path: std::path::PathBuf::from("test.png"),
            verified_at: None,
        };
        catalog.insert_file_location(&variant.content_hash, &loc).unwrap();

        // Rebuild should clear data rows
        catalog.rebuild().unwrap();

        let count = |table: &str| -> i64 {
            catalog
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
                .unwrap()
        };

        assert_eq!(count("assets"), 0);
        assert_eq!(count("variants"), 0);
        assert_eq!(count("file_locations"), 0);
        // Volumes should be preserved
        assert_eq!(count("volumes"), 1);
    }
}
