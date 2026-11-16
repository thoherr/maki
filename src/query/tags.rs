//! Tag mutations of `QueryEngine`: single-asset add/remove (`tag`,
//! `tag_with_source`, `batch_tag`) and the catalog-wide bulk operations
//! `tag_rename`, `tag_split`, `tag_delete`, `tag_fix_unicode`.

use anyhow::Result;

use super::context::Mutation;
use super::{
    MutationContext, QueryEngine, TagDeleteAction, TagDeleteResult, TagFixUnicodeResult,
    TagRenameAction, TagRenameResult, TagResult, TagSplitAction, TagSplitResult,
};

impl QueryEngine {
    // ═══ TAG OPERATIONS ═══

    /// Add or remove tags on an asset. Updates both sidecar YAML and SQLite catalog.
    pub fn tag(&self, asset_id_prefix: &str, tags: &[String], remove: bool) -> Result<TagResult> {
        self.tag_with_source(asset_id_prefix, tags, remove, crate::models::TagSource::User)
    }

    /// Like [`Self::tag`], but records the given provenance source for
    /// genuinely-new tags. Machine pipelines (auto-tag, VLM describe)
    /// route through this so their tags are distinguishable from human
    /// curation. `source` is ignored on remove.
    pub fn tag_with_source(
        &self,
        asset_id_prefix: &str,
        tags: &[String],
        remove: bool,
        source: crate::models::TagSource,
    ) -> Result<TagResult> {
        let ctx = self.recording_context()?;
        let result = self.tag_inner(&ctx, asset_id_prefix, tags, remove, source);
        // One operation boundary per public call. Summarise from the
        // result so the journal carries a human label even on partial
        // success; drain the recorder regardless of Ok/Err.
        let summary = match &result {
            Ok(r) if remove => format!("removed {} tag(s)", r.changed.len()),
            Ok(r) => format!("added {} tag(s)", r.changed.len()),
            Err(_) => if remove { "removed tag(s)".to_string() } else { "added tag(s)".to_string() },
        };
        self.commit_recorder(&ctx, "tag", &summary);
        result
    }

    pub(super) fn tag_inner(
        &self,
        ctx: &MutationContext<'_>,
        asset_id_prefix: &str,
        tags: &[String],
        remove: bool,
        source: crate::models::TagSource,
    ) -> Result<TagResult> {
        // On add: normalize inputs so disallowed delimiters (`,` / `;`) are auto-split
        // and control chars / whitespace are collapsed before reaching storage.
        // On remove: preserve the literal string so a user can remove an
        // existing offending tag by its exact catalog value.
        let normalized_storage;
        let effective_tags: &[String] = if remove {
            tags
        } else {
            let (normalized, changes) = crate::tag_util::normalize_tag_inputs(tags);
            if !changes.is_empty() {
                for (before, after) in &changes {
                    let after_display = if after.is_empty() {
                        "(dropped: empty after normalization)".to_string()
                    } else {
                        after.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>().join(", ")
                    };
                    eprintln!(
                        "note: tag {before:?} was normalized for storage → {after_display}"
                    );
                }
            }
            normalized_storage = normalized;
            &normalized_storage
        };

        self.mutate_asset(ctx, asset_id_prefix, |asset| {
            let changed = if remove {
                // Collect tags to remove, including orphaned ancestors
                let mut all_to_remove = Vec::new();
                for tag in effective_tags {
                    if asset.tags.iter().any(|t| t == tag) {
                        all_to_remove.push(tag.clone());
                    }
                }
                // After removing the requested tags, check for orphaned ancestors
                let remaining_after: Vec<String> = asset.tags.iter()
                    .filter(|t| !all_to_remove.contains(t))
                    .cloned()
                    .collect();
                for tag in effective_tags {
                    for orphan in crate::tag_util::orphaned_ancestors(tag, &remaining_after) {
                        if !all_to_remove.contains(&orphan) && asset.tags.iter().any(|t| t == &orphan) {
                            all_to_remove.push(orphan);
                        }
                    }
                }
                let remove_set: std::collections::HashSet<&str> =
                    all_to_remove.iter().map(|s| s.as_str()).collect();
                let actually_removed: Vec<String> = asset.tags.iter()
                    .filter(|t| remove_set.contains(t.as_str()))
                    .cloned()
                    .collect();
                // Removes from both the tag list and the provenance map.
                asset.remove_tags(&all_to_remove);
                actually_removed
            } else {
                // Expand hierarchical tags to include all ancestor paths
                let expanded = crate::tag_util::expand_all_ancestors(effective_tags);
                let existing: std::collections::HashSet<String> =
                    asset.tags.iter().cloned().collect();
                let added: Vec<String> = expanded.iter()
                    .filter(|t| !existing.contains(*t))
                    .cloned()
                    .collect();
                // Records `source` for the genuinely-new tags only.
                asset.add_tags_with_source(&added, source);
                added
            };

            let result = TagResult {
                changed: changed.clone(),
                current_tags: asset.tags.clone(),
                tag_sources: asset.tag_sources.clone(),
            };
            let (to_add, to_remove) = if remove {
                (Vec::new(), changed)
            } else {
                (changed, Vec::new())
            };
            Ok(Mutation::new(!result.changed.is_empty(), result).then(move |asset| {
                self.write_back_tags_to_xmp(asset, &to_add, &to_remove, ctx)
            }))
        })
    }

