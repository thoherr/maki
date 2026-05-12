//! Public library crate for MAKI — re-exports the modules the binary
//! (`src/main.rs`) builds on, plus the embedded HTML templates.
//!
//! The crate is organised by domain:
//! - `catalog` — SQLite cache of asset/variant/recipe state
//! - `metadata_store` — YAML sidecars (source of truth)
//! - `asset_service` — write-side workflows (import, sync, dedup, …)
//! - `query` — search query parsing + write-path operations on assets
//! - `web` — `maki serve` HTTP layer (Axum + Askama)
//! - `models` — shared domain types (`Asset`, `Variant`, `Volume`, …)
//!
//! All public APIs are re-exported through the top-level `maki::` path so
//! external callers don't depend on internal module structure.

/// Controls the level of diagnostic output sent to stderr.
///
/// - `verbose`: operational decisions and program flow
/// - `debug`: low-level details (external commands, API payloads); implies verbose
#[derive(Clone, Copy, Default)]
pub struct Verbosity {
    pub verbose: bool,
    pub debug: bool,
}

impl Verbosity {
    pub fn new(verbose: bool, debug: bool) -> Self {
        Self {
            verbose: verbose || debug,
            debug,
        }
    }

    /// True when verbose or debug output is enabled.
    pub fn verbose(&self) -> bool {
        self.verbose
    }

    /// True when debug output is enabled.
    pub fn debug(&self) -> bool {
        self.debug
    }

    /// Shorthand for no output.
    pub fn quiet() -> Self {
        Self { verbose: false, debug: false }
    }
}

pub mod asset_service;
pub mod catalog;
pub mod cli_output;
pub mod collection;
pub mod config;
pub mod contact_sheet;
pub mod content_store;
pub mod device_registry;
pub mod embedded_xmp;
pub mod exif_reader;
pub mod format;
pub mod metadata_store;
pub mod models;
pub mod preview;
pub mod query;
pub mod saved_search;
pub mod shell;
pub mod stack;
pub mod status;
pub mod tag_util;
pub mod vlm;
pub mod vocabulary;
pub mod web;
pub mod xmp_reader;

#[cfg(feature = "ai")]
pub mod ai;
#[cfg(feature = "ai")]
pub mod ai_vocabulary;
#[cfg(feature = "ai")]
pub mod embedding_store;
#[cfg(feature = "ai")]
pub mod face;
#[cfg(feature = "ai")]
pub mod face_store;
#[cfg(feature = "ai")]
pub mod model_manager;
