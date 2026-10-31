//! `tags` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.

use super::*;

impl Catalog {
    // ═══ TAG & FORMAT QUERIES ═══

    /// List all unique tags with their usage counts, sorted by count descending.
    pub fn list_all_tags(&self) -> Result<Vec<(String, u64)>> {
        // Use json_each() for SQL-side aggregation — avoids loading all 150k+ tag JSON blobs
        let mut stmt = self.conn.prepare(
            "SELECT je.value, COUNT(*) as cnt \
             FROM assets, json_each(assets.tags) AS je \
             WHERE assets.tags != '[]' \
             GROUP BY je.value \
             ORDER BY cnt DESC, je.value ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// For each tag value present in the catalogue, count assets where that
    /// tag is a *leaf* — i.e. the asset has the tag but no other tag of the
    /// form `<tag>|...` on the same asset.
    ///
    /// Per-row count matches the browse chip's `tag:=^/<full-path>` filter
    /// (case-sensitive whole-path leaf-only). The tags-page row link for
    /// "(N as leaf)" uses exactly that filter, so the displayed count and
    /// the click result population are always equal.
    ///
    /// For a parent node like `location`, leaf-count equals "assets tagged
    /// at exactly that level with no deeper child" — typically a small
    /// number that surfaces lazily-tagged assets (parent-tagged but not
    /// specialised into a child).
    ///
    /// **Implementation**: streams `(asset_id, tag)` pairs from
    /// `json_each(a.tags)` ordered by `(asset_id, tag)`, accumulates each
    /// asset's tags into a small `Vec<String>` in Rust, then linearly
    /// scans the (already-lex-sorted) vector — a tag `T` is a leaf iff
    /// the next tag in the vector doesn't start with `T|`. Equivalent
    /// semantics to the pure-SQL variant (which used a NOT EXISTS
    /// subquery per (asset, tag) pair against `json_each` again) at a
    /// fraction of the cost: the SQL version was O(N × M²) over a
    /// virtual table SQLite can't index, ~1 s on a 90k-asset catalog.
    /// Linear streaming brings it to ~150 ms by trading an unindex'able
    /// nested join for a single ordered pass plus cheap in-Rust work.
    /// The case-sensitivity behaviour is preserved (Rust's
    /// `str::starts_with` is byte-exact; matches the SQL version's
    /// `GLOB` rather than `LIKE`).
    pub fn list_leaf_tag_counts(&self) -> Result<std::collections::HashMap<String, u64>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, je.value \
             FROM assets a, json_each(a.tags) AS je \
             WHERE a.tags != '[]' \
             ORDER BY a.id, je.value",
        )?;
        let mut rows = stmt.query([])?;

        let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        // Tags accumulated for the current asset. SQL's ORDER BY hands
        // them to us already lex-sorted, so within `current_tags` a
        // descendant `T|...` always immediately follows its closest
        // ancestor `T` — we can determine leaf-ness in one linear scan.
        let mut current_asset: Option<String> = None;
        let mut current_tags: Vec<String> = Vec::new();

        let flush = |counts: &mut std::collections::HashMap<String, u64>,
                     tags: &Vec<String>| {
            for (i, t) in tags.iter().enumerate() {
                // A tag is a leaf iff no later tag in the (sorted) list
                // starts with "<tag>|". Because the list is sorted and
                // any descendant `T|...` is lexicographically just
                // after `T` (separated only by tags that share T's
                // prefix), it's enough to check the IMMEDIATELY next
                // tag — if that doesn't start with `T|`, no later tag
                // can either. (Counter-example check: with tags
                // ["a", "ab", "a|x"], sort yields ["a", "a|x", "ab"];
                // for "a" we see next="a|x" which starts with "a|",
                // correctly classifying "a" as not-a-leaf.)
                let prefix_marker = format!("{t}|");
                let is_leaf = match tags.get(i + 1) {
                    Some(next) => !next.starts_with(&prefix_marker),
                    None => true,
                };
                if is_leaf {
                    *counts.entry(t.clone()).or_insert(0) += 1;
                }
            }
        };

        while let Some(row) = rows.next()? {
            let asset_id: String = row.get(0)?;
            let tag: String = row.get(1)?;
            if current_asset.as_ref() != Some(&asset_id) {
                if !current_tags.is_empty() {
                    flush(&mut counts, &current_tags);
                    current_tags.clear();
                }
                current_asset = Some(asset_id);
            }
            current_tags.push(tag);
        }
        if !current_tags.is_empty() {
            flush(&mut counts, &current_tags);
        }

        Ok(counts)
    }

