//! HTTP route handlers for the `maki serve` web UI.
//!
//! Each submodule handles a resource domain (browse, assets, tags, stacks,
//! collections, saved_search, calendar_map, duplicates, import, media, stats,
//! volumes, ai). This `mod.rs` only holds shared helpers used across submodules.

use crate::device_registry::DeviceRegistry;
use crate::query::{normalize_path_for_search, parse_search_query, ParsedSearch};

use super::AppState;

#[cfg(feature = "ai")]
mod ai;
#[cfg(feature = "ai")]
pub use ai::*;
mod assets;
pub use assets::*;
mod browse;
pub use browse::*;
mod calendar_map;
pub use calendar_map::*;
mod collections;
pub use collections::*;
mod duplicates;
pub use duplicates::*;
mod import;
pub use import::*;
mod config;
pub use config::*;
mod jobs;
pub use jobs::*;
mod maintain;
pub use maintain::*;
mod media;
pub use media::*;
mod saved_search;
pub use saved_search::*;
mod stacks;
pub use stacks::*;
mod stats;
pub use stats::*;
mod tags;
pub use tags::*;
mod volumes;
pub use volumes::*;

/// Run a blocking catalog operation on the spawn_blocking pool, mapping
/// the spawn-join error and the inner anyhow error to a uniform 500
/// response so handlers can use `?` instead of triple-matching.
///
/// Replaces the `tokio::task::spawn_blocking(...).await` + `match
/// Ok(Ok)/Ok(Err)/Err` chain repeated across every web route. Use as:
///
/// ```ignore
/// pub async fn handler(State(state): State<Arc<AppState>>) -> Result<Response, Response> {
///     let value = super::spawn_catalog_blocking(move || {
///         // anyhow::Result<T>
///         do_work(&state)
///     })
///     .await?;
///     Ok(Json(value).into_response())
/// }
/// ```
///
/// Returns `Result<T, Response>` so callers can keep choice over the
/// success-shape (Json/Html/redirect/etc.) while the error path is uniform.
pub(super) async fn spawn_catalog_blocking<T, F>(f: F) -> Result<T, axum::response::Response>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {e:#}")).into_response(),
        ),
        Err(e) => Err(
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {e}")).into_response(),
        ),
    }
}

/// Append `HX-Trigger: pending-changed` to a successful HTML response
/// from any metadata-edit endpoint (rating, description, name, label,
/// tags). The asset detail page's recipes block listens for this event
/// and refreshes itself so the pending_writeback markers update
/// immediately — otherwise they're stale until the next full reload.
pub(super) fn with_pending_trigger(html: String) -> axum::response::Response {
    use axum::response::{Html, IntoResponse};
    let mut resp = Html(html).into_response();
    resp.headers_mut().insert(
        "HX-Trigger",
        axum::http::HeaderValue::from_static("pending-changed"),
    );
    resp
}

/// Resolve an asset ID prefix to its full ID, mapping "not found" to a
/// uniform error.
///
/// Replaces the `catalog.resolve_asset_id(...)?` + `.ok_or_else(...)` boilerplate
/// repeated across every web route that mutates a single asset. Callers that
/// need a `String` error (e.g. inner functions returning `Result<_, String>`)
/// can `.map_err(|e| format!("{e:#}"))` at the call site.
pub(super) fn resolve_asset_id_or_err(
    catalog: &crate::catalog::Catalog,
    prefix: &str,
) -> anyhow::Result<String> {
    catalog
        .resolve_asset_id(prefix)?
        .ok_or_else(|| anyhow::anyhow!("no asset found matching '{prefix}'"))
}

/// Resolve the best variant index for an asset, respecting user override.
/// Looks up the stored best_variant_hash, falls back to algorithmic scoring.
pub(super) fn resolve_best_variant_idx(
    catalog: &crate::catalog::Catalog,
    asset_id: &str,
    variants: &[crate::catalog::VariantDetails],
) -> anyhow::Result<usize> {
    let stored_hash = catalog.get_asset_best_variant_hash(asset_id).unwrap_or(None);
    stored_hash.as_ref()
        .and_then(|h| variants.iter().position(|v| &v.content_hash == h))
        .or_else(|| crate::models::variant::best_preview_index_details(variants))
        .ok_or_else(|| anyhow::anyhow!("asset has no variants"))
}

