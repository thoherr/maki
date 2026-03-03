//! Download and cache management for SigLIP ONNX model files.
//!
//! Only compiled when the `ai` feature is enabled.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// HuggingFace base URL for model files.
const HF_BASE_URL: &str =
    "https://huggingface.co/Xenova/siglip-base-patch16-256/resolve/main";

/// Model files to download.
/// SHA-256 verification is skipped — these are large binaries from a known HuggingFace repo
/// and the xetHub content hashes differ from standard file SHA-256.
const MODEL_FILES: &[(&str, Option<&str>)] = &[
    ("onnx/vision_model_quantized.onnx", None),
    ("onnx/text_model_quantized.onnx", None),
    ("tokenizer.json", None),
];

/// Manages downloading and caching of SigLIP model files.
pub struct ModelManager {
    model_dir: PathBuf,
}

impl ModelManager {
    /// Create a new ModelManager with the given model directory.
    pub fn new(model_dir: &Path) -> Self {
        Self {
            model_dir: model_dir.to_path_buf(),
        }
    }

    /// Default model directory: `~/.dam/models/siglip-vit-b16-256/`.
    pub fn default_model_dir() -> Result<PathBuf> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .context("Cannot determine home directory")?;
        Ok(PathBuf::from(home)
            .join(".dam")
            .join("models")
            .join("siglip-vit-b16-256"))
    }

    /// Check if all required model files exist.
    pub fn model_exists(&self) -> bool {
        MODEL_FILES.iter().all(|(rel_path, _)| {
            self.model_dir.join(rel_path).exists()
        })
    }

    /// Return the model directory path.
    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    /// Ensure the model is downloaded. Returns the model directory path.
    pub fn ensure_model(
        &self,
        on_progress: impl Fn(&str, u64, u64),
    ) -> Result<PathBuf> {
        if self.model_exists() {
            return Ok(self.model_dir.clone());
        }
        self.download_model(on_progress)?;
        Ok(self.model_dir.clone())
    }

    /// Download model files from HuggingFace.
    pub fn download_model(
        &self,
        on_progress: impl Fn(&str, u64, u64),
    ) -> Result<()> {
        let total_files = MODEL_FILES.len() as u64;

        for (i, (rel_path, expected_sha)) in MODEL_FILES.iter().enumerate() {
            let dest = self.model_dir.join(rel_path);
            let url = format!("{HF_BASE_URL}/{rel_path}");

            on_progress(rel_path, i as u64 + 1, total_files);

            if dest.exists() {
                // Verify hash if we have one
                if let Some(expected) = expected_sha {
                    let hash = hash_file(&dest)?;
                    if hash == *expected {
                        continue; // Already downloaded and valid
                    }
                    // Hash mismatch — re-download
                    std::fs::remove_file(&dest).ok();
                } else {
                    continue; // No hash to verify, file exists
                }
            }

            // Create parent directories
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
            }

            // Download via curl (available on all platforms)
            let tmp_dest = dest.with_extension("download");
            let status = std::process::Command::new("curl")
                .args([
                    "-fSL",
                    "--progress-bar",
                    "-o",
                    tmp_dest.to_str().unwrap(),
                    &url,
                ])
                .status()
                .context("Failed to run curl. Is curl installed?")?;

            if !status.success() {
                // Clean up partial download
                std::fs::remove_file(&tmp_dest).ok();
                anyhow::bail!("Download failed for {rel_path} (curl exit code: {})", status);
            }

            // Verify hash
            if let Some(expected) = expected_sha {
                let hash = hash_file(&tmp_dest)?;
                if hash != *expected {
                    std::fs::remove_file(&tmp_dest).ok();
                    anyhow::bail!(
                        "Hash mismatch for {rel_path}: expected {expected}, got {hash}"
                    );
                }
            }

            // Rename to final path
            std::fs::rename(&tmp_dest, &dest)
                .with_context(|| format!("Failed to rename {} to {}", tmp_dest.display(), dest.display()))?;
        }

        Ok(())
    }

    /// Remove all cached model files.
    pub fn remove_model(&self) -> Result<()> {
        if self.model_dir.exists() {
            std::fs::remove_dir_all(&self.model_dir)
                .with_context(|| {
                    format!("Failed to remove model directory: {}", self.model_dir.display())
                })?;
        }
        Ok(())
    }

    /// List model files with sizes.
    pub fn list_files(&self) -> Vec<(String, u64)> {
        MODEL_FILES
            .iter()
            .filter_map(|(rel_path, _)| {
                let path = self.model_dir.join(rel_path);
                let size = std::fs::metadata(&path).ok()?.len();
                Some((rel_path.to_string(), size))
            })
            .collect()
    }

    /// Total size of cached model files in bytes.
    pub fn total_size(&self) -> u64 {
        self.list_files().iter().map(|(_, s)| s).sum()
    }
}

/// Compute SHA-256 hash of a file (hex string, no prefix).
fn hash_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];

    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Format a byte count as a human-readable string.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_dir_under_home() {
        let dir = ModelManager::default_model_dir().unwrap();
        assert!(
            dir.to_str().unwrap().contains(".dam/models/siglip-vit-b16-256"),
            "Expected .dam/models path, got: {}",
            dir.display()
        );
    }

    #[test]
    fn model_exists_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path());
        assert!(!mgr.model_exists());
    }

    #[test]
    fn list_files_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path());
        assert!(mgr.list_files().is_empty());
    }

    #[test]
    fn total_size_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path());
        assert_eq!(mgr.total_size(), 0);
    }

    #[test]
    fn remove_model_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(&dir.path().join("nonexistent"));
        mgr.remove_model().unwrap(); // Should not error
    }

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(500), "500 B");
    }

    #[test]
    fn format_size_kilobytes() {
        assert_eq!(format_size(1500), "1.5 KB");
    }

    #[test]
    fn format_size_megabytes() {
        assert_eq!(format_size(94 * 1024 * 1024), "94.0 MB");
    }

    #[test]
    fn format_size_gigabytes() {
        assert_eq!(format_size(2 * 1024 * 1024 * 1024), "2.00 GB");
    }

    #[test]
    fn hash_file_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();

        let h1 = hash_file(&path).unwrap();
        let h2 = hash_file(&path).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex length
    }
}
