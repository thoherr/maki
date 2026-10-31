//! `maki search` — query execution plus the four output renderers
//! (ids / short / full / json / template). Returns the matched asset
//! IDs so the shell can bind them to `_`.

use super::*;

pub fn run_search_command(
    query: Option<String>,
    format: Option<String>,
    quiet: bool,
    image: Option<PathBuf>,
    limit: Option<usize>,
    ctx: CliContext,
) -> anyhow::Result<Vec<String>> {
    use maki::format::{self, OutputFormat};

    let query = query.unwrap_or_default();
    let (catalog_root, config) = maki::config::load_config()?;
    let engine = QueryEngine::with_default_filter(&catalog_root, config.browse.default_filter);
    let results = if let Some(ref image_path) = image {
        let (rows, info) = engine.search_with_image(
            &query,
            maki::query::ImageSearch { path: image_path, limit },
        )?;
        if let Some(w) = info.warning {
            eprintln!("Warning: {w}");
            eprintln!("  (exact copy found by content hash; similar-image ranking skipped)");
        }
        match info.exact_asset_id {
            Some(ref id) => eprintln!("Exact match: {id}"),
            None if ctx.verbosity.verbose => {
                eprintln!("  No byte-identical copy in catalog ({})", info.content_hash)
            }
            None => {}
        }
        rows
    } else {
        engine.search(&query)?
    };

    if ctx.verbosity.verbose {
        eprintln!("  Search: query=\"{query}\", {} result(s)", results.len());
    }

    // Asset IDs for the shell `_` variable
    let asset_ids: Vec<String> = results.iter().map(|r| r.asset_id.clone()).collect();

    // Determine output format
    let output_format = if quiet {
        OutputFormat::Ids
    } else if let Some(fmt) = &format {
        format::parse_format(fmt).map_err(|e| anyhow::anyhow!(e))?
    } else if ctx.json {
        OutputFormat::Json
    } else {
        OutputFormat::Short
    };

    let explicit_format = quiet || format.is_some();

    if results.is_empty() {
        match output_format {
            OutputFormat::Json => println!("[]"),
            _ => {
                if !explicit_format {
                    println!("No results found.");
                    // Hint: if the query has a filter like tag:X followed by
                    // unquoted words, the user likely forgot inner quotes.
                    let parsed = maki::query::parse_search_query(&query);
                    let has_filter = !parsed.tags.is_empty() || !parsed.cameras.is_empty()
                        || !parsed.lenses.is_empty() || !parsed.descriptions.is_empty()
                        || !parsed.collections.is_empty() || !parsed.path_prefixes.is_empty();
                    if has_filter && parsed.text.is_some() {
                        eprintln!("Hint: values with spaces need inner quotes, e.g. tag:\"my tag\"");
                        eprintln!("  Shell example: maki search 'tag:\"my tag\" rating:3+'");
                    }
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
                        "{}  {} [{}] ({}) — {}{}",
                        short_id, display_name, row.asset_type, row.display_format(), row.created_at,
                        similarity_suffix(row)
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
                        "{}  {} [{}] ({}) — {}{}{} {}",
                        short_id, display_name, row.asset_type, row.display_format(),
                        row.created_at, similarity_suffix(row), tags, desc
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
                    let mut values = format::search_row_values(
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
                    values.insert(
                        "similarity",
                        row.similarity.map(|s| format!("{}", (s * 100.0).round() as u32)).unwrap_or_default(),
                    );
                    values.insert("exact", if row.exact_match { "exact".to_string() } else { String::new() });
                    println!("{}", format::render_template(tpl, &values));
                }
            }
        }
    }
    Ok(asset_ids)
}

/// `  92%` / `  100% exact` suffix for similarity-ranked search rows;
/// empty for ordinary searches.
fn similarity_suffix(row: &maki::catalog::SearchRow) -> String {
    match row.similarity {
        Some(s) if row.exact_match => format!("  {}% exact", (s * 100.0).round() as u32),
        Some(s) => format!("  {}%", (s * 100.0).round() as u32),
        None => String::new(),
    }
}
