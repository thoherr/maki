//! Atomic YAML file persistence for the catalog-level source-of-truth
//! files (`stacks.yaml`, `collections.yaml`, `faces.yaml`, `people.yaml`,
//! `volumes.yaml`, …) — architecture audit F6.
//!
//! `MetadataStore::save` already writes sidecars via temp file + rename;
//! this module gives the other YAML files the same guarantee so a crash
//! or a concurrent reader never sees a truncated file. Per-asset sidecars
//! keep their own implementation because they also hold the cross-process
//! write lock.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};

/// Load `path` as YAML, or return `T::default()` when the file does not
/// exist. A file that exists but does not parse is an error (never
/// silently replaced by defaults: the file is the source of truth).
pub fn load_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_norway::from_str(&contents).with_context(|| format!("parsing {}", path.display()))
}

/// Serialise `value` as YAML and replace `path` atomically (write to a
/// sibling temp file, then rename). Creates the parent directory.
pub fn save_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let yaml = serde_norway::to_string(value)
        .with_context(|| format!("serialising {}", path.display()))?;
    save_str_atomic(path, &yaml)
}

/// [`save_atomic`] for already-serialised text.
pub fn save_str_atomic(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file.yaml");
    let tmp = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Doc {
        items: Vec<String>,
    }

    #[test]
    fn missing_file_loads_default() {
        let dir = tempfile::tempdir().unwrap();
        let doc: Doc = load_or_default(&dir.path().join("none.yaml")).unwrap();
        assert_eq!(doc, Doc::default());
    }

    #[test]
    fn round_trips_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("doc.yaml");
        let doc = Doc { items: vec!["a".into(), "b".into()] };
        save_atomic(&path, &doc).unwrap();
        let back: Doc = load_or_default(&path).unwrap();
        assert_eq!(back, doc);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn corrupt_file_is_an_error_not_a_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.yaml");
        std::fs::write(&path, "items: [unclosed").unwrap();
        let err = load_or_default::<Doc>(&path).unwrap_err();
        assert!(format!("{err:#}").contains("parsing"), "{err:#}");
    }
}
