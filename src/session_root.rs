//! Session-root rule — the single source for "which directory is the
//! shoot". Used by `auto-group` / `import --auto-group`
//! ([`find_session_root`]) and by the catalog's `session_root()` SQL
//! function behind the `scattered:` search filter (the two helpers
//! below, so the SQL side can cache the compiled regex per query).

/// Compile the `[group] session_root_pattern` regex. `None` for an empty
/// pattern (detection off) or an invalid one (treated as "no match").
pub fn compile_pattern(session_root_pattern: &str) -> Option<regex::Regex> {
    if session_root_pattern.is_empty() {
        None
    } else {
        regex::Regex::new(session_root_pattern).ok()
    }
}

/// `dir` up to and including its deepest (rightmost) `/`-separated
/// component that matches `re`; `None` when no component matches.
pub fn deepest_matching_prefix(dir: &str, re: &regex::Regex) -> Option<String> {
    let parts: Vec<&str> = dir.split('/').collect();
    let mut session_idx = None;
    for (i, part) in parts.iter().enumerate() {
        if re.is_match(part) {
            session_idx = Some(i);
        }
    }
    session_idx.map(|idx| parts[..=idx].join("/"))
}

/// Find the session/shoot root directory from a file's directory path.
///
/// Walks up from the file's directory looking for the deepest component that
/// matches `session_root_pattern` (default: starts with YYYY-MM). Everything
/// below that level (Capture/, Selects/, Output/ and their subdirectories)
/// belongs to the same session.
///
/// When no component matches, the fallback depends on `import_roots` — the
/// volume-relative directories handed to `maki import` (empty for the
/// standalone `maki auto-group` command):
///
/// - `dir` lies strictly below one of the import roots → that import root.
///   Importing a whole session folder makes the folder the session, however
///   deep its exports are nested (`Output/Final/Web`).
/// - otherwise (no import roots, or `dir` *is* the import root) → the parent
///   directory, one level up, so importing just `Capture/` or `Output/`
///   still sees its sibling folders.
///
/// Examples (default pattern, no import roots):
/// - `Pictures/Masters/2024/2024-10/2024-10-05-jazz-band/Capture` → `Pictures/Masters/2024/2024-10/2024-10-05-jazz-band`
/// - `Pictures/Masters/2024/2024-10/2024-10-05-jazz-band/Output/Web` → `Pictures/Masters/2024/2024-10/2024-10-05-jazz-band`
/// - `Photos/2024-10-05/RAW` → `Photos/2024-10-05`
/// - `Unsorted/photos/batch1` → `Unsorted/photos` (no date found, one level up)
pub fn find_session_root(dir: &str, session_root_pattern: &str, import_roots: &[String]) -> String {
    if let Some(re) = compile_pattern(session_root_pattern) {
        if let Some(root) = deepest_matching_prefix(dir, &re) {
            return root;
        }
    }

    // No pattern match — the deepest import root that strictly contains
    // `dir` is the session (the user imported that tree as one unit).
    let containing_root = import_roots
        .iter()
        .filter(|root| {
            root.is_empty() && !dir.is_empty()
                || dir.len() > root.len() && dir.starts_with(root.as_str()) && dir.as_bytes()[root.len()] == b'/'
        })
        .max_by_key(|root| root.len());
    if let Some(root) = containing_root {
        return root.clone();
    }

    // Otherwise fall back to the parent directory (one level up from the
    // file's directory), so sibling Capture/ and Output/ folders meet.
    std::path::Path::new(dir)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| dir.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepest_matching_prefix_picks_rightmost_component() {
        let re = compile_pattern(r"^\d{4}-\d{2}").unwrap();
        assert_eq!(
            deepest_matching_prefix("Masters/2024/2024-10/2024-10-05-jazz/Capture", &re).as_deref(),
            Some("Masters/2024/2024-10/2024-10-05-jazz")
        );
        assert_eq!(deepest_matching_prefix("Unsorted/photos/batch1", &re), None);
    }

    #[test]
    fn compile_pattern_rejects_empty_and_invalid() {
        assert!(compile_pattern("").is_none());
        assert!(compile_pattern("(").is_none());
        assert!(compile_pattern("^x").is_some());
    }
}
