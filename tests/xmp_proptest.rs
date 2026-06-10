//! Property-based round-trip tests for the XMP reader/writer.
//!
//! The v4.5.14–v4.5.17 bug train (runaway `&amp;amp;…` escalation,
//! namespace-prefix misses, leaf-entry stranding) was exactly the class
//! of bug property testing catches: each fix was example-driven, and
//! the next hostile input found the next hole. These properties lock
//! the *general* contracts:
//!
//! 1. **Round-trip**: whatever the writer writes, the reader reads back
//!    verbatim — for arbitrary hostile text (entities, quotes, unicode).
//! 2. **Idempotence**: re-applying the same update reports "no change"
//!    and leaves the file byte-identical (the anti-escalation property —
//!    one `&` must never become `&amp;amp;` no matter how often a
//!    writeback runs).
//! 3. **Removal**: removing a subset leaves exactly the complement.
//!
//! Runs against the real file-based API (`update_*` + `extract`) on
//! temp files, starting from `create_xmp` output — the same shape MAKI
//! writes for sidecar-less imports.

use proptest::prelude::*;

use maki::xmp_reader;

/// Hostile-but-legal tag text: XML-special characters, quotes, unicode,
/// inner spaces. No `|` (hierarchy separator), no control characters,
/// no leading/trailing whitespace (XML text round-trips trim-sensitive
/// edges are the catalog normalizer's job, not the writer's).
fn tag_text() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9&<>\"'äöüßéñ漢 _.,:;()+-]{1,24}")
        .unwrap()
        .prop_map(|s| s.trim().to_string())
        .prop_filter("non-empty after trim", |s| !s.is_empty())
}

/// A set of distinct tags (case-insensitively distinct, since keyword
/// matching in the writer is case-aware but sets in this test compare
/// exact — avoid generating near-duplicates).
fn tag_set(max: usize) -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(tag_text(), 1..=max).prop_map(|tags| {
        let mut seen = std::collections::HashSet::new();
        tags.into_iter()
            .filter(|t| seen.insert(t.to_lowercase()))
            .collect()
    })
}

/// Hierarchical pipe-paths built from hostile segments. Always 2+
/// segments: `update_hierarchical_subjects` deliberately filters
/// single-segment (pipe-less) entries out of the ADD list — flat tags
/// don't belong in `lr:hierarchicalSubject` (v4.5.17 decision).
fn hier_paths(max: usize) -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(
        proptest::collection::vec(tag_text(), 2..=3).prop_map(|segs| segs.join("|")),
        1..=max,
    )
    .prop_map(|paths| {
        let mut seen = std::collections::HashSet::new();
        paths
            .into_iter()
            .filter(|p| seen.insert(p.to_lowercase()))
            // A path that is a strict prefix of another (`a` vs `a|b`)
            // exercises ancestor semantics that the WRITER treats as
            // distinct entries but ADD-deduplication may merge; keep the
            // property focused on exact round-trips.
            .collect::<Vec<_>>()
    })
    .prop_filter("no path is a prefix of another", |paths| {
        !paths.iter().enumerate().any(|(i, a)| {
            paths.iter().enumerate().any(|(j, b)| {
                i != j && b.to_lowercase().starts_with(&format!("{}|", a.to_lowercase()))
            })
        })
    })
}

fn write_fixture(dir: &std::path::Path, content: &str) -> std::path::PathBuf {
    let path = dir.join("test.xmp");
    std::fs::write(&path, content) .unwrap();
    path
}

