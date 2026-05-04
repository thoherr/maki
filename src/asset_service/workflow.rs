//! Cross-section workflow orchestration on `AssetService`.
//!
//! Per-section primitives (`import_with_callback`, `embed_assets`,
//! `describe_assets`) live in their own submodules and take primitives in /
//! return result structs / report progress via callbacks. This module sits
//! one level above and stitches those primitives into end-to-end workflows
//! that match what users actually invoke from a CLI command or a web
//! request: "import these paths, optionally auto-group neighbours, then
//! optionally embed and describe".
//!
//! The CLI handler (`run_import_command` in main.rs) and the web handler
//! (`web/routes/import.rs`) used to each duplicate this orchestration —
//! profile resolution, filter building, volume resolution, tag merging,
//! auto-group with neighborhood scan, post-import embed phase, post-import
//! describe phase. ~150–200 LOC reproduced on both sides, including subtle
//! discrepancies (the web handler skipped the auto-group neighborhood scan
//! and the post-group preview upgrade). Lifting the orchestration here
//! collapses both handlers to thin "translate input → call workflow →
//! translate output" adapters.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;

use super::AssetService;
use super::FileStatus;

/// A complete import job, as users invoke it. Validated and resolved by
/// `import_workflow`; both the CLI and web handlers fill this in from their
/// respective input shapes (clap args / JSON body) before calling.
pub struct ImportRequest {
    /// Paths to import — files or directories. Already canonicalised by the
    /// caller (each frontend has its own canonicalisation rules: the CLI
    /// canonicalises against `cwd`; the web handler joins `volume.mount_point`
    /// with the optional subfolder).
    pub paths: Vec<PathBuf>,
    /// Volume on which to ingest. CLI uses `Some(label)` from `--volume` or
    /// `None` to auto-detect from the first path. The web handler always
    /// passes `Some(volume_id)`.
    pub volume_label: Option<String>,
    /// Named import profile from `[import.profiles.<name>]`. `None` uses
    /// the base `[import]` config.
    pub profile: Option<String>,
    /// Additional file-type groups to include (e.g. `captureone`).
    pub include: Vec<String>,
    /// File-type groups to skip (e.g. `audio`, `xmp`).
    pub skip: Vec<String>,
    /// Tags to apply to every imported asset. Merged with config `auto_tags`.
    pub add_tags: Vec<String>,
    /// Skip writes; report what would happen.
    pub dry_run: bool,
    /// Generate smart previews alongside regular ones. `false` means "use
    /// the config default `[import] smart_previews`".
    pub smart: bool,
    /// Run auto-group on imported assets and their on-volume neighbours.
    pub auto_group: bool,
    /// Run the post-import embed phase. Only honored on `ai` builds; the
    /// field exists unconditionally so the request shape is uniform across
    /// feature sets.
    #[allow(dead_code)]
    pub embed: bool,
    /// Run the post-import VLM describe phase. Only honored on `pro` builds.
    #[allow(dead_code)]
    pub describe: bool,
}

/// Phase boundary marker. Useful for UI that wants to switch a status line
/// from "Importing…" to "Embedding…" between phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportPhase {
    Import,
    AutoGroup,
    Embed,
    Describe,
}

impl ImportPhase {
    pub fn label(self) -> &'static str {
        match self {
            ImportPhase::Import => "import",
            ImportPhase::AutoGroup => "auto_group",
            ImportPhase::Embed => "embed",
            ImportPhase::Describe => "describe",
        }
    }
}

/// Per-event progress callback payload. Frontends match on the variant they
/// care about and ignore the rest. The CLI prints to stderr; the web
/// handler emits SSE events through the JobRegistry.
pub enum ImportEvent<'a> {
    /// A phase is about to run. Emitted once per phase that's enabled.
    PhaseStarted(ImportPhase),
    /// A phase was skipped — opted out, model missing, VLM endpoint
    /// unreachable, etc. `reason` is human-readable.
    PhaseSkipped {
        phase: ImportPhase,
        reason: &'a str,
    },
    /// Per-file event during the main import phase.
    File {
        path: &'a std::path::Path,
        status: FileStatus,
        elapsed: Duration,
    },
    /// Per-asset event during the post-import embed phase. AI feature only.
    #[cfg(feature = "ai")]
    Embed {
        asset_id: &'a str,
        status: &'a super::EmbedStatus,
    },
    /// Per-asset event during the post-import describe phase. Pro feature only.
    #[cfg(feature = "pro")]
    Describe {
        asset_id: &'a str,
        status: &'a crate::vlm::DescribeStatus,
        elapsed: Duration,
    },
}

