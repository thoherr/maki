//! Shared domain types — `Asset`, `Variant`, `Recipe`, `Volume`,
//! `FileLocation`, `AssetType`, etc. — and the pure vocabulary around
//! them (`format` extension tables, `NumericFilter`, `geo`). Pure data
//! and pure functions: this module imports nothing from storage,
//! services or readers (checked by the architecture audit, F11).

pub mod asset;
pub mod format;
pub mod geo;
pub mod numeric_filter;
pub mod recipe;
pub mod variant;
pub mod volume;

pub use asset::{Asset, AssetType, TagSource};
pub use numeric_filter::NumericFilter;
pub use recipe::{Recipe, RecipeType};
pub use variant::{Variant, VariantRole};
pub use volume::{FileLocation, Volume, VolumePurpose, VolumeType};

// ═══ ENUM ↔ STRING AT THE SQL BOUNDARY ═══
//
// `as_str()` / `FromStr` mirror the serde names (`snake_case`) so SQLite
// columns, YAML sidecars and JSON output all spell a variant the same way.
// `AssetType` is declared in `asset.rs`; its impl lives here with the
// other three so the four tables stay side by side.

impl AssetType {
    /// The serde / SQLite spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            AssetType::Image => "image",
            AssetType::Video => "video",
            AssetType::Audio => "audio",
            AssetType::Document => "document",
            AssetType::Other => "other",
        }
    }
}

impl std::str::FromStr for AssetType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "image" => Ok(AssetType::Image),
            "video" => Ok(AssetType::Video),
            "audio" => Ok(AssetType::Audio),
            "document" => Ok(AssetType::Document),
            "other" => Ok(AssetType::Other),
            _ => Err(format!("unknown asset type '{s}'")),
        }
    }
}

#[cfg(test)]
mod enum_str_tests {
    use super::*;

    fn round_trip<T>(all: &[T])
    where
        T: serde::Serialize + serde::de::DeserializeOwned + std::str::FromStr + PartialEq + std::fmt::Debug,
        <T as std::str::FromStr>::Err: std::fmt::Debug,
    {
        for v in all {
            let json = serde_json::to_value(v).unwrap();
            let s = json.as_str().expect("serde emits a plain string");
            let parsed: T = s.parse().unwrap();
            assert_eq!(&parsed, v, "FromStr must invert serde for {v:?}");
            let back: T = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(&back, v);
        }
    }

    #[test]
    fn asset_type_as_str_matches_serde() {
        let all = [AssetType::Image, AssetType::Video, AssetType::Audio, AssetType::Document, AssetType::Other];
        for v in &all {
            assert_eq!(serde_json::to_value(v).unwrap(), serde_json::json!(v.as_str()));
        }
        round_trip(&all);
        assert!("picture".parse::<AssetType>().is_err());
    }

    #[test]
    fn variant_role_as_str_matches_serde() {
        let all = [
            VariantRole::Original,
            VariantRole::Alternate,
            VariantRole::Processed,
            VariantRole::Export,
            VariantRole::Sidecar,
        ];
        for v in &all {
            assert_eq!(serde_json::to_value(v).unwrap(), serde_json::json!(v.as_str()));
        }
        round_trip(&all);
        assert!("master".parse::<VariantRole>().is_err());
    }

    #[test]
    fn recipe_type_as_str_matches_serde() {
        let all = [RecipeType::Sidecar, RecipeType::EmbeddedExport];
        for v in &all {
            assert_eq!(serde_json::to_value(v).unwrap(), serde_json::json!(v.as_str()));
        }
        assert_eq!(RecipeType::EmbeddedExport.as_str(), "embedded_export");
        round_trip(&all);
        assert!("embeddedexport".parse::<RecipeType>().is_err());
    }

    #[test]
    fn volume_type_as_str_matches_serde() {
        let all = [VolumeType::Local, VolumeType::External, VolumeType::Network];
        for v in &all {
            assert_eq!(serde_json::to_value(v).unwrap(), serde_json::json!(v.as_str()));
        }
        round_trip(&all);
        assert!("cloud".parse::<VolumeType>().is_err());
    }
}
