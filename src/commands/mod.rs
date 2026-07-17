//! Per-command handlers — one `run_X_command` function per CLI command.
//!
//! Extracted from main.rs (which used to be 9.7 kLOC of CLI parsing +
//! dispatch + handlers all jumbled together) so main.rs can be a
//! near-pure dispatcher + CLI entrypoint. Each handler takes the
//! destructured command fields by value plus `json`, `log`, `verbosity`
//! for the small set of cross-cutting CLI flags it needs.
//!
//! Split into one submodule per command family; this `mod.rs` holds the
//! shared imports (submodules start with `use super::*;`) and the
//! cross-family `merge_trailing_ids` helper. All handlers are
//! re-exported flat, so callers keep using `commands::run_x_command`.

use std::path::PathBuf;

use maki::asset_service::AssetService;
use maki::catalog::Catalog;
use maki::cli_output::{format_duration, format_size, item_status};
#[cfg(feature = "ai")]
use maki::config::CatalogConfig;
use maki::device_registry::DeviceRegistry;
use maki::metadata_store::MetadataStore;
use maki::query::QueryEngine;

#[cfg(feature = "ai")]
use crate::FacesCommands;
#[cfg(feature = "ai")]
use crate::AiCommands;
use crate::{
    CollectionCommands, SavedSearchCommands, StackCommands, TagCommands, TrashCommands,
    VolumeCommands,
};

// `ai` and `faces` are fully feature-gated inside; gate the module
// declarations too so non-AI builds don't warn about empty glob imports.
#[cfg(any(feature = "ai", feature = "pro"))]
mod ai;
#[cfg(any(feature = "ai", feature = "pro"))]
pub use ai::*;
mod audio;
pub use audio::*;
mod export;
pub use export::*;
#[cfg(feature = "ai")]
mod faces;
#[cfg(feature = "ai")]
pub use faces::*;
mod ingest;
pub use ingest::*;
mod maintain;
pub use maintain::*;
mod misc;
pub use misc::*;
mod organize;
pub use organize::*;
mod status;
pub use status::*;
mod sync;
pub use sync::*;
mod tag;
pub use tag::*;
mod volume;
pub use volume::*;

/// Merge trailing asset IDs (from shell variable expansion) into query/asset.
/// Single ID → asset; multiple IDs → `id:xxx id:yyy` query.
#[cfg(any(feature = "pro", feature = "ai"))]
pub fn merge_trailing_ids(
    query: Option<String>,
    asset: Option<String>,
    asset_ids: &[String],
) -> (Option<String>, Option<String>) {
    if asset_ids.is_empty() {
        return (query, asset);
    }
    if asset_ids.len() == 1 {
        return (query, Some(asset_ids[0].clone()));
    }
    let id_query = asset_ids.iter().map(|id| format!("id:{id}")).collect::<Vec<_>>().join(" ");
    let combined = match query {
        Some(q) => format!("{q} {id_query}"),
        None => id_query,
    };
    (Some(combined), asset)
}
