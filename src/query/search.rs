//! Search and lookup half of `QueryEngine`: `search`, `search_with_image`,
//! `show`, `resolve_scope`, and the volume-label resolver. The search
//! itself is the free function [`search_in`] / [`run_search`], which
//! services and the web can run on a connection they already hold.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;

use crate::catalog::{AssetDetails, Catalog, SearchOptions, SearchRow};
use crate::config::CatalogConfig;
use crate::device_registry::DeviceRegistry;
use crate::models::volume::Volume;

use super::{
    normalize_path_for_search, parse_search_query, EmptyFilterPolicy, ImageSearch, ImageSearchInfo,
    ParsedSearch, QueryEngine, ResolvedFilterIds,
};

/// Everything a search needs besides the query: the connection, the
/// catalog root (device registry, preview cache), the configuration and
/// the `[browse] default_filter`.
#[derive(Clone, Copy)]
pub struct SearchScope<'a> {
    pub catalog: &'a Catalog,
    pub catalog_root: &'a Path,
    pub config: &'a CatalogConfig,
    /// AND-ed into every query (see [`ParsedSearch::merge_from`]).
    pub default_filter: Option<&'a str>,
}

/// Pre-computed asset-id lists for the AI filters (`similar:`, `text:`,
/// query image). Empty in a build without the `ai` feature; the stub
/// `plan_similarity` bails when such a filter is present.
#[derive(Default)]
struct SimilarityPlan {
    similar_ids: Option<Vec<String>>,
    text_query_ids: Option<Vec<String>>,
    scores: HashMap<String, f32>,
    exact_match_id: Option<String>,
    image_info: Option<ImageSearchInfo>,
}

/// Run `query` on a connection the caller already holds. Services and
/// the web pool use this directly; [`QueryEngine::search`] is the thin
/// wrapper over the engine's own connection.
pub fn search_in(
    catalog: &Catalog,
    catalog_root: &Path,
    config: &CatalogConfig,
    default_filter: Option<&str>,
    query: &str,
) -> Result<Vec<SearchRow>> {
    let scope = SearchScope { catalog, catalog_root, config, default_filter };
    run_search(&scope, query, None).map(|(rows, _)| rows)
}

