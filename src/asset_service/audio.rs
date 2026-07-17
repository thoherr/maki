//! `audio` section of `AssetService` — musical metadata analysis via
//! external tools (`maki audio analyze`).
//!
//! Analysis is opt-in and shells out per file, same policy as
//! dcraw/ffmpeg: MAKI never processes audio itself, it records what the
//! tools report. Key detection runs the configured `[audio] key_command`
//! (default `keyfinder-cli`, prints the key to stdout); tempo runs
//! `[audio] bpm_command` (default `beat_this`, writes `.beats` timestamp
//! files) and derives BPM from the median inter-beat interval.
//!
//! Results land in the analyzed variant's `source_metadata`
//! (`audio_key` / `audio_bpm`) — sidecar first, catalog second — and are
//! denormalized into the typed `assets` columns by `insert_asset`, so
//! `key:` / `bpm:` search filters pick them up.

use super::*;

/// Outcome of one `audio analyze` run.
#[derive(Debug, Default, serde::Serialize)]
pub struct AudioAnalyzeResult {
    pub assets_matched: usize,
    pub analyzed: usize,
    pub skipped_existing: usize,
    pub skipped_offline: usize,
    pub failed: usize,
    pub keys_set: usize,
    pub bpms_set: usize,
    pub key_tool_available: bool,
    pub bpm_tool_available: bool,
    pub errors: Vec<String>,
}

/// Per-asset status for `--log` callbacks.
pub enum AudioAnalyzeStatus {
    /// Analysis ran; key / bpm as detected (either may be None).
    Analyzed { key: Option<String>, bpm: Option<f64> },
    /// Both fields already present and `--force` not given.
    SkippedExisting,
    /// No online location for the variant.
    SkippedOffline,
    Error(String),
}

impl AssetService {
    /// Analyze audio assets with the configured external tools, filling
    /// `audio_key` / `audio_bpm` in variant metadata + typed columns.
    ///
    /// Scope: audio-type assets matching `query` (empty = all audio).
    /// Assets that already carry both fields are skipped unless `force`.
    /// Missing tools degrade to a warning: whichever analyzer exists
    /// still runs.
    pub fn audio_analyze(
        &self,
        query: &str,
        audio_config: &crate::config::AudioConfig,
        force: bool,
        dry_run: bool,
        on_asset: impl Fn(&str, &AudioAnalyzeStatus, std::time::Duration),
    ) -> Result<AudioAnalyzeResult> {
        use crate::models::variant::best_preview_index_details;

        let engine = crate::query::QueryEngine::new(&self.catalog_root);
        let full_query = if query.trim().is_empty() {
            "type:audio".to_string()
        } else {
            format!("type:audio {query}")
        };
        let search_results = engine.search(&full_query)?;

        let mut result = AudioAnalyzeResult {
            assets_matched: search_results.len(),
            key_tool_available: crate::preview::tool_available(&audio_config.key_command),
            bpm_tool_available: crate::preview::tool_available(&audio_config.bpm_command),
            ..Default::default()
        };

        if !result.key_tool_available {
            eprintln!(
                "Warning: key tool '{}' not found in PATH — keys will not be detected. \
                 Install keyfinder-cli or set [audio] key_command in maki.toml.",
                audio_config.key_command
            );
        }
        if !result.bpm_tool_available {
            eprintln!(
                "Warning: BPM tool '{}' not found in PATH — tempo will not be detected. \
                 Install beat_this or set [audio] bpm_command in maki.toml.",
                audio_config.bpm_command
            );
        }
        if !result.key_tool_available && !result.bpm_tool_available {
            return Ok(result);
        }

        let catalog = crate::catalog::Catalog::open(&self.catalog_root)?;
        let registry = DeviceRegistry::new(&self.catalog_root);
        let volumes = registry.list()?;
        let online_volumes = crate::models::Volume::online_map(&volumes);

        for row in &search_results {
            let start = std::time::Instant::now();
            let details = match catalog.load_asset_details(&row.asset_id)? {
                Some(d) => d,
                None => continue,
            };
            let Some(vi) = best_preview_index_details(&details.variants) else {
                continue;
            };
            let variant = &details.variants[vi];

            let has_key = variant.source_metadata.contains_key("audio_key");
            let has_bpm = variant.source_metadata.contains_key("audio_bpm");
            let key_wanted = result.key_tool_available && (force || !has_key);
            let bpm_wanted = result.bpm_tool_available && (force || !has_bpm);
            if !key_wanted && !bpm_wanted {
                result.skipped_existing += 1;
                on_asset(&row.asset_id, &AudioAnalyzeStatus::SkippedExisting, start.elapsed());
                continue;
            }

            let source_path = variant.locations.iter().find_map(|l| {
                let vol = online_volumes.get(l.volume_id.as_str())?;
                let p = vol.mount_point.join(&l.relative_path);
                p.exists().then_some(p)
            });
            let Some(source_path) = source_path else {
                result.skipped_offline += 1;
                on_asset(&row.asset_id, &AudioAnalyzeStatus::SkippedOffline, start.elapsed());
                continue;
            };

            if dry_run {
                result.analyzed += 1;
                on_asset(
                    &row.asset_id,
                    &AudioAnalyzeStatus::Analyzed { key: None, bpm: None },
                    start.elapsed(),
                );
                continue;
            }

            let mut meta = std::collections::HashMap::new();
            let mut tool_errors: Vec<String> = Vec::new();

            let key = if key_wanted {
                match detect_key(&audio_config.key_command, &source_path) {
                    Ok(k) => {
                        if let Some(ref k) = k {
                            meta.insert("audio_key".to_string(), k.clone());
                        }
                        k
                    }
                    Err(e) => {
                        tool_errors.push(format!("key: {e}"));
                        None
                    }
                }
            } else {
                None
            };

            let bpm = if bpm_wanted {
                match detect_bpm(&audio_config.bpm_command, &source_path) {
                    Ok(b) => {
                        if let Some(b) = b {
                            meta.insert("audio_bpm".to_string(), format!("{b:.1}"));
                        }
                        b
                    }
                    Err(e) => {
                        tool_errors.push(format!("bpm: {e}"));
                        None
                    }
                }
            } else {
                None
            };

            if !meta.is_empty() {
                self.merge_variant_metadata(&row.asset_id, &variant.content_hash, meta);
            }
            if key.is_some() {
                result.keys_set += 1;
            }
            if bpm.is_some() {
                result.bpms_set += 1;
            }

            if tool_errors.is_empty() {
                result.analyzed += 1;
                on_asset(
                    &row.asset_id,
                    &AudioAnalyzeStatus::Analyzed { key, bpm },
                    start.elapsed(),
                );
            } else {
                result.failed += 1;
                let msg = format!("{}: {}", &row.asset_id[..8], tool_errors.join("; "));
                result.errors.push(msg.clone());
                on_asset(&row.asset_id, &AudioAnalyzeStatus::Error(msg), start.elapsed());
            }
        }

        Ok(result)
    }
}

