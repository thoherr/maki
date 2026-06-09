//! Organize commands: `saved-search`, `stack`, `collection`, `auto-group`, `split`, `group`, `edit`.

use super::*;

/// Extracted body of `Commands::SavedSearch`.
pub fn run_saved_search_command(
    cmd: SavedSearchCommands,
    json: bool,
    log: bool,
    #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let _ = verbosity;
    let catalog_root = maki::config::find_catalog_root()?;
    match cmd {
        SavedSearchCommands::Save { name, query, sort, favorite } => {
            let mut file = maki::saved_search::load(&catalog_root)?;
            // Replace existing entry with same name, or append
            let entry = maki::saved_search::SavedSearch {
                name: name.clone(),
                query,
                sort,
                favorite,
            };
            if let Some(existing) = file.searches.iter_mut().find(|s| s.name == name) {
                *existing = entry;
            } else {
                file.searches.push(entry);
            }
            maki::saved_search::save(&catalog_root, &file)?;
            if cli.json {
                println!("{}", serde_json::json!({
                    "status": "saved",
                    "name": name,
                }));
            } else {
                println!("Saved search '{name}'");
            }
            Ok(())
        }
        SavedSearchCommands::List => {
            let file = maki::saved_search::load(&catalog_root)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&file.searches)?);
            } else if file.searches.is_empty() {
                println!("No saved searches.");
            } else {
                for ss in &file.searches {
                    let sort_info = ss.sort.as_deref().unwrap_or("date_desc");
                    let fav = if ss.favorite { " [*]" } else { "" };
                    println!("  {}{} — {} (sort: {})", ss.name, fav, ss.query, sort_info);
                }
            }
            Ok(())
        }
        SavedSearchCommands::Run { name, format } => {
            use maki::format::{self, OutputFormat};

            let file = maki::saved_search::load(&catalog_root)?;
            let ss = maki::saved_search::find_by_name(&file, &name)
                .ok_or_else(|| anyhow::anyhow!("no saved search named '{name}'"))?;

            let engine = QueryEngine::new(&catalog_root);
            let results = engine.search(&ss.query)?;

            let output_format = if let Some(fmt) = &format {
                format::parse_format(fmt).map_err(|e| anyhow::anyhow!(e))?
            } else if cli.json {
                OutputFormat::Json
            } else {
                OutputFormat::Short
            };

            let explicit_format = format.is_some();

            if results.is_empty() {
                match output_format {
                    OutputFormat::Json => println!("[]"),
                    _ => {
                        if !explicit_format {
                            println!("No results found.");
                        }
                    }
                }
            } else {
                match output_format {
                    OutputFormat::Ids => {
                        for row in &results {
                            println!("{}", row.asset_id);
                        }
                    }
                    OutputFormat::Short => {
                        for row in &results {
                            let display_name = row
                                .name
                                .as_deref()
                                .unwrap_or(&row.original_filename);
                            let short_id = &row.asset_id[..8];
                            println!(
                                "{}  {} [{}] ({}) — {}",
                                short_id, display_name, row.asset_type, row.display_format(), row.created_at
                            );
                        }
                        if !explicit_format {
                            println!("\n{} result(s)", results.len());
                        }
                    }
                    OutputFormat::Full => {
                        for row in &results {
                            let display_name = row
                                .name
                                .as_deref()
                                .unwrap_or(&row.original_filename);
                            let short_id = &row.asset_id[..8];
                            let tags = if row.tags.is_empty() {
                                String::new()
                            } else {
                                format!(" tags:{}", row.tags.join(","))
                            };
                            let desc = row.description.as_deref().unwrap_or("");
                            println!(
                                "{}  {} [{}] ({}) — {}{} {}",
                                short_id, display_name, row.asset_type, row.display_format(),
                                row.created_at, tags, desc
                            );
                        }
                        if !explicit_format {
                            println!("\n{} result(s)", results.len());
                        }
                    }
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&results)?);
                    }
                    OutputFormat::Template(ref tpl) => {
                        for row in &results {
                            let tags_str = row.tags.join(", ");
                            let desc = row.description.as_deref().unwrap_or("");
                            let label = row.color_label.as_deref().unwrap_or("");
                            let values = format::search_row_values(
                                &row.asset_id,
                                row.name.as_deref(),
                                &row.original_filename,
                                &row.asset_type,
                                row.display_format(),
                                &row.created_at,
                                &tags_str,
                                desc,
                                &row.content_hash,
                                label,
                            );
                            println!("{}", format::render_template(tpl, &values));
                        }
                    }
                }
            }
            Ok(())
        }
        SavedSearchCommands::Delete { name } => {
            let mut file = maki::saved_search::load(&catalog_root)?;
            let before = file.searches.len();
            file.searches.retain(|s| s.name != name);
            if file.searches.len() == before {
                anyhow::bail!("no saved search named '{name}'");
            }
            maki::saved_search::save(&catalog_root, &file)?;
            if cli.json {
                println!("{}", serde_json::json!({
                    "status": "deleted",
                    "name": name,
                }));
            } else {
                println!("Deleted saved search '{name}'");
            }
            Ok(())
        }
    }
}

