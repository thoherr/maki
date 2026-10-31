//! Path-string helpers shared by the storage and service layers.
//!
//! Catalog keys (`file_locations.relative_path`, `recipes.relative_path`)
//! and sidecar paths are always `/`-separated; [`forward_slashes`] is the
//! one conversion from an OS path to that canonical form, so a Windows
//! `strip_prefix` result (backslashes) never reaches a catalog lookup.

use std::path::Path;

/// The path as a `/`-separated string regardless of platform (lossy on
/// non-UTF-8 components, like `to_string_lossy`).
pub fn forward_slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_backslashes_and_keeps_forward_slashes() {
        assert_eq!(forward_slashes(Path::new("a/b/c.jpg")), "a/b/c.jpg");
        assert_eq!(forward_slashes(Path::new("a\\b\\c.jpg")), "a/b/c.jpg");
        assert_eq!(forward_slashes(Path::new("")), "");
    }
}
