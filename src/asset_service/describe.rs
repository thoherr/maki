//! `describe` section of `AssetService` — batch VLM description and
//! tagging. One operation, [`AssetService::describe_with`], serves the
//! CLI (`maki describe`), the post-import describe phase and the web batch
//! action; the scope decides which assets it targets.
//!
//! Free functions and result types live in the parent `asset_service` module.

use super::*;
use crate::vlm::{self, BatchDescribeResult, DescribeError, DescribeMode, DescribeResult, DescribeStatus, SkipReason, VlmParams};

/// Which assets a describe run targets.
#[derive(Debug, Clone)]
pub enum DescribeScope {
    /// The CLI form: a search query and/or a volume label, or one asset
    /// (id or prefix). Everything `None` means every asset.
    Query {
        query: Option<String>,
        asset_id: Option<String>,
        volume: Option<String>,
    },
    /// An explicit list of assets (full ids or unique prefixes), as the
    /// post-import phase and the web batch action pass them.
    Ids(Vec<String>),
}

/// One batch-describe request.
#[derive(Debug, Clone)]
pub struct DescribeRequest {
    pub scope: DescribeScope,
    /// VLM endpoint URL.
    pub endpoint: String,
    /// VLM model name.
    pub model: String,
    /// Prompt, sampling and image-size parameters for the model.
    pub params: VlmParams,
    pub mode: DescribeMode,
    /// Write descriptions and tags to the assets; `false` only reports
    /// what the VLM returned (the result is flagged `dry_run`).
    pub apply: bool,
    /// Describe assets that already have a description.
    pub force: bool,
    /// Resolve targets and images, call no VLM, write nothing.
    pub dry_run: bool,
    /// Parallel VLM calls (clamped to at least 1).
    pub concurrency: u32,
}

/// One settled asset of a describe run, as [`AssetService::describe_with`]
/// reports it. Carries what a progress display needs: the status, what was
/// written for this asset, and the run's position.
pub struct DescribeEvent<'a> {
    pub asset_id: &'a str,
    pub status: &'a DescribeStatus,
    /// Time spent on this asset (the VLM call for described assets).
    pub elapsed: Duration,
    /// A non-empty description was written for this asset.
    pub description_set: bool,
    /// Tags written for this asset (new ones only; the VLM's duplicates of
    /// existing tags are not counted).
    pub tags_applied: usize,
    /// Assets settled so far, this one included, and the number in scope.
    pub processed: usize,
    pub total: usize,
}

impl AssetService {
    // ═══ DESCRIBE (VLM) ═══