    /// Rename a tag across all assets that have it.
    /// Rename a tag across the catalog.
    ///
    /// `old_tag` may be prefixed with the same `=` (exact level) and `^`
    /// (case-sensitive) markers as the `tag:` search filter, in any order:
    /// - `tag rename Foo Bar` — case-insensitive, includes descendants (default)
    /// - `tag rename =Foo Bar` — exact level only, case-insensitive
    /// - `tag rename ^Foo Bar` — case-sensitive, includes descendants
    /// - `tag rename =^Foo Bar` — exact level AND case-sensitive
    ///
    /// `new_tag` is always taken literally (no prefix parsing).
    pub fn tag_rename(
        &self,
        old_tag: &str,
        new_tag: &str,
        apply: bool,
        mut on_asset: impl FnMut(&str, TagRenameAction),
    ) -> Result<TagRenameResult> {
        let ctx = self.batch_context_fast()?;
        let catalog = ctx.catalog;
        let store = ctx.meta_store;

        // Strip the optional =/^// markers from `old_tag`. The new_tag is always
        // taken literally — we never want to rename to a marker-prefixed value.
        // The `|` marker (prefix anchor) is parsed but rejected for rename: it
        // would mean "rename every tag starting with X to Y", which collapses
        // distinct tags into one and is rarely what a user actually wants. We
        // bail with a clear error so users compose multiple targeted renames.
        //
        // Both `=` (whole-path match in search) and `/` (leaf-only-at-any-level
        // in search) collapse to the same internal `exact_only` flag here:
        // `assets_with_tag_or_prefix(exact_only=true)` uses `je.value = ?`
        // (SQL equality on individual tag values), which is whole-path by
        // construction. The descendant-skip logic below handles the leaf
        // semantic. Both markers therefore behave identically in the rename
        // context — the test suite asserts this convergence (see
        // tag_rename_marker_order_independent and friends).
        let mut rest = old_tag;
        let mut exact_only = false;
        let mut case_sensitive = false;
        loop {
            if let Some(s) = rest.strip_prefix('=') { exact_only = true; rest = s; }
            else if let Some(s) = rest.strip_prefix('/') { exact_only = true; rest = s; }
            else if let Some(s) = rest.strip_prefix('^') { case_sensitive = true; rest = s; }
            else if rest.starts_with('|') {
                anyhow::bail!(
                    "The | prefix-anchor marker is not supported for `tag rename` because it would \
                     collapse distinct tags into one. Use `maki search 'tag:{}' --format ids` to find \
                     matching tags first, then run a targeted rename for each.",
                    rest
                );
            }
            else { break; }
        }
        let old_tag = rest;

        // Find assets that have the exact tag OR any descendant (prefix match)
        let matches = catalog.assets_with_tag_or_prefix(old_tag, case_sensitive, exact_only)?;
        let mut result = TagRenameResult { dry_run: !apply, matched: matches.len(), ..Default::default() };

        // Comparison helpers honoring case_sensitive flag.
        let cmp_eq = |a: &str, b: &str| -> bool {
            if case_sensitive { a == b } else { a.to_lowercase() == b.to_lowercase() }
        };
        let cmp_starts = |a: &str, b: &str| -> bool {
            if case_sensitive { a.starts_with(b) } else { a.to_lowercase().starts_with(&b.to_lowercase()) }
        };

        let old_prefix = format!("{old_tag}|");
        let new_prefix = format!("{new_tag}|");

        for (asset_id, _stack_id) in &matches {
            // Tolerate per-asset load failures (e.g. YAML sidecar deleted
            // out of band by a CLI cleanup running concurrently with the
            // server). Record the error and continue with the rest of the
            // matched set rather than aborting the entire rename.
            let uuid: uuid::Uuid = match asset_id.parse() {
                Ok(u) => u,
                Err(e) => {
                    result.errors.push(format!("{asset_id}: invalid UUID: {e}"));
                    continue;
                }
            };
            let mut asset = match store.load(uuid) {
                Ok(a) => a,
                Err(e) => {
                    result.errors.push(format!("{asset_id}: failed to load sidecar: {e:#}"));
                    continue;
                }
            };

            // Find all tags that match: exact match OR (unless exact_only) prefix match (descendants)
            let has_exact = asset.tags.iter().any(|t| cmp_eq(t, old_tag));
            let has_descendants = asset.tags.iter().any(|t| cmp_starts(t, &old_prefix));

            if !has_exact && (exact_only || !has_descendants) {
                result.skipped += 1;
                on_asset(&asset_id[..8.min(asset_id.len())], TagRenameAction::Skipped);
                continue;
            }

            // With exact_only, skip assets where the tag is not a leaf — i.e.,
            // the asset also has descendant tags (old_tag|...). This matches the
            // semantics of `=` in search filters: only assets where the tag sits
            // at a leaf level, not those that merely carry it as an expanded ancestor.
            if exact_only && has_descendants {
                result.skipped += 1;
                on_asset(&asset_id[..8.min(asset_id.len())], TagRenameAction::Skipped);
                continue;
            }

            // Beyond this point, descendants are only relevant when not exact_only
            let has_descendants = !exact_only && has_descendants;

            // Check if the rename would be a no-op (already correct)
            let exact_already_correct = if has_exact {
                let actual = asset.tags.iter().find(|t| cmp_eq(t, old_tag));
                actual.map(|t| t == new_tag).unwrap_or(false)
            } else {
                true
            };
            let descendants_already_correct = !has_descendants || asset.tags.iter()
                .filter(|t| cmp_starts(t, &old_prefix))
                .all(|t| cmp_starts(t, &new_prefix));

            if exact_already_correct && descendants_already_correct {
                result.skipped += 1;
                on_asset(&asset_id[..8.min(asset_id.len())], TagRenameAction::Skipped);
                continue;
            }

            // Check if asset already has the new exact tag (for merge detection)
            let has_new_separately = if cmp_eq(old_tag, new_tag) {
                false
            } else {
                asset.tags.iter().any(|t| cmp_eq(t, new_tag))
            };
            let has_exact_new = asset.tags.contains(&new_tag.to_string());

            let action = if has_exact && (has_exact_new || has_new_separately) && !has_descendants {
                TagRenameAction::Removed
            } else {
                TagRenameAction::Renamed
            };

            let name = asset.name.clone().unwrap_or_else(|| asset_id[..8.min(asset_id.len())].to_string());
            on_asset(&name, action);

            if apply {
                // Build the list of tags to remove and tags to add.
                // `renamed_pairs` tracks old → new value mappings so tag
                // provenance carries over (a renamed machine tag stays a
                // machine tag).
                let mut tags_to_remove = Vec::new();
                let mut tags_to_add = Vec::new();
                let mut renamed_pairs: Vec<(String, String)> = Vec::new();

                for tag in &asset.tags {
                    if cmp_eq(tag, old_tag) {
                        // Exact match
                        tags_to_remove.push(tag.clone());
                        if action == TagRenameAction::Renamed {
                            tags_to_add.push(new_tag.to_string());
                            renamed_pairs.push((tag.clone(), new_tag.to_string()));
                        }
                    } else if !exact_only && cmp_starts(tag, &old_prefix) {
                        // Descendant: replace prefix (preserving the rest of the tag verbatim)
                        tags_to_remove.push(tag.clone());
                        // For case-sensitive: prefix length matches. For case-insensitive:
                        // the prefix may differ in case from old_prefix, so use the matched
                        // prefix length (same as old_prefix.len() since case mapping preserves
                        // ASCII length, which is the only safe assumption here).
                        let new_descendant = format!("{}{}", new_prefix, &tag[old_prefix.len()..]);
                        renamed_pairs.push((tag.clone(), new_descendant.clone()));
                        tags_to_add.push(new_descendant);
                    }
                }

                // Expand tags_to_add to include all ancestor paths
                let expanded_adds = crate::tag_util::expand_all_ancestors(&tags_to_add);

                // Also remove old standalone ancestors that are now covered by
                // the expanded new tags (e.g., standalone "Germany" when we're
                // adding "location|Germany|Bayern|München" which includes "location|Germany").
                // In case-insensitive mode, also collapse case variants into the canonical form.
                for existing_tag in &asset.tags {
                    if !tags_to_remove.contains(existing_tag) {
                        let covered = expanded_adds.iter().any(|a| cmp_eq(a, existing_tag));
                        if covered && !expanded_adds.iter().any(|a| a == existing_tag) {
                            tags_to_remove.push(existing_tag.clone());
                        }
                    }
                }

                // Capture carried provenance before the removals drop the
                // map entries: each renamed value keeps its old source.
                let carried: Vec<(String, crate::models::TagSource)> = renamed_pairs
                    .iter()
                    .map(|(old, new)| (new.clone(), asset.tag_source(old)))
                    .collect();

                // Apply removals, then add expanded tags (dedup honors case_sensitive flag).
                asset.remove_tags(&tags_to_remove);
                for add in &expanded_adds {
                    if !asset.tags.iter().any(|t| cmp_eq(t, add)) {
                        let source = carried
                            .iter()
                            .find(|(new, _)| new == add)
                            .map(|(_, s)| *s)
                            .unwrap_or(crate::models::TagSource::User);
                        asset.add_tags_with_source(std::slice::from_ref(add), source);
                    }
                }
                ctx.persist_asset(&asset)?;

                // Writeback: remove old tags, add new tags in XMP. Inner
                // tracks pending and skips the file write when auto-flush
                // is off.
                self.write_back_tags_to_xmp(
                    &mut asset, &tags_to_add, &tags_to_remove, &ctx,
                );
            }
            match action {
                TagRenameAction::Renamed => result.renamed += 1,
                TagRenameAction::Removed => result.removed += 1,
                TagRenameAction::Skipped => result.skipped += 1,
            }
        }

        Ok(result)
    }