/// Bundled outcome of `import_workflow`. Each phase that ran fills its
/// corresponding field; phases that were skipped (or are gated out by
/// feature flags) leave their field `None`.
pub struct ImportWorkflowResult {
    pub import: super::ImportResult,
    pub auto_group: Option<crate::query::AutoGroupResult>,
    #[cfg(feature = "ai")]
    pub embed: Option<super::EmbedAssetsResult>,
    #[cfg(feature = "pro")]
    pub describe: Option<crate::vlm::BatchDescribeResult>,
}

impl AssetService {
    /// End-to-end import workflow: profile + filter resolution → ingest →
    /// optional auto-group with neighborhood scan + preview upgrade →
    /// optional post-import embed → optional post-import describe.
    ///
    /// Both the CLI handler and the web import endpoint call this. Each
    /// frontend translates its own input shape (CLI args, JSON body) into
    /// `ImportRequest`, attaches an event callback that does whatever
    /// progress reporting it needs (stderr lines, SSE events), and reads
    /// the returned `ImportWorkflowResult` to format its response.
    ///
    /// `config` is passed in so the workflow doesn't re-read `maki.toml` —
    /// the caller usually has it already (and it lets tests inject a custom
    /// config without touching disk).
    pub fn import_workflow(
        &self,
        req: &ImportRequest,
        config: &crate::config::CatalogConfig,
        mut on_event: impl FnMut(ImportEvent<'_>) + Send,
    ) -> Result<ImportWorkflowResult> {
        use crate::asset_service::FileTypeFilter;

        // ── Resolve config + profile ───────────────────────────────────
        let import_config = if let Some(ref profile_name) = req.profile {
            config.import.resolve_profile(profile_name).ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown import profile '{}'. Available profiles: {}",
                    profile_name,
                    if config.import.profiles.is_empty() {
                        "(none configured)".to_string()
                    } else {
                        config.import.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
                    }
                )
            })?
        } else {
            config.import.clone()
        };

        let smart = req.smart || import_config.smart_previews;

        // ── Build file-type filter (profile then CLI overrides) ────────
        let mut filter = FileTypeFilter::default();
        let profile_ref = req
            .profile
            .as_deref()
            .and_then(|name| config.import.profiles.get(name));
        if let Some(p) = profile_ref {
            for group in &p.include {
                filter.include(group)?;
            }
            for group in &p.skip {
                filter.skip(group)?;
            }
        }
        for group in &req.include {
            if req.skip.contains(group) {
                anyhow::bail!("Group '{}' cannot be both included and skipped.", group);
            }
        }
        for group in &req.include {
            filter.include(group)?;
        }
        for group in &req.skip {
            filter.skip(group)?;
        }

        // ── Resolve volume ─────────────────────────────────────────────
        if req.paths.is_empty() {
            anyhow::bail!("no paths specified for import.");
        }
        let registry = crate::device_registry::DeviceRegistry::new(&self.catalog_root);
        let volume = if let Some(label) = &req.volume_label {
            registry.resolve_volume(label)?
        } else {
            registry.find_volume_for_path(&req.paths[0])?
        };

        // ── Merge config auto_tags + request add_tags ──────────────────
        let mut all_tags = import_config.auto_tags.clone();
        for tag in &req.add_tags {
            if !all_tags.contains(tag) {
                all_tags.push(tag.clone());
            }
        }

        // ── Phase 1: import ────────────────────────────────────────────
        on_event(ImportEvent::PhaseStarted(ImportPhase::Import));
        let import_result = self.import_with_callback(
            &req.paths,
            &volume,
            &filter,
            &import_config.exclude,
            &all_tags,
            req.dry_run,
            smart,
            |path, status, elapsed| {
                on_event(ImportEvent::File { path, status, elapsed });
            },
        )?;

        // ── Phase 2: auto-group ────────────────────────────────────────
        let auto_group_result = if req.auto_group
            && (import_result.imported > 0 || import_result.locations_added > 0)
        {
            on_event(ImportEvent::PhaseStarted(ImportPhase::AutoGroup));
            Some(run_auto_group(self, &volume, &import_result, req.dry_run, smart, config)?)
        } else {
            None
        };
        let auto_group_result = auto_group_result.flatten();

        // ── Phase 3: embed (AI feature) ────────────────────────────────
        #[cfg(feature = "ai")]
        let embed_result = run_embed_phase(self, &mut on_event, req, config, &import_result)?;
        #[cfg(not(feature = "ai"))]
        let _ = (req,); // suppress unused warning when feature off (req.embed)

        // ── Phase 4: describe (Pro feature) ────────────────────────────
        #[cfg(feature = "pro")]
        let describe_result = run_describe_phase(self, &mut on_event, req, config, &import_result)?;

        Ok(ImportWorkflowResult {
            import: import_result,
            auto_group: auto_group_result,
            #[cfg(feature = "ai")]
            embed: embed_result,
            #[cfg(feature = "pro")]
            describe: describe_result,
        })
    }
}