/// Resolve the `similar:` filter into `(ordered_ids, score_map)`.
///
/// Two forms:
/// - `similar:<asset-id>` — the stored embedding of a catalog asset; the
///   source asset is pinned first at 100%.
/// - `similar:@<token>` — an uploaded query image (see
///   `routes::ai::query_image`); its exact content-hash match, if any, is
///   pinned first, then the embedding neighbours. An expired token resolves
///   to an empty list (the browse JS shows a "drop it again" notice).
///
/// Returns `Ok(None)` when no `similar:` filter is active or the configured
/// model is unknown (filter ignored, historical behavior); `Ok(Some(..))`
/// whenever the filter resolved — possibly to an empty list, which the
/// caller installs so that "no hit" means no rows rather than every asset.
#[cfg(feature = "ai")]
fn resolve_similar_filter(
    catalog: &crate::catalog::Catalog,
    state: &AppState,
    parsed: &crate::query::ParsedSearch,
) -> anyhow::Result<Option<(Vec<String>, std::collections::HashMap<String, f32>)>> {
    use std::collections::HashMap;
    let Some(ref similar_ref) = parsed.similar else {
        return Ok(None);
    };
    let model_id = &state.ai_config.model;
    let Some(spec) = crate::ai::get_model_spec(model_id) else {
        return Ok(None);
    };
    // limit defaults to 40 results (including the pinned asset);
    // min_sim is specified as percentage 0-100, convert to 0.0-1.0
    let limit = parsed.similar_limit.unwrap_or(40);
    let min_sim = parsed.min_sim.unwrap_or(0.0) / 100.0;

    // (query embedding, pinned asset) for either form
    let (query_emb, pinned): (Option<Vec<f32>>, Option<String>) =
        if let Some(token) = similar_ref.strip_prefix('@') {
            let sessions = state.query_images.lock().unwrap_or_else(|e| e.into_inner());
            match sessions.get(token) {
                Some(session) => (
                    session.query.embedding.clone(),
                    session.query.exact_asset_id.clone(),
                ),
                // Expired (or bogus) token: resolve to "nothing" rather
                // than failing the page — the browse JS asks
                // /api/query-image/{token}, gets a 404, and shows the
                // "drop it again" notice over an empty grid.
                None => return Ok(Some((Vec::new(), HashMap::new()))),
            }
        } else {
            let full_id = resolve_asset_id_or_err(catalog, similar_ref)?;
            let emb_store = crate::embedding_store::EmbeddingStore::new(catalog.conn());
            let emb = emb_store.get(&full_id, model_id)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "No embedding for '{similar_ref}'. Run `maki embed --asset {full_id}` first."
                )
            })?;
            (Some(emb), Some(full_id))
        };

    // Ensure embedding index is loaded (only needed when there is an embedding)
    if query_emb.is_some() {
        let needs_load = state.ai_embedding_index.read().unwrap().is_none();
        if needs_load {
            if let Ok(index) = crate::embedding_store::EmbeddingIndex::load(
                catalog.conn(), model_id, spec.embedding_dim,
            ) {
                *state.ai_embedding_index.write().unwrap() = Some(index);
            }
        }
    }
    let ranked = {
        let idx_guard = state.ai_embedding_index.read().unwrap();
        match *idx_guard {
            Some(ref idx) => crate::embedding_store::rank_similar(
                idx, query_emb.as_deref(), limit, min_sim, pinned.as_deref(),
            ),
            None => crate::embedding_store::rank_similar(
                &crate::embedding_store::EmbeddingIndex::from_rows(1, Vec::new()),
                None, limit, min_sim, pinned.as_deref(),
            ),
        }
    };
    let scores: HashMap<String, f32> = ranked.iter().cloned().collect();
    let ids: Vec<String> = ranked.into_iter().map(|(id, _)| id).collect();
    Ok(Some((ids, scores)))
}