    /// Split a tag into multiple tags across all assets.
    ///
    /// For each asset carrying `old_tag` as a leaf (no descendants), add every
    /// tag in `new_tags` (with ancestor expansion). Unless `keep` is true, the
    /// `old_tag` is also removed — so `tag_split("A", &["B", "C"], false)`
    /// replaces A with B+C, while the same call with `keep=true` keeps A and
    /// adds B+C alongside.
    ///
    /// Split only acts on the exact tag, never on descendants. Assets where
    /// `old_tag` has descendants (e.g. `old_tag|foo`) are skipped — "splitting"
    /// a non-leaf tag has ambiguous semantics. Use `tag rename` for structural
    /// renames that should cascade.
    ///
    /// `old_tag` accepts the same optional markers as `tag_rename`:
    /// - `=old` / `/old` — explicit exact (redundant here; already the default).
    /// - `^old` — case-sensitive.
    /// - `|old` — rejected (split acts on one tag at a time).
    pub fn tag_split(
        &self,
        old_tag: &str,
        new_tags: &[String],
        keep: bool,
        apply: bool,
        mut on_asset: impl FnMut(&str, TagSplitAction),
    ) -> Result<TagSplitResult> {
        if new_tags.is_empty() {
            anyhow::bail!("at least one target tag is required for `tag split`");
        }

        let ctx = self.batch_context_fast()?;
        let catalog = ctx.catalog;
        let store = ctx.meta_store;

        // Parse markers on old_tag. Split is always exact-tag-only; the
        // `=` and `/` markers are accepted as explicit no-ops, `^` enables
        // case sensitivity, `|` is rejected (same reasoning as rename).
        let mut rest = old_tag;
        let mut case_sensitive = false;
        loop {
            if let Some(s) = rest.strip_prefix('=') { rest = s; }
            else if let Some(s) = rest.strip_prefix('/') { rest = s; }
            else if let Some(s) = rest.strip_prefix('^') { case_sensitive = true; rest = s; }
            else if rest.starts_with('|') {
                anyhow::bail!(
                    "The | prefix-anchor marker is not supported for `tag split`. \
                     Split one tag at a time.",
                );
            }
            else { break; }
        }
        let old_tag = rest;

        let cmp_eq = |a: &str, b: &str| -> bool {
            if case_sensitive { a == b } else { a.to_lowercase() == b.to_lowercase() }
        };
        let old_prefix = format!("{old_tag}|");
        let old_prefix_lower = old_prefix.to_lowercase();
        let starts_with_old_prefix = |t: &str| -> bool {
            if case_sensitive { t.starts_with(&old_prefix) } else { t.to_lowercase().starts_with(&old_prefix_lower) }
        };

        // Always exact-only: descendants of old_tag are never touched by split.
        let matches = catalog.assets_with_tag_or_prefix(old_tag, case_sensitive, /*exact_only=*/true)?;
        let mut result = TagSplitResult { dry_run: !apply, matched: matches.len(), ..Default::default() };

        // Pre-expand targets once — ancestor expansion is identical across
        // all assets.
        let expanded_targets = crate::tag_util::expand_all_ancestors(new_tags);

        for (asset_id, _stack_id) in &matches {
            // Same skip-on-load-failure pattern as tag_rename — see that
            // function for the rationale.
            let uuid: uuid::Uuid = match asset_id.parse() {
                Ok(u) => u,
                Err(e) => {
                    result.errors.push(format!("{asset_id}: invalid UUID: {e}"));
                    continue;
                }
            };
            let mut asset = match store.load(uuid) {
                Ok(a) => a,
                Err(e) => {
                    result.errors.push(format!("{asset_id}: failed to load sidecar: {e:#}"));
                    continue;
                }
            };

            let has_exact = asset.tags.iter().any(|t| cmp_eq(t, old_tag));
            let has_descendants = asset.tags.iter().any(|t| starts_with_old_prefix(t));

            // Require exact leaf — skip if old_tag isn't present, or if it
            // has descendants on this asset (non-leaf split is unclear).
            if !has_exact || has_descendants {
                result.skipped += 1;
                on_asset(&asset_id[..8.min(asset_id.len())], TagSplitAction::Skipped);
                continue;
            }

            // Which of the expanded targets are genuinely new to this asset?
            let actually_new: Vec<String> = expanded_targets.iter()
                .filter(|t| !asset.tags.iter().any(|existing| cmp_eq(existing, t)))
                .cloned()
                .collect();

            // Build tags_to_remove: old_tag (unless --keep), plus any existing
            // standalone ancestors that are now redundant (case-normalized) with
            // the expanded targets. Matches tag_rename's canonicalization pass.
            let mut tags_to_remove: Vec<String> = Vec::new();
            if !keep {
                for tag in &asset.tags {
                    if cmp_eq(tag, old_tag) {
                        tags_to_remove.push(tag.clone());
                    }
                }
            }
            for existing_tag in &asset.tags {
                if tags_to_remove.contains(existing_tag) {
                    continue;
                }
                let covered = expanded_targets.iter().any(|a| cmp_eq(a, existing_tag));
                if covered && !expanded_targets.iter().any(|a| a == existing_tag) {
                    tags_to_remove.push(existing_tag.clone());
                }
            }

            if actually_new.is_empty() && tags_to_remove.is_empty() {
                result.skipped += 1;
                on_asset(&asset_id[..8.min(asset_id.len())], TagSplitAction::Skipped);
                continue;
            }

            let name = asset.name.clone().unwrap_or_else(|| asset_id[..8.min(asset_id.len())].to_string());
            on_asset(&name, TagSplitAction::Split);
            result.split += 1;

            if apply {
                // Rename semantics for provenance: the split targets
                // inherit the source of the tag being split (a machine
                // tag split into two stays machine). With --keep the old
                // tag survives, so its source is read but not removed.
                let old_actual = asset.tags.iter()
                    .find(|t| cmp_eq(t, old_tag))
                    .cloned();
                let carried_source = old_actual
                    .as_deref()
                    .map(|t| asset.tag_source(t))
                    .unwrap_or(crate::models::TagSource::User);

                asset.remove_tags(&tags_to_remove);
                for add in &actually_new {
                    if !asset.tags.iter().any(|t| cmp_eq(t, add)) {
                        asset.add_tags_with_source(std::slice::from_ref(add), carried_source);
                    }
                }
                ctx.persist_asset(&asset)?;

                self.write_back_tags_to_xmp(
                    &mut asset, &actually_new, &tags_to_remove, &ctx,
                );
            }
        }

        Ok(result)
    }

    /// Delete a tag (and, by default, its descendants) from every asset that
    /// carries it.
    ///
    /// Defaults to **cascading**: deleting `subject|nature` removes `subject|
    /// nature`, `subject|nature|landscape`, etc. from all matching assets.
    /// Pass the same `=` / `/` markers as the search filter to opt out of the
    /// cascade — `=subject|nature` removes the exact tag value only, skipping
    /// assets where it has descendants (matching `tag rename` semantics).
    /// `^` makes the match case-sensitive.
    ///
    /// After removing a tag, any of its ancestor paths that no longer have a
    /// surviving descendant on the same asset are also cleaned up — same logic
    /// as `tag --remove`. This keeps deletes coherent with the auto-expanded
    /// storage model: a leaf removal won't leave an unused branch hanging on
    /// the asset's tag list.
    ///
    /// `apply=false` is dry-run; counts but doesn't modify anything.
    pub fn tag_delete(
        &self,
        tag_input: &str,
        apply: bool,
        mut on_asset: impl FnMut(&str, TagDeleteAction),
    ) -> Result<TagDeleteResult> {
        let ctx = self.batch_context_fast()?;
        let catalog = ctx.catalog;
        let store = ctx.meta_store;

        // Same marker grammar as `tag rename`. `|` (prefix anchor) is rejected
        // for the same reason — it would expand a deletion across distinct
        // tags rather than across one branch of the hierarchy. The marker
        // strip is order-insensitive: =^foo and ^=foo behave the same.
        let mut rest = tag_input;
        let mut exact_only = false;
        let mut case_sensitive = false;
        loop {
            if let Some(s) = rest.strip_prefix('=') { exact_only = true; rest = s; }
            else if let Some(s) = rest.strip_prefix('/') { exact_only = true; rest = s; }
            else if let Some(s) = rest.strip_prefix('^') { case_sensitive = true; rest = s; }
            else if rest.starts_with('|') {
                anyhow::bail!(
                    "The | prefix-anchor marker is not supported for `tag delete` because it would \
                     collapse distinct tags into one operation. Use `maki search 'tag:{}' --format ids` \
                     to find matching tags first, then run a targeted delete for each.",
                    rest
                );
            }
            else { break; }
        }
        let tag = rest;
        if tag.is_empty() {
            anyhow::bail!("tag must not be empty");
        }

        // Use the same matcher as rename: it returns assets that have the
        // exact tag (when exact_only=true) or any descendant prefix-match
        // (when exact_only=false).
        let matches = catalog.assets_with_tag_or_prefix(tag, case_sensitive, exact_only)?;
        let mut result = TagDeleteResult { dry_run: !apply, matched: matches.len(), ..Default::default() };

        let cmp_eq = |a: &str, b: &str| -> bool {
            if case_sensitive { a == b } else { a.to_lowercase() == b.to_lowercase() }
        };
        let cmp_starts = |a: &str, b: &str| -> bool {
            if case_sensitive { a.starts_with(b) } else { a.to_lowercase().starts_with(&b.to_lowercase()) }
        };
        let prefix = format!("{tag}|");

        for (asset_id, _stack_id) in &matches {
            // See tag_rename for the rationale on tolerating per-asset
            // load failures rather than aborting the whole delete.
            let uuid: uuid::Uuid = match asset_id.parse() {
                Ok(u) => u,
                Err(e) => {
                    result.errors.push(format!("{asset_id}: invalid UUID: {e}"));
                    continue;
                }
            };
            let mut asset = match store.load(uuid) {
                Ok(a) => a,
                Err(e) => {
                    result.errors.push(format!("{asset_id}: failed to load sidecar: {e:#}"));
                    continue;
                }
            };

            // Collect every tag value to drop on this asset. Exact match
            // always; descendants only when not in leaf-only mode.
            let mut tags_to_remove: Vec<String> = asset.tags.iter()
                .filter(|t| {
                    cmp_eq(t, tag) || (!exact_only && cmp_starts(t, &prefix))
                })
                .cloned()
                .collect();

            // Leaf-only on an asset where the tag has descendants is a skip:
            // we can't remove the parent without leaving the descendants
            // dangling (auto-expansion would re-add the parent on next write
            // anyway), and the caller asked specifically for non-cascade.
            if exact_only {
                let has_descendants = asset.tags.iter().any(|t| cmp_starts(t, &prefix));
                if has_descendants {
                    result.skipped += 1;
                    on_asset(&asset_id[..8.min(asset_id.len())], TagDeleteAction::Skipped);
                    continue;
                }
            }

            if tags_to_remove.is_empty() {
                result.skipped += 1;
                on_asset(&asset_id[..8.min(asset_id.len())], TagDeleteAction::Skipped);
                continue;
            }

            // After dropping the explicit removals, walk the orphan-ancestor
            // helper to collect ancestor paths that lose their last surviving
            // descendant — same coherence rule batch-remove uses (see
            // tag_inner / tag_remove_cleans_orphaned_ancestors).
            let remaining_after: Vec<String> = asset.tags.iter()
                .filter(|t| !tags_to_remove.contains(t))
                .cloned()
                .collect();
            for tag_to_remove in tags_to_remove.clone().iter() {
                for orphan in crate::tag_util::orphaned_ancestors(tag_to_remove, &remaining_after) {
                    if !tags_to_remove.contains(&orphan) && asset.tags.iter().any(|t| t == &orphan) {
                        tags_to_remove.push(orphan);
                    }
                }
            }

            let name = asset.name.clone().unwrap_or_else(|| asset_id[..8.min(asset_id.len())].to_string());
            on_asset(&name, TagDeleteAction::Removed);

            if apply {
                // Drops both the tag list entries and their provenance.
                asset.remove_tags(&tags_to_remove);
                ctx.persist_asset(&asset)?;

                self.write_back_tags_to_xmp(
                    &mut asset, &[], &tags_to_remove, &ctx,
                );
            }
            result.removed += 1;
        }

        Ok(result)
    }