/// Auto-group: scan the on-volume neighborhood of just-imported directories,
/// merge those existing asset IDs with the newly-imported ones, and run
/// `engine.auto_group`. On non-dry-run, also regenerate previews for any
/// grouped asset whose best-variant changed (a higher-priority RAW or
/// processed copy joined the group). Returns `Some(AutoGroupResult)` only
/// when at least one group was formed.
fn run_auto_group(
    service: &AssetService,
    volume: &crate::models::volume::Volume,
    import_result: &super::ImportResult,
    dry_run: bool,
    smart: bool,
    config: &crate::config::CatalogConfig,
) -> Result<Option<crate::query::AutoGroupResult>> {
    use std::collections::HashSet;
    use std::path::Path;

    let catalog = crate::catalog::Catalog::open(&service.catalog_root)?;
    let volume_id = volume.id.to_string();

    // Compute session roots: parent directory of each imported file's directory.
    let session_roots: HashSet<String> = import_result
        .imported_directories
        .iter()
        .map(|dir| {
            Path::new(dir)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .collect();
    let prefixes: Vec<String> = session_roots.into_iter().collect();

    // Find existing catalog assets in the neighborhood.
    let neighbor_ids =
        catalog.find_asset_ids_by_volume_and_path_prefixes(&volume_id, &prefixes)?;

    // Merge with newly-imported IDs and dedup.
    let mut all_ids: Vec<String> = import_result.new_asset_ids.clone();
    let existing: HashSet<String> = all_ids.iter().cloned().collect();
    for id in neighbor_ids {
        if !existing.contains(&id) {
            all_ids.push(id);
        }
    }

    if all_ids.len() <= 1 {
        return Ok(None);
    }

    let engine = crate::query::QueryEngine::new(&service.catalog_root);
    let ag_result = engine.auto_group(&all_ids, dry_run)?;
    if ag_result.groups.is_empty() {
        return Ok(None);
    }

    // Preview upgrade for grouped targets: if a higher-priority variant
    // joined the group, regenerate the preview from it. Skipped on dry-run.
    if !dry_run {
        let metadata_store = crate::metadata_store::MetadataStore::new(&service.catalog_root);
        let preview_gen = crate::preview::PreviewGenerator::new(
            &service.catalog_root,
            service.verbosity,
            &config.preview,
        );
        let registry = crate::device_registry::DeviceRegistry::new(&service.catalog_root);
        let volumes = registry.list()?;
        for group in &ag_result.groups {
            if let Ok(uuid) = group.target_id.parse::<uuid::Uuid>() {
                if let Ok(asset) = metadata_store.load(uuid) {
                    if let Some(idx) = crate::models::variant::best_preview_index(&asset.variants)
                    {
                        if idx > 0 {
                            let v = &asset.variants[idx];
                            if let Some(loc) = v.locations.first() {
                                if let Some(vol) = volumes
                                    .iter()
                                    .find(|vl| vl.id == loc.volume_id && vl.is_online)
                                {
                                    let file_path = vol.mount_point.join(&loc.relative_path);
                                    if file_path.exists() {
                                        let _ =
                                            preview_gen.generate(&v.content_hash, &file_path, &v.format);
                                        if smart {
                                            let _ = preview_gen.generate_smart(
                                                &v.content_hash,
                                                &file_path,
                                                &v.format,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Ok(cat) = crate::catalog::Catalog::open(&service.catalog_root) {
                        let _ = cat.update_denormalized_variant_columns(&asset);
                    }
                }
            }
        }
    }

    Ok(Some(ag_result))
}

#[cfg(feature = "ai")]
fn run_embed_phase(
    service: &AssetService,
    on_event: &mut impl FnMut(ImportEvent<'_>),
    req: &ImportRequest,
    config: &crate::config::CatalogConfig,
    import_result: &super::ImportResult,
) -> Result<Option<super::EmbedAssetsResult>> {
    let opted_in = req.embed || config.import.embeddings;
    if req.dry_run || !opted_in || import_result.new_asset_ids.is_empty() {
        return Ok(None);
    }
    on_event(ImportEvent::PhaseStarted(ImportPhase::Embed));

    let model_id = &config.ai.model;
    let model_dir = crate::config::resolve_model_dir(&config.ai.model_dir, model_id);
    let mgr = crate::model_manager::ModelManager::new(&model_dir, model_id)?;
    if !mgr.model_exists() {
        on_event(ImportEvent::PhaseSkipped {
            phase: ImportPhase::Embed,
            reason: "model not downloaded",
        });
        return Ok(None);
    }

    let r = service.embed_assets(
        &import_result.new_asset_ids,
        &model_dir,
        model_id,
        &config.ai.execution_provider,
        false,
        |aid, status, _elapsed| {
            on_event(ImportEvent::Embed {
                asset_id: aid,
                status,
            });
        },
    )?;
    Ok(Some(r))
}

#[cfg(feature = "pro")]
fn run_describe_phase(
    service: &AssetService,
    on_event: &mut (impl FnMut(ImportEvent<'_>) + Send),
    req: &ImportRequest,
    config: &crate::config::CatalogConfig,
    import_result: &super::ImportResult,
) -> Result<Option<crate::vlm::BatchDescribeResult>> {
    let opted_in = req.describe || config.import.descriptions;
    if req.dry_run || !opted_in || import_result.new_asset_ids.is_empty() {
        return Ok(None);
    }
    on_event(ImportEvent::PhaseStarted(ImportPhase::Describe));

    let endpoint = &config.vlm.endpoint;
    let vlm_model = &config.vlm.model;
    if crate::vlm::check_endpoint(endpoint, 5, service.verbosity).is_err() {
        let reason: String = format!("VLM endpoint unavailable at {endpoint}");
        on_event(ImportEvent::PhaseSkipped {
            phase: ImportPhase::Describe,
            reason: &reason,
        });
        return Ok(None);
    }

    let mode = crate::vlm::DescribeMode::from_str(&config.vlm.mode)
        .unwrap_or(crate::vlm::DescribeMode::Describe);
    let params = config.vlm.params_for_model(vlm_model);
    // describe_assets requires `Fn + Sync` because it dispatches across worker
    // threads. Our outer `on_event` is `FnMut` (single-threaded sequential).
    // Wrap it in a Mutex so the per-thread callback can lock-and-call. The
    // contention is negligible — each callback fires after a multi-second
    // VLM HTTP roundtrip.
    let on_event_mu = std::sync::Mutex::new(&mut *on_event);
    let result = service.describe_assets(
        &import_result.new_asset_ids,
        endpoint,
        vlm_model,
        &params,
        mode,
        false, // force
        false, // dry_run (the workflow's dry_run was already short-circuited above)
        config.vlm.concurrency,
        |aid, status, elapsed| {
            if let Ok(mut cb) = on_event_mu.lock() {
                (*cb)(ImportEvent::Describe {
                    asset_id: aid,
                    status,
                    elapsed,
                });
            }
        },
    )?;
    Ok(Some(result))
}