    /// Batch-describe the assets of `req.scope` with a VLM endpoint.
    ///
    /// Three phases: the targets are resolved and checked sequentially
    /// (asset exists, no description yet unless `force`, an image is
    /// reachable); the VLM is called in chunks of `concurrency` parallel
    /// requests; the outputs are applied sequentially (catalog writes are
    /// single-threaded). `on_asset` fires once per asset with its final
    /// status, from whichever phase settled it, on the calling thread.
    pub fn describe_with(
        &self,
        req: &DescribeRequest,
        on_asset: impl FnMut(DescribeEvent<'_>),
    ) -> Result<BatchDescribeResult> {
        let engine = self.query_engine_on(Catalog::open(&self.catalog_root)?);
        let asset_ids = self.resolve_describe_targets(engine.catalog()?, &req.scope)?;
        let concurrency = (req.concurrency.max(1)) as usize;

        if self.verbosity.verbose {
            eprintln!("  Describe: {} candidate asset(s), concurrency={concurrency}", asset_ids.len());
        }

        let mut run = DescribeRun {
            service: self,
            engine,
            preview_gen: self.preview_generator(),
            volumes: DeviceRegistry::new(&self.catalog_root).list()?,
            req,
            result: BatchDescribeResult {
                described: 0,
                skipped: 0,
                failed: 0,
                tags_applied: 0,
                errors: Vec::new(),
                dry_run: !req.apply || req.dry_run,
                mode: req.mode.to_string(),
                results: Vec::new(),
            },
            processed: 0,
            total: asset_ids.len(),
            on_asset,
        };

        // Phase 1: Prepare work items (sequential — needs catalog reads)
        let work_items = run.prepare_work_items(&asset_ids)?;

        // Phase 2 + 3: VLM calls in parallel batches, results applied sequentially
        for chunk in work_items.chunks(concurrency) {
            for (aid, existing_tags, elapsed, vlm_result) in run.call_vlm(chunk) {
                run.apply_output(&aid, &existing_tags, elapsed, vlm_result);
            }
        }

        Ok(run.result)
    }

    /// The asset ids a scope stands for. Prefixes in `Ids` are expanded;
    /// an unresolvable entry is kept as given so the run reports it as
    /// not found.
    fn resolve_describe_targets(&self, catalog: &Catalog, scope: &DescribeScope) -> Result<Vec<String>> {
        match scope {
            DescribeScope::Ids(ids) => ids
                .iter()
                .map(|id| Ok(catalog.resolve_asset_id(id)?.unwrap_or_else(|| id.clone())))
                .collect(),
            DescribeScope::Query { asset_id: Some(id), .. } => {
                Ok(vec![catalog.resolve_asset_id_required(id)?])
            }
            DescribeScope::Query { query, volume, .. } => {
                let q = if let Some(query) = query {
                    let volume_part = volume.as_ref().map(|v| format!(" volume:{v}")).unwrap_or_default();
                    format!("{query}{volume_part}")
                } else if let Some(v) = volume {
                    format!("volume:{v}")
                } else {
                    "*".to_string()
                };
                let results = self.search_in(catalog, &q)?;
                Ok(results.into_iter().map(|r| r.asset_id).collect())
            }
        }
    }
}

/// An asset that passed the pre-flight checks and awaits its VLM call.
struct WorkItem {
    asset_id: String,
    image_path: PathBuf,
    existing_tags: HashSet<String>,
}

/// `(asset id, existing tags, elapsed, VLM output or error message)`.
type VlmCallResult = (String, HashSet<String>, Duration, Result<vlm::VlmOutput, String>);

/// One describe run in progress.
struct DescribeRun<'a, F> {
    service: &'a AssetService,
    /// Owns the run's one catalog connection (`engine.catalog()`).
    engine: crate::query::QueryEngine,
    preview_gen: crate::preview::PreviewGenerator,
    volumes: Vec<Volume>,
    req: &'a DescribeRequest,
    result: BatchDescribeResult,
    processed: usize,
    total: usize,
    on_asset: F,
}

/// What was written for one asset.
#[derive(Default, Clone, Copy)]
struct Written {
    description_set: bool,
    tags_applied: usize,
}

/// The mutable half of a run that settling an asset touches; split off
/// so the immutable half (stores, request) can stay borrowed meanwhile.
struct Progress<'r, F> {
    result: &'r mut BatchDescribeResult,
    processed: &'r mut usize,
    total: usize,
    on_asset: &'r mut F,
}