    /// Walk the whole catalogue and rewrite every tag value to Unicode NFC,
    /// deduplicating per-asset where two encoding-variants of the same tag
    /// (NFC vs NFD — visually identical, byte-wise different) collide.
    ///
    /// Background: external tools roundtrip through different Unicode
    /// normalisation forms (macOS path APIs lean NFD; many XMP writers
    /// produce NFC). Without normalisation `Ö-HA` ends up as two distinct
    /// tag values that sort to different positions on the tags page and
    /// don't collapse on the facet sidebar. This is a one-shot migration;
    /// the input chokepoint (`tag_input_to_storage`) was simultaneously
    /// updated to NFC-normalise every new tag, so once this runs the
    /// catalogue stays canonical.
    ///
    /// Apply gates the writes — without `--apply` the engine reports what
    /// would change without touching sidecars or the catalog. Saves to
    /// the metadata store and re-inserts the asset (denormalised columns
    /// stay in sync). XMP writeback fires when enabled, mirroring how
    /// `tag` and `tag rename` already write back.
    pub fn tag_fix_unicode(
        &self,
        apply: bool,
        mut on_asset: impl FnMut(&str, bool),
    ) -> Result<TagFixUnicodeResult> {
        let ctx = self.batch_context_fast()?;
        let store = ctx.meta_store;

        let mut result = TagFixUnicodeResult { dry_run: !apply, ..Default::default() };

        // Iterate every asset via the metadata store so we capture sidecar
        // values exactly as written. The catalog `assets.tags` JSON column
        // is rebuilt from the same source on `insert_asset`, so syncing
        // both sides is a single save+insert.
        let summaries = store.list()?;
        for s in &summaries {
            result.scanned += 1;
            let mut asset = match store.load(s.id) {
                Ok(a) => a,
                Err(e) => {
                    // Record so the caller can surface it; mirror the
                    // tag_rename / tag_split / tag_delete tolerance pattern
                    // for consistency across the four bulk-mutation flows.
                    result.errors.push(format!("{}: failed to load sidecar: {e:#}", s.id));
                    continue;
                }
            };

            // Build the post-normalisation tag list, tracking what changed.
            let mut nfc_tags: Vec<String> = Vec::with_capacity(asset.tags.len());
            let mut tag_changed_count = 0usize;
            let mut had_dup = false;
            for t in &asset.tags {
                let normalised = crate::tag_util::nfc(t);
                if normalised.as_bytes() != t.as_bytes() {
                    tag_changed_count += 1;
                }
                // Per-asset dedup. Two distinct byte sequences (NFC and NFD
                // forms of the same string) end up as one entry here.
                if nfc_tags.iter().any(|existing| existing == &normalised) {
                    had_dup = true;
                } else {
                    nfc_tags.push(normalised);
                }
            }

            // Skip if nothing actually changed.
            if tag_changed_count == 0 && !had_dup {
                continue;
            }

            // Determine which tags were removed (for XMP writeback): tags
            // present in the original list (after NFC, to compare like-for-
            // like) but absent from the deduped result.
            // Compute removed-tag delta for XMP writeback (so the inner
            // method can mark pending and/or strip the keyword from the
            // file). Cheap multiset diff; always run when applying so the
            // pending tracker stays accurate even with auto-flush off.
            let tags_removed: Vec<String> = if apply {
                let mut original_normalised: Vec<String> = asset.tags.iter()
                    .map(|t| crate::tag_util::nfc(t))
                    .collect();
                for kept in &nfc_tags {
                    if let Some(pos) = original_normalised.iter().position(|t| t == kept) {
                        original_normalised.remove(pos);
                    }
                }
                original_normalised
            } else {
                Vec::new()
            };

            result.fixed += 1;
            result.tags_normalized += tag_changed_count;
            if had_dup {
                result.merged += 1;
            }
            let display = asset.name.clone()
                .unwrap_or_else(|| s.id.to_string()[..8.min(s.id.to_string().len())].to_string());
            on_asset(&display, true);

            if apply {
                asset.tags = nfc_tags.clone();
                // Re-key provenance entries through the same NFC pass —
                // a normalised tag keeps its source. On NFC/NFD dedup
                // collisions the first (BTreeMap order) entry wins.
                let old_sources = std::mem::take(&mut asset.tag_sources);
                for (tag, source) in old_sources {
                    let normalised = crate::tag_util::nfc(&tag);
                    asset.tag_sources.entry(normalised).or_insert(source);
                }
                asset.prune_tag_sources();
                ctx.persist_asset(&asset)?;

                if !tags_removed.is_empty() {
                    // Only the removed half of the dedup is communicated as a
                    // delete; the kept half is identical to what was already
                    // there modulo NFC, so XMP semantics are unchanged for
                    // those values. The inner writeback handles its own
                    // gating (tracks pending when auto-flush is off, writes
                    // when on).
                    self.write_back_tags_to_xmp(
                        &mut asset, &[], &tags_removed, &ctx,
                    );
                }
            }
        }

        Ok(result)
    }

