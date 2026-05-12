//! AI label vocabulary with optional hierarchical-tag mapping.
//!
//! The flat-label list that the SigLIP zero-shot classifier sees is one
//! half of the picture. The other half is what tag(s) MAKI applies when
//! the model picks a given label. Two halves, one file.
//!
//! Format detection by extension on the loader path:
//!
//! - `.yaml` / `.yml` → vocabulary file. Keys are labels; values are
//!   `String`, `Vec<String>`, or `null` (leave label flat).
//! - `.txt` → bare label per line. Mapping is identity — suggested
//!   tag IS the label. Preserves the pre-v4.5.x behaviour for users
//!   who already have a flat labels file.
//!
//! When no file is configured, the built-in [`default_vocabulary`] is
//! used. The built-in is a YAML document embedded at compile time via
//! `include_str!` so there is no install-time file to manage.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

use crate::ai::AutoTagSuggestion;

/// The labels the AI model is scored against, plus the mapping that
/// converts a hit on one of those labels into the hierarchical tag(s)
/// MAKI suggests applying.
#[derive(Debug, Clone)]
pub struct Vocabulary {
    /// The flat label list. Iteration order is the YAML key order
    /// (or the txt-file line order) so the user can group related
    /// labels together — only affects the order labels are fed into
    /// the text encoder, but keeps debug output readable.
    pub labels: Vec<String>,
    /// Mapping: label → one or more hierarchical tags. A label that
    /// is absent from the map (or maps to an empty Vec) is suggested
    /// as-is. A label that maps to multiple tags fans out — each
    /// suggested tag inherits the label's classification confidence.
    mapping: HashMap<String, Vec<String>>,
}

impl Vocabulary {
    /// Build a Vocabulary directly from parts. Used by tests and the
    /// `from_txt_list` constructor.
    pub fn new(labels: Vec<String>, mapping: HashMap<String, Vec<String>>) -> Self {
        Self { labels, mapping }
    }

    /// Number of distinct labels.
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// Map a single label to its hierarchical tag(s). Labels not in the
    /// mapping pass through unchanged — same as the legacy `.txt`-list
    /// behaviour.
    pub fn map_label(&self, label: &str) -> Vec<String> {
        match self.mapping.get(label) {
            Some(v) if !v.is_empty() => v.clone(),
            _ => vec![label.to_string()],
        }
    }

    /// Apply the hierarchical mapping to a list of model suggestions.
    ///
    /// Per-suggestion: each label maps to one or more hierarchical
    /// tags via [`map_label`]. The mapped suggestions inherit the
    /// original suggestion's confidence, and the original label is
    /// retained on each so callers (e.g. the UI) can surface the
    /// model's actual hit underneath the applied tag.
    ///
    /// Then dedup: when two source labels map to the same tag (or a
    /// single label fans into a tag that another label also produced),
    /// keep the highest-confidence instance. Tag identity is the
    /// exact string match — case is preserved, since hierarchical tags
    /// in MAKI are case-sensitive.
    pub fn apply(&self, suggestions: Vec<AutoTagSuggestion>) -> Vec<MappedSuggestion> {
        let mut by_tag: HashMap<String, MappedSuggestion> = HashMap::new();
        for s in suggestions {
            let mapped = self.map_label(&s.tag);
            for tag in mapped {
                let entry = by_tag.entry(tag.clone()).or_insert_with(|| MappedSuggestion {
                    tag: tag.clone(),
                    confidence: f32::NEG_INFINITY,
                    source_label: None,
                });
                if s.confidence > entry.confidence {
                    entry.confidence = s.confidence;
                    entry.source_label = if tag == s.tag { None } else { Some(s.tag.clone()) };
                }
            }
        }
        // Stable ordering: highest confidence first, ties broken by tag
        // string for determinism (tests + diff-friendly output).
        let mut out: Vec<MappedSuggestion> = by_tag.into_values().collect();
        out.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.tag.cmp(&b.tag))
        });
        out
    }
}

/// A suggestion after vocabulary mapping has been applied.
///
/// `source_label` is `Some(label)` when the suggestion came from a
/// non-identity mapping (the model picked `label`, we applied
/// `tag`). Identity mappings leave it `None`. Surfaced as a tooltip
/// in the suggest-tags dropdown so the user can see what the AI
/// actually classified the image as underneath the hierarchical tag.
#[derive(Debug, Clone)]
pub struct MappedSuggestion {
    pub tag: String,
    pub confidence: f32,
    pub source_label: Option<String>,
}

