//! `Recipe` — a sidecar edit file (XMP, .CR3 .xmp, etc.) attached to a
//! variant. Tracks the file's content hash so MAKI can detect when a
//! downstream editor (Lightroom / Capture One) modified it.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::volume::FileLocation;

/// The type of processing recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeType {
    Sidecar,
    EmbeddedExport,
}

impl RecipeType {
    /// The serde / SQLite spelling (`embedded_export`, not the former
    /// Debug-derived `embeddedexport`; no such row has ever been written).
    pub fn as_str(&self) -> &'static str {
        match self {
            RecipeType::Sidecar => "sidecar",
            RecipeType::EmbeddedExport => "embedded_export",
        }
    }
}

impl std::str::FromStr for RecipeType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "sidecar" => Ok(RecipeType::Sidecar),
            "embedded_export" => Ok(RecipeType::EmbeddedExport),
            _ => Err(format!("unknown recipe type '{s}'")),
        }
    }
}

/// Processing instructions associated with a variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub id: Uuid,
    pub variant_hash: String,
    pub software: String,
    pub recipe_type: RecipeType,
    pub content_hash: String,
    pub location: FileLocation,
    /// True when metadata was edited in the DAM but the XMP file could not be
    /// updated (e.g. because the volume was offline). Cleared after successful
    /// write-back.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pending_writeback: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}