    /// Tag multiple assets using a single shared catalog connection. The
    /// whole batch is journaled as ONE undoable operation.
    pub fn batch_tag(&self, asset_ids: &[String], tags: &[String], remove: bool) -> Vec<Result<TagResult>> {
        let ctx = match self.batch_recording_context() {
            Ok(c) => c,
            Err(e) => return asset_ids.iter().map(|_| Err(anyhow::anyhow!("{e:#}"))).collect(),
        };
        let results: Vec<_> = asset_ids.iter().map(|id| self.tag_inner(&ctx, id, tags, remove, crate::models::TagSource::User)).collect();
        let n = ctx.recorder.as_ref().map(|c| c.borrow().len()).unwrap_or(0);
        let verb = if remove { "removed tag(s) on" } else { "added tag(s) on" };
        self.commit_recorder(&ctx, "tag", &format!("{verb} {n} asset(s)"));
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::metadata_store::MetadataStore;
    use crate::query::test_support::*;

    #[test]
    fn tag_add_new() {
        let (dir, id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());

        let result = engine
            .tag(&id, &["landscape".to_string(), "nature".to_string()], false)
            .unwrap();

        assert_eq!(result.changed, vec!["landscape", "nature"]);
        assert_eq!(result.current_tags, vec!["existing", "landscape", "nature"]);
    }

    #[test]
    fn tag_add_duplicate_is_noop() {
        let (dir, id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());

        let result = engine.tag(&id, &["existing".to_string()], false).unwrap();

        assert!(result.changed.is_empty());
        assert_eq!(result.current_tags, vec!["existing"]);
    }

    #[test]
    fn tag_remove_existing() {
        let (dir, id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());

        let result = engine.tag(&id, &["existing".to_string()], true).unwrap();

        assert_eq!(result.changed, vec!["existing"]);
        assert!(result.current_tags.is_empty());
    }

    #[test]
    fn tag_remove_nonexistent_is_noop() {
        let (dir, id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());

        let result = engine.tag(&id, &["nope".to_string()], true).unwrap();

        assert!(result.changed.is_empty());
        assert_eq!(result.current_tags, vec!["existing"]);
    }

    #[test]
    fn tag_persists_to_sidecar_and_catalog() {
        let (dir, id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());

        engine.tag(&id, &["new_tag".to_string()], false).unwrap();

        // Verify sidecar
        let uuid: uuid::Uuid = id.parse().unwrap();
        let store = MetadataStore::new(dir.path());
        let asset = store.load(uuid).unwrap();
        assert!(asset.tags.contains(&"new_tag".to_string()));

        // Verify catalog
        let details = engine.show(&id).unwrap();
        assert!(details.tags.contains(&"new_tag".to_string()));
    }

    #[test]
    fn tag_via_engine_records_user_source() {
        let (dir, id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());

        engine.tag(&id, &["manual".to_string()], false).unwrap();

        // User-source tags stay out of the provenance map (absent = user)
        // in both stores.
        let uuid: uuid::Uuid = id.parse().unwrap();
        let asset = MetadataStore::new(dir.path()).load(uuid).unwrap();
        assert!(asset.tag_sources.is_empty(), "{:?}", asset.tag_sources);
        let details = engine.show(&id).unwrap();
        assert!(details.tag_sources.is_empty());
    }

    #[test]
    fn tag_with_source_records_machine_source_in_both_stores() {
        use crate::models::TagSource;
        let (dir, id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());

        engine
            .tag_with_source(&id, &["machine".to_string()], false, TagSource::AutoTag)
            .unwrap();

        let uuid: uuid::Uuid = id.parse().unwrap();
        let asset = MetadataStore::new(dir.path()).load(uuid).unwrap();
        assert_eq!(asset.tag_source("machine"), TagSource::AutoTag);
        // Pre-existing user tag is untouched.
        assert_eq!(asset.tag_source("existing"), TagSource::User);
        // SQLite column mirrors the sidecar (via load_asset_details).
        let details = engine.show(&id).unwrap();
        assert_eq!(details.tag_sources.get("machine"), Some(&TagSource::AutoTag));

        // Removing the tag drops the provenance entry again.
        engine.tag(&id, &["machine".to_string()], true).unwrap();
        let asset = MetadataStore::new(dir.path()).load(uuid).unwrap();
        assert!(asset.tag_sources.is_empty(), "{:?}", asset.tag_sources);
    }

    #[test]
    fn tag_rename_carries_machine_source() {
        use crate::models::TagSource;
        let (dir, id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());

        engine
            .tag_with_source(&id, &["robotag".to_string()], false, TagSource::Vlm)
            .unwrap();
        let result = engine.tag_rename("robotag", "better-tag", true, |_, _| {}).unwrap();
        assert_eq!(result.renamed, 1);

        let uuid: uuid::Uuid = id.parse().unwrap();
        let asset = MetadataStore::new(dir.path()).load(uuid).unwrap();
        assert!(asset.tags.contains(&"better-tag".to_string()));
        assert_eq!(
            asset.tag_source("better-tag"),
            TagSource::Vlm,
            "a renamed machine tag stays a machine tag"
        );
        assert!(!asset.tag_sources.contains_key("robotag"));
    }

    #[test]
    fn tag_delete_drops_machine_source_entries() {
        use crate::models::TagSource;
        let (dir, id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());

        engine
            .tag_with_source(&id, &["doomed".to_string()], false, TagSource::AutoTag)
            .unwrap();
        let result = engine.tag_delete("doomed", true, |_, _| {}).unwrap();
        assert_eq!(result.removed, 1);

        let uuid: uuid::Uuid = id.parse().unwrap();
        let asset = MetadataStore::new(dir.path()).load(uuid).unwrap();
        assert!(!asset.tags.contains(&"doomed".to_string()));
        assert!(asset.tag_sources.is_empty(), "{:?}", asset.tag_sources);
    }

    // ── tag rename tests ───────────────────────────────────

    fn setup_tag_rename_catalog(tags: &[&str]) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        crate::config::CatalogConfig::default().save(root).unwrap();
        crate::device_registry::DeviceRegistry::init(root).unwrap();
        let catalog = crate::catalog::Catalog::open(root).unwrap();
        catalog.initialize().unwrap();

        let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:rename_test");
        asset.tags = tags.iter().map(|t| t.to_string()).collect();
        let variant = crate::models::Variant {
            content_hash: "sha256:rename_test".to_string(),
            asset_id: asset.id,
            role: crate::models::VariantRole::Original,
            format: "jpg".to_string(),
            file_size: 100,
            original_filename: "test.jpg".to_string(),
            source_metadata: Default::default(),
            locations: vec![],
        };
        asset.variants.push(variant.clone());
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&variant).unwrap();
        let store = crate::metadata_store::MetadataStore::new(root);
        store.save(&asset).unwrap();

