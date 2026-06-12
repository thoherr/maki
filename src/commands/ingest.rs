//! Ingest commands: `import`, `watch`, `create-sidecars`, `delete`.

use super::*;

/// Extracted body of `Commands::Import`. Kept as a free function so the
/// `run_command` match arm stays readable — the body is identical to the
/// inline version it replaced.
pub fn run_import_command(
    paths: Vec<String>,
    volume: Option<String>,
    profile: Option<String>,
    include: Vec<String>,
    skip: Vec<String>,
    add_tags: Vec<String>,
    dry_run: bool,
    auto_group: bool,
    smart: bool,
    #[cfg(feature = "ai")] embed: bool,
    #[cfg(feature = "pro")] describe: bool,
    json: bool,
    log: bool,
    #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    use maki::asset_service::{ImportEvent, ImportPhase, ImportRequest};

    let (catalog_root, config) = maki::config::load_config()?;

    // Canonicalize input paths against the working directory. The workflow
    // takes already-canonicalised paths so it can be called from contexts
    // (like the web handler) that don't have a meaningful CWD.
    let canonical_paths: Vec<PathBuf> = paths
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p)))
        .collect();

    let req = ImportRequest {
        paths: canonical_paths,
        volume_label: volume,
        profile: profile.clone(),
        include,
        skip,
        add_tags,
        dry_run,
        smart,
        auto_group,
        #[cfg(feature = "ai")]
        embed,
        #[cfg(not(feature = "ai"))]
        embed: false,
        #[cfg(feature = "pro")]
        describe,
        #[cfg(not(feature = "pro"))]
        describe: false,
    };

    if verbosity.verbose {
        eprintln!(
            "  Import: {} file(s){}",
            req.paths.len(),
            req.volume_label.as_ref().map(|v| format!(" on volume \"{v}\"")).unwrap_or_default()
        );
        if let Some(ref p) = profile {
            eprintln!("  Import: using profile \"{}\"", p);
        }
        if !req.add_tags.is_empty() {
            eprintln!("  Import: extra tags: {}", req.add_tags.join(", "));
        }
        if smart {
            eprintln!("  Import: smart previews enabled");
        }
    }

    let service = AssetService::new(&catalog_root, verbosity, &config.preview);
    let workflow_result = service.import_workflow(&req, &config, |evt| {
        // Per-event progress: only emit per-item lines in --log mode. Phase
        // boundaries print a one-line announcement when --log is on (or
        // verbose); skipped phases always print so the user knows why a
        // phase didn't run.
        match evt {
            ImportEvent::PhaseStarted(phase) => {
                if log && phase != ImportPhase::Import {
                    eprintln!("  Phase: {}", phase.label());
                }
            }
            ImportEvent::PhaseSkipped { phase, reason } => {
                eprintln!("  Skipping {} phase: {reason}", phase.label());
            }
            ImportEvent::File { path, status, elapsed } => {
                if !log { return; }
                use maki::asset_service::FileStatus;
                let label = match status {
                    FileStatus::Imported => "imported",
                    FileStatus::LocationAdded => "location added",
                    FileStatus::Skipped => "skipped",
                    FileStatus::RecipeAttached => "recipe",
                    FileStatus::RecipeLocationAdded => "recipe location added",
                    FileStatus::RecipeUpdated => "recipe updated",
                };
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                item_status(name, label, Some(elapsed));
            }
            #[cfg(feature = "ai")]
            ImportEvent::Embed { asset_id, status } => {
                if !log { return; }
                let short = &asset_id[..8.min(asset_id.len())];
                match status {
                    maki::asset_service::EmbedStatus::Embedded => {
                        eprintln!("  {short} — embedded");
                    }
                    maki::asset_service::EmbedStatus::Error(msg) => {
                        eprintln!("  {short} — embed error: {msg}");
                    }
                    maki::asset_service::EmbedStatus::Skipped(_) => {}
                }
            }
            #[cfg(feature = "pro")]
            ImportEvent::Describe { asset_id, status, elapsed } => {
                if !log { return; }
                let short = &asset_id[..8.min(asset_id.len())];
                match status {
                    maki::vlm::DescribeStatus::Described => {
                        item_status(short, "described", Some(elapsed));
                    }
                    maki::vlm::DescribeStatus::Skipped(msg) => {
                        eprintln!("  {short} — skipped: {msg}");
                    }
                    maki::vlm::DescribeStatus::Error(msg) => {
                        eprintln!("  {short} — error: {msg}");
                    }
                }
            }
        }
    })?;

    // Format output. The workflow returns a single bundle; the CLI just
    // unpacks it into JSON or human text. Frontend-specific concern only.
    let result = &workflow_result.import;
    let auto_group_result = workflow_result.auto_group.as_ref();
    #[cfg(feature = "ai")]
    let embed_result = workflow_result.embed.as_ref();
    #[cfg(feature = "pro")]
    let describe_result = workflow_result.describe.as_ref();

    if json {
        #[allow(unused_mut)]
        let mut json_val = serde_json::to_value(result)?;
        if let Some(ag) = auto_group_result {
            json_val["auto_group"] = serde_json::to_value(ag)?;
        }
        #[cfg(feature = "ai")]
        if let Some(er) = embed_result {
            json_val["embeddings_generated"] = serde_json::json!(er.embedded);
            json_val["embeddings_skipped"] = serde_json::json!(er.skipped);
        }
        #[cfg(feature = "pro")]
        if let Some(dr) = describe_result {
            json_val["descriptions_generated"] = serde_json::json!(dr.described);
            json_val["descriptions_skipped"] = serde_json::json!(dr.skipped);
            if dr.tags_applied > 0 {
                json_val["describe_tags_applied"] = serde_json::json!(dr.tags_applied);
            }
        }
        println!("{}", serde_json::to_string_pretty(&json_val)?);
    } else {
        let mut parts: Vec<String> = Vec::new();
        if result.imported > 0          { parts.push(format!("{} imported", result.imported)); }
        if result.skipped > 0           { parts.push(format!("{} skipped", result.skipped)); }
        if result.locations_added > 0   { parts.push(format!("{} location(s) added", result.locations_added)); }
        if result.recipes_attached > 0  { parts.push(format!("{} recipe(s) attached", result.recipes_attached)); }
        if result.recipes_location_added > 0 { parts.push(format!("{} recipe location(s) added", result.recipes_location_added)); }
        if result.recipes_updated > 0   { parts.push(format!("{} recipe(s) updated", result.recipes_updated)); }
        if result.previews_generated > 0       { parts.push(format!("{} preview(s) generated", result.previews_generated)); }
        if result.smart_previews_generated > 0 { parts.push(format!("{} smart preview(s) generated", result.smart_previews_generated)); }
        #[cfg(feature = "ai")]
        if let Some(er) = embed_result {
            if er.embedded > 0 { parts.push(format!("{} embedding(s) generated", er.embedded)); }
        }
        #[cfg(feature = "pro")]
        if let Some(dr) = describe_result {
            if dr.described > 0 { parts.push(format!("{} described", dr.described)); }
        }
        if parts.is_empty() {
            println!("Import: nothing to import");
        } else if dry_run {
            println!("Dry run — would import: {}", parts.join(", "));
        } else {
            println!("Import complete: {}", parts.join(", "));
        }

        if let Some(ag) = auto_group_result {
            if log {
                for group in &ag.groups {
                    let short_id = &group.target_id[..8.min(group.target_id.len())];
                    eprintln!(
                        "  {} — {} asset(s) → target {short_id}",
                        group.stem,
                        group.asset_ids.len(),
                    );
                }
            }
            println!(
                "Auto-group: {} stem group(s), {} donor(s) {}, {} variant(s) moved",
                ag.groups.len(),
                ag.total_donors_merged,
                if dry_run { "would merge" } else { "merged" },
                ag.total_variants_moved,
            );
        }

        // Tier-A hints: tell users about post-import phases they didn't
        // engage. The `*_result.is_none()` check covers both "didn't opt in"
        // and "opted in but the phase was skipped" (model missing, VLM
        // unreachable). Only emit on a real (non-dry) successful import.
        if !dry_run && result.imported > 0 {
            #[cfg(feature = "ai")]
            {
                if !embed && !config.import.embeddings {
                    println!(
                        "  Tip: run 'maki embed' to generate visual-similarity \
                         embeddings (or pass --embed on import / set \
                         [import] embeddings = true in maki.toml)."
                    );
                }
            }
            #[cfg(feature = "pro")]
            {
                if !describe && !config.import.descriptions {
                    println!(
                        "  Tip: run 'maki describe' to generate VLM \
                         descriptions (or pass --describe on import / set \
                         [import] descriptions = true in maki.toml)."
                    );
                }
            }
        }
    }
    Ok(())
}

