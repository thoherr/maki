//! CLI output helpers: human-readable formatting and common progress/status messages.
//!
//! Centralizes patterns previously duplicated across `main.rs`, `model_manager.rs`,
//! `web/templates.rs`, and `web/routes.rs`.

use std::path::PathBuf;
use std::time::Duration;

/// Cross-cutting CLI state every command handler receives as its last
/// parameter: `--json`, `--log`, the `--verbose`/`--debug` pair, and the
/// catalog the entry point resolved — its root and its `maki.toml`,
/// loaded exactly once per invocation (`main` for one-shot commands, once
/// per line in the shell).
///
/// Replaces the former positional `json: bool, log: bool, verbosity`
/// triple (and the per-handler `struct Ctx { json, log }` shims) and the
/// per-handler `find_catalog_root()` / `load_config()` calls.
#[derive(Clone, Default)]
pub struct CliContext {
    /// `--json`: machine-readable JSON on stdout, messages on stderr.
    pub json: bool,
    /// `--log`: per-file progress on stderr.
    pub log: bool,
    /// `--verbose` / `--debug`.
    pub verbosity: crate::Verbosity,
    /// The catalog's `maki.toml`, loaded once by the entry point.
    /// Defaults outside a catalog and when the file is unreadable —
    /// handlers that must not run on defaults use [`Self::catalog`],
    /// which surfaces the load error.
    pub config: std::sync::Arc<crate::config::CatalogConfig>,
    /// Catalog root as resolved by the entry point; `None` outside a catalog.
    catalog_root: Option<PathBuf>,
    /// Rendered load error when `maki.toml` exists but did not parse.
    config_error: Option<String>,
}

impl CliContext {
    pub fn new(json: bool, log: bool, verbosity: crate::Verbosity) -> Self {
        Self { json, log, verbosity, ..Self::default() }
    }

    /// Context with every flag off and no catalog (tests, internal callers).
    pub fn quiet() -> Self {
        Self::default()
    }

    /// Attach the catalog the entry point resolved. `loaded` is the
    /// outcome of reading its `maki.toml`: on `Err` the context carries
    /// the defaults and [`Self::catalog`] reports the error, exactly as
    /// the former per-handler `load_config()?` did.
    pub fn with_catalog(mut self, catalog_root: PathBuf, loaded: anyhow::Result<crate::config::CatalogConfig>) -> Self {
        self.catalog_root = Some(catalog_root);
        match loaded {
            Ok(config) => self.config = std::sync::Arc::new(config),
            Err(e) => self.config_error = Some(format!("{e:#}")),
        }
        self
    }

    /// The catalog root, or the usual "no maki catalog found" error
    /// when the entry point resolved none (`find_catalog_root()?`).
    pub fn catalog_root(&self) -> anyhow::Result<PathBuf> {
        match &self.catalog_root {
            Some(root) => Ok(root.clone()),
            None => crate::config::find_catalog_root(),
        }
    }

    /// The catalog root and its configuration, failing when there is no
    /// catalog or its `maki.toml` did not parse (`load_config()?`).
    pub fn catalog(&self) -> anyhow::Result<(PathBuf, &crate::config::CatalogConfig)> {
        let root = self.catalog_root()?;
        if let Some(e) = &self.config_error {
            anyhow::bail!("{e}");
        }
        Ok((root, &self.config))
    }

    /// True when verbose or debug output is enabled.
    pub fn verbose(&self) -> bool {
        self.verbosity.verbose()
    }

    /// True when debug output is enabled.
    pub fn debug(&self) -> bool {
        self.verbosity.debug()
    }

    /// Print `value` as pretty JSON on stdout when `--json` is set,
    /// otherwise run the human-readable printer.
    pub fn emit<T: serde::Serialize>(&self, value: &T, human: impl FnOnce()) -> anyhow::Result<()> {
        if self.json {
            println!("{}", serde_json::to_string_pretty(value)?);
        } else {
            human();
        }
        Ok(())
    }
}

/// Format a Duration as `h m s.ms`, dropping leading zero fields.
pub fn format_duration(d: Duration) -> String {
    let total_millis = d.as_millis();
    let hours = total_millis / 3_600_000;
    let minutes = (total_millis % 3_600_000) / 60_000;
    let seconds = (total_millis % 60_000) / 1_000;
    let millis = total_millis % 1_000;

    if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}.{millis:03}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}.{millis:03}s")
    } else {
        format!("{seconds}.{millis:03}s")
    }
}

/// Format a byte count as a human-readable string (B / KB / MB / GB).
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Print an indented per-item status line to stderr.
///
/// With duration:    `  {id} — {verb} ({duration})`
/// Without duration: `  {id} — {verb}`
///
/// Consolidates the dominant status-line pattern used across import, cleanup,
/// refresh, fix-* and similar commands.
pub fn item_status(id: &str, verb: &str, elapsed: Option<Duration>) {
    match elapsed {
        Some(d) => eprintln!("  {id} — {verb} ({})", format_duration(d)),
        None => eprintln!("  {id} — {verb}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_without_catalog_reports_none() {
        let ctx = CliContext::quiet();
        // No catalog attached: both accessors fall back to the directory
        // walk, which fails outside a catalog with the usual message.
        let cwd = std::env::current_dir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let root = ctx.catalog_root();
        let both = ctx.catalog().map(|(r, _)| r);
        std::env::set_current_dir(cwd).unwrap();
        assert!(root.unwrap_err().to_string().contains("no maki catalog found"));
        assert!(both.unwrap_err().to_string().contains("no maki catalog found"));
    }

    #[test]
    fn context_with_loaded_config_serves_it() {
        let mut cfg = crate::config::CatalogConfig::default();
        cfg.serve.port = 4242;
        let ctx = CliContext::quiet().with_catalog(PathBuf::from("/cat"), Ok(cfg));
        assert_eq!(ctx.catalog_root().unwrap(), PathBuf::from("/cat"));
        let (root, config) = ctx.catalog().unwrap();
        assert_eq!(root, PathBuf::from("/cat"));
        assert_eq!(config.serve.port, 4242);
        assert_eq!(ctx.config.serve.port, 4242);
    }

    #[test]
    fn context_with_broken_config_keeps_defaults_and_surfaces_error() {
        let ctx = CliContext::quiet()
            .with_catalog(PathBuf::from("/cat"), Err(anyhow::anyhow!("bad toml")));
        // The root is still known; `catalog()` fails like `load_config()?` did.
        assert_eq!(ctx.catalog_root().unwrap(), PathBuf::from("/cat"));
        let err = ctx.catalog().map(|(r, _)| r).unwrap_err();
        assert_eq!(format!("{err:#}"), "bad toml");
        // Handlers that tolerate a broken file run on defaults.
        assert_eq!(ctx.config.serve.port, crate::config::CatalogConfig::default().serve.port);
    }

    #[test]
    fn format_duration_sub_minute() {
        assert_eq!(format_duration(Duration::from_millis(1234)), "1.234s");
        assert_eq!(format_duration(Duration::from_millis(0)), "0.000s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(Duration::from_secs(75)), "1m 15.000s");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(
            format_duration(Duration::from_secs(3725)),
            "1h 02m 05.000s"
        );
    }

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn format_size_kilobytes() {
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1500), "1.5 KB");
    }

    #[test]
    fn format_size_megabytes() {
        assert_eq!(format_size(94 * 1024 * 1024), "94.0 MB");
    }

    #[test]
    fn format_size_gigabytes() {
        assert_eq!(format_size(2 * 1024 * 1024 * 1024), "2.0 GB");
    }
}