impl SearchParams {
    /// `true` when the request carries a non-empty `sort=`; an absent or
    /// empty value lets a similarity view default to score order.
    pub(super) fn sort_explicit(&self) -> bool {
        self.sort.as_deref().is_some_and(|s| !s.is_empty())
    }

    /// The sort the grid actually uses, given whether the resolved search
    /// carries similarity scores:
    /// - a similarity view with no explicit sort → `similarity_desc`;
    /// - a similarity sort *without* scores (stale URL after the
    ///   `similar:` filter was removed) → `date_desc`, because the SQL
    ///   fallback for similarity sorts is a meaningless id order;
    /// - otherwise the requested sort.
    ///
    /// The browse form mirrors this on the client (`getSubmitSort` in
    /// `browse.html`): starting a similarity search switches the sort to
    /// `similarity_desc`, leaving it drops the sort again.
    pub(super) fn effective_sort<'a>(&self, has_similarity: bool, sort_str: &'a str) -> &'a str {
        if has_similarity && !self.sort_explicit() {
            "similarity_desc"
        } else if !has_similarity && sort_str.starts_with("similarity_") {
            "date_desc"
        } else {
            sort_str
        }
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    #[serde(rename = "type")]
    pub asset_type: Option<String>,
    pub tag: Option<String>,
    pub format: Option<String>,
    pub volume: Option<String>,
    pub rating: Option<String>,
    pub label: Option<String>,
    pub collection: Option<String>,
    pub path: Option<String>,
    pub person: Option<String>,
    pub sort: Option<String>,
    pub page: Option<u32>,
    pub stacks: Option<String>,
    /// Set to "1" to disable the default filter from maki.toml [browse].
    pub nodefault: Option<String>,
}








/// Shared result from `build_parsed_search` — holds the parsed query and
/// extracted filter state that every browse/search/calendar/map handler needs.
pub(super) struct BrowseFilters {
    pub(super) parsed: ParsedSearch,
    // Raw param values for template rendering (display current filter state)
    pub(super) query: String,
    pub(super) asset_type: String,
    pub(super) tag: String,
    pub(super) format_filter: String,
    pub(super) volume: String,
    pub(super) rating: String,
    pub(super) label: String,
    pub(super) collection: String,
    pub(super) path: String,
    pub(super) person: String,
    pub(super) path_volume_id: Option<String>,
    pub(super) sort_str: String,
    pub(super) page: u32,
    pub(super) collapse_stacks: bool,
    pub(super) nodefault: bool,
}

/// Extract and merge all browse filter parameters from URL query params.
/// This is the single source of truth for how SearchParams → ParsedSearch
/// works across browse_page, search_api, page_ids_api, calendar_api, map_api,
/// and facets_api. Each handler calls this, then adds handler-specific logic
/// (template rendering, JSON formatting, etc.).
/// Re-exported from the shared resolver so handler code keeps using
/// `super::EmptyFilterPolicy` unchanged. The collection/person name → asset-ID
/// resolution itself (formerly local `resolve_collection_ids` /
/// `intersect_name_groups` helpers) lives in `crate::query::resolve` now,
/// shared with the CLI search path — see that module's drift matrix for the
/// historical CLI-vs-web behavioral differences.
pub(super) use crate::query::EmptyFilterPolicy;