/// Run the key tool (`<cmd> <file>`); trimmed stdout is the key.
/// Empty stdout with exit 0 means "no key detected" (e.g. silence) —
/// that's a None, not an error.
fn detect_key(cmd: &str, path: &Path) -> Result<Option<String>> {
    let output = std::process::Command::new(cmd)
        .arg(path)
        .output()
        .with_context(|| format!("failed to run '{cmd}'"))?;
    if !output.status.success() {
        anyhow::bail!(
            "'{cmd}' failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!key.is_empty()).then_some(key))
}

/// Run the beat tracker (`<cmd> <file> -o <out>`), parse the `.beats`
/// output (one `<seconds> [beat-number]` line per beat), and derive BPM
/// from the median inter-beat interval — robust against missed or extra
/// beats, unlike a simple count/duration ratio.
fn detect_bpm(cmd: &str, path: &Path) -> Result<Option<f64>> {
    let temp_dir = std::env::temp_dir().join(format!(
        "maki-beats-{}-{}",
        std::process::id(),
        path.file_stem().and_then(|s| s.to_str()).unwrap_or("audio")
    ));
    std::fs::create_dir_all(&temp_dir)?;

    let output = std::process::Command::new(cmd)
        .arg(path)
        .arg("-o")
        .arg(&temp_dir)
        .output()
        .with_context(|| format!("failed to run '{cmd}'"));

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            std::fs::remove_dir_all(&temp_dir).ok();
            return Err(e);
        }
    };
    if !output.status.success() {
        std::fs::remove_dir_all(&temp_dir).ok();
        anyhow::bail!(
            "'{cmd}' failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    // Find the produced .beats file (the tool names it after the input).
    let beats_file = std::fs::read_dir(&temp_dir)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .find(|p| p.extension().and_then(|x| x.to_str()) == Some("beats"))
        });
    let Some(beats_file) = beats_file else {
        std::fs::remove_dir_all(&temp_dir).ok();
        anyhow::bail!("'{cmd}' produced no .beats output in {}", temp_dir.display());
    };

    let content = std::fs::read_to_string(&beats_file)?;
    std::fs::remove_dir_all(&temp_dir).ok();

    Ok(bpm_from_beat_lines(&content))
}

/// Median inter-beat interval → BPM. Needs at least 4 beats to say
/// anything trustworthy; returns None below that (short clip / silence).
pub(crate) fn bpm_from_beat_lines(content: &str) -> Option<f64> {
    let mut times: Vec<f64> = content
        .lines()
        .filter_map(|l| l.split_whitespace().next()?.parse::<f64>().ok())
        .collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if times.len() < 4 {
        return None;
    }
    let mut intervals: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).filter(|d| *d > 0.0).collect();
    if intervals.is_empty() {
        return None;
    }
    intervals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = intervals[intervals.len() / 2];
    if median <= 0.0 {
        return None;
    }
    Some(60.0 / median)
}

#[cfg(test)]
mod tests {
    use super::bpm_from_beat_lines;

    #[test]
    fn bpm_from_regular_beats() {
        // 120 BPM = 0.5s intervals; madmom-style "time\tbeat" lines
        let content = "0.500\t1\n1.000\t2\n1.500\t3\n2.000\t4\n2.500\t1\n";
        let bpm = bpm_from_beat_lines(content).unwrap();
        assert!((bpm - 120.0).abs() < 0.01, "got {bpm}");
    }

    #[test]
    fn bpm_median_robust_against_missed_beat() {
        // One missed beat (double interval) must not shift the median
        let content = "0.5\n1.0\n1.5\n2.5\n3.0\n3.5\n4.0\n";
        let bpm = bpm_from_beat_lines(content).unwrap();
        assert!((bpm - 120.0).abs() < 0.01, "got {bpm}");
    }

    #[test]
    fn bpm_too_few_beats_is_none() {
        assert!(bpm_from_beat_lines("0.5\n1.0\n1.5\n").is_none());
        assert!(bpm_from_beat_lines("").is_none());
    }

    #[test]
    fn bpm_ignores_non_numeric_lines() {
        let content = "# header\n0.5 1\n1.0 2\n1.5 3\n2.0 4\n2.5 1\n";
        let bpm = bpm_from_beat_lines(content).unwrap();
        assert!((bpm - 120.0).abs() < 0.01, "got {bpm}");
    }
}