/// Extracted body of `Commands::Stack`.
pub fn run_stack_command(
    cmd: StackCommands,
    json: bool,
    log: bool,
    #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let _ = verbosity;
    let catalog_root = maki::config::find_catalog_root()?;
    let catalog = Catalog::open(&catalog_root)?;
    let store = maki::stack::StackStore::new(catalog.conn());
    match cmd {
        StackCommands::Create { asset_ids } => {
            if asset_ids.len() < 2 {
                anyhow::bail!("a stack requires at least 2 assets");
            }
            let stack = store.create(&asset_ids)?;
            let yaml = store.export_all()?;
            maki::stack::save_yaml(&catalog_root, &yaml)?;
            if cli.json {
                println!("{}", serde_json::json!({
                    "id": stack.id.to_string(),
                    "member_count": stack.asset_ids.len(),
                    "pick": stack.asset_ids[0],
                }));
            } else {
                println!("Created stack {} ({} assets, pick: {})",
                    &stack.id.to_string()[..8],
                    stack.asset_ids.len(),
                    &stack.asset_ids[0][..8.min(stack.asset_ids[0].len())]);
            }
            Ok(())
        }
        StackCommands::Add { reference, asset_ids } => {
            let added = store.add(&reference, &asset_ids)?;
            let yaml = store.export_all()?;
            maki::stack::save_yaml(&catalog_root, &yaml)?;
            if cli.json {
                println!("{}", serde_json::json!({ "added": added }));
            } else {
                println!("Added {} asset(s) to stack", added);
            }
            Ok(())
        }
        StackCommands::Remove { asset_ids } => {
            if asset_ids.is_empty() {
                anyhow::bail!("no asset IDs specified.");
            }
            let removed = store.remove(&asset_ids)?;
            let yaml = store.export_all()?;
            maki::stack::save_yaml(&catalog_root, &yaml)?;
            if cli.json {
                println!("{}", serde_json::json!({ "removed": removed }));
            } else {
                println!("Removed {} asset(s) from stack(s)", removed);
            }
            Ok(())
        }
        StackCommands::Pick { asset_id } => {
            store.set_pick(&asset_id)?;
            let yaml = store.export_all()?;
            maki::stack::save_yaml(&catalog_root, &yaml)?;
            if cli.json {
                println!("{}", serde_json::json!({ "pick": asset_id }));
            } else {
                println!("Set {} as stack pick", &asset_id[..8.min(asset_id.len())]);
            }
            Ok(())
        }
        StackCommands::Dissolve { asset_id } => {
            store.dissolve(&asset_id)?;
            let yaml = store.export_all()?;
            maki::stack::save_yaml(&catalog_root, &yaml)?;
            if cli.json {
                println!("{}", serde_json::json!({ "status": "dissolved" }));
            } else {
                println!("Stack dissolved");
            }
            Ok(())
        }
        StackCommands::List => {
            let list = store.list()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else if list.is_empty() {
                println!("No stacks.");
            } else {
                for s in &list {
                    let pick = s.pick_asset_id.as_deref().unwrap_or("?");
                    let short_id = &s.id[..8.min(s.id.len())];
                    let short_pick = &pick[..8.min(pick.len())];
                    println!("  {} ({} assets, pick: {})", short_id, s.member_count, short_pick);
                }
            }
            Ok(())
        }
        StackCommands::Show { asset_id, format } => {
            let (stack_id, members) = store.stack_for_asset(&asset_id)?
                .ok_or_else(|| anyhow::anyhow!("asset {asset_id} is not in a stack"))?;
            if cli.json {
                println!("{}", serde_json::json!({
                    "stack_id": stack_id,
                    "members": members,
                    "pick": members.first(),
                }));
            } else if let Some(ref fmt) = format {
                if fmt == "ids" {
                    for id in &members {
                        println!("{}", id);
                    }
                } else {
                    let short_sid = &stack_id[..8.min(stack_id.len())];
                    println!("Stack {}:", short_sid);
                    for (i, id) in members.iter().enumerate() {
                        let marker = if i == 0 { " [pick]" } else { "" };
                        println!("  {}{}", id, marker);
                    }
                }
            } else {
                let short_sid = &stack_id[..8.min(stack_id.len())];
                println!("Stack {}:", short_sid);
                for (i, id) in members.iter().enumerate() {
                    let marker = if i == 0 { " [pick]" } else { "" };
                    println!("  {}{}", id, marker);
                }
            }
            Ok(())
        }
        StackCommands::FromTag { pattern, remove_tags, apply } => {
            let engine = QueryEngine::new(&catalog_root);
            let result = engine.stack_from_tag(&pattern, remove_tags, apply, cli.log)?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                let mode = if result.dry_run { " (dry run)" } else { "" };
                println!("Tags matched: {}{}", result.tags_matched, mode);
                println!("Tags skipped: {}", result.tags_skipped);
                println!("Stacks created: {}", result.stacks_created);
                println!("Assets stacked: {}", result.assets_stacked);
                println!("Assets already stacked (skipped): {}", result.assets_skipped);
                if remove_tags {
                    println!("Tags removed: {}", result.tags_removed);
                }
            }
            Ok(())
        }
    }
}