fn empty_xmp() -> String {
    xmp_reader::create_xmp(&[], None, None, None)
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Writer → reader round-trip for dc:subject keywords, and the
    /// anti-escalation property: a second identical update is a no-op
    /// and the file is byte-stable across repeated writebacks.
    #[test]
    fn tags_round_trip_and_stay_byte_stable(tags in tag_set(6)) {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), &empty_xmp());

        let changed = xmp_reader::update_tags(&path, &tags, &[]).unwrap();
        prop_assert!(changed, "first add must report a change");
        let data = xmp_reader::extract(&path);
        prop_assert_eq!(sorted(data.keywords.clone()), sorted(tags.clone()));

        // Idempotence: same update again → no change, identical bytes.
        let bytes1 = std::fs::read(&path).unwrap();
        let changed2 = xmp_reader::update_tags(&path, &tags, &[]).unwrap();
        prop_assert!(!changed2, "second identical add must be a no-op");
        let bytes2 = std::fs::read(&path).unwrap();
        prop_assert_eq!(&bytes1, &bytes2, "no-op must be byte-stable");

        // Escalation check: five more writebacks, then the reader must
        // still see the original strings (no &amp;amp; accumulation).
        for _ in 0..5 {
            let _ = xmp_reader::update_tags(&path, &tags, &[]).unwrap();
        }
        let data = xmp_reader::extract(&path);
        prop_assert_eq!(sorted(data.keywords), sorted(tags));
    }

    /// Removing a subset leaves exactly the complement.
    #[test]
    fn tags_remove_leaves_complement(tags in tag_set(6), pick in any::<proptest::sample::Index>()) {
        prop_assume!(tags.len() >= 2);
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), &empty_xmp());
        xmp_reader::update_tags(&path, &tags, &[]).unwrap();

        let victim = tags[pick.index(tags.len())].clone();
        let changed = xmp_reader::update_tags(&path, &[], &[victim.clone()]).unwrap();
        prop_assert!(changed);

        let data = xmp_reader::extract(&path);
        let expected: Vec<String> = tags.iter().filter(|t| **t != victim).cloned().collect();
        prop_assert_eq!(sorted(data.keywords), sorted(expected));
    }

    /// lr:hierarchicalSubject round-trip + idempotence for hostile
    /// pipe-paths.
    #[test]
    fn hierarchical_round_trip_and_idempotent(paths in hier_paths(5)) {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), &empty_xmp());

        let changed = xmp_reader::update_hierarchical_subjects(&path, &paths, &[]).unwrap();
        prop_assert!(changed);
        let data = xmp_reader::extract(&path);
        prop_assert_eq!(sorted(data.hierarchical_keywords.clone()), sorted(paths.clone()));

        let bytes1 = std::fs::read(&path).unwrap();
        let changed2 = xmp_reader::update_hierarchical_subjects(&path, &paths, &[]).unwrap();
        prop_assert!(!changed2, "second identical add must be a no-op");
        prop_assert_eq!(&std::fs::read(&path).unwrap(), &bytes1);

        for _ in 0..5 {
            let _ = xmp_reader::update_hierarchical_subjects(&path, &paths, &[]).unwrap();
        }
        let data = xmp_reader::extract(&path);
        prop_assert_eq!(sorted(data.hierarchical_keywords), sorted(paths));
    }

    /// Description round-trip + idempotence for hostile single-line text.
    #[test]
    fn description_round_trip_and_idempotent(desc in tag_text()) {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), &empty_xmp());

        let changed = xmp_reader::update_description(&path, Some(&desc)).unwrap();
        prop_assert!(changed);
        let data = xmp_reader::extract(&path);
        prop_assert_eq!(data.description.as_deref(), Some(desc.as_str()));

        let bytes1 = std::fs::read(&path).unwrap();
        let changed2 = xmp_reader::update_description(&path, Some(&desc)).unwrap();
        prop_assert!(!changed2);
        prop_assert_eq!(&std::fs::read(&path).unwrap(), &bytes1);
    }

    /// Label round-trip + idempotence for hostile text.
    #[test]
    fn label_round_trip_and_idempotent(label in tag_text()) {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), &empty_xmp());

        let changed = xmp_reader::update_label(&path, Some(&label)).unwrap();
        prop_assert!(changed);
        let data = xmp_reader::extract(&path);
        prop_assert_eq!(data.source_metadata.get("label").map(|s| s.as_str()), Some(label.as_str()));

        let bytes1 = std::fs::read(&path).unwrap();
        let changed2 = xmp_reader::update_label(&path, Some(&label)).unwrap();
        prop_assert!(!changed2);
        prop_assert_eq!(&std::fs::read(&path).unwrap(), &bytes1);
    }

    /// Rating round-trip + idempotence for the full value range.
    #[test]
    fn rating_round_trip_and_idempotent(rating in 1u8..=5) {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), &empty_xmp());

        let changed = xmp_reader::update_rating(&path, Some(rating)).unwrap();
        prop_assert!(changed);
        let data = xmp_reader::extract(&path);
        let expected_rating = rating.to_string();
        prop_assert_eq!(
            data.source_metadata.get("rating").map(|s| s.as_str()),
            Some(expected_rating.as_str())
        );

        let bytes1 = std::fs::read(&path).unwrap();
        let changed2 = xmp_reader::update_rating(&path, Some(rating)).unwrap();
        prop_assert!(!changed2);
        prop_assert_eq!(&std::fs::read(&path).unwrap(), &bytes1);
    }

    /// The full create_xmp path round-trips hostile keywords, label,
    /// and description in one document.
    #[test]
    fn create_xmp_round_trips(tags in tag_set(4), label in tag_text(), desc in tag_text(), rating in 1u8..=5) {
        let dir = tempfile::tempdir().unwrap();
        let content = xmp_reader::create_xmp(&tags, Some(rating), Some(&label), Some(&desc));
        let path = write_fixture(dir.path(), &content);

        let data = xmp_reader::extract(&path);
        // create_xmp writes flat components to dc:subject: a tag with no
        // '|' is its own component, so the set round-trips as-is.
        prop_assert_eq!(sorted(data.keywords), sorted(tags));
        prop_assert_eq!(data.source_metadata.get("label").map(|s| s.as_str()), Some(label.as_str()));
        prop_assert_eq!(data.description.as_deref(), Some(desc.as_str()));
        let expected_rating = rating.to_string();
        prop_assert_eq!(
            data.source_metadata.get("rating").map(|s| s.as_str()),
            Some(expected_rating.as_str())
        );
    }
}
