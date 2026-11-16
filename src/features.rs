//! Compile-time feature facts in one place (architecture audit F25).
//!
//! Code that *needs* an optional crate stays behind `#[cfg(feature = …)]`;
//! everything that merely *reports* the build — the edition suffix in
//! `--version` and `--help`, the "AI available" flag in `status` and the
//! web UI, tests that branch on the edition — reads these constants
//! instead of repeating the `cfg!` check.

/// SigLIP embeddings, auto-tag, faces, similarity search are compiled in.
pub const AI: bool = cfg!(feature = "ai");

/// MAKI Pro edition (implies [`AI`]): VLM describe, writeback, sync-metadata.
pub const PRO: bool = cfg!(feature = "pro");

/// The version string shown by `maki --version`: the crate version, with
/// the edition suffix appended in the Pro build.
pub const VERSION: &str = if PRO {
    concat!(env!("CARGO_PKG_VERSION"), " Pro")
} else {
    env!("CARGO_PKG_VERSION")
};

/// Edition suffix appended to the product name in the help header
/// (`maki Pro 4.12.3 …`): `" Pro"` in the Pro build, empty otherwise.
pub fn edition() -> &'static str {
    if PRO {
        " Pro"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pro_implies_ai() {
        let consistent = !PRO || AI;
        assert!(consistent, "the pro feature must enable ai");
    }

    #[test]
    fn version_carries_edition() {
        assert!(VERSION.starts_with(env!("CARGO_PKG_VERSION")));
        assert_eq!(VERSION.ends_with(" Pro"), PRO);
        assert_eq!(edition().is_empty(), !PRO);
    }
}
