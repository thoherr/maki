//! AI-gated route handlers, split by domain.
//!
//! All items are `#[cfg(feature = "ai")]` — this module is only compiled
//! when the `ai` feature is enabled. The parent `routes::mod` declares it
//! behind the same feature gate.
//!
//! Submodules:
//! - [`tags`] — `suggest-tags`, batch `auto-tag`.
//! - [`embed`] — standalone embed (browse toolbar, asset detail).
//! - [`similarity`] — `find-similar` and stack-by-similarity.
//! - [`faces`] — face detection, person assignment, people page.
//! - [`stroll`] — visual exploration page.
//!
//! Shared helpers (model dir resolution, label loading) live here so each
//! submodule can use them via `super::`.

mod embed;
mod faces;
mod similarity;
mod stroll;
mod tags;

pub use embed::*;
pub use faces::*;
pub use similarity::*;
pub use stroll::*;
pub use tags::*;

/// Resolve the directory holding the active SigLIP model.
///
/// Re-exported for `web/routes/browse.rs` (used by similar-search resolution).
/// Submodules call this via `super::resolve_model_dir`.
pub(super) fn resolve_model_dir(config: &crate::config::AiConfig) -> std::path::PathBuf {
    crate::config::resolve_model_dir(&config.model_dir, &config.model)
}

/// Load the active vocabulary — labels the model is scored against,
/// plus the (possibly identity) mapping from those flat labels to
/// hierarchical MAKI tags.
///
/// Resolution order:
/// 1. `[ai].labels = "x.yaml"` (or `.yml`) — full vocabulary with mapping
/// 2. `[ai].labels = "x.txt"` (or any other extension) — flat labels,
///    identity mapping (preserves pre-v4.5.x semantics for users who
///    haven't migrated to the YAML format yet)
/// 3. neither — built-in [`default_vocabulary`] (96 labels organised
///    by facet, see `src/default-vocabulary.yaml`)
pub(super) fn resolve_vocabulary(
    config: &crate::config::AiConfig,
) -> Result<crate::ai_vocabulary::Vocabulary, String> {
    if let Some(ref labels_path) = config.labels {
        crate::ai_vocabulary::load_from_path(std::path::Path::new(labels_path))
            .map_err(|e| format!("Failed to load vocabulary: {e:#}"))
    } else {
        Ok(crate::ai_vocabulary::default_vocabulary())
    }
}
