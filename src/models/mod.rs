//! Shared domain types — `Asset`, `Variant`, `Recipe`, `Volume`,
//! `FileLocation`, `AssetType`, etc. Pure data; no DB or filesystem deps.

pub mod asset;
pub mod recipe;
pub mod variant;
pub mod volume;

pub use asset::{Asset, AssetType};
pub use recipe::{Recipe, RecipeType};
pub use variant::{Variant, VariantRole};
pub use volume::{FileLocation, Volume, VolumePurpose, VolumeType};
