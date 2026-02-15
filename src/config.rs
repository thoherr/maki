use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Catalog configuration stored in dam.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_volume: Option<Uuid>,
}

impl Default for CatalogConfig {
    fn default() -> Self {
        Self {
            default_volume: None,
        }
    }
}

impl CatalogConfig {
    /// Load configuration from a dam.toml file.
    pub fn load(catalog_root: &Path) -> Result<Self> {
        let path = catalog_root.join("dam.toml");
        if path.exists() {
            let contents = std::fs::read_to_string(&path)?;
            let config: Self = toml_minimal_parse(&contents)?;
            Ok(config)
        } else {
            Ok(Self::default())
        }
    }

    /// Save configuration to a dam.toml file.
    pub fn save(&self, catalog_root: &Path) -> Result<()> {
        let path = catalog_root.join("dam.toml");
        let contents = toml_minimal_serialize(self)?;
        std::fs::write(path, contents)?;
        Ok(())
    }
}

/// Find the catalog root by looking for dam.toml in current and parent directories.
pub fn find_catalog_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("dam.toml").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            anyhow::bail!("No dam catalog found. Run `dam init` to create one.");
        }
    }
}

// Simple TOML handling without a full toml crate dependency.
// We only have one optional field, so hand-written parsing is fine.
fn toml_minimal_parse(contents: &str) -> Result<CatalogConfig> {
    let mut default_volume = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("default_volume") {
            let rest = rest.trim();
            if let Some(value) = rest.strip_prefix('=') {
                let value = value.trim().trim_matches('"');
                default_volume = Some(value.parse::<Uuid>()?);
            }
        }
    }
    Ok(CatalogConfig { default_volume })
}

fn toml_minimal_serialize(config: &CatalogConfig) -> Result<String> {
    let mut out = String::from("# dam catalog configuration\n");
    if let Some(vol) = &config.default_volume {
        out.push_str(&format!("default_volume = \"{vol}\"\n"));
    }
    Ok(out)
}