impl<F: FnMut(DescribeEvent<'_>)> Progress<'_, F> {
    /// Count a settled asset, report it and keep its row. Error rows also
    /// land in `errors`. `reported` replaces the row's status in the event
    /// (dry run: the row counts as described, the event says what would
    /// happen).
    fn record(&mut self, row: DescribeResult, reported: Option<&DescribeStatus>, elapsed: Duration, written: Written) {
        match &row.status {
            DescribeStatus::Error(error) => {
                self.result.errors.push(error.to_string());
                self.result.failed += 1;
            }
            DescribeStatus::Skipped(_) => self.result.skipped += 1,
            DescribeStatus::Described => self.result.described += 1,
        }
        *self.processed += 1;
        (self.on_asset)(DescribeEvent {
            asset_id: &row.asset_id,
            status: reported.unwrap_or(&row.status),
            elapsed,
            description_set: written.description_set,
            tags_applied: written.tags_applied,
            processed: *self.processed,
            total: self.total,
        });
        self.result.results.push(row);
    }
}

fn row(asset_id: &str, status: DescribeStatus) -> DescribeResult {
    DescribeResult {
        asset_id: asset_id.to_string(),
        description: None,
        tags: Vec::new(),
        status,
    }
}

impl<F: FnMut(DescribeEvent<'_>)> DescribeRun<'_, F> {
    fn progress(&mut self) -> Progress<'_, F> {
        Progress {
            result: &mut self.result,
            processed: &mut self.processed,
            total: self.total,
            on_asset: &mut self.on_asset,
        }
    }

    /// Phase 1: settle every asset that needs no VLM call (missing,
    /// already described, no image, dry run) and collect the rest.
    fn prepare_work_items(&mut self, asset_ids: &[String]) -> Result<Vec<WorkItem>> {
        let mode = self.req.mode;
        let wants_description = mode == DescribeMode::Describe || mode == DescribeMode::Both;
        let force = self.req.force;
        let dry_run = self.req.dry_run;
        let online_volumes = Volume::online_map(&self.volumes);
        let mut work_items: Vec<WorkItem> = Vec::new();
        let (service, catalog, preview_gen) = (self.service, self.engine.catalog()?, &self.preview_gen);
        let mut progress = Progress {
            result: &mut self.result,
            processed: &mut self.processed,
            total: self.total,
            on_asset: &mut self.on_asset,
        };

        for aid in asset_ids {
            let asset_start = Instant::now();
            let short_id = &aid[..8.min(aid.len())];

            // Load asset details
            let details = match catalog.load_asset_details(aid)? {
                Some(d) => d,
                None => {
                    let error = DescribeError::NotFound { short_id: short_id.to_string() };
                    progress.record(row(aid, DescribeStatus::Error(error)), None, asset_start.elapsed(), Written::default());
                    continue;
                }
            };

            // In describe/both modes, skip if description exists and --force not set
            if wants_description && !force {
                if let Some(ref desc) = details.description {
                    if !desc.is_empty() {
                        let mut r = row(aid, DescribeStatus::Skipped(SkipReason::AlreadyDescribed));
                        r.description = Some(desc.clone());
                        progress.record(r, None, asset_start.elapsed(), Written::default());
                        continue;
                    }
                }
            }

            // Find image
            let image_path = service.find_image_for_vlm(&details, preview_gen, &online_volumes);
            let image_path = match image_path {
                Some(p) => p,
                None => {
                    let reason = SkipReason::NoImage { short_id: short_id.to_string() };
                    progress.record(row(aid, DescribeStatus::Skipped(reason)), None, asset_start.elapsed(), Written::default());
                    continue;
                }
            };

            if dry_run {
                // Counted as described (the row says so); the event
                // carries what would happen.
                let reported = DescribeStatus::Skipped(SkipReason::WouldProcess {
                    image: image_path.display().to_string(),
                });
                progress.record(row(aid, DescribeStatus::Described), Some(&reported), asset_start.elapsed(), Written::default());
                continue;
            }

            let existing_tags: HashSet<String> = details
                .tags
                .iter()
                .map(|t| t.to_lowercase())
                .collect();

            work_items.push(WorkItem {
                asset_id: aid.clone(),
                image_path,
                existing_tags,
            });
        }

        Ok(work_items)
    }

    /// Phase 2: call the VLM for one chunk of work items, each on its own
    /// scoped thread. A worker that panics yields an error for its asset
    /// instead of taking the run down.
    fn call_vlm(&self, chunk: &[WorkItem]) -> Vec<VlmCallResult> {
        let req = self.req;
        let verbosity = self.service.verbosity;
        std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|item| s.spawn(move || call_vlm_for(req, verbosity, &item.asset_id, &item.image_path)))
                .collect();

            handles
                .into_iter()
                .zip(chunk.iter())
                .map(|(h, item)| {
                    let (aid, elapsed, vlm_result) = h.join().unwrap_or_else(|_| {
                        let short_id = &item.asset_id[..8.min(item.asset_id.len())];
                        (
                            item.asset_id.clone(),
                            Duration::ZERO,
                            Err(format!("VLM worker panicked for {short_id}")),
                        )
                    });
                    (aid, item.existing_tags.clone(), elapsed, vlm_result)
                })
                .collect()
        })
    }

    /// Phase 3: apply one asset's VLM output (description, then tags
    /// deduplicated against the existing ones) and settle the asset.
    fn apply_output(
        &mut self,
        aid: &str,
        existing_tags: &HashSet<String>,
        elapsed: Duration,
        vlm_result: Result<vlm::VlmOutput, String>,
    ) {
        let short_id = &aid[..8.min(aid.len())];

        let output = match vlm_result {
            Err(msg) => {
                let error = DescribeError::Failed(msg);
                self.progress().record(row(aid, DescribeStatus::Error(error)), None, elapsed, Written::default());
                return;
            }
            Ok(output) => output,
        };

        let mut written = Written::default();
        if self.req.apply {
            // Apply description
            if let Some(ref desc) = output.description {
                if !desc.is_empty() {
                    let edit_fields = crate::query::EditFields {
                        name: None,
                        description: Some(Some(desc.clone())),
                        rating: None,
                        color_label: None,
                        created_at: None,
                    };
                    if let Err(e) = self.engine.edit(aid, edit_fields) {
                        let msg = format!("Failed to save description for {short_id}: {e}");
                        let r = DescribeResult {
                            asset_id: aid.to_string(),
                            description: output.description,
                            tags: output.tags,
                            status: DescribeStatus::Error(DescribeError::Failed(msg)),
                        };
                        self.progress().record(r, None, elapsed, written);
                        return;
                    }
                    written.description_set = true;
                }
            }

            // Apply tags — deduplicated against existing tags
            if !output.tags.is_empty() {
                let new_tags: Vec<String> = output
                    .tags
                    .iter()
                    .filter(|t| !existing_tags.contains(&t.to_lowercase()))
                    .cloned()
                    .collect();

                if !new_tags.is_empty() {
                    match self.engine.tag_with_source(aid, &new_tags, false, crate::models::TagSource::Vlm) {
                        Ok(_) => {
                            self.result.tags_applied += new_tags.len();
                            written.tags_applied = new_tags.len();
                        }
                        Err(e) => {
                            let msg = format!("Failed to apply tags for {short_id}: {e}");
                            self.result.errors.push(msg);
                        }
                    }
                }
            }
        }

        let r = DescribeResult {
            asset_id: aid.to_string(),
            description: output.description,
            tags: output.tags,
            status: DescribeStatus::Described,
        };
        self.progress().record(r, None, elapsed, written);
    }
}