/// Extracted body of `Commands::Collection`.
pub fn run_collection_command(
    cmd: CollectionCommands,
    json: bool,
    log: bool,
    #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let _ = verbosity;
    let catalog_root = maki::config::find_catalog_root()?;
    let catalog = Catalog::open(&catalog_root)?;
    let store = maki::collection::CollectionStore::new(catalog.conn());
    match cmd {
        CollectionCommands::Create { name, description } => {
            let col = store.create(&name, description.as_deref())?;
            // Persist to YAML
            let yaml = store.export_all()?;
            maki::collection::save_yaml(&catalog_root, &yaml)?;
            if cli.json {
                println!("{}", serde_json::json!({
                    "id": col.id.to_string(),
                    "name": col.name,
                }));
            } else {
                println!("Created collection '{}'", col.name);
            }
            Ok(())
        }
        CollectionCommands::List => {
            let list = store.list()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else if list.is_empty() {
                println!("No collections.");
            } else {
                for c in &list {
                    let desc = c.description.as_deref().unwrap_or("");
                    if desc.is_empty() {
                        println!("  {} ({} assets)", c.name, c.asset_count);
                    } else {
                        println!("  {} ({} assets) — {}", c.name, c.asset_count, desc);
                    }
                }
            }
            Ok(())
        }
        CollectionCommands::Show { name, format } => {
            use maki::format::{self, OutputFormat};

            let col = store.get_by_name(&name)?
                .ok_or_else(|| anyhow::anyhow!("no collection named '{name}'"))?;

            if col.asset_ids.is_empty() {
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&col)?);
                } else {
                    println!("Collection '{}' is empty.", name);
                }
                return Ok(());
            }

            // Search with collection filter
            let engine = QueryEngine::new(&catalog_root);
            let query_str = format!("collection:{}", name);
            let results = engine.search(&query_str)?;

            let output_format = if let Some(fmt) = &format {
                format::parse_format(fmt).map_err(|e| anyhow::anyhow!(e))?
            } else if cli.json {
                OutputFormat::Json
            } else {
                OutputFormat::Short
            };

            let explicit_format = format.is_some();

            if results.is_empty() {
                match output_format {
                    OutputFormat::Json => println!("[]"),
                    _ => {
                        if !explicit_format {
                            println!("Collection '{}': no matching assets.", name);
                        }
                    }
                }
            } else {
                match output_format {
                    OutputFormat::Ids => {
                        for row in &results {
                            println!("{}", row.asset_id);
                        }
                    }
                    OutputFormat::Short => {
                        if !explicit_format {
                            println!("Collection '{}':", name);
                        }
                        for row in &results {
                            let display_name = row.name.as_deref().unwrap_or(&row.original_filename);
                            let short_id = &row.asset_id[..8];
                            println!("  {}  {} [{}] ({})", short_id, display_name, row.asset_type, row.display_format());
                        }
                        if !explicit_format {
                            println!("\n{} asset(s)", results.len());
                        }
                    }
                    OutputFormat::Full => {
                        if !explicit_format {
                            println!("Collection '{}':", name);
                        }
                        for row in &results {
                            let display_name = row.name.as_deref().unwrap_or(&row.original_filename);
                            let short_id = &row.asset_id[..8];
                            let tags = if row.tags.is_empty() {
                                String::new()
                            } else {
                                format!(" tags:{}", row.tags.join(","))
                            };
                            println!("  {}  {} [{}] ({}){}", short_id, display_name, row.asset_type, row.display_format(), tags);
                        }
                        if !explicit_format {
                            println!("\n{} asset(s)", results.len());
                        }
                    }
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&results)?);
                    }
                    OutputFormat::Template(ref tpl) => {
                        for row in &results {
                            let tags_str = row.tags.join(", ");
                            let desc = row.description.as_deref().unwrap_or("");
                            let label = row.color_label.as_deref().unwrap_or("");
                            let values = format::search_row_values(
                                &row.asset_id,
                                row.name.as_deref(),
                                &row.original_filename,
                                &row.asset_type,
                                row.display_format(),
                                &row.created_at,
                                &tags_str,
                                desc,
                                &row.content_hash,
                                label,
                            );
                            println!("{}", format::render_template(tpl, &values));
                        }
                    }
                }
            }
            Ok(())
        }
        CollectionCommands::Add { name, asset_ids } => {
            // Read from stdin if no IDs provided
            let ids = if asset_ids.is_empty() {
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
            let added = store.add_assets(&name, &ids)?;
            // Persist to YAML
            let yaml = store.export_all()?;
            maki::collection::save_yaml(&catalog_root, &yaml)?;
            if cli.json {
                println!("{}", serde_json::json!({
                    "added": added,
                    "collection": name,
                }));
            } else {
                println!("Added {} asset(s) to '{}'", added, name);
            }
            Ok(())
        }
        CollectionCommands::Remove { name, asset_ids } => {
            if asset_ids.is_empty() {
                anyhow::bail!("no asset IDs specified.");
            }
            let removed = store.remove_assets(&name, &asset_ids)?;
            // Persist to YAML
            let yaml = store.export_all()?;
            maki::collection::save_yaml(&catalog_root, &yaml)?;
            if cli.json {
                println!("{}", serde_json::json!({
                    "removed": removed,
                    "collection": name,
                }));
            } else {
                println!("Removed {} asset(s) from '{}'", removed, name);
            }
            Ok(())
        }
        CollectionCommands::Delete { name } => {
            store.delete(&name)?;
            // Persist to YAML
            let yaml = store.export_all()?;
            maki::collection::save_yaml(&catalog_root, &yaml)?;
            if cli.json {
                println!("{}", serde_json::json!({
                    "status": "deleted",
                    "name": name,
                }));
            } else {
                println!("Deleted collection '{name}'");
            }
            Ok(())
        }
    }
}

