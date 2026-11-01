//! Typed error model (architecture audit F19, first slice).
//!
//! `anyhow` stays the error type at the edges; the engine raises
//! [`MakiError`] for the outcomes that callers *act on* — a missing
//! asset becomes a 404 or exit code 3, an offline volume a 409 or exit
//! code 4 — so no layer has to match on message text any more. Anything
//! else is a plain `anyhow::Error` and maps to "general failure".
//!
//! Classification helpers walk the whole `anyhow` context chain, so a
//! `MakiError` wrapped in `.context(..)` still classifies.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MakiError {
    /// A referenced entity does not exist. `kind` is the noun used in the
    /// message (`asset`, `volume`, `stack`, `collection`, `person`, …).
    /// The wording `no <kind> found matching '<key>'` is the one the CLI
    /// has always printed for assets.
    #[error("no {kind} found matching '{key}'{}", match hint { Some(h) => format!(" ({h})"), None => String::new() })]
    NotFound { kind: &'static str, key: String, hint: Option<String> },

    /// The operation needs a volume that is not mounted.
    #[error("volume '{volume}' is offline{}", match detail { Some(d) => format!(" ({d})"), None => String::new() })]
    Offline { volume: String, detail: Option<String> },

    /// The request contradicts existing state (duplicate name, member
    /// already present, cannot remove while referenced, …).
    #[error("{0}")]
    Conflict(String),

    /// The request itself is malformed (bad filter syntax, invalid value).
    #[error("{0}")]
    InvalidInput(String),

    /// The request names something the caller may not reach (a path that
    /// escapes a volume mount point).
    #[error("{0}")]
    Forbidden(String),

    /// A consistency check (`doctor`, `verify`) ran to completion and
    /// found problems. Not an operational failure: the report has been
    /// printed, the exit status just has to say so.
    #[error("{command}: {count} unresolved issue(s)")]
    CheckFailed { command: &'static str, count: usize },
}

impl MakiError {
    pub fn not_found(kind: &'static str, key: impl Into<String>) -> Self {
        Self::NotFound { kind, key: key.into(), hint: None }
    }

    /// Not-found with a short pointer appended in parentheses, e.g.
    /// `known volumes: Archive, Travel`.
    pub fn not_found_with_hint(kind: &'static str, key: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::NotFound { kind, key: key.into(), hint: Some(hint.into()) }
    }

    pub fn offline(volume: impl Into<String>, detail: Option<String>) -> Self {
        Self::Offline { volume: volume.into(), detail }
    }
}

/// Process exit codes. `2` is clap's usage-error code and is listed here
/// only so the table is complete in one place.
pub mod exit_code {
    pub const SUCCESS: i32 = 0;
    /// Any failure not classified below (I/O, SQLite, parse, check failed).
    pub const GENERAL: i32 = 1;
    pub const USAGE: i32 = 2;
    /// [`super::MakiError::NotFound`].
    pub const NOT_FOUND: i32 = 3;
    /// [`super::MakiError::Offline`].
    pub const OFFLINE: i32 = 4;
}

/// Find the first [`MakiError`] anywhere in an `anyhow` chain.
pub fn classify(err: &anyhow::Error) -> Option<&MakiError> {
    err.chain().find_map(|e| e.downcast_ref::<MakiError>())
}

/// Exit code for a failed command (see [`exit_code`]).
pub fn exit_code_for(err: &anyhow::Error) -> i32 {
    match classify(err) {
        Some(MakiError::NotFound { .. }) => exit_code::NOT_FOUND,
        Some(MakiError::Offline { .. }) => exit_code::OFFLINE,
        _ => exit_code::GENERAL,
    }
}

/// HTTP status for a failed request, when the error is classified.
/// `None` means "the handler's default" (usually 500).
pub fn http_status_for(err: &anyhow::Error) -> Option<u16> {
    match classify(err)? {
        MakiError::NotFound { .. } => Some(404),
        MakiError::Offline { .. } | MakiError::Conflict(_) => Some(409),
        MakiError::InvalidInput(_) => Some(400),
        MakiError::Forbidden(_) => Some(403),
        MakiError::CheckFailed { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_through_context_chain() {
        let err: anyhow::Error = MakiError::not_found("asset", "abc").into();
        let err = err.context("while editing").context("outer");
        assert!(matches!(classify(&err), Some(MakiError::NotFound { .. })));
        assert_eq!(exit_code_for(&err), exit_code::NOT_FOUND);
        assert_eq!(http_status_for(&err), Some(404));
    }

    #[test]
    fn unclassified_errors_are_general() {
        let err = anyhow::anyhow!("disk on fire");
        assert!(classify(&err).is_none());
        assert_eq!(exit_code_for(&err), exit_code::GENERAL);
        assert_eq!(http_status_for(&err), None);
    }

    #[test]
    fn wording_matches_the_historical_cli_text() {
        assert_eq!(
            MakiError::not_found("asset", "a1b2").to_string(),
            "no asset found matching 'a1b2'"
        );
        assert_eq!(
            MakiError::not_found_with_hint("volume", "X", "known volumes: A, B").to_string(),
            "no volume found matching 'X' (known volumes: A, B)"
        );
        assert_eq!(
            MakiError::offline("Archive", Some("mount point /Volumes/A not found".into())).to_string(),
            "volume 'Archive' is offline (mount point /Volumes/A not found)"
        );
        assert_eq!(exit_code_for(&MakiError::offline("x", None).into()), exit_code::OFFLINE);
    }
}