        let asset_id = asset.id.to_string();
        (dir, asset_id)
    }

    #[test]
    fn tag_rename_cascades_to_descendants() {
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "localtion|Germany|Bayern|München",
            "localtion|Germany|Bayern|Wolfratshausen",
            "localtion|Germany",
            "sunset",
        ]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_rename("localtion", "location", true, |_, _| {}).unwrap();
        assert_eq!(result.renamed, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert!(asset.tags.contains(&"location|Germany|Bayern|München".to_string()));
        assert!(asset.tags.contains(&"location|Germany|Bayern|Wolfratshausen".to_string()));
        assert!(asset.tags.contains(&"location|Germany".to_string()));
        assert!(asset.tags.contains(&"sunset".to_string()));
        // Old prefix must be gone
        assert!(!asset.tags.iter().any(|t| t.starts_with("localtion")));
    }

    #[test]
    fn tag_rename_does_not_match_similar_prefix() {
        // "localtionvenue" should NOT be renamed when renaming "localtion"
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "localtion|Germany",
            "localtionvenue",
        ]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_rename("localtion", "location", true, |_, _| {}).unwrap();
        assert_eq!(result.renamed, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert!(asset.tags.contains(&"location|Germany".to_string()));
        assert!(asset.tags.contains(&"localtionvenue".to_string()), "similar prefix should not be renamed");
    }

    #[test]
    fn tag_rename_case_insensitive_cascade() {
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "Localtion|Germany|Bayern",
        ]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_rename("localtion", "location", true, |_, _| {}).unwrap();
        assert_eq!(result.renamed, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert!(asset.tags.contains(&"location|Germany|Bayern".to_string()));
        assert!(!asset.tags.iter().any(|t| t.to_lowercase().starts_with("localtion")));
    }

    #[test]
    fn tag_rename_case_only_no_deletion() {
        let (dir, asset_id) = setup_tag_rename_catalog(&["Livestream"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_rename("Livestream", "livestream", true, |_, _| {}).unwrap();
        assert_eq!(result.renamed, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert!(asset.tags.contains(&"livestream".to_string()));
    }

    #[test]
    fn tag_rename_case_sensitive_only_targets_exact_case() {
        // Two assets, one tagged "Landscape" (capitalized), one "landscape" (lowercase).
        // Renaming with `^Landscape` → `nature` must touch only the capitalized one.
        let (dir, a1_id) = setup_tag_rename_catalog(&["Landscape"]);
        let store = crate::metadata_store::MetadataStore::new(dir.path());

        let mut a2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:rt2");
        a2.tags = vec!["landscape".to_string()];
        let v2 = crate::models::Variant {
            content_hash: "sha256:rt2".to_string(),
            asset_id: a2.id,
            role: crate::models::VariantRole::Original,
            format: "jpg".to_string(),
            file_size: 100,
            original_filename: "rt2.jpg".to_string(),
            source_metadata: Default::default(),
            locations: vec![],
        };
        a2.variants.push(v2.clone());
        let a2_id = a2.id.to_string();
        let catalog = Catalog::open(dir.path()).unwrap();
        catalog.insert_asset(&a2).unwrap();
        catalog.insert_variant(&v2).unwrap();
        store.save(&a2).unwrap();

        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_rename("^Landscape", "nature", true, |_, _| {}).unwrap();
        assert_eq!(result.renamed, 1, "only the exact-case tag should be renamed");

        let a1: crate::models::Asset = store.load(a1_id.parse().unwrap()).unwrap();
        let a2: crate::models::Asset = store.load(a2_id.parse().unwrap()).unwrap();
        assert!(a1.tags.contains(&"nature".to_string()), "Landscape → nature");
        assert!(!a1.tags.contains(&"Landscape".to_string()));
        assert!(a2.tags.contains(&"landscape".to_string()), "lowercase landscape untouched");
        assert!(!a2.tags.contains(&"nature".to_string()));
    }

    #[test]
    fn tag_rename_exact_only_skips_descendants() {
        // `=` means "leaf only": only rename on assets where the tag has no descendants.
        // Asset 1: has both parent and child → tag is NOT a leaf → skipped
        // Asset 2: has only the parent tag → tag IS a leaf → renamed
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        crate::config::CatalogConfig::default().save(root).unwrap();
        crate::device_registry::DeviceRegistry::init(root).unwrap();
        let catalog = crate::catalog::Catalog::open(root).unwrap();
        catalog.initialize().unwrap();
        let store = crate::metadata_store::MetadataStore::new(root);

        // Asset 1: has descendant → not a leaf
        let mut a1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:ren1");
        a1.tags = vec!["location|Germany|Bayern".to_string(), "location|Germany".to_string()];
        let v1 = crate::models::Variant {
            content_hash: "sha256:ren1".to_string(), asset_id: a1.id,
            role: crate::models::VariantRole::Original, format: "jpg".to_string(),
            file_size: 100, original_filename: "a1.jpg".to_string(),
            source_metadata: Default::default(), locations: vec![],
        };
        a1.variants.push(v1.clone());
        catalog.insert_asset(&a1).unwrap();
        catalog.insert_variant(&v1).unwrap();
        store.save(&a1).unwrap();
        let a1_id = a1.id.to_string();

        // Asset 2: leaf only
        let mut a2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:ren2");
        a2.tags = vec!["location|Germany".to_string()];
        let v2 = crate::models::Variant {
            content_hash: "sha256:ren2".to_string(), asset_id: a2.id,
            role: crate::models::VariantRole::Original, format: "jpg".to_string(),
            file_size: 100, original_filename: "a2.jpg".to_string(),
            source_metadata: Default::default(), locations: vec![],
        };
        a2.variants.push(v2.clone());
        catalog.insert_asset(&a2).unwrap();
        catalog.insert_variant(&v2).unwrap();
        store.save(&a2).unwrap();
        let a2_id = a2.id.to_string();

        let engine = QueryEngine::new(root);
        let result = engine.tag_rename("=location|Germany", "country|Germany", true, |_, _| {}).unwrap();
        assert_eq!(result.renamed, 1, "only the leaf asset should be renamed");
        assert_eq!(result.skipped, 1, "non-leaf asset should be skipped");

        let a1_after: crate::models::Asset = store.load(a1_id.parse().unwrap()).unwrap();
        assert!(a1_after.tags.contains(&"location|Germany".to_string()), "non-leaf untouched");
        assert!(a1_after.tags.contains(&"location|Germany|Bayern".to_string()), "descendant untouched");

        let a2_after: crate::models::Asset = store.load(a2_id.parse().unwrap()).unwrap();
        assert!(a2_after.tags.contains(&"country|Germany".to_string()), "leaf renamed");
        assert!(!a2_after.tags.contains(&"location|Germany".to_string()), "old leaf tag removed");
    }

    #[test]
    fn tag_rename_combined_exact_and_case_sensitive() {
        // `=^Animals` → leaf-only + case-sensitive. An asset that has descendants
        // of "Animals" is NOT a leaf at that tag, so it gets skipped.
        // Only an asset with standalone "Animals" (no descendant) is renamed.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        crate::config::CatalogConfig::default().save(root).unwrap();
        crate::device_registry::DeviceRegistry::init(root).unwrap();
        let catalog = crate::catalog::Catalog::open(root).unwrap();
        catalog.initialize().unwrap();
        let store = crate::metadata_store::MetadataStore::new(root);

        // Asset 1: has "Animals" but also descendants → not a leaf → skipped
        let mut a1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:ec1");
        a1.tags = vec!["Animals|Cats".to_string(), "animals|Birds".to_string(), "Animals".to_string()];
        let v1 = crate::models::Variant {
            content_hash: "sha256:ec1".to_string(), asset_id: a1.id,
            role: crate::models::VariantRole::Original, format: "jpg".to_string(),
            file_size: 100, original_filename: "a1.jpg".to_string(),
            source_metadata: Default::default(), locations: vec![],
        };
        a1.variants.push(v1.clone());
        catalog.insert_asset(&a1).unwrap();
        catalog.insert_variant(&v1).unwrap();
        store.save(&a1).unwrap();
        let a1_id = a1.id.to_string();

        // Asset 2: standalone "Animals" only (leaf) — this is the one to rename
        let mut a2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:ec2");
        a2.tags = vec!["Animals".to_string()];
        let v2 = crate::models::Variant {
            content_hash: "sha256:ec2".to_string(), asset_id: a2.id,
            role: crate::models::VariantRole::Original, format: "jpg".to_string(),
            file_size: 100, original_filename: "a2.jpg".to_string(),
            source_metadata: Default::default(), locations: vec![],
        };
        a2.variants.push(v2.clone());
        catalog.insert_asset(&a2).unwrap();
        catalog.insert_variant(&v2).unwrap();
        store.save(&a2).unwrap();
        let a2_id = a2.id.to_string();

        let engine = QueryEngine::new(root);
        let result = engine.tag_rename("=^Animals", "Wildlife", true, |_, _| {}).unwrap();
        assert_eq!(result.renamed, 1, "only the leaf asset should be renamed");
        assert_eq!(result.skipped, 1, "non-leaf asset should be skipped");

        let a1_after: crate::models::Asset = store.load(a1_id.parse().unwrap()).unwrap();
        assert!(a1_after.tags.contains(&"Animals".to_string()), "non-leaf Animals untouched");
        assert!(a1_after.tags.contains(&"Animals|Cats".to_string()), "descendant untouched");

        let a2_after: crate::models::Asset = store.load(a2_id.parse().unwrap()).unwrap();
        assert!(a2_after.tags.contains(&"Wildlife".to_string()), "leaf Animals → Wildlife");
        assert!(!a2_after.tags.contains(&"Animals".to_string()), "old tag removed");
    }

    /// Regression: a missing YAML sidecar (e.g. deleted out of band by a
    /// concurrent CLI cleanup, or just plain catalog/disk drift) used to
    /// abort the entire bulk rename with "no such file or directory".
    /// The fix records the per-asset failure in `result.errors` and
    /// continues with the remaining matches.
    #[test]
    fn tag_rename_tolerates_missing_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        crate::config::CatalogConfig::default().save(root).unwrap();
        crate::device_registry::DeviceRegistry::init(root).unwrap();
        let catalog = crate::catalog::Catalog::open(root).unwrap();
        catalog.initialize().unwrap();
        let store = crate::metadata_store::MetadataStore::new(root);

        // Two assets with the same tag. We'll persist the YAML for the
        // first and then *delete* it from disk before running the rename
        // — this simulates the cleanup-out-of-band scenario the user hit.
        let mut a1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:ms1");
        a1.tags = vec!["Foo".to_string()];
        let v1 = crate::models::Variant {
            content_hash: "sha256:ms1".to_string(), asset_id: a1.id,
            role: crate::models::VariantRole::Original, format: "jpg".to_string(),
            file_size: 100, original_filename: "ms1.jpg".to_string(),
            source_metadata: Default::default(), locations: vec![],
        };
        a1.variants.push(v1.clone());
        catalog.insert_asset(&a1).unwrap();
        catalog.insert_variant(&v1).unwrap();
        store.save(&a1).unwrap();

        let mut a2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:ms2");
        a2.tags = vec!["Foo".to_string()];
        let v2 = crate::models::Variant {
            content_hash: "sha256:ms2".to_string(), asset_id: a2.id,
            role: crate::models::VariantRole::Original, format: "jpg".to_string(),
            file_size: 100, original_filename: "ms2.jpg".to_string(),
            source_metadata: Default::default(), locations: vec![],
        };
        a2.variants.push(v2.clone());
        catalog.insert_asset(&a2).unwrap();
        catalog.insert_variant(&v2).unwrap();
        store.save(&a2).unwrap();

        // Nuke a1's sidecar but leave the catalog row — exactly the state
        // a half-finished cleanup or external delete would leave behind.
        // (Use metadata_store.delete so we hit the same sharded path the
        // production code uses.)
        store.delete(a1.id).unwrap();

        let engine = QueryEngine::new(root);
        let result = engine.tag_rename("Foo", "Bar", true, |_, _| {}).unwrap();

        // a2 renamed cleanly; a1 surfaced as a per-asset error.
        assert_eq!(result.renamed, 1, "the surviving asset got renamed");
        assert_eq!(result.errors.len(), 1, "the missing-sidecar failure was recorded");
        assert!(
            result.errors[0].contains(&a1.id.to_string()),
            "error mentions the offending asset: {}",
            result.errors[0]
        );
        // Verify a2's tag really did flip.
        let a2_after: crate::models::Asset = store.load(a2.id).unwrap();
        assert!(a2_after.tags.contains(&"Bar".to_string()));
        assert!(!a2_after.tags.contains(&"Foo".to_string()));
    }

    #[test]
    fn tag_rename_case_sensitive_descendants() {
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "Sport|Football",
            "sport|Football",
        ]);
        let engine = QueryEngine::new(dir.path());
        // `^Sport` → match descendants of "Sport" (capitalized) but NOT "sport"
        let result = engine.tag_rename("^Sport", "Sports", true, |_, _| {}).unwrap();
        assert_eq!(result.renamed, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert!(asset.tags.contains(&"Sports|Football".to_string()), "Sport|Football → Sports|Football");
        assert!(asset.tags.contains(&"sport|Football".to_string()), "lowercase sport|Football untouched");
    }

    #[test]
    fn tag_rename_marker_order_independent() {
        // Test that `=^` and `^=` produce the same result (order doesn't matter).
        // Use a leaf-only tag so the `=` (leaf) semantics don't skip the asset.
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "Foo",
        ]);
        let store = crate::metadata_store::MetadataStore::new(dir.path());

        let engine = QueryEngine::new(dir.path());
        // Both `=^` and `^=` should produce the same result
        let r1 = engine.tag_rename("=^Foo", "X", false, |_, _| {}).unwrap();
        let r2 = engine.tag_rename("^=Foo", "X", false, |_, _| {}).unwrap();
        assert_eq!(r1.renamed, r2.renamed);
        assert_eq!(r1.matched, r2.matched);

        // Apply via the second form and verify
        let _ = engine.tag_rename("^=Foo", "X", true, |_, _| {}).unwrap();
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert!(asset.tags.contains(&"X".to_string()));
    }

    #[test]
    fn tag_rename_merge_when_target_exists() {
        let (dir, asset_id) = setup_tag_rename_catalog(&["Konzert", "concert"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_rename("Konzert", "concert", true, |_, _| {}).unwrap();
        assert_eq!(result.removed, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert_eq!(asset.tags, vec!["concert".to_string()]);
    }

    #[test]
    fn tag_rename_skip_when_already_correct() {
        let (dir, _) = setup_tag_rename_catalog(&["concert"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_rename("concert", "concert", true, |_, _| {}).unwrap();
        assert_eq!(result.skipped, 1);
        assert_eq!(result.renamed, 0);
    }

    #[test]
    fn tag_split_replaces_old_with_multiple_new() {
        let (dir, asset_id) = setup_tag_rename_catalog(&["A & B", "unrelated"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_split(
            "A & B",
            &["A".to_string(), "B".to_string()],
            /*keep=*/false, /*apply=*/true,
            |_, _| {},
        ).unwrap();
        assert_eq!(result.split, 1);
        assert_eq!(result.skipped, 0);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert!(asset.tags.contains(&"A".to_string()));
        assert!(asset.tags.contains(&"B".to_string()));
        assert!(asset.tags.contains(&"unrelated".to_string()));
        assert!(!asset.tags.contains(&"A & B".to_string()));
    }

    #[test]
    fn tag_split_with_keep_preserves_source() {
        let (dir, asset_id) = setup_tag_rename_catalog(&["concert-jane-2024"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_split(
            "concert-jane-2024",
            &["subject|performing arts|concert".to_string()],
            /*keep=*/true, /*apply=*/true,
            |_, _| {},
        ).unwrap();
        assert_eq!(result.split, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert!(asset.tags.contains(&"concert-jane-2024".to_string()), "source tag preserved with --keep");
        assert!(asset.tags.contains(&"subject|performing arts|concert".to_string()));
        assert!(asset.tags.contains(&"subject|performing arts".to_string()), "ancestor expanded");
        assert!(asset.tags.contains(&"subject".to_string()), "root ancestor expanded");
    }

    #[test]
    fn tag_split_dry_run_does_not_persist() {
        let (dir, asset_id) = setup_tag_rename_catalog(&["foo"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_split(
            "foo",
            &["bar".to_string(), "baz".to_string()],
            false, /*apply=*/false,
            |_, _| {},
        ).unwrap();
        assert!(result.dry_run);
        assert_eq!(result.split, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert_eq!(asset.tags, vec!["foo".to_string()], "dry-run must not mutate");
    }

    #[test]
    fn tag_split_skips_non_leaf_assets() {
        // Asset has the exact tag AND a descendant — we refuse to guess
        // the semantics and skip it.
        let (dir, asset_id) = setup_tag_rename_catalog(&["A", "A|child"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_split(
            "A",
            &["B".to_string()],
            false, true,
            |_, _| {},
        ).unwrap();
        assert_eq!(result.skipped, 1);
        assert_eq!(result.split, 0);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert!(asset.tags.contains(&"A".to_string()), "unchanged");
        assert!(asset.tags.contains(&"A|child".to_string()), "unchanged");
    }

    #[test]
    fn tag_split_dedups_when_target_already_present() {
        let (dir, asset_id) = setup_tag_rename_catalog(&["A", "B"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_split(
            "A",
            &["B".to_string(), "C".to_string()],
            false, true,
            |_, _| {},
        ).unwrap();
        assert_eq!(result.split, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        let b_count = asset.tags.iter().filter(|t| *t == "B").count();
        assert_eq!(b_count, 1, "B should not be duplicated");
        assert!(asset.tags.contains(&"C".to_string()));
        assert!(!asset.tags.contains(&"A".to_string()));
    }

    #[test]
    fn tag_split_rejects_empty_targets() {
        let (dir, _) = setup_tag_rename_catalog(&["A"]);
        let engine = QueryEngine::new(dir.path());
        let err = engine.tag_split("A", &[], false, true, |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("at least one"));
    }

    #[test]
    fn tag_split_rejects_pipe_prefix_marker() {
        let (dir, _) = setup_tag_rename_catalog(&["foo"]);
        let engine = QueryEngine::new(dir.path());
        let err = engine.tag_split("|foo", &["bar".to_string()], false, true, |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("prefix-anchor"));
    }

    #[test]
    fn tag_add_expands_ancestors() {
        let (dir, asset_id) = setup_tag_rename_catalog(&["sunset"]);
        let engine = QueryEngine::new(dir.path());
        // Adding a hierarchical tag should expand ancestors
        let result = engine.tag(&asset_id, &["subject|nature|landscape|mountain".to_string()], false).unwrap();
        // Should add: subject, subject|nature, subject|nature|landscape, subject|nature|landscape|mountain
        assert!(result.changed.len() >= 4, "should add tag + 3 ancestors, got: {:?}", result.changed);
        assert!(result.current_tags.contains(&"subject".to_string()));
        assert!(result.current_tags.contains(&"subject|nature".to_string()));
        assert!(result.current_tags.contains(&"subject|nature|landscape".to_string()));
        assert!(result.current_tags.contains(&"subject|nature|landscape|mountain".to_string()));
        assert!(result.current_tags.contains(&"sunset".to_string()), "existing tag should be preserved");
    }

    #[test]
    fn tag_remove_cleans_orphaned_ancestors() {
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "subject|nature|landscape|mountain",
            "subject|nature|landscape",
            "subject|nature",
            "subject",
        ]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag(&asset_id, &["subject|nature|landscape|mountain".to_string()], true).unwrap();
        // All ancestors should be removed (no other descendants keep them alive)
        assert!(!result.current_tags.contains(&"subject|nature|landscape|mountain".to_string()));
        assert!(!result.current_tags.contains(&"subject|nature|landscape".to_string()));
        assert!(!result.current_tags.contains(&"subject|nature".to_string()));
        assert!(!result.current_tags.contains(&"subject".to_string()));
    }

    #[test]
    fn tag_remove_keeps_shared_ancestors() {
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "subject|nature|landscape|mountain",
            "subject|nature|landscape|beach",
            "subject|nature|landscape",
            "subject|nature",
            "subject",
        ]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag(&asset_id, &["subject|nature|landscape|mountain".to_string()], true).unwrap();
        // mountain removed, but ancestors stay because beach keeps them alive
        assert!(!result.current_tags.contains(&"subject|nature|landscape|mountain".to_string()));
        assert!(result.current_tags.contains(&"subject|nature|landscape|beach".to_string()));
        assert!(result.current_tags.contains(&"subject|nature|landscape".to_string()));
        assert!(result.current_tags.contains(&"subject|nature".to_string()));
        assert!(result.current_tags.contains(&"subject".to_string()));
    }

    #[test]
    fn tag_rename_with_ancestor_expansion() {
        let (dir, asset_id) = setup_tag_rename_catalog(&["München", "Germany", "Bayern"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_rename("München", "location|Germany|Bayern|München", true, |_, _| {}).unwrap();
        assert_eq!(result.renamed, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        // Full tag + all ancestors should be present
        assert!(asset.tags.contains(&"location|Germany|Bayern|München".to_string()));
        assert!(asset.tags.contains(&"location|Germany|Bayern".to_string()));
        assert!(asset.tags.contains(&"location|Germany".to_string()));
        assert!(asset.tags.contains(&"location".to_string()));
        // Standalone flat tags remain (they're separate concepts)
        assert!(asset.tags.contains(&"Germany".to_string()));
        assert!(asset.tags.contains(&"Bayern".to_string()));
    }

    // ── tag delete tests ───────────────────────────────────

    #[test]
    fn tag_delete_cascades_to_descendants() {
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "location|Germany|Bayern|München",
            "location|Germany|Bayern|Wolfratshausen",
            "location|Germany",
            "sunset",
        ]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_delete("location", true, |_, _| {}).unwrap();
        assert_eq!(result.matched, 1);
        assert_eq!(result.removed, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        // The whole `location` branch is gone, including the bare `location`
        // root (orphan-ancestor cleanup) and every descendant. Unrelated tags
        // stay.
        assert!(asset.tags.iter().all(|t| !t.starts_with("location")), "tags still present: {:?}", asset.tags);
        assert!(asset.tags.contains(&"sunset".to_string()));
    }

    #[test]
    fn tag_delete_dry_run_does_not_persist() {
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "subject|nature|landscape",
            "sunset",
        ]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_delete("subject|nature", false, |_, _| {}).unwrap();
        assert_eq!(result.matched, 1);
        assert_eq!(result.removed, 1);
        assert!(result.dry_run);

        // Sidecar untouched.
        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert!(asset.tags.contains(&"subject|nature|landscape".to_string()));
    }

    #[test]
    fn tag_delete_leaves_unrelated_branches_alone() {
        // Sibling under same root must survive.
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "subject|nature|landscape",
            "subject|nature",
            "subject",
            "subject|portrait",
        ]);
        let engine = QueryEngine::new(dir.path());
        engine.tag_delete("subject|nature", true, |_, _| {}).unwrap();

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        // `subject|nature` and its descendant gone; `subject` and `subject|portrait` stay.
        assert!(!asset.tags.iter().any(|t| t.starts_with("subject|nature")));
        assert!(asset.tags.contains(&"subject".to_string()));
        assert!(asset.tags.contains(&"subject|portrait".to_string()));
    }

    #[test]
    fn tag_delete_leaf_only_skips_when_descendants_present() {
        // With `=` (or `/`) marker we explicitly opt out of cascade. On an
        // asset whose tag has live descendants, the only coherent action is
        // to skip — auto-expansion would re-add the parent on next write
        // anyway.
        let (dir, _asset_id) = setup_tag_rename_catalog(&[
            "subject|nature|landscape",
            "subject|nature",
            "subject",
        ]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_delete("=subject|nature", true, |_, _| {}).unwrap();
        assert_eq!(result.matched, 1);
        assert_eq!(result.removed, 0);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn tag_delete_leaf_only_removes_when_no_descendants() {
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "subject|nature",
            "subject",
            "sunset",
        ]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_delete("=subject|nature", true, |_, _| {}).unwrap();
        assert_eq!(result.removed, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        // The leaf is gone; the now-orphaned `subject` ancestor is gone too;
        // `sunset` stays.
        assert!(!asset.tags.iter().any(|t| t == "subject|nature"));
        assert!(!asset.tags.iter().any(|t| t == "subject"));
        assert!(asset.tags.contains(&"sunset".to_string()));
    }

    #[test]
    fn tag_delete_rejects_empty_tag() {
        let (dir, _) = setup_tag_rename_catalog(&["foo"]);
        let engine = QueryEngine::new(dir.path());
        assert!(engine.tag_delete("", true, |_, _| {}).is_err());
        // Markers without a name should also bail.
        assert!(engine.tag_delete("=", true, |_, _| {}).is_err());
        assert!(engine.tag_delete("^", true, |_, _| {}).is_err());
    }

    #[test]
    fn tag_delete_rejects_pipe_prefix_marker() {
        let (dir, _) = setup_tag_rename_catalog(&["foo"]);
        let engine = QueryEngine::new(dir.path());
        let r = engine.tag_delete("|foo", true, |_, _| {});
        assert!(r.is_err());
        assert!(format!("{:#}", r.unwrap_err()).contains("not supported"));
    }

    // ── tag fix-unicode tests ───────────────────────────────

    /// The original repro: a tag stored as NFD ("O" + combining diaeresis)
    /// looks identical to NFC ("Ö") but is a different string. After
    /// fix-unicode, the asset's tag list is byte-equal to NFC.
    #[test]
    fn tag_fix_unicode_normalises_nfd_to_nfc() {
        let nfd = "\u{004F}\u{0308}-HA";
        let nfc = "\u{00D6}-HA";
        let (dir, asset_id) = setup_tag_rename_catalog(&[nfd]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_fix_unicode(true, |_, _| {}).unwrap();
        assert_eq!(result.fixed, 1);
        assert_eq!(result.tags_normalized, 1);
        assert_eq!(result.merged, 0);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert_eq!(asset.tags, vec![nfc.to_string()]);
    }

    /// Asset has both NFC and NFD forms of the same logical tag — dedup
    /// collapses to one. `merged` counter tracks this for reporting.
    #[test]
    fn tag_fix_unicode_dedups_mixed_encoding() {
        let nfd = "\u{004F}\u{0308}-HA";
        let nfc = "\u{00D6}-HA";
        let (dir, asset_id) = setup_tag_rename_catalog(&[nfd, nfc, "sunset"]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_fix_unicode(true, |_, _| {}).unwrap();
        assert_eq!(result.fixed, 1);
        assert_eq!(result.merged, 1);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        // NFD entry rewrites to NFC, then dedup against the existing NFC
        // entry collapses to one. `sunset` is unaffected.
        assert_eq!(asset.tags.len(), 2);
        assert!(asset.tags.contains(&nfc.to_string()));
        assert!(asset.tags.contains(&"sunset".to_string()));
    }

    /// Pure-ASCII catalog: nothing changes, no writes happen.
    #[test]
    fn tag_fix_unicode_skips_already_canonical() {
        let (dir, asset_id) = setup_tag_rename_catalog(&[
            "subject|nature|landscape",
            "sunset",
        ]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_fix_unicode(true, |_, _| {}).unwrap();
        assert_eq!(result.scanned, 1);
        assert_eq!(result.fixed, 0);
        assert_eq!(result.tags_normalized, 0);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        assert_eq!(asset.tags.len(), 2);
    }

    /// Dry-run reports counts but doesn't persist.
    #[test]
    fn tag_fix_unicode_dry_run_does_not_persist() {
        let nfd = "\u{004F}\u{0308}-HA";
        let (dir, asset_id) = setup_tag_rename_catalog(&[nfd]);
        let engine = QueryEngine::new(dir.path());
        let result = engine.tag_fix_unicode(false, |_, _| {}).unwrap();
        assert_eq!(result.fixed, 1);
        assert!(result.dry_run);

        let store = crate::metadata_store::MetadataStore::new(dir.path());
        let asset: crate::models::Asset = store.load(asset_id.parse().unwrap()).unwrap();
        // NFD is still on disk — dry run was a no-op for storage.
        assert_eq!(asset.tags, vec![nfd.to_string()]);
    }
}