/// Per-tick action counters for `maki watch`.
#[derive(Default, serde::Serialize)]
struct WatchTickSummary {
    imported: usize,
    locations_added: usize,
    recipes_attached: usize,
    recipes_updated: usize,
    recipes_refreshed: usize,
    skipped: usize,
    errors: Vec<String>,
}

impl WatchTickSummary {
    /// Human-readable counter fragments, omitting zeros.
    fn parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if self.imported > 0 { parts.push(format!("{} imported", self.imported)); }
        if self.locations_added > 0 { parts.push(format!("{} location(s) added", self.locations_added)); }
        if self.recipes_attached > 0 { parts.push(format!("{} recipe(s) attached", self.recipes_attached)); }
        if self.recipes_updated > 0 { parts.push(format!("{} recipe(s) updated", self.recipes_updated)); }
        if self.recipes_refreshed > 0 { parts.push(format!("{} recipe(s) refreshed", self.recipes_refreshed)); }
        if self.skipped > 0 { parts.push(format!("{} skipped", self.skipped)); }
        parts
    }
}

/// Group candidate paths by their owning registered volume. Paths whose
/// volume cannot be resolved (e.g. the volume went offline mid-watch)
/// are recorded as errors instead of aborting the tick.
fn group_paths_by_volume(
    registry: &DeviceRegistry,
    paths: &[PathBuf],
    errors: &mut Vec<String>,
) -> Vec<(maki::models::Volume, Vec<PathBuf>)> {
    let mut groups: Vec<(maki::models::Volume, Vec<PathBuf>)> = Vec::new();
    for p in paths {
        match registry.find_volume_for_path(p) {
            Ok(vol) => {
                if let Some((_, list)) = groups.iter_mut().find(|(v, _)| v.id == vol.id) {
                    list.push(p.clone());
                } else {
                    groups.push((vol, vec![p.clone()]));
                }
            }
            Err(e) => errors.push(format!("{}: {e}", p.display())),
        }
    }
    groups
}

