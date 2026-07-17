//! Audio commands: `audio analyze`.

use super::*;

/// Extracted body of `Commands::Audio / AudioCommands::Analyze`. See
/// `run_import_command` for the extraction pattern.
pub fn run_audio_analyze_command(
        query: String,
        force: bool,
        dry_run: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    use maki::asset_service::AudioAnalyzeStatus;

    let (catalog_root, config) = maki::config::load_config()?;
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);

    let show_log = log;
    let result = service.audio_analyze(
        &query,
        &config.audio,
        force,
        dry_run,
        |asset_id, status, elapsed| {
            if !show_log {
                return;
            }
            let short = &asset_id[..8.min(asset_id.len())];
            match status {
                AudioAnalyzeStatus::Analyzed { key, bpm } => {
                    if dry_run {
                        item_status(short, "would analyze", Some(elapsed));
                    } else {
                        let key_str = key.as_deref().unwrap_or("-");
                        let bpm_str = bpm.map(|b| format!("{b:.1}")).unwrap_or_else(|| "-".to_string());
                        item_status(short, &format!("key {key_str}, bpm {bpm_str}"), Some(elapsed));
                    }
                }
                AudioAnalyzeStatus::SkippedExisting => {
                    item_status(short, "skipped (already analyzed)", Some(elapsed));
                }
                AudioAnalyzeStatus::SkippedOffline => {
                    item_status(short, "skipped (offline)", Some(elapsed));
                }
                AudioAnalyzeStatus::Error(msg) => {
                    eprintln!("  {short} — error: {msg}");
                }
            }
        },
    )?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        for err in &result.errors {
            eprintln!("  {err}");
        }
        if result.assets_matched == 0 {
            println!("No audio assets matched.");
        } else if dry_run {
            println!(
                "Audio analyze (dry run): {} matched, {} would be analyzed, {} already analyzed, {} offline",
                result.assets_matched, result.analyzed, result.skipped_existing, result.skipped_offline,
            );
        } else {
            let mut parts = vec![format!("{} analyzed", result.analyzed)];
            if result.keys_set > 0 {
                parts.push(format!("{} keys set", result.keys_set));
            }
            if result.bpms_set > 0 {
                parts.push(format!("{} BPMs set", result.bpms_set));
            }
            if result.skipped_existing > 0 {
                parts.push(format!("{} skipped (already analyzed)", result.skipped_existing));
            }
            if result.skipped_offline > 0 {
                parts.push(format!("{} offline", result.skipped_offline));
            }
            if result.failed > 0 {
                parts.push(format!("{} failed", result.failed));
            }
            println!("Audio analyze: {}", parts.join(", "));
        }
    }

    Ok(())
}