/// Built-in default vocabulary (the keys map roughly 96 photographic
/// labels into the facet structure documented in the tagging guide:
/// subject, event, lighting, technique, composition, season, type).
///
/// Compiled in via `include_str!` so there's no install-time file.
pub fn default_vocabulary() -> Vocabulary {
    let yaml = include_str!("default-vocabulary.yaml");
    parse_yaml(yaml).expect("default vocabulary YAML must be valid — checked at build via test")
}

/// Load a vocabulary from `path`, detecting format by file extension.
///
/// `.yaml` / `.yml`: rich vocabulary with mapping. Parsed via [`parse_yaml`].
/// `.txt` (or any other extension): flat labels, one per line, with
/// `#`-comments and blank lines ignored. Mapping is identity.
pub fn load_from_path(path: &Path) -> Result<Vocabulary> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read labels file: {}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "yaml" | "yml" => parse_yaml(&content)
            .with_context(|| format!("Failed to parse YAML vocabulary: {}", path.display())),
        _ => Ok(parse_txt_list(&content)),
    }
}

/// Parse a YAML vocabulary string. Format: top-level mapping where
/// keys are labels and values are one of `String`, `Vec<String>`, or
/// `null`. Iteration order of `labels` matches the source order so
/// hand-grouped layouts (Scene types, Nature, …) come through intact
/// in `--verbose` model output.
fn parse_yaml(content: &str) -> Result<Vocabulary> {
    // serde_yaml::from_str on a Mapping doesn't preserve insertion
    // order, but we want order-stability for nicer debug output, so
    // parse into a serde_yaml::Value and walk it manually.
    let value: serde_yaml::Value = serde_yaml::from_str(content)
        .context("Failed to parse vocabulary YAML")?;

    let map = match value {
        serde_yaml::Value::Mapping(m) => m,
        serde_yaml::Value::Null => {
            anyhow::bail!("vocabulary YAML is empty");
        }
        other => {
            anyhow::bail!(
                "vocabulary YAML must be a top-level mapping, got {}",
                value_kind(&other)
            );
        }
    };

    let mut labels = Vec::with_capacity(map.len());
    let mut mapping = HashMap::with_capacity(map.len());

    for (k, v) in &map {
        let label = match k {
            serde_yaml::Value::String(s) => s.clone(),
            other => anyhow::bail!(
                "vocabulary keys must be strings, got {}",
                value_kind(other)
            ),
        };

        let tags: Vec<String> = match v {
            serde_yaml::Value::Null => Vec::new(),
            serde_yaml::Value::String(s) => vec![s.clone()],
            serde_yaml::Value::Sequence(seq) => {
                let mut out = Vec::with_capacity(seq.len());
                for item in seq {
                    match item {
                        serde_yaml::Value::String(s) => out.push(s.clone()),
                        other => anyhow::bail!(
                            "vocabulary list items for label '{label}' must be strings, got {}",
                            value_kind(other)
                        ),
                    }
                }
                out
            }
            other => anyhow::bail!(
                "vocabulary value for label '{label}' must be a string, list, or null, got {}",
                value_kind(other)
            ),
        };

        labels.push(label.clone());
        mapping.insert(label, tags);
    }

    if labels.is_empty() {
        anyhow::bail!("vocabulary YAML has no labels");
    }

    Ok(Vocabulary { labels, mapping })
}

/// Parse a flat-text labels file. Mapping is identity (suggested tag
/// equals label). Comments (`#`) and blank lines are dropped. Matches
/// the pre-v4.5.x `load_labels_from_file` semantics exactly.
fn parse_txt_list(content: &str) -> Vocabulary {
    let labels: Vec<String> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect();
    Vocabulary {
        labels,
        mapping: HashMap::new(),
    }
}

