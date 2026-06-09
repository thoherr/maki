//! `maki tag` — tag CRUD and hierarchy maintenance.

use super::*;

/// Print a warning to stderr listing tags that were modified during
/// keyword-text sanitization (XML entity decode, comma/semicolon stripping).
/// Helps the user find tags they may want to rename at the source.
pub fn report_sanitized_tags(changes: &[(String, String)]) {
    if changes.is_empty() {
        return;
    }
    eprintln!(
        "\nwarning: {} tag(s) were sanitized for Lightroom/Capture One compatibility.",
        changes.len(),
    );
    eprintln!("         Consider renaming them in your catalog (see `maki tag rename`):");
    for (before, after) in changes {
        if after.is_empty() {
            eprintln!("         - {before:?} -> (skipped: empty after sanitize)");
        } else {
            eprintln!("         - {before:?} -> {after:?}");
        }
    }
    eprintln!();
}

/// Extracted body of `Commands::Tag`. See `run_import_command` for the
/// extraction pattern.
pub fn run_tag_command(
    asset_id: Option<String>,
    remove: bool,
    tags: Vec<String>,
    subcmd: Option<TagCommands>,
    json: bool,
    log: bool,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    match subcmd {
        Some(TagCommands::Rename { old_tag, new_tag, apply }) => {
            let catalog_root = maki::config::find_catalog_root()?;
            let engine = QueryEngine::new(&catalog_root);
            let old_storage = maki::tag_util::tag_input_to_storage(&old_tag);
            let new_storage = maki::tag_util::tag_input_to_storage(&new_tag);
            let show_log = cli.log;

            use maki::query::TagRenameAction;
            let result = engine.tag_rename(&old_storage, &new_storage, apply, |name, action| {
                if show_log {
                    let verb = match (action, apply) {
                        (TagRenameAction::Renamed, true) => "renamed",
                        (TagRenameAction::Renamed, false) => "would rename",
                        (TagRenameAction::Removed, true) => "removed (already had target)",
                        (TagRenameAction::Removed, false) => "would remove (already has target)",
                        (TagRenameAction::Skipped, _) => "skipped (already correct)",
                    };
                    eprintln!("  {} — {}", name, verb);
                }
            })?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                let old_display = maki::tag_util::tag_storage_to_display(&old_storage);
                let new_display = maki::tag_util::tag_storage_to_display(&new_storage);
                if result.matched == 0 {
                    println!("No assets found with tag \"{}\".", old_display);
                } else {
                    if !apply && (result.renamed > 0 || result.removed > 0) {
                        eprint!("Dry run — ");
                    }
                    let mut parts = Vec::new();
                    if result.renamed > 0 {
                        parts.push(format!("{} renamed", result.renamed));
                    }
                    if result.removed > 0 {
                        parts.push(format!("{} removed (merged)", result.removed));
                    }
                    if result.skipped > 0 {
                        parts.push(format!("{} skipped", result.skipped));
                    }
                    println!(
                        "Tag rename: \"{}\" → \"{}\": {}",
                        old_display, new_display, parts.join(", "),
                    );
                    if !apply && (result.renamed > 0 || result.removed > 0) {
                        println!("  Run with --apply to rename tags.");
                    }
                }
            }
            Ok(())
        }
        Some(TagCommands::Split { old_tag, new_tags, keep, apply }) => {
            let catalog_root = maki::config::find_catalog_root()?;
            let engine = QueryEngine::new(&catalog_root);
            let old_storage = maki::tag_util::tag_input_to_storage(&old_tag);
            let new_storage: Vec<String> = new_tags.iter()
                .map(|t| maki::tag_util::tag_input_to_storage(t))
                .collect();
            let show_log = cli.log;

            use maki::query::TagSplitAction;
            let result = engine.tag_split(&old_storage, &new_storage, keep, apply, |name, action| {
                if show_log {
                    let verb = match (action, apply, keep) {
                        (TagSplitAction::Split, true, false) => "split",
                        (TagSplitAction::Split, false, false) => "would split",
                        (TagSplitAction::Split, true, true) => "added targets",
                        (TagSplitAction::Split, false, true) => "would add targets",
                        (TagSplitAction::Skipped, _, _) => "skipped (no change needed)",
                    };
                    eprintln!("  {} — {}", name, verb);
                }
            })?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                let old_display = maki::tag_util::tag_storage_to_display(&old_storage);
                let targets_display = new_storage.iter()
                    .map(|t| format!("\"{}\"", maki::tag_util::tag_storage_to_display(t)))
                    .collect::<Vec<_>>()
                    .join(", ");
                if result.matched == 0 {
                    println!("No assets found with tag \"{}\".", old_display);
                } else {
                    if !apply && result.split > 0 {
                        eprint!("Dry run — ");
                    }
                    let verb = if keep { "add" } else { "split" };
                    let mut parts = Vec::new();
                    if result.split > 0 {
                        parts.push(format!("{} {}", result.split, if keep { "augmented" } else { "split" }));
                    }
                    if result.skipped > 0 {
                        parts.push(format!("{} skipped", result.skipped));
                    }
                    println!(
                        "Tag {}: \"{}\" → [{}]: {}",
                        verb, old_display, targets_display, parts.join(", "),
                    );
                    if !apply && result.split > 0 {
                        println!("  Run with --apply to {} tags.", verb);
                    }
                }
            }
            Ok(())
        }
        Some(TagCommands::Delete { tag, apply }) => {
            let catalog_root = maki::config::find_catalog_root()?;
            let engine = QueryEngine::new(&catalog_root);
            let storage_tag = maki::tag_util::tag_input_to_storage(&tag);
            let show_log = cli.log;

            use maki::query::TagDeleteAction;
            let result = engine.tag_delete(&storage_tag, apply, |name, action| {
                if show_log {
                    let verb = match (action, apply) {
                        (TagDeleteAction::Removed, true) => "removed",
                        (TagDeleteAction::Removed, false) => "would remove",
                        (TagDeleteAction::Skipped, _) => "skipped",
                    };
                    eprintln!("  {} — {}", name, verb);
                }
            })?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                // Strip markers from the display string for the user-facing message.
                let display = storage_tag
                    .trim_start_matches(|c: char| c == '=' || c == '/' || c == '^')
                    .to_string();
                if result.matched == 0 {
                    println!("No assets found with tag \"{}\".", display);
                } else {
                    if !apply && result.removed > 0 {
                        eprint!("Dry run — ");
                    }
                    let mut parts = Vec::new();
                    if result.removed > 0 {
                        parts.push(format!(
                            "{} {}",
                            result.removed,
                            if apply { "removed" } else { "would remove" }
                        ));
                    }
                    if result.skipped > 0 {
                        parts.push(format!("{} skipped", result.skipped));
                    }
                    println!(
                        "Tag delete \"{}\": {} matched, {}",
                        display,
                        result.matched,
                        parts.join(", "),
                    );
                    if !apply && result.removed > 0 {
                        println!("  Run with --apply to delete the tag.");
                    }
                }
            }
            Ok(())
        }
        Some(TagCommands::FixUnicode { apply }) => {
            let catalog_root = maki::config::find_catalog_root()?;
            let engine = QueryEngine::new(&catalog_root);
            let show_log = cli.log;

            let result = engine.tag_fix_unicode(apply, |name, _changed| {
                if show_log {
                    eprintln!("  {} — {}", name, if apply { "fixed" } else { "would fix" });
                }
            })?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                if !apply && result.fixed > 0 {
                    eprint!("Dry run — ");
                }
                if result.fixed == 0 {
                    println!(
                        "Tag fix-unicode: {} asset(s) scanned, all already NFC.",
                        result.scanned
                    );
                } else {
                    let merge_msg = if result.merged > 0 {
                        format!(", {} with merged duplicates", result.merged)
                    } else {
                        String::new()
                    };
                    println!(
                        "Tag fix-unicode: {} scanned, {} {}, {} tag value(s) normalised{}",
                        result.scanned,
                        result.fixed,
                        if apply { "fixed" } else { "would be fixed" },
                        result.tags_normalized,
                        merge_msg,
                    );
                    if !apply && result.fixed > 0 {
                        println!("  Run with --apply to normalise tags.");
                    }
                }
            }
            Ok(())
        }
        Some(TagCommands::Scan { json }) => {
            // Read-only scan across all assets via the metadata store
            // (sidecar = source of truth, same path `tag_fix_unicode`
            // uses). A tag is "malformed" iff normalize_tag_for_storage
            // would change its bytes — covers leading/trailing
            // whitespace including non-breaking space (U+00A0), tabs,
            // stray `|` separators, control chars, empty segments,
            // NFD-vs-NFC duplicates. The user's diagnostic case was a
            // tag like ` |München` where the leading "space" turned
            // out not to be a regular ASCII 0x20 — this scan catches
            // it regardless of which whitespace flavour snuck in.
            let catalog_root = maki::config::find_catalog_root()?;
            let store = maki::metadata_store::MetadataStore::new(&catalog_root);
            let summaries = store.list()?;
            let mut findings: Vec<serde_json::Value> = Vec::new();
            let mut asset_count = 0u32;
            let mut bad_count = 0u32;
            for s in &summaries {
                asset_count += 1;
                let asset = match store.load(s.id) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("  warn: failed to load {}: {e:#}", s.id);
                        continue;
                    }
                };
                let mut offenders: Vec<(String, Vec<String>)> = Vec::new();
                for t in &asset.tags {
                    let n = maki::tag_util::normalize_tag_for_storage(t);
                    // Mismatch iff the normaliser would have split the
                    // input, dropped it, or produced a different single
                    // string. Single-entry equal → tag is canonical.
                    let canonical = n.tags.len() == 1 && n.tags[0] == *t;
                    if !canonical {
                        offenders.push((t.clone(), n.tags));
                    }
                }
                if offenders.is_empty() {
                    continue;
                }
                bad_count += 1;
                if json {
                    findings.push(serde_json::json!({
                        "asset_id": s.id.to_string(),
                        "name": asset.name,
                        "tags": offenders.iter().map(|(raw, normalised)| serde_json::json!({
                            "raw": raw,
                            "raw_bytes_hex": raw.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(""),
                            "normalised": normalised,
                        })).collect::<Vec<_>>(),
                    }));
                } else {
                    let display = asset.name.as_deref().unwrap_or("(unnamed)");
                    println!("{}  {}", s.id, display);
                    for (raw, normalised) in &offenders {
                        let hex = raw.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
                        let suggestion = if normalised.is_empty() {
                            "(would be dropped)".to_string()
                        } else {
                            normalised.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(", ")
                        };
                        println!("    raw:  {raw:?}");
                        println!("    hex:  {hex}");
                        println!("    fix:  {suggestion}");
                    }
                }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "scanned": asset_count,
                    "malformed": bad_count,
                    "assets": findings,
                }))?);
            } else if bad_count == 0 {
                println!("Tag scan: {asset_count} asset(s) scanned, no malformed tags.");
            } else {
                println!("Tag scan: {asset_count} scanned, {bad_count} with malformed tag(s).");
                println!("  Tip: run `maki tag fix-unicode --apply` to auto-fix tags that only differ in NFC/NFD form,");
                println!("       or edit sidecars by hand and run `maki rebuild-catalog`.");
            }
            Ok(())
        }
        Some(TagCommands::Clear { asset_id }) => {
            let catalog_root = maki::config::find_catalog_root()?;
            let engine = QueryEngine::new(&catalog_root);

            // Load asset to get current tags, then remove all
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let full_id = catalog.resolve_asset_id(&asset_id)?
                .ok_or_else(|| anyhow::anyhow!("no asset found matching '{asset_id}'"))?;
            let uuid: uuid::Uuid = full_id.parse()?;
            let metadata_store = maki::metadata_store::MetadataStore::new(&catalog_root);
            let asset = metadata_store.load(uuid)?;

            if asset.tags.is_empty() {
                if cli.json {
                    println!("{}", serde_json::json!({ "changed": [], "tags": [] }));
                } else {
                    println!("Tags: (none)");
                }
                return Ok(());
            }

            let tags_to_remove = asset.tags.clone();
            let result = engine.tag(&asset_id, &tags_to_remove, true)?;

            if cli.json {
                println!("{}", serde_json::json!({
                    "changed": result.changed,
                    "tags": result.current_tags,
                }));
            } else {
                let display_removed: Vec<String> = result.changed.iter()
                    .map(|t| maki::tag_util::tag_storage_to_display(t))
                    .collect();
                println!("Cleared {} tag(s): {}", display_removed.len(), display_removed.join(", "));
            }
            Ok(())
        }
        Some(TagCommands::ExpandAncestors { query, asset, apply, asset_ids }) => {
            let catalog_root = maki::config::find_catalog_root()?;
            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let metadata_store = maki::metadata_store::MetadataStore::new(&catalog_root);
            let engine = maki::query::QueryEngine::new(&catalog_root);

            let scope = engine.resolve_scope(query.as_deref(), asset.as_deref(), &asset_ids)?;
            let summaries = metadata_store.list()?;
            let ids: Vec<uuid::Uuid> = match scope {
                Some(set) => set.iter().filter_map(|id| id.parse().ok()).collect(),
                None => summaries.iter().map(|s| s.id).collect(),
            };

            let mut checked = 0usize;
            let mut expanded = 0usize;
            let mut tags_added = 0usize;

            for asset_id in &ids {
                let mut asset_obj = match metadata_store.load(*asset_id) {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                checked += 1;

                let full = maki::tag_util::expand_all_ancestors(&asset_obj.tags);
                let existing: std::collections::HashSet<&str> = asset_obj.tags.iter().map(|s| s.as_str()).collect();
                let missing: Vec<String> = full.iter()
                    .filter(|t| !existing.contains(t.as_str()))
                    .cloned()
                    .collect();

                if missing.is_empty() {
                    continue;
                }

                if cli.log {
                    let id_str = asset_id.to_string();
                    let name = asset_obj.name.as_deref().unwrap_or(&id_str[..8]);
                    if apply {
                        eprintln!("  {} — {} ancestor(s) added", name, missing.len());
                    } else {
                        eprintln!("  {} — {} ancestor(s) would add", name, missing.len());
                    }
                }

                if apply {
                    for tag in &missing {
                        asset_obj.tags.push(tag.clone());
                    }
                    metadata_store.save(&asset_obj)?;
                    catalog.insert_asset(&asset_obj)?;
                }

                expanded += 1;
                tags_added += missing.len();
            }

            if cli.json {
                println!("{}", serde_json::json!({
                    "dry_run": !apply,
                    "checked": checked,
                    "expanded": expanded,
                    "tags_added": tags_added,
                }));
            } else {
                if !apply && expanded > 0 {
                    eprint!("Dry run — ");
                }
                println!("Expand ancestors: {} checked, {} assets with missing ancestors, {} tags {}",
                    checked, expanded, tags_added,
                    if apply { "added" } else { "would add" },
                );
                if !apply && expanded > 0 {
                    println!("  Run with --apply to expand ancestor tags.");
                }
            }
            Ok(())
        }
        Some(TagCommands::ExportVocabulary { output, format, prune, default, counts }) => {
            let catalog_root = maki::config::find_catalog_root()?;

            let format = format.to_lowercase();
            #[derive(Clone, Copy, PartialEq)]
            enum Fmt { Yaml, Text, Json }
            let (fmt, default_filename) = match format.as_str() {
                "yaml" | "yml" => (Fmt::Yaml, "vocabulary.yaml"),
                "text" | "txt" => (Fmt::Text, "vocabulary.txt"),
                "json"         => (Fmt::Json, "vocabulary.json"),
                other => anyhow::bail!("unknown --format '{}': expected 'yaml', 'text', or 'json'", other),
            };

            if default {
                // Export only the built-in default vocabulary. The default
                // tree has no asset counts, so --counts has nothing useful
                // to add (every node would say "0 assets") — silently
                // ignored regardless of format.
                let (content, sanitized) = match fmt {
                    Fmt::Text => {
                        let flat = maki::vocabulary::parse_vocabulary(maki::vocabulary::default_vocabulary());
                        let pairs: Vec<(String, u64)> = flat.into_iter().map(|t| (t, 0)).collect();
                        maki::vocabulary::tags_to_keyword_text(&pairs)
                    }
                    Fmt::Json => {
                        let flat = maki::vocabulary::parse_vocabulary(maki::vocabulary::default_vocabulary());
                        let pairs: Vec<(String, u64)> = flat.into_iter().map(|t| (t, 0)).collect();
                        (maki::vocabulary::tags_to_vocabulary_json(&pairs), Vec::new())
                    }
                    Fmt::Yaml => {
                        // The built-in vocab is itself YAML; emit verbatim.
                        (maki::vocabulary::default_vocabulary().to_string(), Vec::new())
                    }
                };
                let out_path = output.map(std::path::PathBuf::from)
                    .unwrap_or_else(|| catalog_root.join(default_filename));
                std::fs::write(&out_path, content)?;
                report_sanitized_tags(&sanitized);
                println!("Exported default vocabulary to {}", out_path.display());
                return Ok(());
            }

            let catalog = maki::catalog::Catalog::open(&catalog_root)?;
            let catalog_tags = catalog.list_all_tags()?;

            // Merge with existing vocabulary (preserve planned-but-unused entries)
            let mut all_tags = catalog_tags;
            if !prune {
                let vocab = maki::vocabulary::load_vocabulary(&catalog_root);
                let existing: std::collections::HashSet<String> = all_tags.iter().map(|(name, _)| name.clone()).collect();
                for vt in vocab {
                    if !existing.contains(&vt) {
                        all_tags.push((vt, 0));
                    }
                }
            }
            all_tags.sort_by(|a, b| a.0.cmp(&b.0));

            let (content, sanitized) = match fmt {
                Fmt::Text => {
                    // Counts can't go in keyword-text (LR/C1 reject comments).
                    if counts {
                        eprintln!("  Note: --counts is ignored for text format (Lightroom/Capture One don't accept comments).");
                    }
                    maki::vocabulary::tags_to_keyword_text(&all_tags)
                }
                Fmt::Yaml => {
                    let yaml = if counts {
                        maki::vocabulary::tags_to_vocabulary_yaml_with_counts(&all_tags)
                    } else {
                        maki::vocabulary::tags_to_vocabulary_yaml(&all_tags)
                    };
                    (yaml, Vec::new())
                }
                Fmt::Json => (maki::vocabulary::tags_to_vocabulary_json(&all_tags), Vec::new()),
            };
            let out_path = output.map(std::path::PathBuf::from)
                .unwrap_or_else(|| catalog_root.join(default_filename));
            std::fs::write(&out_path, &content)?;

            report_sanitized_tags(&sanitized);

            let used = all_tags.iter().filter(|(_, c)| *c > 0).count();
            let planned = all_tags.len() - used;
            if prune {
                println!("Exported {} tags to {} (pruned, unused entries removed)", used, out_path.display());
            } else {
                println!("Exported {} tags to {} ({} used, {} planned)", all_tags.len(), out_path.display(), used, planned);
            }
            Ok(())
        }
        None => {
            // Direct tag add/remove: maki tag <asset> <tags> [--remove]
            let asset_id = asset_id.map(|s| s.trim().to_string())
                .ok_or_else(|| anyhow::anyhow!("asset ID is required for tag add/remove"))?;
            if tags.is_empty() {
                anyhow::bail!("no tags specified.");
            }
            let catalog_root = maki::config::find_catalog_root()?;
            let engine = QueryEngine::new(&catalog_root);
            let storage_tags: Vec<String> = tags.iter()
                .map(|t| maki::tag_util::tag_input_to_storage(t))
                .collect();
            let result = engine.tag(&asset_id, &storage_tags, remove)?;

            if cli.json {
                println!("{}", serde_json::json!({
                    "changed": result.changed,
                    "tags": result.current_tags,
                }));
            } else {
                let display_changed: Vec<String> = result.changed.iter()
                    .map(|t| maki::tag_util::tag_storage_to_display(t))
                    .collect();
                let display_tags: Vec<String> = result.current_tags.iter()
                    .map(|t| maki::tag_util::tag_storage_to_display(t))
                    .collect();
                if !display_changed.is_empty() {
                    if remove {
                        println!("Removed tags: {}", display_changed.join(", "));
                    } else {
                        println!("Added tags: {}", display_changed.join(", "));
                    }
                }
                if display_tags.is_empty() {
                    println!("Tags: (none)");
                } else {
                    println!("Tags: {}", display_tags.join(", "));
                }
            }
            Ok(())
        }
    }
}