/// Run one watch tick's actions: feed new stable files through the
/// regular import pipeline (per owning volume) and changed recipe files
/// through the refresh pipeline. Errors are collected per volume group
/// so one failing group never aborts the others (or the watch loop).
fn watch_act(
    service: &AssetService,
    registry: &DeviceRegistry,
    config: &maki::config::CatalogConfig,
    excludes: &[String],
    to_import: &[PathBuf],
    to_refresh: &[PathBuf],
    log: bool,
) -> WatchTickSummary {
    use maki::asset_service::{FileStatus, ImportEvent, ImportRequest, RefreshStatus};

    let mut summary = WatchTickSummary::default();

    // ── Import: new stable files, one workflow call per volume ─────
    for (vol, group) in group_paths_by_volume(registry, to_import, &mut summary.errors) {
        let req = ImportRequest {
            paths: group,
            volume_label: Some(vol.label.clone()),
            profile: None,
            include: Vec::new(),
            skip: Vec::new(),
            add_tags: Vec::new(),
            dry_run: false,
            smart: false,
            auto_group: false,
            embed: false,
            describe: false,
        };
        let result = service.import_workflow(&req, config, |evt| {
            if !log { return; }
            if let ImportEvent::File { path, status, elapsed } = evt {
                let label = match status {
                    FileStatus::Imported => "imported",
                    FileStatus::LocationAdded => "location added",
                    FileStatus::Skipped => "skipped",
                    FileStatus::RecipeAttached => "recipe",
                    FileStatus::RecipeLocationAdded => "recipe location added",
                    FileStatus::RecipeUpdated => "recipe updated",
                };
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                item_status(name, label, Some(elapsed));
            }
        });
        match result {
            Ok(r) => {
                summary.imported += r.import.imported;
                summary.locations_added += r.import.locations_added;
                summary.recipes_attached += r.import.recipes_attached;
                summary.recipes_updated += r.import.recipes_updated;
                summary.skipped += r.import.skipped;
            }
            Err(e) => summary.errors.push(format!("import on \"{}\": {e:#}", vol.label)),
        }
    }

    // ── Refresh: changed recipe files already tracked by the catalog ──
    for (vol, group) in group_paths_by_volume(registry, to_refresh, &mut summary.errors) {
        let result = service.refresh(
            &group,
            Some(&vol),
            None,
            false,
            false,
            excludes,
            |path, status, elapsed| {
                if !log { return; }
                let label = match status {
                    RefreshStatus::Unchanged => "unchanged",
                    RefreshStatus::Refreshed => "recipe refreshed",
                    RefreshStatus::Missing => "missing",
                    RefreshStatus::Offline => "offline",
                    RefreshStatus::SidecarPresent => "skipped (sidecar present)",
                };
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                item_status(name, label, Some(elapsed));
            },
        );
        match result {
            Ok(r) => {
                summary.recipes_refreshed += r.refreshed;
                summary.errors.extend(r.errors);
            }
            Err(e) => summary.errors.push(format!("refresh on \"{}\": {e:#}", vol.label)),
        }
    }

    summary
}