/// Extracted body of `Commands::AutoGroup`. See `run_import_command` for the
/// extraction pattern.
pub fn run_auto_group_command(
        query: Option<String>,
        apply: bool,
        global: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let catalog_root = maki::config::find_catalog_root()?;
    let engine = QueryEngine::new(&catalog_root);

    // Search to get asset IDs, deduplicate (search returns one row per variant)
    let results = engine.search(query.as_deref().unwrap_or(""))?;
    let asset_ids: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        results
            .iter()
            .filter(|r| seen.insert(r.asset_id.clone()))
            .map(|r| r.asset_id.clone())
            .collect()
    };

    let show_log = cli.log;
    let result = if global {
        engine.auto_group_global(&asset_ids, !apply)?
    } else {
        engine.auto_group_with_log(&asset_ids, !apply, |stem, count| {
            if show_log {
                eprintln!("  {} — {} asset(s)", stem, count);
            }
        })?
    };

    if cli.json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        if result.groups.is_empty() {
            eprintln!("No groupable assets found");
        } else {
            println!(
                "{} stem group(s), {} donor(s) {}, {} variant(s) moved",
                result.groups.len(),
                result.total_donors_merged,
                if apply { "merged" } else { "would merge" },
                result.total_variants_moved,
            );
        }
        if !apply {
            eprintln!("Dry run — use --apply to merge");
        }
        // Merging variants into a target reorders variants and may
        // change which one is the best-preview pick. Cached previews
        // for the target still reflect the pre-merge best — refresh
        // with `generate-previews --upgrade`.
        if apply && result.total_donors_merged > 0 {
            println!(
                "  Tip: {} group(s) gained variants. Run \
                 'maki generate-previews --upgrade' to refresh \
                 previews for assets whose best variant changed.",
                result.groups.len()
            );
        }
    }
    Ok(())
}