fn value_kind(v: &serde_yaml::Value) -> &'static str {
    match v {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "bool",
        serde_yaml::Value::Number(_) => "number",
        serde_yaml::Value::String(_) => "string",
        serde_yaml::Value::Sequence(_) => "list",
        serde_yaml::Value::Mapping(_) => "mapping",
        serde_yaml::Value::Tagged(_) => "tagged",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sugg(tag: &str, confidence: f32) -> AutoTagSuggestion {
        AutoTagSuggestion {
            tag: tag.to_string(),
            confidence,
        }
    }

    /// The compiled-in default must parse cleanly. This is the only
    /// guard against shipping a broken built-in vocabulary.
    #[test]
    fn default_vocabulary_loads() {
        let v = default_vocabulary();
        assert!(v.len() >= 90, "expected ≥ 90 default labels, got {}", v.len());
        // Spot-check a couple of mappings the rest of the suite relies on.
        assert_eq!(v.map_label("sunset"), vec!["lighting|sunset"]);
        assert_eq!(v.map_label("wedding"), vec!["event|wedding"]);
        // Unmapped label passes through.
        assert_eq!(v.map_label("absolutely-not-in-vocab"),
                   vec!["absolutely-not-in-vocab"]);
    }

    #[test]
    fn yaml_parses_string_list_and_null_values() {
        let yaml = r#"
sunset: lighting|sunset
wedding:
  - event|wedding
  - subject|people
abstract: null
unmapped:
"#;
        let v = parse_yaml(yaml).unwrap();
        assert_eq!(v.len(), 4, "expected 4 labels: {:?}", v.labels);
        assert_eq!(v.map_label("sunset"), vec!["lighting|sunset"]);
        assert_eq!(v.map_label("wedding"), vec!["event|wedding", "subject|people"]);
        // null and absent-value both → identity (pass-through).
        assert_eq!(v.map_label("abstract"), vec!["abstract"]);
        assert_eq!(v.map_label("unmapped"), vec!["unmapped"]);
    }

    #[test]
    fn yaml_preserves_key_order() {
        let yaml = "z: tag-z\nm: tag-m\na: tag-a\n";
        let v = parse_yaml(yaml).unwrap();
        assert_eq!(v.labels, vec!["z", "m", "a"]);
    }

    #[test]
    fn apply_identity_when_label_unmapped() {
        let v = Vocabulary::new(vec!["foo".into()], HashMap::new());
        let mapped = v.apply(vec![sugg("foo", 0.8)]);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].tag, "foo");
        assert_eq!(mapped[0].source_label, None);
        assert!((mapped[0].confidence - 0.8).abs() < 1e-6);
    }

    #[test]
    fn apply_one_to_one_records_source_label() {
        let mut map = HashMap::new();
        map.insert("sunset".to_string(), vec!["lighting|sunset".to_string()]);
        let v = Vocabulary::new(vec!["sunset".into()], map);
        let mapped = v.apply(vec![sugg("sunset", 0.93)]);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].tag, "lighting|sunset");
        assert_eq!(mapped[0].source_label, Some("sunset".into()));
    }

    #[test]
    fn apply_one_to_many_fans_out() {
        let mut map = HashMap::new();
        map.insert(
            "wedding".to_string(),
            vec!["event|wedding".into(), "subject|people".into()],
        );
        let v = Vocabulary::new(vec!["wedding".into()], map);
        let mapped = v.apply(vec![sugg("wedding", 0.71)]);
        let tags: Vec<&str> = mapped.iter().map(|m| m.tag.as_str()).collect();
        assert!(tags.contains(&"event|wedding"));
        assert!(tags.contains(&"subject|people"));
        for m in &mapped {
            assert!((m.confidence - 0.71).abs() < 1e-6, "fan-out keeps confidence");
            assert_eq!(m.source_label, Some("wedding".into()));
        }
    }

    /// When two labels both map to the same tag (e.g. "wedding" and
    /// "bride" both → `subject|people`), keep the higher confidence.
    #[test]
    fn apply_dedups_by_max_confidence() {
        let mut map = HashMap::new();
        map.insert("wedding".into(), vec!["event|wedding".into()]);
        map.insert("ceremony".into(), vec!["event|wedding".into()]);
        let v = Vocabulary::new(vec!["wedding".into(), "ceremony".into()], map);
        let mapped = v.apply(vec![sugg("wedding", 0.60), sugg("ceremony", 0.81)]);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].tag, "event|wedding");
        assert!((mapped[0].confidence - 0.81).abs() < 1e-6);
        assert_eq!(mapped[0].source_label, Some("ceremony".into()));
    }

    /// Ordering: highest-confidence first.
    #[test]
    fn apply_sorts_by_confidence_descending() {
        let v = Vocabulary::new(
            vec!["a".into(), "b".into(), "c".into()],
            HashMap::new(),
        );
        let mapped = v.apply(vec![
            sugg("a", 0.40),
            sugg("b", 0.90),
            sugg("c", 0.65),
        ]);
        let confs: Vec<f32> = mapped.iter().map(|m| m.confidence).collect();
        assert!(
            confs.windows(2).all(|w| w[0] >= w[1]),
            "expected descending confidence, got {confs:?}"
        );
    }

    #[test]
    fn txt_load_yields_identity_mapping() {
        let content = "# comment\n  sunset  \n\nwedding\n";
        let v = parse_txt_list(content);
        assert_eq!(v.labels, vec!["sunset", "wedding"]);
        assert_eq!(v.map_label("sunset"), vec!["sunset"]);
    }

    #[test]
    fn yaml_rejects_top_level_list() {
        let yaml = "- foo\n- bar\n";
        let err = parse_yaml(yaml).unwrap_err();
        assert!(format!("{err:#}").contains("top-level mapping"), "{err:#}");
    }
}
