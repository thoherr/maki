//! `maki faces` — face detection, clustering, and people management (AI feature).

use super::*;

/// Execute a parsed CLI command. Returns asset IDs produced by the command (for shell _ variable).
#[cfg(feature = "ai")]
pub fn run_faces_command(
    cmd: FacesCommands,
    json: bool,
    log: bool,
    #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    let catalog_root = maki::config::find_catalog_root()?;
    let config = maki::config::CatalogConfig::load(&catalog_root)?;
    let face_model_dir = maki::face::resolve_face_model_dir(&config.ai);

    // Shadow `cli` with a lightweight struct carrying the two flags the
    // faces subcommands read, so the extracted body can keep referencing
    // `cli.json` and `cli.log` unchanged.
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };

    match cmd {
        FacesCommands::Download => {
            maki::face::FaceDetector::download_models(&face_model_dir, |name, i, total| {
                eprintln!("  Downloading {name} ({i}/{total})...");
            })?;
            println!("Face models downloaded to {}", face_model_dir.display());
            Ok(())
        }
        FacesCommands::Status => {
            let exists = maki::face::FaceDetector::models_exist(&face_model_dir);
            let current_model = maki::face::RECOGNITION_MODEL.id;
            println!("Face model directory: {}", face_model_dir.display());
            println!("Models downloaded: {}", if exists { "yes" } else { "no" });
            println!("Current recognition model: {current_model}");

            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let _ = maki::face_store::FaceStore::initialize(catalog.conn());
            let store = maki::face_store::FaceStore::new(catalog.conn());
            println!("Total faces detected: {}", store.total_faces());
            println!("Total people: {}", store.total_people());

            let by_model = store.face_counts_by_model().unwrap_or_default();
            if !by_model.is_empty() {
                println!("Faces by recognition model:");
                for (model, count) in &by_model {
                    let marker = if model == current_model { " (current)" } else { "" };
                    println!("  {model}: {count}{marker}");
                }
                let stale: u32 = by_model.iter()
                    .filter(|(m, _)| m != current_model)
                    .map(|(_, c)| *c).sum();
                if stale > 0 {
                    println!("  {stale} face(s) use a different recognition model and will be");
                    println!("  ignored by clustering. Re-run `maki faces detect --force` to update.");
                }
            }

            if cli.json {
                let json = serde_json::json!({
                    "model_dir": face_model_dir.to_string_lossy(),
                    "models_downloaded": exists,
                    "current_recognition_model": current_model,
                    "total_faces": store.total_faces(),
                    "total_people": store.total_people(),
                    "faces_by_model": by_model.iter().map(|(m, c)| serde_json::json!({"model": m, "count": c})).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
            Ok(())
        }
        FacesCommands::Detect { query, asset, volume, min_confidence, apply, force } => {
            if !maki::face::FaceDetector::models_exist(&face_model_dir) {
                anyhow::bail!(
                    "Face models not downloaded. Run 'maki faces download' first."
                );
            }

            if query.is_none() && asset.is_none() && volume.is_none() {
                anyhow::bail!(
                    "No scope specified. Use --query, --asset, or --volume to select assets.\n  \
                     Examples:\n    \
                     maki faces detect --query '' --apply     # all assets\n    \
                     maki faces detect --asset <id> --apply   # single asset\n    \
                     maki faces detect --volume <label> --apply"
                );
            }

            let engine = QueryEngine::new(&catalog_root);
            let service = AssetService::new(&catalog_root, verbosity, &config.preview);

            // Resolve target assets
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let asset_ids: Vec<String> = if let Some(ref aid) = asset {
                let full_id = catalog
                    .resolve_asset_id(aid)?
                    .ok_or_else(|| anyhow::anyhow!("no asset found matching '{aid}'"))?;
                vec![full_id]
            } else {
                let q = if let Some(ref query) = query {
                    let volume_part = volume.as_deref().map(|v| format!(" volume:{v}")).unwrap_or_default();
                    format!("{query}{volume_part}")
                } else if let Some(ref v) = volume {
                    format!("volume:{v}")
                } else {
                    "*".to_string()
                };
                let results = engine.search(&q)?;
                let mut seen = std::collections::HashSet::new();
                results.into_iter()
                    .filter(|r| seen.insert(r.asset_id.clone()))
                    .map(|r| r.asset_id)
                    .collect()
            };
            drop(catalog);

            let mut detector = maki::face::FaceDetector::load_with_provider(&face_model_dir, verbosity, &config.ai.execution_provider)?;

            let show_log = cli.log;
            let result = service.detect_faces(
                &asset_ids,
                &mut detector,
                min_confidence,
                force,
                apply,
                |aid, n, elapsed| {
                    if show_log {
                        let short_id = &aid[..8.min(aid.len())];
                        if n == 0 {
                            item_status(short_id, "skipped", Some(elapsed));
                        } else {
                            eprintln!(
                                "  {short_id} — {} face{} detected ({})",
                                n, if n == 1 { "" } else { "s" }, format_duration(elapsed)
                            );
                        }
                    }
                },
            )?;

            let total_faces = result.faces_detected;
            let total_assets = result.assets_processed;
            let total_skipped = result.assets_skipped;
            let errors = result.errors;

            if cli.json {
                let json = serde_json::json!({
                    "assets_processed": total_assets,
                    "assets_skipped": total_skipped,
                    "faces_detected": total_faces,
                    "errors": errors,
                    "dry_run": !apply,
                });
                println!("{}", serde_json::to_string_pretty(&json)?);
            } else {
                for err in &errors {
                    eprintln!("  {err}");
                }
                let mode = if apply { "Face detect" } else { "Face detect (dry run)" };
                let mut parts = vec![
                    format!("{total_assets} assets processed"),
                ];
                if total_skipped > 0 {
                    parts.push(format!("{total_skipped} skipped"));
                }
                parts.push(format!("{total_faces} faces detected"));
                if !errors.is_empty() {
                    parts.push(format!("{} errors", errors.len()));
                }
                println!("{mode}: {}", parts.join(", "));
                if !apply && total_faces > 0 {
                    println!("  Run with --apply to store face detections.");
                }
            }
            Ok(())
        }
        FacesCommands::Cluster { query, asset, volume, threshold, min_confidence, apply } => {
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let _ = maki::face_store::FaceStore::initialize(catalog.conn());
            let face_store = maki::face_store::FaceStore::new(catalog.conn());

            // CLI threshold/min_confidence args are f32 (clap default for `--threshold`);
            // config carries f64 since v4.5.5 (for clean TOML round-trips). Cast to f32
            // at this boundary so the rest of the function sees a single type.
            let thresh = threshold.unwrap_or(config.ai.face_cluster_threshold as f32);
            let min_conf = min_confidence.unwrap_or(config.ai.face_min_confidence as f32);

            // Resolve scope to asset IDs (same pattern as maki embed)
            let scoped_ids: Option<Vec<String>> = if query.is_some() || asset.is_some() || volume.is_some() {
                let engine = QueryEngine::new(&catalog_root);
                if let Some(ref a) = asset {
                    let full_id = catalog
                        .resolve_asset_id(a)?
                        .ok_or_else(|| anyhow::anyhow!("no asset found matching '{a}'"))?;
                    Some(vec![full_id])
                } else {
                    let q = if let Some(ref query) = query {
                        let volume_part = volume.as_deref().map(|v| format!(" volume:{v}")).unwrap_or_default();
                        format!("{query}{volume_part}")
                    } else if let Some(ref v) = volume {
                        format!("volume:{v}")
                    } else {
                        "*".to_string()
                    };
                    let rows = engine.search(&q)?;
                    Some(rows.into_iter().map(|r| r.asset_id).collect())
                }
            } else {
                None
            };
            let scope = scoped_ids.as_deref();

            if apply {
                let result = face_store.auto_cluster(thresh, min_conf, scope)?;
                // Persist faces/people YAML
                if let Err(e) = face_store.save_all_yaml(&catalog_root) {
                    eprintln!("  Warning: failed to save faces/people YAML: {e:#}");
                }
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!(
                        "Clustered: {} people created, {} faces assigned, {} singletons skipped",
                        result.people_created, result.faces_assigned, result.singletons_skipped
                    );
                }
            } else {
                let (clusters, _unassigned) = face_store.cluster_faces(thresh, min_conf, scope)?;
                let total_faces: usize = clusters.iter().map(|c| c.len()).sum();
                if cli.json {
                    println!("{}", serde_json::json!({
                        "dry_run": true,
                        "clusters": clusters.len(),
                        "faces_in_clusters": total_faces,
                        "cluster_sizes": clusters.iter().map(|c| c.len()).collect::<Vec<_>>(),
                        "threshold": thresh,
                        "min_confidence": min_conf,
                    }));
                } else {
                    println!("Cluster preview (threshold={thresh:.2}, min_confidence={min_conf:.2}):");
                    for (i, cluster) in clusters.iter().enumerate() {
                        println!("  Cluster {}: {} faces", i + 1, cluster.len());
                    }
                    println!("Total: {} clusters, {} faces", clusters.len(), total_faces);
                    println!("  Run with --apply to create people and assign faces.");
                }
            }
            Ok(())
        }
        FacesCommands::People => {
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let _ = maki::face_store::FaceStore::initialize(catalog.conn());
            let face_store = maki::face_store::FaceStore::new(catalog.conn());

            let people = face_store.list_people()?;
            if cli.json {
                let json_people: Vec<_> = people.iter().map(|(p, count)| {
                    serde_json::json!({
                        "id": p.id,
                        "name": p.name,
                        "representative_face_id": p.representative_face_id,
                        "face_count": count,
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&json_people)?);
            } else {
                if people.is_empty() {
                    println!("No people found. Run 'maki faces cluster --apply' to create people from detected faces.");
                } else {
                    println!("{:<10} {:<30} {}", "ID", "Name", "Faces");
                    for (person, count) in &people {
                        let short_id = &person.id[..8.min(person.id.len())];
                        let name = person.name.as_deref().unwrap_or("(unnamed)");
                        println!("{:<10} {:<30} {}", short_id, name, count);
                    }
                }
            }
            Ok(())
        }
        FacesCommands::Name { person_id, name } => {
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let _ = maki::face_store::FaceStore::initialize(catalog.conn());
            let face_store = maki::face_store::FaceStore::new(catalog.conn());

            // Resolve person ID prefix
            let full_id = resolve_person_id(&face_store, &person_id)?;
            face_store.name_person(&full_id, &name)?;
            let _ = face_store.save_all_yaml(&catalog_root);
            let short = &full_id[..8.min(full_id.len())];
            println!("Named person {short} as \"{name}\"");
            Ok(())
        }
        FacesCommands::Merge { target_id, source_id } => {
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let _ = maki::face_store::FaceStore::initialize(catalog.conn());
            let face_store = maki::face_store::FaceStore::new(catalog.conn());

            let target = resolve_person_id(&face_store, &target_id)?;
            let source = resolve_person_id(&face_store, &source_id)?;
            let moved = face_store.merge_people(&target, &source)?;
            let _ = face_store.save_all_yaml(&catalog_root);
            let short_t = &target[..8.min(target.len())];
            let short_s = &source[..8.min(source.len())];
            println!("Merged {short_s} into {short_t}: {moved} faces moved");
            Ok(())
        }
        FacesCommands::DeletePerson { person_id } => {
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let _ = maki::face_store::FaceStore::initialize(catalog.conn());
            let face_store = maki::face_store::FaceStore::new(catalog.conn());

            let full_id = resolve_person_id(&face_store, &person_id)?;
            face_store.delete_person(&full_id)?;
            let _ = face_store.save_all_yaml(&catalog_root);
            let short = &full_id[..8.min(full_id.len())];
            println!("Deleted person {short} (faces unassigned)");
            Ok(())
        }
        FacesCommands::Unassign { face_id } => {
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let _ = maki::face_store::FaceStore::initialize(catalog.conn());
            let face_store = maki::face_store::FaceStore::new(catalog.conn());

            // Resolve face ID prefix
            let full_id = resolve_face_id(&face_store, &face_id)?;
            face_store.unassign_face(&full_id)?;
            let _ = face_store.save_all_yaml(&catalog_root);
            let short = &full_id[..8.min(full_id.len())];
            println!("Unassigned face {short} from its person");
            Ok(())
        }
        FacesCommands::Clean { apply } => {
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let _ = maki::face_store::FaceStore::initialize(catalog.conn());
            let face_store = maki::face_store::FaceStore::new(catalog.conn());

            let count = face_store.count_unassigned_faces()?;
            if cli.json {
                if apply {
                    let deleted = face_store.delete_unassigned_faces()?;
                    if let Err(e) = face_store.save_all_yaml(&catalog_root) {
                        eprintln!("  Warning: failed to save faces/people YAML: {e:#}");
                    }
                    println!("{}", serde_json::json!({
                        "dry_run": false,
                        "deleted": deleted,
                    }));
                } else {
                    println!("{}", serde_json::json!({
                        "dry_run": true,
                        "would_delete": count,
                    }));
                }
            } else if count == 0 {
                println!("No unassigned faces to delete.");
            } else if apply {
                let deleted = face_store.delete_unassigned_faces()?;
                if let Err(e) = face_store.save_all_yaml(&catalog_root) {
                    eprintln!("  Warning: failed to save faces/people YAML: {e:#}");
                }
                println!("Deleted {deleted} unassigned face record(s).");
            } else {
                println!("Dry run — would delete {count} unassigned face record(s).");
                println!("  Run with --apply to actually delete.");
            }
            Ok(())
        }
        FacesCommands::DumpAligned { query, asset, output, limit } => {
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let engine = QueryEngine::new(&catalog_root);

            // Resolve scope to asset IDs.
            let scoped_ids: Vec<String> = if let Some(ref a) = asset {
                let full_id = catalog
                    .resolve_asset_id(a)?
                    .ok_or_else(|| anyhow::anyhow!("no asset found matching '{a}'"))?;
                vec![full_id]
            } else {
                let q = query.as_deref().unwrap_or("*");
                let rows = engine.search(q)?;
                rows.into_iter().map(|r| r.asset_id).collect()
            };
            eprintln!("Scope: {} asset(s)", scoped_ids.len());

            std::fs::create_dir_all(&output)?;
            // Use the same path-resolution that `faces detect` uses —
            // falls back to the smart/regular preview when the original
            // file is on an offline volume. Matches how embeddings are
            // computed so the aligned crops we save reflect what the
            // model actually sees.
            let volumes = maki::device_registry::DeviceRegistry::new(&catalog_root).list()?;
            let online_volumes = maki::models::Volume::online_map(&volumes);
            let preview_config = config.preview.clone();
            let preview_gen = maki::preview::PreviewGenerator::new(
                &catalog_root, maki::Verbosity::default(), &preview_config,
            );
            let asset_service = maki::asset_service::AssetService::new(
                &catalog_root, maki::Verbosity::default(), &preview_config,
            );
            let face_model_dir = maki::face::resolve_face_model_dir(&config.ai);
            let mut detector = maki::face::FaceDetector::load(&face_model_dir, maki::Verbosity::default())?;

            let mut saved = 0usize;
            let mut skipped_no_image = 0usize;
            let mut skipped_no_faces = 0usize;
            for aid in &scoped_ids {
                if limit > 0 && saved >= limit { break; }
                let details = match engine.show(aid) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let full_path = match asset_service.find_image_for_ai(&details, &preview_gen, &online_volumes) {
                    Some(p) => p,
                    None => { skipped_no_image += 1; continue; }
                };

                let detected = detector.detect_faces(&full_path, 0.5).unwrap_or_default();
                if detected.is_empty() { skipped_no_faces += 1; continue; }

                let img = match image::open(&full_path) {
                    Ok(i) => i,
                    Err(_) => continue,
                };

                for (i, df) in detected.iter().enumerate() {
                    if limit > 0 && saved >= limit { break; }
                    let aligned = maki::face::align_face_to_arcface(&img, &df.landmarks);
                    let short_asset = &aid[..8.min(aid.len())];
                    let path = output.join(format!("{short_asset}_{i}.jpg"));
                    aligned.save(&path)?;
                    eprintln!("  saved {} (conf={:.2})", path.display(), df.confidence);
                    saved += 1;
                }
            }
            println!(
                "Saved {saved} aligned face crop(s) to {}",
                output.display()
            );
            if skipped_no_image + skipped_no_faces > 0 {
                println!(
                    "Skipped: {skipped_no_image} no accessible image, {skipped_no_faces} no faces detected"
                );
            }
            Ok(())
        }
        FacesCommands::Similarity { query, asset, volume, min_confidence, top, all } => {
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let _ = maki::face_store::FaceStore::initialize(catalog.conn());
            let face_store = maki::face_store::FaceStore::new(catalog.conn());

            // Resolve scope (same pattern as cluster)
            let scoped_ids: Option<Vec<String>> = if query.is_some() || asset.is_some() || volume.is_some() {
                let engine = QueryEngine::new(&catalog_root);
                if let Some(ref a) = asset {
                    let full_id = catalog
                        .resolve_asset_id(a)?
                        .ok_or_else(|| anyhow::anyhow!("no asset found matching '{a}'"))?;
                    Some(vec![full_id])
                } else {
                    let q = if let Some(ref query) = query {
                        let volume_part = volume.as_deref().map(|v| format!(" volume:{v}")).unwrap_or_default();
                        format!("{query}{volume_part}")
                    } else if let Some(ref v) = volume {
                        format!("volume:{v}")
                    } else {
                        "*".to_string()
                    };
                    let rows = engine.search(&q)?;
                    Some(rows.into_iter().map(|r| r.asset_id).collect())
                }
            } else {
                None
            };

            let faces = face_store.face_embeddings_scoped(scoped_ids.as_deref())?;
            // Filter by assignment + confidence; also look up asset_id for each
            // kept face so the output can point back to a real asset.
            let filtered: Vec<(String, String, String, Vec<f32>, f32)> = faces.into_iter()
                .filter(|(_, pid, _, conf, _)| (all || pid.is_none()) && *conf >= min_confidence)
                .filter_map(|(id, pid, emb, conf, _model)| {
                    let asset_id = face_store.get_face(&id).ok().flatten().map(|f| f.asset_id)?;
                    Some((id, pid.unwrap_or_default(), asset_id, emb, conf))
                })
                .collect();

            let n = filtered.len();
            if n < 2 {
                if cli.json {
                    println!("{}", serde_json::json!({"faces": n, "pairs": 0}));
                } else {
                    println!("Not enough faces to analyze ({n}). Need at least 2.");
                }
                return Ok(());
            }

            // Compute all pairwise similarities (embeddings are at tuple index 3)
            let mut sims: Vec<f32> = Vec::with_capacity(n * (n - 1) / 2);
            let mut per_face: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
            for i in 0..n {
                for j in (i + 1)..n {
                    let s = maki::ai::cosine_similarity(&filtered[i].3, &filtered[j].3);
                    sims.push(s);
                    per_face[i].push((j, s));
                    per_face[j].push((i, s));
                }
            }
            sims.sort_by(|a, b| a.partial_cmp(b).unwrap());

            // Stats
            let pct = |p: f32| -> f32 {
                let idx = ((sims.len() as f32 - 1.0) * p).round() as usize;
                sims[idx]
            };
            let mean: f32 = sims.iter().sum::<f32>() / sims.len() as f32;
            let min = sims[0];
            let max = *sims.last().unwrap();
            let p10 = pct(0.10);
            let p25 = pct(0.25);
            let p50 = pct(0.50);
            let p75 = pct(0.75);
            let p90 = pct(0.90);
            let p95 = pct(0.95);
            let p99 = pct(0.99);

            // Histogram: 10 buckets from min to max
            let bucket_count = 10;
            let mut buckets = vec![0u32; bucket_count];
            let range = (max - min).max(1e-6);
            for &s in &sims {
                let mut b = (((s - min) / range) * bucket_count as f32) as usize;
                if b >= bucket_count { b = bucket_count - 1; }
                buckets[b] += 1;
            }

            if cli.json {
                println!("{}", serde_json::json!({
                    "faces": n,
                    "pairs": sims.len(),
                    "stats": {
                        "min": min, "max": max, "mean": mean,
                        "p10": p10, "p25": p25, "p50": p50,
                        "p75": p75, "p90": p90, "p95": p95, "p99": p99,
                    },
                    "histogram": {
                        "min": min, "max": max, "buckets": buckets,
                    },
                }));
            } else {
                let mode = if all { "all faces" } else { "unassigned faces" };
                println!("Face similarity analysis — {n} {mode}, {} pairs (min_confidence={min_confidence:.2})", sims.len());
                println!();
                println!("Pairwise cosine similarity:");
                println!("  min:    {min:.3}");
                println!("  p10:    {p10:.3}");
                println!("  p25:    {p25:.3}");
                println!("  median: {p50:.3}");
                println!("  mean:   {mean:.3}");
                println!("  p75:    {p75:.3}");
                println!("  p90:    {p90:.3}");
                println!("  p95:    {p95:.3}");
                println!("  p99:    {p99:.3}");
                println!("  max:    {max:.3}");
                println!();
                println!("Histogram ({bucket_count} buckets, {min:.2}–{max:.2}):");
                let max_count = *buckets.iter().max().unwrap_or(&1);
                for (i, &count) in buckets.iter().enumerate() {
                    let lo = min + (i as f32) * range / bucket_count as f32;
                    let hi = min + ((i + 1) as f32) * range / bucket_count as f32;
                    let bar_width = (count as f32 * 40.0 / max_count as f32) as usize;
                    let bar: String = "█".repeat(bar_width);
                    println!("  {lo:.3}–{hi:.3}  {count:>6}  {bar}");
                }
                println!();
                println!("Interpretation:");
                println!("  • A bimodal distribution (two humps) means the model separates people well.");
                println!("    Pick a threshold in the gap between humps.");
                println!("  • A single hump or flat distribution means embeddings are not discriminating.");
                println!("    Check face model quality, face crop size, or filter by higher --min-confidence.");

                if top > 0 {
                    println!();
                    println!("Top-{top} nearest neighbors per face (format: face_id/asset_id):");
                    for i in 0..n {
                        per_face[i].sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                        let short_face = &filtered[i].0[..8.min(filtered[i].0.len())];
                        let short_asset = &filtered[i].2[..8.min(filtered[i].2.len())];
                        let person = if filtered[i].1.is_empty() { "unassigned".to_string() } else { format!("person={}", &filtered[i].1[..8.min(filtered[i].1.len())]) };
                        println!("  {short_face}/{short_asset} (conf={:.2}, {person}):", filtered[i].4);
                        for (j, s) in per_face[i].iter().take(top) {
                            let short_jf = &filtered[*j].0[..8.min(filtered[*j].0.len())];
                            let short_ja = &filtered[*j].2[..8.min(filtered[*j].2.len())];
                            let person_j = if filtered[*j].1.is_empty() { "unassigned".to_string() } else { format!("person={}", &filtered[*j].1[..8.min(filtered[*j].1.len())]) };
                            println!("    → {short_jf}/{short_ja} [{s:.3}] ({person_j})");
                        }
                    }
                }
            }
            Ok(())
        }
        FacesCommands::Export => {
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let _ = maki::face_store::FaceStore::initialize(catalog.conn());
            let face_store = maki::face_store::FaceStore::new(catalog.conn());

            // Export faces + people YAML
            face_store.save_all_yaml(&catalog_root)?;
            let faces_file = face_store.export_all_faces()?;
            let people_file = face_store.export_all_people()?;

            // Export ArcFace embedding binaries
            let mut arcface_count = 0u32;
            for face in &faces_file.faces {
                if let Ok(Some(emb)) = face_store.get_face_embedding(&face.id) {
                    if !emb.is_empty() {
                        if let Err(e) = maki::face_store::write_arcface_binary(&catalog_root, &face.id, &emb) {
                            eprintln!("  Warning: {}: {e:#}", &face.id[..8.min(face.id.len())]);
                        } else {
                            arcface_count += 1;
                        }
                    }
                }
            }

            if cli.json {
                println!("{}", serde_json::json!({
                    "faces": faces_file.faces.len(),
                    "people": people_file.people.len(),
                    "arcface_binaries": arcface_count,
                }));
            } else {
                println!("Exported {} faces, {} people to YAML", faces_file.faces.len(), people_file.people.len());
                println!("Exported {arcface_count} ArcFace embedding binaries");
            }
            Ok(())
        }
    }
}

/// Resolve a person ID prefix to a full ID.
#[cfg(feature = "ai")]
pub fn resolve_person_id(face_store: &maki::face_store::FaceStore, prefix: &str) -> anyhow::Result<String> {
    let people = face_store.list_people()?;
    let matches: Vec<_> = people
        .iter()
        .filter(|(p, _)| p.id.starts_with(prefix))
        .collect();
    match matches.len() {
        0 => anyhow::bail!("no person found matching '{prefix}'"),
        1 => Ok(matches[0].0.id.clone()),
        _ => anyhow::bail!("ambiguous person ID prefix '{prefix}' — matches {} people", matches.len()),
    }
}

/// Resolve a face ID prefix to a full ID.
#[cfg(feature = "ai")]
pub fn resolve_face_id(face_store: &maki::face_store::FaceStore, prefix: &str) -> anyhow::Result<String> {
    // Try exact match first
    if let Ok(Some(_)) = face_store.get_face(prefix) {
        return Ok(prefix.to_string());
    }
    // Fall back to prefix search via all faces
    let conn = face_store.conn();
    let mut stmt = conn.prepare("SELECT id FROM faces WHERE id LIKE ?1")?;
    let ids: Vec<String> = stmt
        .query_map(rusqlite::params![format!("{prefix}%")], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    match ids.len() {
        0 => anyhow::bail!("no face found matching '{prefix}'"),
        1 => Ok(ids[0].clone()),
        _ => anyhow::bail!("ambiguous face ID prefix '{prefix}' — matches {} faces", ids.len()),
    }
}