/// Extracted body of `Commands::Split`. See `run_import_command` for the
/// extraction pattern.
pub fn run_split_command(
        asset_id: String,
        variant_hashes: Vec<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let catalog_root = maki::config::find_catalog_root()?;
    let engine = QueryEngine::new(&catalog_root);
    let result = engine.split(&asset_id, &variant_hashes)?;

    if cli.json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        let short_src = &result.source_id[..8];
        println!(
            "Split {} variant(s) from asset {short_src}",
            result.new_assets.len()
        );
        for new_asset in &result.new_assets {
            let short_id = &new_asset.asset_id[..8];
            println!(
                "  → {short_id} ({}, {})",
                new_asset.original_filename, new_asset.variant_hash
            );
        }
    }
    Ok(())
}

/// Extracted body of `Commands::Group`. See `run_import_command` for the
/// extraction pattern.
pub fn run_group_command(
        variant_hashes: Vec<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let catalog_root = maki::config::find_catalog_root()?;
    let engine = QueryEngine::new(&catalog_root);
    let result = engine.group(&variant_hashes)?;

    if cli.json {
        println!("{}", serde_json::json!({
            "target_id": result.target_id,
            "variants_moved": result.variants_moved,
            "donors_removed": result.donors_removed,
        }));
    } else {
        let short_id = &result.target_id[..8];
        println!(
            "Grouped {} variant(s) into asset {short_id}",
            variant_hashes.len()
        );
        if result.donors_removed > 0 {
            println!("  Merged {} donor asset(s)", result.donors_removed);
        } else {
            println!("  Already grouped (no changes)");
        }
    }
    Ok(())
}