/// Owned holder for the per-request "ParsedSearch → SearchOptions
/// enrichment" results shared by the browse/search/calendar/map handlers.
///
/// `SearchOptions<'a>` borrows `&[String]` slices, so every handler used
/// to keep ad-hoc owned locals alive around the `SearchOptions`
/// construction — the same multi-step resolution block copy-pasted seven
/// times. This struct owns those Vecs instead:
///
/// 1. [`ResolvedSearch::resolve`] runs the shared resolution
///    (volume/path-volume fallback, collection include/exclude IDs,
///    person-name intersection).
/// 2. [`ResolvedSearch::resolve_ai_filters`] optionally runs the AI
///    text-query (SigLIP) and `similar:` resolution. Called by every
///    endpoint that must mirror the grid's visible result set:
///    browse_page, search_api, all_ids_api / page_ids_api (selection
///    and lightbox navigation — skipping it here was the v4.5.x
///    "select-all on a text search selects the unfiltered set" trap),
///    and facets_api (dropdown counts). Deliberately NOT called by
///    calendar_api / map_api: the date-heatmap and map views have never
///    supported text/similar filters, and extending them is a product
///    decision, not a consistency fix.
/// 3. [`ResolvedSearch::apply`] installs the borrowed slices into a
///    `SearchOptions` according to the handler's [`EmptyFilterPolicy`].
pub(super) struct ResolvedSearch {
    pub(super) volume: String,
    pub(super) path_volume_id: Option<String>,
    /// Collection/person ID resolution shared with the CLI search path.
    /// The web layer always uses `PersonCombine::AllOf` (separate `person:`
    /// entries intersect) — see `crate::query::resolve` for the drift matrix.
    pub(super) filters: crate::query::ResolvedFilterIds,
    /// `Some` once the text-query pipeline produced an ID list (installed
    /// even when empty — an unmatched text query yields zero results).
    /// `None` when no text query was given or the model/index failed to
    /// load (filter silently ignored, matching the historical behavior).
    #[cfg(feature = "ai")]
    pub(super) text_query_ids: Option<Vec<String>>,
    #[cfg(feature = "ai")]
    pub(super) similar_ids: Vec<String>,
    #[cfg(feature = "ai")]
    pub(super) similarity_scores: std::collections::HashMap<String, f32>,
    #[cfg(feature = "ai")]
    similar_requested: bool,
    /// `true` once `similar:` resolved (even to an empty list) — drives
    /// the install of `similar_ids` in [`Self::apply`].
    #[cfg(feature = "ai")]
    similar_resolved: bool,
}

impl ResolvedSearch {
    /// Run the shared resolution steps (collections, collection excludes,
    /// person names) and capture the volume / path-volume fallback inputs.
    pub(super) fn resolve(
        catalog: &crate::catalog::Catalog,
        parsed: &ParsedSearch,
        volume: String,
        path_volume_id: Option<String>,
        empty_filter_policy: EmptyFilterPolicy,
    ) -> Self {
        let filters = crate::query::ResolvedFilterIds::resolve(
            catalog,
            parsed,
            empty_filter_policy,
            crate::query::PersonCombine::AllOf,
        );

        Self {
            volume,
            path_volume_id,
            filters,
            #[cfg(feature = "ai")]
            text_query_ids: None,
            #[cfg(feature = "ai")]
            similar_ids: Vec::new(),
            #[cfg(feature = "ai")]
            similarity_scores: std::collections::HashMap::new(),
            #[cfg(feature = "ai")]
            similar_requested: false,
            #[cfg(feature = "ai")]
            similar_resolved: false,
        }
    }

