use std::collections::HashMap;

/// Output format for listing commands (search, duplicates, volume list).
#[derive(Debug, Clone)]
pub enum OutputFormat {
    /// Default compact view (command-specific)
    Short,
    /// Full UUID per line, nothing else — for scripting
    Ids,
    /// Detailed multi-field view
    Full,
    /// JSON array
    Json,
    /// Custom template string with `{placeholder}` substitution
    Template(String),
}

/// Parse a `--format` argument string into an `OutputFormat`.
pub fn parse_format(s: &str) -> Result<OutputFormat, String> {
    match s {
        "ids" => Ok(OutputFormat::Ids),
        "short" => Ok(OutputFormat::Short),
        "full" => Ok(OutputFormat::Full),
        "json" => Ok(OutputFormat::Json),
        other => {
            if other.contains('{') {
                Ok(OutputFormat::Template(other.to_string()))
            } else {
                Err(format!(
                    "Unknown format preset '{}'. Use ids, short, full, json, or a template like '{{id}}\\t{{name}}'",
                    other
                ))
            }
        }
    }
}

/// Render a template string by replacing `{key}` placeholders with values from a map.
///
/// Supports escape sequences: `\t` → tab, `\n` → newline.
/// Unknown placeholders are left as-is.
pub fn render_template(template: &str, values: &HashMap<&str, String>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek() {
                Some('t') => {
                    chars.next();
                    result.push('\t');
                }
                Some('n') => {
                    chars.next();
                    result.push('\n');
                }
                Some('\\') => {
                    chars.next();
                    result.push('\\');
                }
                _ => result.push('\\'),
            }
        } else if ch == '{' {
            // Collect placeholder name until '}'
            let mut name = String::new();
            let mut found_close = false;
            for inner in chars.by_ref() {
                if inner == '}' {
                    found_close = true;
                    break;
                }
                name.push(inner);
            }
            if found_close {
                if let Some(value) = values.get(name.as_str()) {
                    result.push_str(value);
                } else {
                    // Unknown placeholder — leave as-is
                    result.push('{');
                    result.push_str(&name);
                    result.push('}');
                }
            } else {
                // Unclosed brace — emit literal
                result.push('{');
                result.push_str(&name);
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Build a template values map for a search result row.
pub fn search_row_values<'a>(
    asset_id: &'a str,
    name: Option<&'a str>,
    original_filename: &'a str,
    asset_type: &'a str,
    format: &'a str,
    created_at: &'a str,
    tags: &'a str,
    description: &'a str,
    content_hash: &'a str,
    label: &'a str,
) -> HashMap<&'a str, String> {
    let short_id = if asset_id.len() >= 8 {
        &asset_id[..8]
    } else {
        asset_id
    };
    let display_name = name.unwrap_or(original_filename);

    let mut m = HashMap::new();
    m.insert("id", asset_id.to_string());
    m.insert("short_id", short_id.to_string());
    m.insert("name", display_name.to_string());
    m.insert("filename", original_filename.to_string());
    m.insert("type", asset_type.to_string());
    m.insert("format", format.to_string());
    m.insert("date", created_at.to_string());
    m.insert("tags", tags.to_string());
    m.insert("description", description.to_string());
    m.insert("hash", content_hash.to_string());
    m.insert("label", label.to_string());
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_format_presets() {
        assert!(matches!(parse_format("ids"), Ok(OutputFormat::Ids)));
        assert!(matches!(parse_format("short"), Ok(OutputFormat::Short)));
        assert!(matches!(parse_format("full"), Ok(OutputFormat::Full)));
        assert!(matches!(parse_format("json"), Ok(OutputFormat::Json)));
    }

    #[test]
    fn parse_format_template() {
        match parse_format("{id}\t{name}") {
            Ok(OutputFormat::Template(t)) => assert_eq!(t, "{id}\t{name}"),
            other => panic!("Expected Template, got {:?}", other),
        }
    }

    #[test]
    fn parse_format_unknown_preset_errors() {
        assert!(parse_format("foobar").is_err());
    }

    #[test]
    fn render_template_basic() {
        let mut values = HashMap::new();
        values.insert("id", "abc-123".to_string());
        values.insert("name", "photo.jpg".to_string());

        let result = render_template("{id} — {name}", &values);
        assert_eq!(result, "abc-123 — photo.jpg");
    }

    #[test]
    fn render_template_escape_sequences() {
        let mut values = HashMap::new();
        values.insert("id", "abc".to_string());
        values.insert("name", "test".to_string());

        let result = render_template("{id}\\t{name}\\n", &values);
        assert_eq!(result, "abc\ttest\n");
    }

    #[test]
    fn render_template_unknown_placeholder_preserved() {
        let values = HashMap::new();
        let result = render_template("{unknown}", &values);
        assert_eq!(result, "{unknown}");
    }

    #[test]
    fn render_template_unclosed_brace() {
        let values = HashMap::new();
        let result = render_template("hello {world", &values);
        assert_eq!(result, "hello {world");
    }

    #[test]
    fn search_row_values_builds_map() {
        let m = search_row_values(
            "12345678-abcd-1234-5678-abcdef012345",
            Some("sunset photo"),
            "sunset.jpg",
            "image",
            "jpg",
            "2024-01-15T10:00:00Z",
            "landscape, nature",
            "A sunset",
            "sha256:abc",
            "Blue",
        );
        assert_eq!(m["short_id"], "12345678");
        assert_eq!(m["name"], "sunset photo");
        assert_eq!(m["filename"], "sunset.jpg");
        assert_eq!(m["tags"], "landscape, nature");
        assert_eq!(m["label"], "Blue");
    }

    #[test]
    fn search_row_values_uses_filename_when_no_name() {
        let m = search_row_values(
            "12345678-abcd",
            None,
            "DSC_001.nef",
            "image",
            "nef",
            "2024-01-15",
            "",
            "",
            "sha256:xyz",
            "",
        );
        assert_eq!(m["name"], "DSC_001.nef");
    }
}