/// Extracted body of `Commands::Edit`. See `run_import_command` for the
/// extraction pattern.
pub fn run_edit_command(
        asset_id: String,
        name: Option<String>,
        clear_name: bool,
        description: Option<String>,
        clear_description: bool,
        rating: Option<u8>,
        clear_rating: bool,
        label: Option<String>,
        clear_label: bool,
        clear_tags: bool,
        date: Option<String>,
        clear_date: bool,
        role: Option<String>,
        variant: Option<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    use maki::query::{EditFields, parse_date_input};

    // Handle --role --variant separately from asset-level edits
    if role.is_some() || variant.is_some() {
        let role = role.ok_or_else(|| anyhow::anyhow!("--variant requires --role"))?;
        let variant_hash = variant.ok_or_else(|| anyhow::anyhow!("--role requires --variant"))?;

        let catalog_root = maki::config::find_catalog_root()?;
        let engine = QueryEngine::new(&catalog_root);
        engine.set_variant_role(&asset_id, &variant_hash, &role)?;

        if cli.json {
            println!("{}", serde_json::json!({
                "asset_id": asset_id,
                "variant": variant_hash,
                "role": role,
            }));
        } else {
            let short_hash = &variant_hash[..16.min(variant_hash.len())];
            println!("Variant {short_hash}… role set to {role}");
        }
        return Ok(());
    }

    if name.is_none() && !clear_name && description.is_none() && !clear_description && rating.is_none() && !clear_rating && label.is_none() && !clear_label && !clear_tags && date.is_none() && !clear_date {
        anyhow::bail!("no edit flags provided. Use --name, --description, --rating, --label, --date, --role/--variant, or --clear-*.");
    }

    // Validate label if provided
    let label_field = if clear_label {
        Some(None)
    } else if let Some(ref l) = label {
        match maki::models::Asset::validate_color_label(l) {
            Ok(canonical) => Some(canonical),
            Err(e) => anyhow::bail!(e),
        }
    } else {
        None
    };

    // Parse date if provided
    let date_field = if clear_date {
        Some(None)
    } else if let Some(ref d) = date {
        Some(Some(parse_date_input(d)?))
    } else {
        None
    };

    let fields = EditFields {
        name: if clear_name {
            Some(None)
        } else {
            name.map(Some)
        },
        description: if clear_description {
            Some(None)
        } else {
            description.map(Some)
        },
        rating: if clear_rating {
            Some(None)
        } else {
            rating.map(Some)
        },
        color_label: label_field,
        created_at: date_field,
    };

    let catalog_root = maki::config::find_catalog_root()?;
    let engine = QueryEngine::new(&catalog_root);

    // Clear all tags if requested (before edit, so JSON output includes the result)
    let tags_cleared = if clear_tags {
        let details = engine.show(&asset_id)?;
        if !details.tags.is_empty() {
            let tag_result = engine.tag(&asset_id, &details.tags, true)?;
            tag_result.current_tags.is_empty()
        } else {
            true
        }
    } else {
        false
    };

    let result = engine.edit(&asset_id, fields)?;

    if cli.json {
        let mut json = serde_json::to_value(&result)?;
        if clear_tags {
            json["tags_cleared"] = serde_json::json!(tags_cleared);
        }
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        if let Some(name) = &result.name {
            println!("Name: {name}");
        } else {
            println!("Name: (none)");
        }
        if let Some(desc) = &result.description {
            println!("Description: {desc}");
        } else {
            println!("Description: (none)");
        }
        if let Some(r) = result.rating {
            let stars: String = (1..=5).map(|i| if i <= r { '\u{2605}' } else { '\u{2606}' }).collect();
            println!("Rating: {stars} ({r}/5)");
        } else {
            println!("Rating: (none)");
        }
        if let Some(l) = &result.color_label {
            println!("Label: {l}");
        } else {
            println!("Label: (none)");
        }
        if tags_cleared {
            println!("Tags: cleared");
        }
        // Show date (truncate to YYYY-MM-DD)
        let date_display = result.created_at.split('T').next().unwrap_or(&result.created_at);
        println!("Date: {date_display}");
    }
    Ok(())
}