/// The search pipeline: parse, merge the default filter, normalise path
/// prefixes, resolve collection / person / missing filters, plan the AI
/// filters, resolve volume labels, run the SQL, annotate similarity.
pub fn run_search(
    scope: &SearchScope<'_>,
    query: &str,
    image: Option<ImageSearch<'_>>,
) -> Result<(Vec<SearchRow>, Option<ImageSearchInfo>)> {
    let catalog = scope.catalog;
    let mut parsed = parse_search_query(query);

    // Apply default filter from config (AND semantics)
    if let Some(df) = scope.default_filter {
        if !df.is_empty() {
            let default_parsed = parse_search_query(df);
            parsed.merge_from(&default_parsed);
        }
    }

    // Normalize path prefixes: ~, ./, ../, /absolute → volume-relative + volume filter
    let path_volume_id: Option<String> = if !parsed.path_prefixes.is_empty() {
        let registry = DeviceRegistry::new(scope.catalog_root);
        let volumes = registry.list()?;
        let cwd = std::env::current_dir().ok();
        // Normalize the first path prefix (CLI context)
        let (normalized, vol_id) = normalize_path_for_search(
            &parsed.path_prefixes[0],
            &volumes,
            cwd.as_deref(),
        );
        parsed.path_prefixes[0] = normalized;
        vol_id
    } else {
        None
    };

    // Session root pattern from config for the scattered: filter
    let session_pattern = &scope.config.group.session_root_pattern;

    let mut opts = SearchOptions {
        per_page: u32::MAX,
        session_root_pattern: session_pattern,
        ..parsed.to_search_options()
    };

    if let Some(ref vid) = path_volume_id {
        opts.volume = Some(vid);
    }

    // Resolve collection include/exclude, person include/exclude and
    // `missing:true` (disk probe, only when set) via the shared resolver
    // (see `crate::query::resolve`) — the same step the web routes run.
    // The CLI installs resolved lists whenever the filter is present in
    // the query — an unmatched filter matches nothing (MatchNothing).
    let resolved_filters = ResolvedFilterIds::resolve(
        catalog,
        scope.catalog_root,
        &parsed,
        EmptyFilterPolicy::MatchNothing,
    )?;
    resolved_filters.apply(&mut opts);

    // Pre-compute similar / text-query asset IDs from embedding
    // similarity (the `ai` feature); a build without it bails here when
    // such a filter is present.
    let plan = plan_similarity(scope, &parsed, image)?;
    if let Some(ids) = &plan.similar_ids {
        opts.similar_asset_ids = Some(ids);
    }
    if let Some(ids) = &plan.text_query_ids {
        opts.text_search_ids = Some(ids);
    }

    // Resolve volume labels to volume IDs, and handle volume:none
    let resolved_volume_ids;
    let resolved_volume_exclude_ids;
    let online_vol_ids;
    if !parsed.volumes.is_empty() || !parsed.volumes_exclude.is_empty() || parsed.volume_none || parsed.volume_any_online {
        let registry = DeviceRegistry::new(scope.catalog_root);
        let volumes = registry.list()?;

        resolved_volume_ids = QueryEngine::resolve_volume_labels(&parsed.volumes, &volumes)?;
        if !resolved_volume_ids.is_empty() {
            opts.volume_ids = &resolved_volume_ids;
        }

        resolved_volume_exclude_ids = QueryEngine::resolve_volume_labels(&parsed.volumes_exclude, &volumes)?;
        if !resolved_volume_exclude_ids.is_empty() {
            opts.volume_ids_exclude = &resolved_volume_exclude_ids;
        }

        if parsed.volume_none || parsed.volume_any_online {
            online_vol_ids = volumes
                .iter()
                .filter(|v| v.is_online)
                .map(|v| v.id.to_string())
                .collect::<Vec<_>>();
            if parsed.volume_none {
                opts.no_online_locations = Some(&online_vol_ids);
            }
            if parsed.volume_any_online {
                opts.has_online_locations = Some(&online_vol_ids);
            }
        }
    }

    let mut rows = catalog.search_paginated(&opts)?;

    // Similarity searches come back in the caller's sort order from
    // SQL; annotate the rows with their scores and order by score so
    // the CLI shows the ranking the web grid shows.
    if !plan.scores.is_empty() {
        for row in &mut rows {
            row.similarity = plan.scores.get(&row.asset_id).copied();
            row.exact_match = plan.exact_match_id.as_deref() == Some(row.asset_id.as_str());
        }
        rows.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    Ok((rows, plan.image_info))
}

/// Plan the AI filters: `similar:<id>` (stored embedding of a catalog
/// asset) or a query image file (`--image`), both through the shared
/// ranker so scores, pinning and min_sim behave identically; and
/// `text:<query>` through text-to-image embedding similarity.
#[cfg(feature = "ai")]
fn plan_similarity(
    scope: &SearchScope<'_>,
    parsed: &ParsedSearch,
    image: Option<ImageSearch<'_>>,
) -> Result<SimilarityPlan> {
    let catalog = scope.catalog;
    let config = scope.config;
    let mut plan = SimilarityPlan::default();

    if let Some(ref similar_ref) = parsed.similar {
        if image.is_some() {
            anyhow::bail!("--image cannot be combined with a similar: filter");
        }
        let full_id = catalog.resolve_asset_id_required(similar_ref)?;
        let model_id = &config.ai.model;
        let emb_store = crate::embedding_store::EmbeddingStore::new(catalog);
        let query_emb = emb_store
            .get(&full_id, model_id)?
            .ok_or_else(|| anyhow::anyhow!(
                "No embedding found for asset '{similar_ref}'. Run `maki embed --asset {full_id}` first."
            ))?;
        let limit = parsed.similar_limit.unwrap_or(40);
        let dim = query_emb.len();
        let index = crate::embedding_store::EmbeddingIndex::load(catalog.conn(), model_id, dim)?;
        // min_sim is specified as percentage 0-100, convert to 0.0-1.0
        let min_sim = parsed.min_sim.unwrap_or(0.0) / 100.0;
        // The source asset itself is pinned first at 100%
        let ranked = crate::embedding_store::rank_similar(
            &index, Some(&query_emb), limit, min_sim, Some(&full_id),
        );
        plan.scores = ranked.iter().cloned().collect();
        plan.similar_ids = Some(ranked.into_iter().map(|(id, _)| id).collect::<Vec<_>>());
    } else if let Some(img) = image {
        let model_id = config.ai.model.clone();
        let preview_gen = crate::preview::PreviewGenerator::new(
            scope.catalog_root, crate::Verbosity::quiet(), &config.preview,
        );
        let (query_image, warning) = crate::query_image::resolve_query_image(
            catalog,
            img.path,
            |path| {
                let model_dir = crate::config::resolve_model_dir(&config.ai.model_dir, &model_id);
                let mgr = crate::model_manager::ModelManager::new(&model_dir, &model_id)?;
                if !mgr.model_exists() {
                    anyhow::bail!(
                        "Model not downloaded. Run 'maki auto-tag --download --model {model_id}' first."
                    );
                }
                let mut model = crate::ai::SigLipModel::load_with_provider(
                    &model_dir, &model_id, crate::Verbosity::quiet(), &config.ai.execution_provider,
                )?;
                crate::query_image::encode_query_image(&mut model, &preview_gen, path)
            },
        )?;
        let limit = img.limit.unwrap_or(crate::query_image::DEFAULT_LIMIT);
        let min_sim = parsed.min_sim.unwrap_or(0.0) / 100.0;
        let ranked = match query_image.embedding {
            Some(ref emb) => {
                let index = crate::embedding_store::EmbeddingIndex::load(
                    catalog.conn(), &model_id, emb.len(),
                )?;
                query_image.rank(&index, limit, min_sim)
            }
            // Exact hit without an embedding: the index is irrelevant.
            None => query_image.rank(
                &crate::embedding_store::EmbeddingIndex::from_rows(1, Vec::new()), limit, min_sim,
            ),
        };
        plan.scores = ranked.iter().cloned().collect();
        // An empty list is installed on purpose: no hit means no rows,
        // not "every asset".
        plan.similar_ids = Some(ranked.into_iter().map(|(id, _)| id).collect::<Vec<_>>());
        plan.exact_match_id = query_image.exact_asset_id.clone();
        plan.image_info = Some(ImageSearchInfo {
            content_hash: query_image.content_hash,
            exact_asset_id: query_image.exact_asset_id,
            warning,
        });
    }

    // Pre-compute text search asset IDs from text-to-image embedding similarity
    if let Some(ref text_q) = parsed.text_query {
        let model_id = &config.ai.model;
        let spec = crate::ai::get_model_spec(model_id)
            .ok_or_else(|| anyhow::anyhow!("unknown AI model: {model_id}"))?;

        // Resolve model directory
        let model_dir_str = &config.ai.model_dir;
        let model_base = if let Some(rest) = model_dir_str.strip_prefix("~/") {
            let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))
                .map_err(|_| anyhow::anyhow!("cannot determine home directory"))?;
            std::path::PathBuf::from(home).join(rest)
        } else {
            std::path::PathBuf::from(model_dir_str)
        };
        let model_dir = model_base.join(model_id);

        // Load model and encode the text query
        let mut model = crate::ai::SigLipModel::load_with_provider(
            &model_dir, model_id, crate::Verbosity::quiet(), &config.ai.execution_provider,
        )?;
        let query_emb = model.encode_texts(std::slice::from_ref(text_q))?;
        let query_emb = &query_emb[0];

        // Search embedding index
        let limit = parsed.text_query_limit.unwrap_or(config.ai.text_limit);
        let index = crate::embedding_store::EmbeddingIndex::load(
            catalog.conn(), model_id, spec.embedding_dim,
        )?;
        // min_sim: applies to text queries too (percent → 0.0–1.0)
        let min_sim = parsed.min_sim.unwrap_or(0.0) / 100.0;
        let results = index.search(query_emb, limit, None);
        plan.text_query_ids = Some(
            results
                .into_iter()
                .filter(|(_id, score)| *score >= min_sim)
                .map(|(id, _score)| id)
                .collect::<Vec<_>>(),
        );
    }

    Ok(plan)
}