/// One VLM call: encode the image, call the endpoint in the request's
/// mode, reject empty answers. Returns the asset id with the elapsed time
/// and the output or an error message.
fn call_vlm_for(
    req: &DescribeRequest,
    verbosity: crate::Verbosity,
    aid: &str,
    image_path: &Path,
) -> (String, Duration, Result<vlm::VlmOutput, String>) {
    let start = Instant::now();
    let short_id = &aid[..8.min(aid.len())];
    let params = &req.params;
    let model = &req.model;

    // Encode image to base64
    let vlm_max_edge = if params.max_image_edge > 0 { Some(params.max_image_edge) } else { None };
    let image_base64 = match vlm::encode_image_base64(image_path, vlm_max_edge) {
        Ok(b) => b,
        Err(e) => {
            return (
                aid.to_string(),
                start.elapsed(),
                Err(format!("Failed to read image for {short_id}: {e}")),
            );
        }
    };

    // Call VLM
    let prompt = params.prompt.as_deref()
        .unwrap_or_else(|| vlm::default_prompt_for_mode(req.mode));
    let outcome = match vlm::call_vlm_with_mode(
        &req.endpoint, model, &image_base64, prompt,
        params, req.mode, verbosity,
    ) {
        Ok(output) => {
            if output.description.as_ref().is_none_or(|d| d.is_empty())
                && output.tags.is_empty()
            {
                Err(format!(
                    "VLM returned empty response for {short_id} — \
                     model \"{model}\" may not support vision or failed to load"
                ))
            } else {
                Ok(output)
            }
        }
        Err(e) => Err(format!("VLM failed for {short_id}: {e}")),
    };
    (aid.to_string(), start.elapsed(), outcome)
}