/// Sleep for `total`, polling the stop flag every 200ms. Returns false
/// when the flag was raised (Ctrl-C) before the sleep completed.
fn sleep_unless_stopped(
    total: std::time::Duration,
    stop: &std::sync::atomic::AtomicBool,
) -> bool {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let mut remaining = total;
    while remaining > Duration::ZERO {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let step = remaining.min(Duration::from_millis(200));
        std::thread::sleep(step);
        remaining -= step;
    }
    !stop.load(Ordering::SeqCst)
}

/// Extracted body of `Commands::Watch`. See `run_import_command` for the
/// extraction pattern.
pub fn run_watch_command(
    paths: Vec<String>,
    volume: Option<String>,
    interval: Option<u64>,
    once: bool,
    json: bool,
    log: bool,
    verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use maki::asset_service::{is_recipe_path, WatchScanner};

    let (catalog_root, config) = maki::config::load_config()?;
    let registry = DeviceRegistry::new(&catalog_root);

    // ── Resolve watch roots ────────────────────────────────────────
    let volume_filter = volume
        .as_deref()
        .map(|l| registry.resolve_volume(l))
        .transpose()?;
    let mut roots: Vec<PathBuf> = Vec::new();
    if !paths.is_empty() {
        for p in &paths {
            let cp = std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
            // Same containment requirement (and error) as import:
            // watched paths must lie inside a registered volume.
            let vol = registry.find_volume_for_path(&cp)?;
            if let Some(ref vf) = volume_filter {
                if vol.id != vf.id {
                    anyhow::bail!(
                        "path {} is not on volume \"{}\"",
                        cp.display(),
                        vf.label
                    );
                }
            }
            roots.push(cp);
        }
    } else if let Some(vf) = volume_filter {
        if !vf.is_online {
            anyhow::bail!(
                "volume \"{}\" is offline (mount point {} not found)",
                vf.label,
                vf.mount_point.display()
            );
        }
        roots.push(vf.mount_point.clone());
    } else {
        for v in registry.list()? {
            if v.is_online {
                roots.push(v.mount_point.clone());
            }
        }
        if roots.is_empty() {
            anyhow::bail!(
                "no online volumes to watch — register one with `maki volume add` or pass PATHS"
            );
        }
    }

    // ── Excludes: [import] exclude + [watch] exclude (additive) ────
    let mut excludes = config.import.exclude.clone();
    excludes.extend(config.watch.exclude.iter().cloned());

    // ── Never track the catalog's own derived files (relevant when a
    //    watched root contains the catalog directory) ───────────────
    let canonical_root = std::fs::canonicalize(&catalog_root)
        .unwrap_or_else(|_| catalog_root.clone());
    let ignore_prefixes: Vec<PathBuf> =
        ["previews", "smart-previews", "metadata", "embeddings", "faces", ".trash"]
            .iter()
            .map(|d| canonical_root.join(d))
            .collect();

    let poll_seconds = interval.unwrap_or(config.watch.poll_seconds).max(2);
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);

    if once {
        // ── Single cycle: scan, ~1s stability wait, re-scan, act ───
        // Unlike the continuous loop (whose baseline is "what's on disk
        // when the watch starts"), --once diffs against the *catalog*:
        // stable files the catalog doesn't track yet are imported, and
        // tracked .xmp recipes are refreshed if their content changed.
        let mut scanner = WatchScanner::new(roots.clone(), excludes.clone(), ignore_prefixes, true);
        let baseline = scanner.scan();
        if verbosity.verbose {
            eprintln!(
                "  Watch: single cycle over {} root(s), {} candidate file(s); waiting 1s for stability",
                roots.len(),
                baseline.tracked
            );
        }
        std::thread::sleep(Duration::from_millis(1100));
        let outcome = scanner.scan();

        let mut stable = outcome.new_stable;
        stable.extend(outcome.changed_stable);

        let mut to_import: Vec<PathBuf> = Vec::new();
        let mut to_refresh: Vec<PathBuf> = Vec::new();
        let mut pre_errors: Vec<String> = Vec::new();
        {
            let catalog = Catalog::open(&catalog_root)?;
            for path in stable {
                let vol = match registry.find_volume_for_path(&path) {
                    Ok(v) => v,
                    Err(e) => {
                        pre_errors.push(format!("{}: {e}", path.display()));
                        continue;
                    }
                };
                let rel = match path.strip_prefix(&vol.mount_point) {
                    Ok(r) => r.to_string_lossy().to_string(),
                    Err(_) => continue,
                };
                let vol_id = vol.id.to_string();
                if is_recipe_path(&path) {
                    if catalog.find_recipe_by_volume_and_path(&vol_id, &rel)?.is_some() {
                        // Tracked recipe — refresh hash-compares and
                        // reapplies XMP only when the content changed.
                        to_refresh.push(path);
                    } else if catalog.find_variant_by_volume_and_path(&vol_id, &rel)?.is_none() {
                        to_import.push(path);
                    }
                } else if catalog.find_variant_by_volume_and_path(&vol_id, &rel)?.is_none() {
                    to_import.push(path);
                }
            }
        }

        let mut summary = watch_act(&service, &registry, &config, &excludes, &to_import, &to_refresh, log);
        summary.errors.extend(pre_errors);

        for e in &summary.errors {
            eprintln!("  {e}");
        }
        if outcome.pending > 0 {
            eprintln!(
                "  {} file(s) still changing — run `maki watch --once` again once they settle",
                outcome.pending
            );
        }
        if json {
            println!("{}", serde_json::to_string_pretty(&summary)?);
        } else {
            let parts = summary.parts();
            if parts.is_empty() {
                println!("Watch: nothing to do");
            } else {
                println!("Watch: {}", parts.join(", "));
            }
        }
        return Ok(());
    }

    // ── Continuous foreground loop (like `maki serve`) ─────────────
    // Ctrl-C raises a flag via tokio's signal handler; the loop checks
    // it while sleeping so shutdown is prompt and clean.
    let stop = Arc::new(AtomicBool::new(false));
    let rt = tokio::runtime::Runtime::new()?;
    {
        let stop = stop.clone();
        rt.spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            stop.store(true, Ordering::SeqCst);
        });
    }

    let mut scanner = WatchScanner::new(roots.clone(), excludes.clone(), ignore_prefixes, false);
    let baseline = scanner.scan();
    eprintln!(
        "Watching {} root(s) every {}s — baseline: {} file(s), not imported (run `maki import` for any backlog). Ctrl-C to stop.",
        roots.len(),
        poll_seconds,
        baseline.tracked
    );
    if verbosity.verbose {
        for r in &roots {
            eprintln!("  watching {}", r.display());
        }
    }

    loop {
        if !sleep_unless_stopped(Duration::from_secs(poll_seconds), &stop) {
            break;
        }
        let outcome = scanner.scan();
        if verbosity.verbose {
            for p in &outcome.deleted {
                eprintln!("  deleted (ignored — see `maki cleanup`): {}", p.display());
            }
        }

        let to_import = outcome.new_stable;
        let mut to_refresh: Vec<PathBuf> = Vec::new();
        for p in outcome.changed_stable {
            if is_recipe_path(&p) {
                to_refresh.push(p);
            } else if verbosity.verbose {
                eprintln!("  changed (ignored — see `maki sync`): {}", p.display());
            }
        }

        if to_import.is_empty() && to_refresh.is_empty() {
            if verbosity.verbose {
                eprintln!(
                    "[{}] Watch: quiet tick ({} tracked, {} pending)",
                    chrono::Local::now().format("%H:%M:%S"),
                    outcome.tracked,
                    outcome.pending
                );
            }
            continue;
        }

        let summary = watch_act(&service, &registry, &config, &excludes, &to_import, &to_refresh, log);
        for e in &summary.errors {
            eprintln!("  {e}");
        }
        let parts = summary.parts();
        eprintln!(
            "[{}] Watch: {}",
            chrono::Local::now().format("%H:%M:%S"),
            if parts.is_empty() { "no changes applied".to_string() } else { parts.join(", ") }
        );
        if json {
            println!("{}", serde_json::to_string(&summary)?);
        }
    }

    eprintln!("Watch stopped.");
    Ok(())
}

