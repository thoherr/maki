//! `search_builder` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.

use super::*;

impl Catalog {
    // ═══ SEARCH BUILDER ═══

    /// Build the WHERE clause and parameters for search queries.
    /// Returns (where_clause, params, needs_fl_join, needs_v_join).
    /// `needs_v_join`: true when any filter references the `v` (variants) table directly.
    /// `needs_fl_join`: true when any filter references `fl` (file_locations); implies `needs_v_join`.
    /// Generate SQL WHERE clause for a NumericFilter on a given column.
    /// Rating-specific clause builder that treats `rating IS NULL` as equivalent
    /// to `rating = 0`. Users expect `rating:0` to match unrated assets.
    fn rating_clause(
        filter: &NumericFilter,
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    ) {
        // True if the filter matches the value 0.
        let matches_zero = match filter {
            NumericFilter::Exact(v) => *v == 0.0,
            NumericFilter::Min(v) => *v <= 0.0,
            NumericFilter::Range(lo, hi) => *lo <= 0.0 && *hi >= 0.0,
            NumericFilter::Values(vs) => vs.iter().any(|v| *v == 0.0),
            NumericFilter::ValuesOrMin { values, min } => {
                values.iter().any(|v| *v == 0.0) || *min <= 0.0
            }
        };

        if matches_zero {
            // Build the normal clause into a temporary, then wrap in (IS NULL OR <clause>).
            let mut inner_clauses: Vec<String> = Vec::new();
            Self::numeric_clause(filter, "a.rating", &mut inner_clauses, params);
            // numeric_clause always adds exactly one clause.
            if let Some(inner) = inner_clauses.into_iter().next() {
                clauses.push(format!("(a.rating IS NULL OR {inner})"));
            }
        } else {
            Self::numeric_clause(filter, "a.rating", clauses, params);
        }
    }

    fn numeric_clause(
        filter: &NumericFilter,
        column: &str,
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    ) {
        match filter {
            NumericFilter::Exact(v) => {
                clauses.push(format!("{column} = ?"));
                params.push(Box::new(*v));
            }
            NumericFilter::Min(v) => {
                clauses.push(format!("{column} >= ?"));
                params.push(Box::new(*v));
            }
            NumericFilter::Range(lo, hi) => {
                clauses.push(format!("({column} >= ? AND {column} <= ?)"));
                params.push(Box::new(*lo));
                params.push(Box::new(*hi));
            }
            NumericFilter::Values(vals) => {
                let placeholders: Vec<&str> = vals.iter().map(|_| "?").collect();
                clauses.push(format!("{column} IN ({})", placeholders.join(",")));
                for v in vals {
                    params.push(Box::new(*v));
                }
            }
            NumericFilter::ValuesOrMin { values, min } => {
                let placeholders: Vec<&str> = values.iter().map(|_| "?").collect();
                clauses.push(format!(
                    "({column} IN ({}) OR {column} >= ?)",
                    placeholders.join(",")
                ));
                for v in values {
                    params.push(Box::new(*v));
                }
                params.push(Box::new(*min));
            }
        }
    }

    /// Generate SQL WHERE clause for a NumericFilter using a subquery expression.
    fn numeric_clause_expr(
        filter: &NumericFilter,
        expr: &str,
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    ) {
        match filter {
            NumericFilter::Exact(v) => {
                clauses.push(format!("{expr} = ?"));
                params.push(Box::new(*v));
            }
            NumericFilter::Min(v) => {
                clauses.push(format!("{expr} >= ?"));
                params.push(Box::new(*v));
            }
            NumericFilter::Range(lo, hi) => {
                clauses.push(format!("({expr} >= ? AND {expr} <= ?)"));
                params.push(Box::new(*lo));
                params.push(Box::new(*hi));
            }
            NumericFilter::Values(vals) => {
                let mut parts = Vec::new();
                for v in vals {
                    parts.push(format!("{expr} = ?"));
                    params.push(Box::new(*v));
                }
                clauses.push(format!("({})", parts.join(" OR ")));
            }
            NumericFilter::ValuesOrMin { values, min } => {
                let mut parts = Vec::new();
                for v in values {
                    parts.push(format!("{expr} = ?"));
                    params.push(Box::new(*v));
                }
                parts.push(format!("{expr} >= ?"));
                params.push(Box::new(*min));
                clauses.push(format!("({})", parts.join(" OR ")));
            }
        }
    }