    /// Find assets with a specific exact tag, returning (asset_id, stack_id) pairs.
    /// Ordered by created_at ASC so the oldest asset comes first.
    pub fn assets_with_exact_tag(&self, tag: &str) -> Result<Vec<(String, Option<String>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.stack_id \
             FROM assets a, json_each(a.tags) AS je \
             WHERE je.value = ?1 COLLATE NOCASE \
             ORDER BY a.created_at ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tag], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find assets that have a tag matching exactly or starting with `tag|` (prefix match).
    /// Used by tag rename to cascade renames to descendant tags.
    /// Find assets whose tag set contains `tag`, optionally including descendants
    /// (`tag|child`) and optionally case-sensitive.
    ///
    /// - `case_sensitive = false` (default): uses `COLLATE NOCASE` for the equality
    ///   check and `LIKE … COLLATE NOCASE` for the prefix check.
    /// - `case_sensitive = true`: uses byte-exact equality and `GLOB` for the
    ///   prefix check (GLOB is case-sensitive in SQLite).
    /// - `exact_only = true`: skips the descendant prefix check, returning only
    ///   assets tagged at exactly this level.
    pub fn assets_with_tag_or_prefix(
        &self,
        tag: &str,
        case_sensitive: bool,
        exact_only: bool,
    ) -> Result<Vec<(String, Option<String>)>> {
        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match (case_sensitive, exact_only) {
            (false, false) => {
                // Default: case-insensitive, include descendants
                (
                    "SELECT DISTINCT a.id, a.stack_id \
                     FROM assets a, json_each(a.tags) AS je \
                     WHERE je.value = ?1 COLLATE NOCASE \
                        OR je.value LIKE ?2 COLLATE NOCASE \
                     ORDER BY a.created_at ASC".to_string(),
                    vec![Box::new(tag.to_string()), Box::new(format!("{}|%", tag))],
                )
            }
            (false, true) => {
                // Case-insensitive, exact level only (no descendants)
                (
                    "SELECT DISTINCT a.id, a.stack_id \
                     FROM assets a, json_each(a.tags) AS je \
                     WHERE je.value = ?1 COLLATE NOCASE \
                     ORDER BY a.created_at ASC".to_string(),
                    vec![Box::new(tag.to_string())],
                )
            }
            (true, false) => {
                // Case-sensitive, include descendants. GLOB uses `*` as wildcard.
                (
                    "SELECT DISTINCT a.id, a.stack_id \
                     FROM assets a, json_each(a.tags) AS je \
                     WHERE je.value = ?1 \
                        OR je.value GLOB ?2 \
                     ORDER BY a.created_at ASC".to_string(),
                    vec![Box::new(tag.to_string()), Box::new(format!("{}|*", tag))],
                )
            }
            (true, true) => {
                // Case-sensitive, exact level only
                (
                    "SELECT DISTINCT a.id, a.stack_id \
                     FROM assets a, json_each(a.tags) AS je \
                     WHERE je.value = ?1 \
                     ORDER BY a.created_at ASC".to_string(),
                    vec![Box::new(tag.to_string())],
                )
            }
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// List all distinct variant formats.
    pub fn list_all_formats(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT format FROM variants ORDER BY format",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// List all variant formats with their counts (for grouped format filter).
    pub fn list_all_format_counts(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT format, COUNT(*) as cnt FROM variants GROUP BY format ORDER BY cnt DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

}