/// Extracted body of `Commands::CreateSidecars`. See `run_import_command` for the
/// extraction pattern.
pub fn run_create_sidecars_command(
        query: Option<String>,
        volume: Option<String>,
        asset: Option<String>,
        apply: bool,
        asset_ids: Vec<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let catalog_root = maki::config::find_catalog_root()?;
    let catalog = Catalog::open(&catalog_root)?;
    let metadata_store = MetadataStore::new(&catalog_root);
    let content_store = maki::content_store::ContentStore::new(&catalog_root);
    let registry = DeviceRegistry::new(&catalog_root);
    let engine = maki::query::QueryEngine::new(&catalog_root);
    let volumes = registry.list()?;

    // Resolve target volume filter
    let volume_filter = if let Some(ref vol_label) = volume {
        Some(registry.resolve_volume(vol_label)?)
    } else {
        None
    };

    // Resolve asset scope
    let scope = engine.resolve_scope(query.as_deref(), asset.as_deref(), &asset_ids)?;
    let summaries = metadata_store.list()?;
    let ids: Vec<uuid::Uuid> = match scope {
        Some(set) => set.iter().filter_map(|id| id.parse().ok()).collect(),
        None => summaries.iter().map(|s| s.id).collect(),
    };

    let mut created = 0usize;
    let mut skipped = 0usize;
    let mut checked = 0usize;

    for asset_id in &ids {
        let mut asset = match metadata_store.load(*asset_id) {
            Ok(a) => a,
            Err(_) => continue,
        };

        let has_metadata = !asset.tags.is_empty()
            || asset.rating.is_some()
            || asset.color_label.is_some()
            || asset.description.is_some();

        if !has_metadata {
            skipped += 1;
            continue;
        }

        checked += 1;
        let mut asset_changed = false;

        for variant in &asset.variants {
            for loc in &variant.locations {
                // Filter by volume if specified
                if let Some(ref vf) = volume_filter {
                    if loc.volume_id != vf.id {
                        continue;
                    }
                }

                // Check if volume is online
                let vol = match volumes.iter().find(|v| v.id == loc.volume_id) {
                    Some(v) if v.mount_point.exists() => v,
                    _ => continue,
                };

                // Check if this variant already has an XMP recipe on this volume
                let has_xmp = asset.recipes.iter().any(|r| {
                    r.variant_hash == variant.content_hash
                        && r.location.volume_id == loc.volume_id
                        && r.recipe_type == maki::models::recipe::RecipeType::Sidecar
                });
                if has_xmp {
                    continue;
                }

                // Build XMP sidecar path
                let xmp_relative = loc.relative_path.with_extension(
                    format!("{}.xmp", loc.relative_path.extension().unwrap_or_default().to_string_lossy())
                );
                let xmp_path = vol.mount_point.join(&xmp_relative);

                if cli.log {
                    let name = xmp_relative.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("?");
                    if apply {
                        eprintln!("  {} — created", name);
                    } else {
                        eprintln!("  {} — would create", name);
                    }
                }

                if apply {
                    if let Some(parent) = xmp_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }

                    let xmp_content = maki::xmp_reader::create_xmp(
                        &asset.tags,
                        asset.rating,
                        asset.color_label.as_deref(),
                        asset.description.as_deref(),
                    );

                    std::fs::write(&xmp_path, &xmp_content)?;
                    let xmp_hash = content_store.hash_file(&xmp_path)?;

                    let recipe = maki::models::recipe::Recipe {
                        id: uuid::Uuid::new_v4(),
                        variant_hash: variant.content_hash.clone(),
                        software: "MAKI".to_string(),
                        recipe_type: maki::models::recipe::RecipeType::Sidecar,
                        content_hash: xmp_hash,
                        location: maki::models::FileLocation {
                            volume_id: loc.volume_id,
                            relative_path: xmp_relative,
                            verified_at: None,
                        },
                        pending_writeback: false,
                    };
                    catalog.insert_recipe(&recipe)?;
                    asset.recipes.push(recipe);
                    asset_changed = true;
                }

                created += 1;
            }
        }

        if asset_changed {
            metadata_store.save(&asset)?;
        }
    }

    if cli.json {
        let result = serde_json::json!({
            "dry_run": !apply,
            "checked": checked,
            "created": created,
            "skipped_no_metadata": skipped,
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        if !apply && created > 0 {
            eprint!("Dry run — ");
        }
        println!("Create-sidecars: {} checked, {} created, {} skipped (no metadata)", checked, created, skipped);
        if !apply && created > 0 {
            println!("  Run with --apply to create XMP files.");
        }
    }

    Ok(())
}

/// Extracted body of `Commands::Delete`. See `run_import_command` for the
/// extraction pattern.
pub fn run_delete_command(
        asset_ids: Vec<String>,
        apply: bool,
        remove_files: bool,
        no_trash: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    if remove_files && !apply {
        anyhow::bail!("--remove-files requires --apply");
    }

    // Read from stdin if no IDs provided
    let ids: Vec<String> = if asset_ids.is_empty() {
        use std::io::BufRead;
        std::io::stdin().lock().lines()
            .filter_map(|l| l.ok())
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    } else {
        asset_ids
    };
    if ids.is_empty() {
        anyhow::bail!("no asset IDs specified.");
    }

    let (catalog_root, config) = maki::config::load_config()?;
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);

    // Collect face IDs for deleted assets before deletion (for AI cleanup)
    #[cfg(feature = "ai")]
    let ai_cleanup_info: Vec<(String, Vec<String>)> = if apply {
        let catalog = maki::catalog::Catalog::open(&catalog_root)?;
        let _ = maki::face_store::FaceStore::initialize(catalog.conn());
        let face_store = maki::face_store::FaceStore::new(catalog.conn());
        ids.iter().filter_map(|id| {
            let full_id = catalog.resolve_asset_id(id).ok().flatten()?;
            let faces = face_store.faces_for_asset(&full_id).unwrap_or_default();
            let face_ids: Vec<String> = faces.into_iter().map(|f| f.id).collect();
            Some((full_id, face_ids))
        }).collect()
    } else {
        Vec::new()
    };

    let show_log = cli.log;
    let result = service.delete_assets(
        &ids,
        apply,
        remove_files,
        no_trash,
        |id, status, elapsed| {
            if show_log {
                let short_id = &id[..8.min(id.len())];
                match status {
                    maki::asset_service::DeleteStatus::Deleted => {
                        item_status(short_id, "deleted", Some(elapsed));
                    }
                    maki::asset_service::DeleteStatus::NotFound => {
                        item_status(short_id, "not found", None);
                    }
                    maki::asset_service::DeleteStatus::Error(msg) => {
                        eprintln!("  {short_id} — error: {msg}");
                    }
                }
            }
        },
    )?;

    // Clean up AI files for deleted assets
    #[cfg(feature = "ai")]
    if apply && result.deleted > 0 {
        for (asset_id, face_ids) in &ai_cleanup_info {
            // Delete ArcFace binaries for each face
            for face_id in face_ids {
                maki::face_store::delete_arcface_binary(&catalog_root, face_id);
            }
            // Delete per-asset embedding binaries for every model directory
            // present on disk (a hardcoded model list here leaked binaries
            // for newer models — same bug class as the orphan-purge fix).
            // The arcface dir is keyed by face ID (handled above), so
            // `<asset_id>.bin` simply doesn't exist there.
            if let Ok(entries) = std::fs::read_dir(catalog_root.join("embeddings")) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let model = entry.file_name().to_string_lossy().to_string();
                        maki::embedding_store::delete_embedding_binary(&catalog_root, &model, asset_id);
                    }
                }
            }
        }
        // Update faces/people YAML
        let catalog = maki::catalog::Catalog::open(&catalog_root)?;
        let _ = maki::face_store::FaceStore::initialize(catalog.conn());
        let face_store = maki::face_store::FaceStore::new(catalog.conn());
        let _ = face_store.save_all_yaml(&catalog_root);
    }

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        for err in &result.errors {
            eprintln!("  {err}");
        }

        if apply {
            let mut parts = vec![
                format!("{} deleted", result.deleted),
            ];
            if !result.not_found.is_empty() {
                parts.push(format!("{} not found", result.not_found.len()));
            }
            if result.files_removed > 0 {
                parts.push(format!("{} files removed", result.files_removed));
            }
            if result.previews_removed > 0 {
                parts.push(format!("{} previews removed", result.previews_removed));
            }
            println!("Delete complete: {}", parts.join(", "));
            if result.files_trashed > 0 {
                println!(
                    "  {} file(s) moved to the trash — recoverable via `maki trash list` / `maki trash restore`.",
                    result.files_trashed
                );
            }
        } else {
            let mut parts = vec![
                format!("{} would be deleted", result.deleted),
            ];
            if !result.not_found.is_empty() {
                parts.push(format!("{} not found", result.not_found.len()));
            }
            println!("Delete (dry run): {}", parts.join(", "));
            if result.deleted > 0 {
                println!("  Run with --apply to delete.");
            }
        }
    }
    Ok(())
}