    pub(super) fn build_search_where(opts: &SearchOptions) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>, bool, bool) {
        let mut clauses = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut needs_fl_join = opts.volume.is_some() || !opts.volume_ids.is_empty() || !opts.volume_ids_exclude.is_empty();
        let mut needs_v_join = false;

        // --- Asset ID prefix match (supports multiple IDs) ---
        if !opts.asset_ids.is_empty() {
            if opts.asset_ids.len() == 1 {
                clauses.push("a.id LIKE ?".to_string());
                params.push(Box::new(format!("{}%", opts.asset_ids[0])));
            } else {
                let placeholders: Vec<&str> = opts.asset_ids.iter().map(|_| "a.id LIKE ?").collect();
                clauses.push(format!("({})", placeholders.join(" OR ")));
                for id in opts.asset_ids {
                    params.push(Box::new(format!("{id}%")));
                }
            }
        }

        Self::add_text_filters(&mut clauses, &mut params, opts.text, opts.text_exclude);

        // --- Asset type (equality filter on a.asset_type) ---
        Self::add_equality_filter(&mut clauses, &mut params, opts.asset_types, opts.asset_types_exclude, "a.asset_type", &mut false, false);

        // --- Tags (hierarchy-aware LIKE) ---
        // Positive: each entry is ANDed; commas within an entry are ORed
        for tag_entry in opts.tags {
            let values: Vec<&str> = tag_entry.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            if values.len() == 1 {
                Self::add_tag_clause(&mut clauses, &mut params, values[0], false);
            } else {
                // Multiple comma values — OR group
                let mut or_parts = Vec::new();
                for v in &values {
                    or_parts.extend(Self::tag_like_parts(&mut params, v));
                }
                clauses.push(format!("({})", or_parts.join(" OR ")));
            }
        }
        // Negative: each entry is ANDed as NOT; commas within an entry are ORed
        for tag_entry in opts.tags_exclude {
            let values: Vec<&str> = tag_entry.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            let mut or_parts = Vec::new();
            for v in &values {
                or_parts.extend(Self::tag_like_parts(&mut params, v));
            }
            clauses.push(format!("NOT ({})", or_parts.join(" OR ")));
        }

        Self::add_format_filter(&mut clauses, &mut params, opts.formats, opts.formats_exclude, &mut needs_v_join);

        Self::add_volume_filter(&mut clauses, &mut params, opts.volume, opts.volume_ids, opts.volume_ids_exclude);

        // --- Numeric filters (all use unified NumericFilter type) ---
        // Rating is special: an unrated asset has `rating IS NULL`, but users
        // mentally treat "0 stars" and "unrated" as the same thing. We rewrite
        // any rating filter that matches 0 (Exact(0), Values containing 0,
        // Range 0-N, ValuesOrMin with 0) to also match NULL.
        if let Some(ref f) = opts.rating {
            Self::rating_clause(f, &mut clauses, &mut params);
        }

        // --- Color label (equality on a.color_label) ---
        if opts.color_label_none {
            clauses.push("a.color_label IS NULL".to_string());
        }
        Self::add_equality_filter(&mut clauses, &mut params, opts.color_labels, opts.color_labels_exclude, "a.color_label", &mut false, false);

        Self::add_path_filter(&mut clauses, &mut params, opts.path_prefixes, opts.path_prefixes_exclude, &mut needs_fl_join);

        // --- Camera (LIKE on v.camera_model) ---
        Self::add_like_filter(&mut clauses, &mut params, opts.cameras, opts.cameras_exclude, "v.camera_model", &mut needs_v_join);

        // --- Lens (LIKE on v.lens_model) ---
        Self::add_like_filter(&mut clauses, &mut params, opts.lenses, opts.lenses_exclude, "v.lens_model", &mut needs_v_join);

        // --- Description (LIKE on a.description) ---
        // Pure assets-table filter, no JOIN required. Use a throwaway flag.
        let mut _desc_no_join = false;
        Self::add_like_filter(&mut clauses, &mut params, opts.descriptions, opts.descriptions_exclude, "a.description", &mut _desc_no_join);

        // --- Numeric variant filters ---
        if let Some(ref f) = opts.iso { Self::numeric_clause(f, "v.iso", &mut clauses, &mut params); needs_v_join = true; }
        if let Some(ref f) = opts.focal { Self::numeric_clause(f, "v.focal_length_mm", &mut clauses, &mut params); needs_v_join = true; }
        if let Some(ref f) = opts.aperture { Self::numeric_clause(f, "v.f_number", &mut clauses, &mut params); needs_v_join = true; }
        if let Some(ref f) = opts.width { Self::numeric_clause(f, "v.image_width", &mut clauses, &mut params); needs_v_join = true; }
        if let Some(ref f) = opts.height { Self::numeric_clause(f, "v.image_height", &mut clauses, &mut params); needs_v_join = true; }

        // JSON fallback filters (meta:key=value)
        for (key, value) in &opts.meta_filters {
            clauses.push(format!("json_extract(v.source_metadata, '$.{key}') LIKE ?"));
            params.push(Box::new(format!("%{value}%")));
            needs_v_join = true;
        }

        // Location health filters
        Self::add_location_health_filters(&mut clauses, &mut params, opts);

        // Collection filter: restrict to a pre-computed set of asset IDs
        Self::add_id_list_filter(&mut clauses, &mut params, opts.collection_asset_ids, false);
        Self::add_id_list_filter(&mut clauses, &mut params, opts.collection_exclude_ids, true);

        // Copies filter — count DISTINCT volumes where this asset has file
        // locations. This matches the backup-status semantics: copies:1 means
        // "exists on exactly one volume" (at risk), regardless of how many
        // variants or file locations exist on that volume.
        if let Some(ref f) = opts.copies {
            let expr = "(SELECT COUNT(DISTINCT fl2.volume_id) FROM file_locations fl2 \
                 JOIN variants v2 ON fl2.content_hash = v2.content_hash \
                 WHERE v2.asset_id = a.id)";
            Self::numeric_clause_expr(f, expr, &mut clauses, &mut params);
        }

        // Variant count (denormalized column)
        if let Some(ref f) = opts.variant_count { Self::numeric_clause(f, "a.variant_count", &mut clauses, &mut params); }

        // Scattered filter — count distinct session roots for this asset's
        // file locations. Uses the same session root detection as auto-group:
        // the deepest directory component matching [group] session_root_pattern.
        // An asset whose files all live under the same session root (e.g.
        // Capture/, Selects/, Output/ of the same shoot) has scattered:1.
        // An asset with files in different session roots (different shoots)
        // has scattered:2+, indicating a potential mis-grouping.
        if let Some(ref f) = opts.scattered {
            let pattern_escaped = opts.session_root_pattern.replace('\'', "''");
            let expr = format!(
                "(SELECT COUNT(DISTINCT session_root(fl2.relative_path, '{pattern_escaped}')) \
                 FROM file_locations fl2 \
                 JOIN variants v2 ON fl2.content_hash = v2.content_hash \
                 WHERE v2.asset_id = a.id)"
            );
            Self::numeric_clause_expr(f, &expr, &mut clauses, &mut params);
        }

        Self::add_date_filters(&mut clauses, &mut params, opts.date_prefix, opts.date_from, opts.date_until);

        // Stack collapse
        if opts.collapse_stacks {
            clauses.push("(a.stack_id IS NULL OR a.stack_position = 0)".to_string());
        }

        // Stacked filter
        if let Some(stacked) = opts.stacked_filter {
            if stacked {
                clauses.push("a.stack_id IS NOT NULL".to_string());
            } else {
                clauses.push("a.stack_id IS NULL".to_string());
            }
        }

        Self::add_geo_filters(&mut clauses, &mut params, opts.geo_bbox, opts.has_gps);

        // Face filters (use denormalized face_count column)
        if let Some(has_faces) = opts.has_faces {
            if has_faces {
                clauses.push("a.face_count > 0".to_string());
            } else {
                clauses.push("a.face_count = 0".to_string());
            }
        }
        if let Some(ref f) = opts.face_count { Self::numeric_clause(f, "a.face_count", &mut clauses, &mut params); }

        // tagcount: — number of intentional (leaf) tags per asset. Denormalised
        // into assets.leaf_tag_count at write time (schema v8); query is a
        // direct column comparison rather than a JSON-each subquery so it
        // stays cheap on large catalogues.
        if let Some(ref f) = opts.tag_count { Self::numeric_clause(f, "a.leaf_tag_count", &mut clauses, &mut params); }
        if let Some(ref f) = opts.duration { Self::numeric_clause(f, "a.video_duration", &mut clauses, &mut params); }
        if let Some(ref c) = opts.codec {
            clauses.push("a.video_codec LIKE ?".to_string());
            params.push(Box::new(format!("%{c}%")));
        }

        // Embedding presence filter
        if let Some(has_embed) = opts.has_embed {
            if has_embed {
                clauses.push(
                    "EXISTS (SELECT 1 FROM embeddings e WHERE e.asset_id = a.id)".to_string(),
                );
            } else {
                clauses.push(
                    "NOT EXISTS (SELECT 1 FROM embeddings e WHERE e.asset_id = a.id)".to_string(),
                );
            }
        }

        // Pre-computed asset ID filters — all use the same IN/NOT IN pattern
        Self::add_id_list_filter(&mut clauses, &mut params, opts.person_asset_ids, false);
        Self::add_id_list_filter(&mut clauses, &mut params, opts.person_exclude_ids, true);
        Self::add_id_list_filter(&mut clauses, &mut params, opts.similar_asset_ids, false);
        Self::add_id_list_filter(&mut clauses, &mut params, opts.text_search_ids, false);

        let where_clause = if clauses.is_empty() {
            " WHERE 1=1".to_string()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };

        // fl join implies v join (fl joins through v)
        if needs_fl_join {
            needs_v_join = true;
        }

        (where_clause, params, needs_fl_join, needs_v_join)
    }

    /// Helper: generate tag LIKE clause parts for a single tag value.
    /// Returns a Vec of SQL expressions (each with params already pushed).
    ///
    /// Build SQL clauses for a `tag:` filter value.
    ///
    /// Prefix markers (any order, all stackable):
    /// - `=` — whole-path match: tag matches if and only if the **full path**
    ///   equals the given value. `tag:=Legoland` matches ONLY the standalone
    ///   `Legoland` tag, never `location|Denmark|Legoland`. Use this when a
    ///   root-level tag shares a name with a leaf elsewhere in the hierarchy
    ///   and you need to disambiguate. The `=` reads naturally as "equals",
    ///   matching the user's mental model of exact equality. Works with any
    ///   depth: `tag:=location|Denmark|Legoland` matches only that exact path.
    /// - `/` — leaf only at any level: tag matches at any hierarchy level
    ///   but only as a leaf (no descendants in the same branch).
    ///   `tag:/location|Germany|Bayern` matches assets whose deepest tag in
    ///   this branch is `Bayern` — NOT assets that also have
    ///   `location|Germany|Bayern|München`. Niche use case for distinguishing
    ///   "this is the deepest tagged level" from "this is a parent."
    /// - `^` — case-sensitive (SQLite GLOB instead of LIKE)
    /// - `|` — anchored prefix: match any tag whose hierarchy component STARTS
    ///   with the rest of the value, at any level. Mutually exclusive with `=`
    ///   and `/` (a prefix-anchor can't also be an exact or leaf-only match).
    ///   Examples: `tag:|wed` matches `wedding`, `wedding-2024`, `events|wedding`,
    ///   `events|wedding|2024-05`. `tag:^|Wed` matches the same set
    ///   case-sensitively.
    ///
    /// Without any markers, both exact and descendant matches are generated,
    /// case-insensitively (the SQLite LIKE default for ASCII).
    ///
    /// Tags containing `"` may be stored in JSON two ways:
    /// - Unescaped: `"\"Sir\" Oliver Mally"` (serde_json proper)
    /// - Raw: `""Sir" Oliver Mally"` (legacy/malformed JSON)
    /// We match both forms.
    ///
    /// Note: prior to v4.4.4 the `=` and `/` markers were swapped — `=` meant
    /// leaf-only-at-any-level and `/` meant whole-path. The swap was made
    /// because `=` reads as "equals", which most users expect to mean exact
    /// value equality.
    fn tag_like_parts(params: &mut Vec<Box<dyn rusqlite::types::ToSql>>, tag: &str) -> Vec<String> {
        // Strip the `=`, `/`, `^`, and `|` prefix markers in any order.
        // `=` → path_exact (whole-path equality). `/` → exact_only (leaf-only at any level).
        let mut rest = tag;
        let mut exact_only = false;
        let mut path_exact = false;
        let mut case_sensitive = false;
        let mut prefix_anchor = false;
        loop {
            if let Some(s) = rest.strip_prefix('=') { path_exact = true; rest = s; }
            else if let Some(s) = rest.strip_prefix('/') { exact_only = true; rest = s; }
            else if let Some(s) = rest.strip_prefix('^') { case_sensitive = true; rest = s; }
            else if let Some(s) = rest.strip_prefix('|') { prefix_anchor = true; rest = s; }
            else { break; }
        }
        // Conflict resolution:
        // - Prefix-anchor (`|`) conceptually includes descendants and matches
        //   by component prefix, so it can't combine with `=` or `/`; when
        //   paired, the prefix-anchor semantic wins and the other flags are
        //   silently dropped.
        // - `=` and `/` combine: `=` constrains the value to a whole-path
        //   match, `/` adds a no-descendants constraint. The combined form
        //   matches "this exact path AND no descendants on the asset" — the
        //   semantic the tags-page leaf-count link needs to make its count
        //   align with the row's leaf number.
        if prefix_anchor { exact_only = false; path_exact = false; }
        let tag_value = rest;
        let stored = crate::tag_util::tag_input_to_storage(tag_value);
        let mut exprs = Vec::new();

        // Helper: build the wildcard pattern for either LIKE (%..%) or GLOB (*..*).
        // GLOB is case-sensitive; LIKE is case-insensitive for ASCII. Tag values
        // almost never contain `*` or `?`, but if they do, GLOB would treat them
        // as wildcards — this is a documented edge case for case-sensitive search.
        let op = if case_sensitive { "GLOB" } else { "LIKE" };
        let wild = if case_sensitive { "*" } else { "%" };
        let pat = |middle: &str| -> String { format!("{wild}{middle}{wild}") };
        // For the "not-descendant" clause we need a trailing `|` before the wild,
        // so the pattern is `<wild>"tag|<wild>` (matches any descendant).
        let desc_pat = |stored: &str| -> String { format!("{wild}\"{stored}|{wild}") };

        if prefix_anchor {
            // Match any component starting with `stored`. In JSON, a tag
            // component starts either right after a `"` (root) or right after
            // a `|` (descendant level). Two patterns cover both cases.
            params.push(Box::new(pat(&format!("\"{stored}"))));
            exprs.push(format!("a.tags {op} ?"));
            params.push(Box::new(pat(&format!("|{stored}"))));
            exprs.push(format!("a.tags {op} ?"));
            // Don't bother with the legacy "input form differs from stored"
            // path: prefix-anchor mode is a power-user shortcut, the user
            // should use the storage form (`|`) directly.
            return exprs;
        }

        if path_exact {
            // Whole-path match: the full tag value equals `stored`, bounded
            // by the JSON quotes. Matches nothing else — no level sliding,
            // no leaf-of-hierarchy variants. Descendants ARE allowed on the
            // asset unless `/` is also given (see combined branch below).
            //
            // Use case: disambiguate a root-level tag from same-named leaves
            // elsewhere in the hierarchy. `tag:=Legoland` matches only the
            // standalone "Legoland" tag, not "location|Denmark|Legoland" or
            // "location|Germany|Legoland".
            //
            // Combined `=/`: whole-path AND no descendants. Used by the
            // tags-page "(N as leaf)" link so click count == row leaf count.
            // The negative LIKE clauses cover both descendant positions:
            //   - `"stored|…`  (the path is at root and has children)
            //   - `|stored|…`  (the path is mid-tree and has children)
            let positives_start = exprs.len();
            params.push(Box::new(pat(&format!("\"{stored}\""))));
            exprs.push(format!("a.tags {op} ?"));
            // Input-form fallback (e.g. user typed `>` for hierarchy and the
            // stored form has `|`).
            if tag_value != stored {
                params.push(Box::new(pat(&format!("\"{tag_value}\""))));
                exprs.push(format!("a.tags {op} ?"));
            }
            // JSON-escape variant for tags containing `"`.
            if tag_value.contains('"') {
                let json_escaped = tag_value.replace('"', "\\\"");
                params.push(Box::new(pat(&format!("\"{json_escaped}\""))));
                exprs.push(format!("a.tags {op} ?"));
            }
            if exact_only {
                // Wrap the OR'd positives and append AND-NOT descendant guards.
                let positives = exprs.split_off(positives_start);
                let or_block = positives.join(" OR ");
                params.push(Box::new(desc_pat(&stored)));
                params.push(Box::new(format!("{wild}|{stored}|{wild}")));
                exprs.push(format!(
                    "(({or_block}) AND a.tags NOT {op} ? AND a.tags NOT {op} ?)"
                ));
            }
            return exprs;
        }

        if exact_only {
            // Exact/leaf match: the tag exists on the asset at any level in
            // the hierarchy BUT is never followed by `|child`.
            //
            // Positive (OR): tag appears as…
            //   1. standalone: "stored"  (root-level tag)
            //   2. leaf child: |stored"  (end of a hierarchy path)
            // Negative (AND NOT): no descendants exist…
            //   1. NOT "stored|…  (no descendants from root)
            //   2. NOT |stored|…  (no descendants from mid-path)
            params.push(Box::new(pat(&format!("\"{stored}\""))));
            params.push(Box::new(pat(&format!("|{stored}\""))));
            params.push(Box::new(desc_pat(&stored)));
            let mid_desc_pat = format!("{wild}|{stored}|{wild}");
            params.push(Box::new(mid_desc_pat));
            exprs.push(format!("((a.tags {op} ? OR a.tags {op} ?) AND a.tags NOT {op} ? AND a.tags NOT {op} ?)"));
        } else {
            // Default: match the tag at any level in the hierarchy, with or
            // without descendants.
            //   1. "stored"  — standalone (root-level exact)
            //   2. "stored|  — parent from root (has descendants)
            //   3. |stored"  — leaf child (end of a hierarchy path)
            //   4. |stored|  — mid-path component (has descendants, is a child)
            params.push(Box::new(pat(&format!("\"{stored}\""))));
            exprs.push(format!("a.tags {op} ?"));
            params.push(Box::new(desc_pat(&stored)));
            exprs.push(format!("a.tags {op} ?"));
            params.push(Box::new(pat(&format!("|{stored}\""))));
            exprs.push(format!("a.tags {op} ?"));
            let mid_child_pat = format!("{wild}|{stored}|{wild}");
            params.push(Box::new(mid_child_pat));
            exprs.push(format!("a.tags {op} ?"));
        }

        // If stored form differs from input, also match input form
        if tag_value != stored {
            params.push(Box::new(pat(&format!("\"{tag_value}\""))));
            exprs.push(format!("a.tags {op} ?"));
        }

        // If tag contains ", also match JSON-escaped form (\" in stored JSON)
        if tag_value.contains('"') {
            let json_escaped = tag_value.replace('"', "\\\"");
            params.push(Box::new(pat(&format!("\"{json_escaped}\""))));
            exprs.push(format!("a.tags {op} ?"));
        }

        exprs
    }

    /// Helper: add a single positive tag clause (AND).
    fn add_tag_clause(clauses: &mut Vec<String>, params: &mut Vec<Box<dyn rusqlite::types::ToSql>>, tag: &str, negate: bool) {
        let parts = Self::tag_like_parts(params, tag);
        let inner = parts.join(" OR ");
        if negate {
            clauses.push(format!("NOT ({inner})"));
        } else {
            clauses.push(format!("({inner})"));
        }
    }

    /// Helper: add equality filter with IN/NOT IN for comma-OR and negation.
    /// Uses IFNULL for NOT conditions to handle nullable columns correctly
    /// (NULL != 'x' returns NULL, which is falsy — we want NULL to survive exclusion).
    fn add_equality_filter(
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        entries: &[String],
        exclude_entries: &[String],
        column: &str,
        _needs_join: &mut bool,
        _is_join_col: bool,
    ) {
        // Case-insensitive equality via COLLATE NOCASE. This handles both
        // asset_type (stored lowercase) and color_label (stored capitalized
        // like "Red"/"Blue") without having to know the canonical case per
        // column. The user can type any casing in the query.
        let include: Vec<&str> = entries.iter()
            .flat_map(|e| e.split(',').map(|s| s.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        if include.len() == 1 {
            clauses.push(format!("{column} = ? COLLATE NOCASE"));
            params.push(Box::new(include[0].to_string()));
        } else if include.len() > 1 {
            let placeholders: Vec<&str> = include.iter().map(|_| "?").collect();
            clauses.push(format!("{column} COLLATE NOCASE IN ({})", placeholders.join(",")));
            for v in &include {
                params.push(Box::new(v.to_string()));
            }
        }
        let exclude: Vec<&str> = exclude_entries.iter()
            .flat_map(|e| e.split(',').map(|s| s.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        if exclude.len() == 1 {
            clauses.push(format!("({column} IS NULL OR {column} != ? COLLATE NOCASE)"));
            params.push(Box::new(exclude[0].to_string()));
        } else if exclude.len() > 1 {
            let placeholders: Vec<&str> = exclude.iter().map(|_| "?").collect();
            clauses.push(format!("({column} IS NULL OR {column} COLLATE NOCASE NOT IN ({}))", placeholders.join(",")));
            for v in &exclude {
                params.push(Box::new(v.to_string()));
            }
        }
    }

    /// Helper: add orphan/stale/missing/no-online-locations clauses.
    fn add_location_health_filters(
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        opts: &SearchOptions,
    ) {
        if opts.orphan {
            clauses.push(
                "NOT EXISTS (SELECT 1 FROM file_locations fl2 JOIN variants v2 ON fl2.content_hash = v2.content_hash WHERE v2.asset_id = a.id)"
                    .to_string(),
            );
        }
        if opts.orphan_false {
            clauses.push(
                "EXISTS (SELECT 1 FROM file_locations fl2 JOIN variants v2 ON fl2.content_hash = v2.content_hash WHERE v2.asset_id = a.id)"
                    .to_string(),
            );
        }
        if let Some(ref f) = opts.stale_days {
            let days = match f {
                NumericFilter::Exact(v) | NumericFilter::Min(v) => *v as u64,
                NumericFilter::Range(v, _) => *v as u64,
                NumericFilter::Values(v) => v.first().copied().unwrap_or(30.0) as u64,
                NumericFilter::ValuesOrMin { min, .. } => *min as u64,
            };
            clauses.push(format!(
                "EXISTS (SELECT 1 FROM file_locations fl2 \
                 JOIN variants v2 ON fl2.content_hash = v2.content_hash \
                 WHERE v2.asset_id = a.id AND \
                 (fl2.verified_at IS NULL OR fl2.verified_at < datetime('now', '-{} days')))",
                days
            ));
        }
        Self::add_id_list_filter(clauses, params, opts.missing_asset_ids, false);
        if let Some(online_ids) = opts.no_online_locations {
            if !online_ids.is_empty() {
                let placeholders: Vec<&str> = online_ids.iter().map(|_| "?").collect();
                clauses.push(format!(
                    "NOT EXISTS (SELECT 1 FROM file_locations fl2 \
                     JOIN variants v2 ON fl2.content_hash = v2.content_hash \
                     WHERE v2.asset_id = a.id AND fl2.volume_id IN ({}))",
                    placeholders.join(",")
                ));
                for id in online_ids {
                    params.push(Box::new(id.clone()));
                }
            }
        }
    }

    /// Helper: add an `a.id IN (...)` or `a.id NOT IN (...)` clause from a
    /// pre-computed list of asset IDs. Used by collection, person, similar,
    /// and text-search filters — all pre-resolve to an ID list before the
    /// search query runs.
    ///
    /// When `exclude` is false: empty list → `0` (no matches); None → no clause.
    /// When `exclude` is true: empty list → no clause; None → no clause.
    fn add_id_list_filter(
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        ids: Option<&[String]>,
        exclude: bool,
    ) {
        let ids = match ids {
            Some(ids) => ids,
            None => return,
        };
        if exclude {
            if ids.is_empty() { return; }
            let placeholders: Vec<&str> = ids.iter().map(|_| "?").collect();
            clauses.push(format!("a.id NOT IN ({})", placeholders.join(",")));
        } else {
            if ids.is_empty() {
                clauses.push("0".to_string());
                return;
            }
            let placeholders: Vec<&str> = ids.iter().map(|_| "?").collect();
            clauses.push(format!("a.id IN ({})", placeholders.join(",")));
        }
        for id in ids {
            params.push(Box::new(id.clone()));
        }
    }

    /// Helper: add LIKE filter with OR groups for comma-separated values.
    /// Uses `IS NULL OR NOT LIKE` for exclusions to handle nullable columns.
    fn add_like_filter(
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        entries: &[String],
        exclude_entries: &[String],
        column: &str,
        needs_join: &mut bool,
    ) {
        let include: Vec<&str> = entries.iter()
            .flat_map(|e| e.split(',').map(|s| s.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        let exclude: Vec<&str> = exclude_entries.iter()
            .flat_map(|e| e.split(',').map(|s| s.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        if !include.is_empty() || !exclude.is_empty() {
            *needs_join = true;
        }
        if include.len() == 1 {
            clauses.push(format!("{column} LIKE ?"));
            params.push(Box::new(format!("%{}%", include[0])));
        } else if include.len() > 1 {
            let mut or_parts = Vec::new();
            for v in &include {
                or_parts.push(format!("{column} LIKE ?"));
                params.push(Box::new(format!("%{v}%")));
            }
            clauses.push(format!("({})", or_parts.join(" OR ")));
        }
        for v in &exclude {
            clauses.push(format!("({column} IS NULL OR {column} NOT LIKE ?)"));
            params.push(Box::new(format!("%{v}%")));
        }
    }

    /// Free-text filter on `name`, `original_filename`, `description`,
    /// `source_metadata`. Negative terms wrap the same disjunction in `NOT (...)`
    /// with `IFNULL` to coalesce nullable columns (NULL LIKE returns NULL,
    /// which would make NOT(...) undefined).
    fn add_text_filters(
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        text: Option<&str>,
        text_exclude: &[String],
    ) {
        if let Some(text) = text {
            if !text.is_empty() {
                clauses.push(
                    "(a.name LIKE ? OR bv.original_filename LIKE ? OR a.description LIKE ? OR bv.source_metadata LIKE ?)".to_string(),
                );
                let pattern = format!("%{text}%");
                params.push(Box::new(pattern.clone()));
                params.push(Box::new(pattern.clone()));
                params.push(Box::new(pattern.clone()));
                params.push(Box::new(pattern));
            }
        }
        for term in text_exclude {
            clauses.push(
                "NOT (IFNULL(a.name,'') LIKE ? OR bv.original_filename LIKE ? OR IFNULL(a.description,'') LIKE ? OR bv.source_metadata LIKE ?)".to_string(),
            );
            let pattern = format!("%{term}%");
            params.push(Box::new(pattern.clone()));
            params.push(Box::new(pattern.clone()));
            params.push(Box::new(pattern.clone()));
            params.push(Box::new(pattern));
        }
    }

    /// Equality filter on `v.format` with comma-split include/exclude lists.
    /// Sets `needs_v_join` since the column is on the variants table.
    fn add_format_filter(
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        formats: &[String],
        formats_exclude: &[String],
        needs_v_join: &mut bool,
    ) {
        let include: Vec<&str> = formats.iter()
            .flat_map(|e| e.split(',').map(|s| s.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        let exclude: Vec<&str> = formats_exclude.iter()
            .flat_map(|e| e.split(',').map(|s| s.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        if !include.is_empty() || !exclude.is_empty() {
            *needs_v_join = true;
        }
        if include.len() == 1 {
            clauses.push("v.format = ?".to_string());
            params.push(Box::new(include[0].to_lowercase()));
        } else if include.len() > 1 {
            let placeholders: Vec<&str> = include.iter().map(|_| "?").collect();
            clauses.push(format!("v.format IN ({})", placeholders.join(",")));
            for v in &include {
                params.push(Box::new(v.to_lowercase()));
            }
        }
        if exclude.len() == 1 {
            clauses.push("v.format != ?".to_string());
            params.push(Box::new(exclude[0].to_lowercase()));
        } else if exclude.len() > 1 {
            let placeholders: Vec<&str> = exclude.iter().map(|_| "?").collect();
            clauses.push(format!("v.format NOT IN ({})", placeholders.join(",")));
            for v in &exclude {
                params.push(Box::new(v.to_lowercase()));
            }
        }
    }

    /// Volume filter: equality (`opts.volume`), inclusion list (`opts.volume_ids`),
    /// and exclusion (`opts.volume_ids_exclude` — exclude assets with ANY
    /// location on those volumes via correlated subquery).
    fn add_volume_filter(
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        volume: Option<&str>,
        volume_ids: &[String],
        volume_ids_exclude: &[String],
    ) {
        if let Some(volume) = volume {
            if !volume.is_empty() {
                clauses.push("fl.volume_id = ?".to_string());
                params.push(Box::new(volume.to_string()));
            }
        }
        if !volume_ids.is_empty() {
            let placeholders: Vec<String> = volume_ids.iter().map(|_| "?".to_string()).collect();
            clauses.push(format!("fl.volume_id IN ({})", placeholders.join(",")));
            for vid in volume_ids {
                params.push(Box::new(vid.clone()));
            }
        }
        if !volume_ids_exclude.is_empty() {
            let placeholders: Vec<String> = volume_ids_exclude.iter().map(|_| "?".to_string()).collect();
            clauses.push(format!(
                "a.id NOT IN (SELECT DISTINCT v2.asset_id FROM variants v2 \
                 JOIN file_locations fl2 ON fl2.content_hash = v2.content_hash \
                 WHERE fl2.volume_id IN ({}))",
                placeholders.join(",")
            ));
            for vid in volume_ids_exclude {
                params.push(Box::new(vid.clone()));
            }
        }
    }

    /// Path-prefix LIKE filter on `fl.relative_path`. Supports `*` as a
    /// wildcard (mapped to `%`); literal `%`/`_` are escaped via `ESCAPE '\'`.
    /// A trailing `%` is appended automatically so `path:Pictures/2026` keeps
    /// prefix semantics. Sets `needs_fl_join`.
    fn add_path_filter(
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        path_prefixes: &[String],
        path_prefixes_exclude: &[String],
        needs_fl_join: &mut bool,
    ) {
        let include: Vec<&str> = path_prefixes.iter()
            .flat_map(|e| e.split(',').map(|s| s.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        let exclude: Vec<&str> = path_prefixes_exclude.iter()
            .flat_map(|e| e.split(',').map(|s| s.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        if !include.is_empty() || !exclude.is_empty() {
            *needs_fl_join = true;
        }
        if include.len() == 1 {
            clauses.push("fl.relative_path LIKE ? ESCAPE '\\'".to_string());
            params.push(Box::new(path_pattern_to_like(include[0])));
        } else if include.len() > 1 {
            let mut or_parts = Vec::new();
            for v in &include {
                or_parts.push("fl.relative_path LIKE ? ESCAPE '\\'".to_string());
                params.push(Box::new(path_pattern_to_like(v)));
            }
            clauses.push(format!("({})", or_parts.join(" OR ")));
        }
        for v in &exclude {
            clauses.push("fl.relative_path NOT LIKE ? ESCAPE '\\'".to_string());
            params.push(Box::new(path_pattern_to_like(v)));
        }
    }

    /// Date range filters on `a.created_at`: prefix (LIKE `YYYY-MM%`),
    /// from (>=), until (<, exclusive — `next_date_bound` rolls forward).
    fn add_date_filters(
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        date_prefix: Option<&str>,
        date_from: Option<&str>,
        date_until: Option<&str>,
    ) {
        if let Some(prefix) = date_prefix {
            if !prefix.is_empty() {
                clauses.push("a.created_at LIKE ?".to_string());
                params.push(Box::new(format!("{prefix}%")));
            }
        }
        if let Some(from) = date_from {
            if !from.is_empty() {
                clauses.push("a.created_at >= ?".to_string());
                params.push(Box::new(from.to_string()));
            }
        }
        if let Some(until) = date_until {
            if !until.is_empty() {
                let exclusive = next_date_bound(until);
                clauses.push("a.created_at < ?".to_string());
                params.push(Box::new(exclusive));
            }
        }
    }

    /// GPS-related filters: bounding-box constraint and presence/absence of
    /// any latitude/longitude. Both columns live on `a` so no extra join.
    fn add_geo_filters(
        clauses: &mut Vec<String>,
        params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        geo_bbox: Option<(f64, f64, f64, f64)>,
        has_gps: Option<bool>,
    ) {
        if let Some((south, west, north, east)) = geo_bbox {
            clauses.push("a.latitude >= ? AND a.latitude <= ? AND a.longitude >= ? AND a.longitude <= ?".to_string());
            params.push(Box::new(south));
            params.push(Box::new(north));
            params.push(Box::new(west));
            params.push(Box::new(east));
        }
        if let Some(has_gps) = has_gps {
            if has_gps {
                clauses.push("a.latitude IS NOT NULL AND a.longitude IS NOT NULL".to_string());
            } else {
                clauses.push("(a.latitude IS NULL OR a.longitude IS NULL)".to_string());
            }
        }
    }

}