/// Build without the `ai` feature: the parser still produces the same
/// `ParsedSearch`, the filters fail here at execution.
#[cfg(not(feature = "ai"))]
fn plan_similarity(
    _scope: &SearchScope<'_>,
    parsed: &ParsedSearch,
    image: Option<ImageSearch<'_>>,
) -> Result<SimilarityPlan> {
    if image.is_some() {
        anyhow::bail!("search by query image requires a build with the `ai` feature");
    }
    if parsed.similar.is_some() {
        anyhow::bail!("the similar: filter requires a build with the `ai` feature");
    }
    if parsed.text_query.is_some() {
        anyhow::bail!("the text: filter requires a build with the `ai` feature");
    }
    Ok(SimilarityPlan::default())
}

impl QueryEngine {
    // ═══ SEARCH & SHOW ═══

    /// The engine's connection, root, config and default filter as a
    /// [`SearchScope`] for [`run_search`].
    fn search_scope(&self) -> Result<SearchScope<'_>> {
        Ok(SearchScope {
            catalog: self.catalog()?,
            catalog_root: &self.catalog_root,
            config: &self.config,
            default_filter: self.default_filter.as_deref(),
        })
    }

    /// Search assets by a free-text query string.
    ///
    /// The query syntax (some forty `prefix:value` filters plus free text) is
    /// parsed by [`parse_search_query`]; the full filter list is documented in
    /// `doc/manual/reference/06-search-filters.md` and `maki search --help`.
    /// Remaining tokens are joined as free-text search against name/filename/description/metadata.
    pub fn search(&self, query: &str) -> Result<Vec<SearchRow>> {
        run_search(&self.search_scope()?, query, None).map(|(rows, _)| rows)
    }

    /// Search by a query image file that is not in the catalog (`maki
    /// search --image`), optionally narrowed by `query`. Rows come back
    /// sorted by similarity with `similarity` / `exact_match` filled in;
    /// see [`crate::query_image`] for the two-stage resolution. Requires
    /// the `ai` feature — errors otherwise.
    pub fn search_with_image(
        &self,
        query: &str,
        image: ImageSearch<'_>,
    ) -> Result<(Vec<SearchRow>, ImageSearchInfo)> {
        let (rows, info) = run_search(&self.search_scope()?, query, Some(image))?;
        let info = info.ok_or_else(|| anyhow::anyhow!("query-image search produced no query info"))?;
        Ok((rows, info))
    }

    /// Resolve volume label strings to volume UUIDs.
    ///
    /// Each entry may be comma-separated for OR semantics (e.g. "Photos,Archive").
    /// Label matching is case-insensitive.
    pub(super) fn resolve_volume_labels(labels: &[String], volumes: &[Volume]) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for entry in labels {
            for label in entry.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                let vol = volumes.iter().find(|v| v.label.eq_ignore_ascii_case(label))
                    .ok_or_else(|| anyhow::anyhow!("unknown volume: '{label}'"))?;
                ids.push(vol.id.to_string());
            }
        }
        Ok(ids)
    }

    /// Look up a single asset by its full ID or a unique prefix.
    pub fn show(&self, asset_id_prefix: &str) -> Result<AssetDetails> {
        let catalog = self.catalog()?;
        let full_id = catalog.resolve_asset_id_required(asset_id_prefix)?;
        catalog
            .load_asset_details(&full_id)?
            .ok_or_else(|| anyhow::anyhow!("asset '{full_id}' not found in catalog"))
    }

    /// Resolve a scope (query, single asset, explicit IDs) to a set of asset IDs.
    ///
    /// This is the standard way to turn the CLI's scope options into a concrete
    /// set of assets to process. Returns `None` if no scope was specified (caller
    /// should process everything). Returns `Some(set)` to filter by membership.
    pub fn resolve_scope(
        &self,
        query: Option<&str>,
        asset: Option<&str>,
        asset_ids: &[String],
    ) -> Result<Option<HashSet<String>>> {
        // Explicit asset ID list (from shell variable expansion)
        if !asset_ids.is_empty() {
            let catalog = self.catalog()?;
            let mut ids = HashSet::new();
            for raw_id in asset_ids {
                let full_id = catalog.resolve_asset_id_required(raw_id)?;
                ids.insert(full_id);
            }
            return Ok(Some(ids));
        }
        // Single asset ID
        if let Some(prefix) = asset {
            let catalog = self.catalog()?;
            let full_id = catalog.resolve_asset_id_required(prefix)?;
            return Ok(Some(HashSet::from([full_id])));
        }
        // Search query
        if let Some(q) = query {
            let rows = self.search(q)?;
            let ids: HashSet<String> = rows.into_iter().map(|r| r.asset_id).collect();
            return Ok(Some(ids));
        }
        // No scope — process everything
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::test_support::*;

    // ── resolve_volume_labels tests ────────────────────────────

    #[test]
    pub(super) fn resolve_volume_labels_single() {
        let vol = make_volume("Photos", "/Volumes/Photos");
        let id = vol.id.to_string();
        let result = QueryEngine::resolve_volume_labels(
            &["Photos".to_string()], &[vol],
        ).unwrap();
        assert_eq!(result, vec![id]);
    }

    #[test]
    pub(super) fn resolve_volume_labels_case_insensitive() {
        let vol = make_volume("ScreenSaver", "/path");
        let id = vol.id.to_string();
        let result = QueryEngine::resolve_volume_labels(
            &["screensaver".to_string()], &[vol],
        ).unwrap();
        assert_eq!(result, vec![id]);
    }

    #[test]
    pub(super) fn resolve_volume_labels_comma_or() {
        let vol1 = make_volume("Photos", "/Volumes/Photos");
        let vol2 = make_volume("Archive", "/Volumes/Archive");
        let id1 = vol1.id.to_string();
        let id2 = vol2.id.to_string();
        let result = QueryEngine::resolve_volume_labels(
            &["Photos,Archive".to_string()], &[vol1, vol2],
        ).unwrap();
        assert_eq!(result, vec![id1, id2]);
    }

    #[test]
    pub(super) fn resolve_volume_labels_unknown_errors() {
        let vol = make_volume("Photos", "/Volumes/Photos");
        let result = QueryEngine::resolve_volume_labels(
            &["Nonexistent".to_string()], &[vol],
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown volume"));
    }
}

#[cfg(all(test, feature = "ai"))]
mod query_image_tests {
    use super::*;
    use std::path::Path;
    use crate::content_store::ContentStore;
    use crate::metadata_store::MetadataStore;
    use crate::models::{Asset, AssetType, Variant, VariantRole};

    /// Temp catalog with the model dir pointed at nowhere, so the
    /// engine's lazy model load fails deterministically.
    fn catalog_without_model() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("maki.toml"),
            "[ai]\nmodel_dir = \"/nonexistent/maki-test-models\"\n",
        )
        .unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        catalog.initialize().unwrap();
        dir
    }

    fn seed(root: &Path, hash: &str, name: &str) -> String {
        let catalog = Catalog::open(root).unwrap();
        let mut asset = Asset::new(AssetType::Image, hash);
        asset.name = Some(name.to_string());
        asset.variants.push(Variant {
            content_hash: hash.to_string(),
            asset_id: asset.id,
            role: VariantRole::Original,
            format: "jpg".to_string(),
            file_size: 1,
            original_filename: format!("{name}.jpg"),
            source_metadata: Default::default(),
            locations: vec![],
        });
        MetadataStore::new(root).save(&asset).unwrap();
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&asset.variants[0]).unwrap();
        asset.id.to_string()
    }

    #[test]
    fn search_with_image_exact_match_without_model() {
        let dir = catalog_without_model();
        let query_file = dir.path().join("preview.jpg");
        std::fs::write(&query_file, b"bytes of the preview").unwrap();
        let hash = ContentStore::new(dir.path()).hash_file(&query_file).unwrap();
        let original = seed(dir.path(), &hash, "original");
        let _other = seed(dir.path(), "sha256:unrelated", "other");

        let engine = QueryEngine::new(dir.path());
        let (rows, info) = engine
            .search_with_image("", ImageSearch { path: &query_file, limit: None })
            .unwrap();
        assert_eq!(rows.len(), 1, "only the exact match without an embedding");
        assert_eq!(rows[0].asset_id, original);
        assert!(rows[0].exact_match);
        assert_eq!(rows[0].similarity, Some(1.0));
        assert_eq!(info.exact_asset_id.as_deref(), Some(original.as_str()));
        assert_eq!(info.content_hash, hash);
        assert!(info.warning.unwrap().contains("not downloaded"));

        // Other filters compose (AND) with the pinned match
        let (rows, _) = engine
            .search_with_image("type:video", ImageSearch { path: &query_file, limit: None })
            .unwrap();
        assert!(rows.is_empty());

        // JSON output carries the similarity fields only when set
        let json = serde_json::to_string(&engine.search("").unwrap()).unwrap();
        assert!(!json.contains("similarity"), "plain searches stay unchanged: {json}");
    }

    #[test]
    fn search_with_image_errors_without_match_or_model() {
        let dir = catalog_without_model();
        let query_file = dir.path().join("stranger.jpg");
        std::fs::write(&query_file, b"nothing like this in the catalog").unwrap();
        let engine = QueryEngine::new(dir.path());
        let err = engine
            .search_with_image("", ImageSearch { path: &query_file, limit: None })
            .unwrap_err();
        assert!(format!("{err:#}").contains("not downloaded"), "{err:#}");

        let missing = dir.path().join("missing.jpg");
        assert!(engine
            .search_with_image("", ImageSearch { path: &missing, limit: None })
            .is_err());
    }

    #[test]
    fn search_with_image_rejects_similar_filter() {
        let dir = catalog_without_model();
        let query_file = dir.path().join("q.jpg");
        std::fs::write(&query_file, b"q").unwrap();
        let engine = QueryEngine::new(dir.path());
        let err = engine
            .search_with_image("similar:abcd", ImageSearch { path: &query_file, limit: None })
            .unwrap_err();
        assert!(format!("{err:#}").contains("cannot be combined"), "{err:#}");
    }
}