    /// Resolve the AI text-query (SigLIP lazy-load + embedding-index
    /// search) and `similar:` filters. Separate from [`Self::resolve`] on
    /// purpose: callers opt in per-endpoint. Everything that mirrors the
    /// grid's visible result set calls it (browse_page, search_api,
    /// all_ids_api, page_ids_api, facets_api); calendar_api / map_api
    /// deliberately don't (see the struct doc).
    ///
    /// Each call re-encodes the text query (~tens of ms on CPU); the
    /// model and embedding index themselves are cached in `AppState`
    /// after first use.
    ///
    /// The non-AI variant is a no-op so callers don't need cfg blocks.
    #[cfg(feature = "ai")]
    pub(super) fn resolve_ai_filters(
        &mut self,
        catalog: &crate::catalog::Catalog,
        state: &AppState,
        parsed: &ParsedSearch,
    ) -> anyhow::Result<()> {
        self.similar_requested = parsed.similar.is_some();
        if let Some(ref text_q) = parsed.text_query {
            let model_id = &state.ai_config.model;
            let spec = crate::ai::get_model_spec(model_id);
            if let Some(spec) = spec {
                let model_dir = ai::resolve_model_dir(&state.ai_config);
                let mut model_guard = state.ai_model.blocking_lock();
                if model_guard.is_none() {
                    if let Ok(m) = crate::ai::SigLipModel::load_with_provider(
                        &model_dir, model_id, state.verbosity, &state.ai_config.execution_provider,
                    ) {
                        *model_guard = Some(m);
                    }
                }
                if let Some(ref mut model) = *model_guard {
                    if let Ok(embs) = model.encode_texts(&[text_q.clone()]) {
                        let query_emb = &embs[0];
                        let needs_load = state.ai_embedding_index.read().unwrap().is_none();
                        if needs_load {
                            if let Ok(index) = crate::embedding_store::EmbeddingIndex::load(
                                catalog.conn(), model_id, spec.embedding_dim,
                            ) {
                                *state.ai_embedding_index.write().unwrap() = Some(index);
                            }
                        }
                        let results = {
                            let idx_guard = state.ai_embedding_index.read().unwrap();
                            if let Some(ref idx) = *idx_guard {
                                idx.search(query_emb, parsed.text_query_limit.unwrap_or(state.ai_config.text_limit), None)
                            } else {
                                Vec::new()
                            }
                        };
                        self.text_query_ids =
                            Some(results.into_iter().map(|(id, _)| id).collect());
                    }
                }
            }
        }
        if let Some((ids, scores)) = resolve_similar_filter(catalog, state, parsed)? {
            self.similar_ids = ids;
            self.similarity_scores = scores;
            self.similar_resolved = true;
        }
        Ok(())
    }

    /// No-op without the `ai` feature (text-query / `similar:` filters
    /// don't exist in that build).
    #[cfg(not(feature = "ai"))]
    pub(super) fn resolve_ai_filters(
        &mut self,
        _catalog: &crate::catalog::Catalog,
        _state: &AppState,
        _parsed: &ParsedSearch,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// True when an active `similar:` filter produced scores — drives the
    /// single-page similarity view in browse_page / search_api.
    pub(super) fn has_similarity(&self) -> bool {
        #[cfg(feature = "ai")]
        {
            self.similar_requested && !self.similarity_scores.is_empty()
        }
        #[cfg(not(feature = "ai"))]
        {
            false
        }
    }

    /// Install the resolved borrowed slices into `opts`.
    pub(super) fn apply<'a>(&'a self, opts: &mut crate::catalog::SearchOptions<'a>) {
        if !self.volume.is_empty() {
            opts.volume = Some(&self.volume);
        }
        if let Some(ref vid) = self.path_volume_id {
            if opts.volume.is_none() {
                opts.volume = Some(vid);
            }
        }
        self.filters.apply_collections(opts);
        self.filters.apply_collection_excludes(opts);
        self.filters.apply_persons(opts);
        // `-person:` honored since v4.7.1 — see the drift matrix in
        // `crate::query::resolve` for the history of this gap.
        self.filters.apply_person_excludes(opts);
        #[cfg(feature = "ai")]
        {
            if let Some(ref ids) = self.text_query_ids {
                opts.text_search_ids = Some(ids);
            }
            if self.similar_resolved {
                opts.similar_asset_ids = Some(&self.similar_ids);
            }
        }
    }
}

pub(super) fn build_parsed_search(params: &SearchParams, state: &AppState) -> BrowseFilters {
    let query = params.q.as_deref().unwrap_or("");
    let asset_type = params.asset_type.as_deref().unwrap_or("");
    let tag = params.tag.as_deref().unwrap_or("");
    let fmt = params.format.as_deref().unwrap_or("");
    let volume = params.volume.as_deref().unwrap_or("").to_string();
    let rating_str = params.rating.as_deref().unwrap_or("");
    let label_str = params.label.as_deref().unwrap_or("");
    // An empty `sort=` is "no sort given" — the browse form sends it that
    // way so a similarity view (similar:<id> / similar:@token) can default
    // to score order instead of being pinned to date order by the form's
    // implicit default.
    let sort_str = params
        .sort
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("date_desc")
        .to_string();
    let page = params.page.unwrap_or(1).max(1);
    let collection_str = params.collection.as_deref().unwrap_or("");
    let path_str = params.path.as_deref().unwrap_or("");
    let person_str = params.person.as_deref().unwrap_or("");
    let collapse_stacks = params.stacks.as_deref().unwrap_or("1") == "1";
    let nodefault = params.nodefault.as_deref() == Some("1");

    // Parse query + overlay explicit dropdown params
    let mut parsed = parse_search_query(query);
    if !asset_type.is_empty() { parsed.asset_types.push(asset_type.to_string()); }
    // Tags from the URL param: comma is the chip-list separator (= AND across
    // entries at the catalog level). Note: this overrides the historical
    // "comma = OR" behaviour for the dedicated `tag=` URL param. Power users
    // who want OR can still type `tag:a,b` in the q field — that goes through
    // parse_search_query, which preserves the comma as one entry → catalog OR.
    //
    // A leading `-` on a chip-list entry negates it — the entry routes into
    // `tags_exclude` (server-side NOT-clause) instead of `tags`. Mirrors the
    // `-tag:foo` syntax users can type in the q field, surfaced via the
    // browse-page chip's negate toggle. The remaining mode/case prefixes
    // (`=`, `/`, `^`) are downstream of negation in the chip's wire format
    // (`-=foo`, `-^foo`, etc.) — strip the `-` first, pass the rest through.
    if !tag.is_empty() {
        for t in tag.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if t.starts_with('-') && t.len() > 1 {
                parsed.tags_exclude.push(t[1..].to_string());
            } else {
                parsed.tags.push(t.to_string());
            }
        }
    }
    if !fmt.is_empty() { parsed.formats.push(fmt.to_string()); }
    if !rating_str.is_empty() { parsed.rating = crate::query::parse_numeric_filter(rating_str); }
    if label_str == "none" {
        parsed.color_label_none = true;
    } else if !label_str.is_empty() {
        parsed.color_labels.push(label_str.to_string());
    }

    // Apply default filter from config
    apply_default_filter(&mut parsed, &state.default_filter, nodefault);

    // Normalize absolute path → volume-relative + implicit volume filter
    let path_volume_id = if !path_str.is_empty() {
        let registry = DeviceRegistry::new(&state.catalog_root);
        let vols = registry.list().unwrap_or_default();
        let (normalized, vol_id) = normalize_path_for_search(path_str, &vols, None);
        if !normalized.is_empty() {
            parsed.path_prefixes.push(normalized);
        }
        vol_id
    } else {
        None
    };

    // Push collection/person from dropdowns. The `person` URL param accepts
    // a comma-separated list (sent by the chip-based people picker); legacy
    // single-value URLs from shared links still work since they have no comma.
    if !collection_str.is_empty() { parsed.collections.push(collection_str.to_string()); }
    if !person_str.is_empty() {
        for p in person_str.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            parsed.persons.push(p.to_string());
        }
    }

    BrowseFilters {
        parsed,
        query: query.to_string(),
        asset_type: asset_type.to_string(),
        tag: tag.to_string(),
        format_filter: fmt.to_string(),
        volume,
        rating: rating_str.to_string(),
        label: label_str.to_string(),
        collection: collection_str.to_string(),
        path: path_str.to_string(),
        person: person_str.to_string(),
        path_volume_id,
        sort_str,
        page,
        collapse_stacks,
        nodefault,
    }
}

/// Apply the default filter from config to a parsed search, unless disabled.
fn apply_default_filter(parsed: &mut ParsedSearch, default_filter: &Option<String>, nodefault: bool) {
    if nodefault {
        return;
    }
    if let Some(df) = default_filter {
        if !df.is_empty() {
            let default_parsed = parse_search_query(df);
            parsed.merge_from(&default_parsed);
        }
    }
}


// --- Calendar heatmap & map --- (moved to routes::calendar_map)


// --- Facets ---

