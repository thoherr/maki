//! Scalar field setters of `QueryEngine`: `edit`, `set_name`, `set_date`,
//! `set_rating`, `set_color_label`, `set_description`, the preview
//! overrides (`set_preview_rotation`, `set_preview_variant`),
//! `set_variant_role`, and the batch entry points.
//!
//! Every setter is one [`QueryEngine::mutate_asset`] call: the closure
//! edits the `Asset`, reports whether anything changed, and attaches the
//! XMP write-back as the post-commit side effect. The public entry
//! points own the operation boundary (recording context → edit →
//! `commit_recorder`), so a single edit and a batch journal the same way.

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::context::Mutation;
use super::{EditFields, EditResult, MutationContext, QueryEngine};

impl QueryEngine {
    // ═══ EDIT & SET FIELDS ═══

    /// Edit asset metadata (name, description, rating, label, date) in
    /// one operation. Updates both sidecar YAML and SQLite, writes the
    /// XMP-backed fields back, and journals the edit as one operation.
    pub fn edit(&self, asset_id_prefix: &str, fields: EditFields) -> Result<EditResult> {
        let ctx = self.recording_context()?;
        let ctx = &ctx;
        let (result, summary) = self.mutate_asset(ctx, asset_id_prefix, |asset| {
            let before = (
                asset.name.clone(),
                asset.description.clone(),
                asset.rating,
                asset.color_label.clone(),
                asset.created_at,
            );

            if let Some(name) = &fields.name {
                asset.name = name.clone();
            }
            if let Some(description) = &fields.description {
                // Normalize empty string to None (clear)
                asset.description = description
                    .as_ref()
                    .filter(|s| !s.is_empty())
                    .cloned();
            }
            if let Some(rating) = &fields.rating {
                asset.rating = *rating;
            }
            if let Some(label) = &fields.color_label {
                asset.color_label = label.clone();
            }
            if let Some(date) = &fields.created_at {
                match date {
                    Some(dt) => asset.created_at = *dt,
                    None => asset.created_at = Utc::now(),
                }
            }

            // Summarise from the fields that actually changed.
            let mut parts: Vec<String> = Vec::new();
            if before.2 != asset.rating {
                parts.push(match asset.rating {
                    Some(r) => format!("rating {r}"),
                    None => "cleared rating".to_string(),
                });
            }
            if before.3 != asset.color_label {
                parts.push("label".to_string());
            }
            if before.0 != asset.name {
                parts.push("name".to_string());
            }
            if before.1 != asset.description {
                parts.push("description".to_string());
            }
            if before.4 != asset.created_at {
                parts.push("date".to_string());
            }
            let changed = !parts.is_empty();
            let summary = format!("edited {}", parts.join(", "));

            let result = EditResult {
                asset_id: asset.id.to_string(),
                name: asset.name.clone(),
                description: asset.description.clone(),
                rating: asset.rating,
                color_label: asset.color_label.clone(),
                created_at: asset.created_at.to_rfc3339(),
            };
            let rating = fields.rating;
            let description = fields.description.clone();
            let label = fields.color_label.clone();
            Ok(Mutation::new(changed, (result, summary)).then(move |asset| {
                if rating.is_some() {
                    let rating = asset.rating;
                    self.write_back_rating_to_xmp(asset, rating, ctx);
                }
                if description.is_some() {
                    let desc = asset.description.clone();
                    self.write_back_description_to_xmp(asset, desc.as_deref(), ctx);
                }
                if label.is_some() {
                    let label = asset.color_label.clone();
                    self.write_back_label_to_xmp(asset, label.as_deref(), ctx);
                }
            }))
        })?;
        self.commit_recorder(ctx, "edit", &summary);
        Ok(result)
    }

    /// Set the name on an asset. Updates both sidecar YAML and SQLite catalog.
    /// No XMP write-back needed — name has no XMP equivalent.
    /// Returns the new name value.
    pub fn set_name(
        &self,
        asset_id_prefix: &str,
        name: Option<String>,
    ) -> Result<Option<String>> {
        let ctx = self.recording_context()?;
        let summary = match &name {
            Some(n) => format!("set name to {n}"),
            None => "cleared name".to_string(),
        };
        let result = self.mutate_asset(&ctx, asset_id_prefix, move |asset| {
            let changed = asset.name != name;
            asset.name = name;
            Ok(Mutation::new(changed, asset.name.clone()))
        });
        self.commit_recorder(&ctx, "name", &summary);
        result
    }

    /// Set the date on an asset. Updates both sidecar YAML and SQLite catalog.
    /// No XMP write-back needed — date has no XMP equivalent in our workflow.
    /// Returns the new date as an RFC 3339 string.
    pub fn set_date(&self, asset_id_prefix: &str, date: DateTime<Utc>) -> Result<String> {
        let ctx = self.recording_context()?;
        let result = self.mutate_asset(&ctx, asset_id_prefix, |asset| {
            let changed = asset.created_at != date;
            asset.created_at = date;
            Ok(Mutation::new(changed, date.to_rfc3339()))
        });
        self.commit_recorder(&ctx, "date", &format!("set date to {}", date.to_rfc3339()));
        result
    }

    /// Set the rating on an asset. Updates both sidecar YAML and SQLite catalog.
    /// Also writes back the rating to any `.xmp` recipe files on disk.
    /// Returns the new rating value.
    pub fn set_rating(&self, asset_id_prefix: &str, rating: Option<u8>) -> Result<Option<u8>> {
        let ctx = self.recording_context()?;
        let result = self.set_rating_inner(&ctx, asset_id_prefix, rating);
        let summary = match rating {
            Some(r) => format!("set rating to {r}"),
            None => "cleared rating".to_string(),
        };
        self.commit_recorder(&ctx, "rating", &summary);
        result
    }

    pub(super) fn set_rating_inner(
        &self,
        ctx: &MutationContext<'_>,
        asset_id_prefix: &str,
        rating: Option<u8>,
    ) -> Result<Option<u8>> {
        self.mutate_asset(ctx, asset_id_prefix, |asset| {
            let changed = asset.rating != rating;
            asset.rating = rating;
            Ok(Mutation::new(changed, rating)
                .then(move |asset| self.write_back_rating_to_xmp(asset, rating, ctx)))
        })
    }

    /// Set the color label on an asset. Updates both sidecar YAML and SQLite catalog.
    /// Also writes back the label to any `.xmp` recipe files on disk.
    /// Returns the new label value.
    pub fn set_color_label(&self, asset_id_prefix: &str, label: Option<String>) -> Result<Option<String>> {
        let ctx = self.recording_context()?;
        let result = self.set_color_label_inner(&ctx, asset_id_prefix, label.clone());
        let summary = match &label {
            Some(l) => format!("set label to {l}"),
            None => "cleared label".to_string(),
        };
        self.commit_recorder(&ctx, "label", &summary);
        result
    }

    pub(super) fn set_color_label_inner(
        &self,
        ctx: &MutationContext<'_>,
        asset_id_prefix: &str,
        label: Option<String>,
    ) -> Result<Option<String>> {
        self.mutate_asset(ctx, asset_id_prefix, move |asset| {
            let changed = asset.color_label != label;
            asset.color_label = label.clone();
            Ok(Mutation::new(changed, label.clone()).then(move |asset| {
                self.write_back_label_to_xmp(asset, label.as_deref(), ctx)
            }))
        })
    }

    /// Set the description on an asset. Updates both sidecar YAML and SQLite catalog.
    /// Also writes back the description to any `.xmp` recipe files on disk.
    /// Returns the new description value.
    pub fn set_description(
        &self,
        asset_id_prefix: &str,
        description: Option<String>,
    ) -> Result<Option<String>> {
        let ctx = self.recording_context()?;
        let summary = match &description {
            Some(_) => "set description".to_string(),
            None => "cleared description".to_string(),
        };
        let ctx = &ctx;
        let result = self.mutate_asset(ctx, asset_id_prefix, move |asset| {
            let changed = asset.description != description;
            asset.description = description.clone();
            Ok(Mutation::new(changed, description.clone()).then(move |asset| {
                self.write_back_description_to_xmp(asset, description.as_deref(), ctx)
            }))
        });
        self.commit_recorder(ctx, "description", &summary);
        result
    }

    /// Set the preview rotation override on an asset. Updates both sidecar YAML and SQLite catalog.
    /// Returns the new rotation value.
    pub fn set_preview_rotation(
        &self,
        asset_id_prefix: &str,
        rotation: Option<u16>,
    ) -> Result<Option<u16>> {
        let ctx = self.batch_context_fast()?;
        self.mutate_asset(&ctx, asset_id_prefix, |asset| {
            let changed = asset.preview_rotation != rotation;
            asset.preview_rotation = rotation;
            Ok(Mutation::new(changed, rotation))
        })
    }

    /// Set the preview variant override on an asset. Updates both sidecar YAML and SQLite catalog.
    /// Pass `None` to clear the override and revert to algorithmic selection.
    /// `best_variant_hash` follows the override (or the rule when cleared).
    pub fn set_preview_variant(
        &self,
        asset_id_prefix: &str,
        content_hash: Option<&str>,
    ) -> Result<()> {
        let ctx = self.batch_context_fast()?;
        self.mutate_asset(&ctx, asset_id_prefix, |asset| {
            // Validate that the content_hash belongs to this asset
            if let Some(hash) = content_hash {
                if !asset.variants.iter().any(|v| v.content_hash == hash) {
                    anyhow::bail!("variant {hash} does not belong to asset {}", asset.id);
                }
            }
            let new_value = content_hash.map(|s| s.to_string());
            let changed = asset.preview_variant != new_value;
            asset.preview_variant = new_value;
            Ok(Mutation::new(changed, ()))
        })
    }

    /// Change a variant's role. Updates both sidecar YAML and SQLite catalog.
    /// Also updates denormalized columns (primary_format, best_variant_hash).
    pub fn set_variant_role(
        &self,
        asset_id_prefix: &str,
        variant_hash: &str,
        role: &str,
    ) -> Result<()> {
        let valid_roles = ["original", "alternate", "processed", "export", "sidecar"];
        let role_lower = role.to_lowercase();
        if !valid_roles.contains(&role_lower.as_str()) {
            anyhow::bail!(
                "Invalid role '{role}'. Valid roles: {}",
                valid_roles.join(", ")
            );
        }
        // Validated against the role list above; `parse` mirrors serde.
        let variant_role: crate::models::VariantRole =
            role_lower.parse().map_err(|e: String| anyhow::anyhow!(e))?;

        let ctx = self.batch_context_fast()?;
        self.mutate_asset(&ctx, asset_id_prefix, |asset| {
            // Verify variant belongs to this asset
            let Some(v) = asset.variants.iter_mut().find(|v| v.content_hash == variant_hash) else {
                anyhow::bail!("variant {variant_hash} does not belong to asset {}", asset.id);
            };
            let changed = v.role != variant_role;
            v.role = variant_role;
            // The role feeds the preview rule: the `assets` row written by
            // `mutate_asset` refreshes best_variant_hash (the grid reads it
            // directly); the variant's own row goes in the same transaction.
            Ok(Mutation::new(changed, ())
                .rows(move |c| c.update_variant_role(variant_hash, &role_lower)))
        })
    }

    // ═══ BATCH METHODS ═══

    /// Set rating on multiple assets using a single shared catalog
    /// connection. The whole batch is journaled as ONE undoable operation.
    pub fn batch_set_rating(&self, asset_ids: &[String], rating: Option<u8>) -> Vec<Result<Option<u8>>> {
        let ctx = match self.batch_recording_context() {
            Ok(c) => c,
            Err(e) => return asset_ids.iter().map(|_| Err(anyhow::anyhow!("{e:#}"))).collect(),
        };
        let results: Vec<_> = asset_ids.iter().map(|id| self.set_rating_inner(&ctx, id, rating)).collect();
        let n = ctx.recorder.as_ref().map(|c| c.borrow().len()).unwrap_or(0);
        let summary = match rating {
            Some(r) => format!("set rating to {r} on {n} asset(s)"),
            None => format!("cleared rating on {n} asset(s)"),
        };
        self.commit_recorder(&ctx, "rating", &summary);
        results
    }

    /// Set color label on multiple assets using a single shared catalog
    /// connection. The whole batch is journaled as ONE undoable operation.
    pub fn batch_set_color_label(&self, asset_ids: &[String], label: Option<String>) -> Vec<Result<Option<String>>> {
        let ctx = match self.batch_recording_context() {
            Ok(c) => c,
            Err(e) => return asset_ids.iter().map(|_| Err(anyhow::anyhow!("{e:#}"))).collect(),
        };
        let results: Vec<_> = asset_ids.iter().map(|id| self.set_color_label_inner(&ctx, id, label.clone())).collect();
        let n = ctx.recorder.as_ref().map(|c| c.borrow().len()).unwrap_or(0);
        let summary = match &label {
            Some(l) => format!("set label to {l} on {n} asset(s)"),
            None => format!("cleared label on {n} asset(s)"),
        };
        self.commit_recorder(&ctx, "label", &summary);
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::query::test_support::*;

    #[test]
    fn set_variant_role_refreshes_best_variant_hash() {
        let (dir, hash1, hash2, id1, _id2) = setup_group_env();
        let engine = QueryEngine::new(dir.path());
        engine.group(&[hash1.clone(), hash2.clone()]).unwrap();

        let catalog = Catalog::open(dir.path()).unwrap();
        assert_eq!(
            catalog.get_asset_best_variant_hash(&id1).unwrap().as_deref(),
            Some(hash2.as_str()),
            "JPEG export represents the ARW+JPEG pair"
        );
        // Sabotage the stored column, then change a role: the editor must
        // refresh the denormalized value, not just the role.
        catalog
            .conn()
            .execute(
                "UPDATE assets SET best_variant_hash = ?1 WHERE id = ?2",
                rusqlite::params![hash1, id1],
            )
            .unwrap();
        engine.set_variant_role(&id1, &hash2, "processed").unwrap();
        assert_eq!(
            catalog.get_asset_best_variant_hash(&id1).unwrap().as_deref(),
            Some(hash2.as_str())
        );
        let details = catalog.load_asset_details(&id1).unwrap().unwrap();
        let v = &details.variants[details.best_variant_index().unwrap()];
        assert_eq!(v.content_hash, hash2);
        assert_eq!(v.role, "processed");
    }

    #[test]
    fn clearing_preview_override_recomputes_with_the_shared_rule() {
        let (dir, hash1, hash2, id1, _id2) = setup_group_env();
        let engine = QueryEngine::new(dir.path());
        engine.group(&[hash1.clone(), hash2.clone()]).unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        // Override to the ARW, then clear: the JPEG must come back (format-first rule).
        engine.set_preview_variant(&id1, Some(&hash1)).unwrap();
        assert_eq!(catalog.get_asset_best_variant_hash(&id1).unwrap().as_deref(), Some(hash1.as_str()));
        engine.set_preview_variant(&id1, None).unwrap();
        assert_eq!(catalog.get_asset_best_variant_hash(&id1).unwrap().as_deref(), Some(hash2.as_str()));
    }

    #[test]
    fn set_name_date_description_join_the_history_journal() {
        let (dir, asset_id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());
        engine.set_name(&asset_id, Some("Sunset".into())).unwrap();
        engine.set_date(&asset_id, "2024-06-15T12:00:00Z".parse().unwrap()).unwrap();
        engine.set_description(&asset_id, Some("golden hour".into())).unwrap();
        // Identical values again: no-ops, no operations.
        engine.set_name(&asset_id, Some("Sunset".into())).unwrap();
        engine.set_description(&asset_id, Some("golden hour".into())).unwrap();

        let ops = crate::history::HistoryStore::from_config(dir.path()).list(None).unwrap();
        let commands: Vec<&str> = ops.iter().map(|o| o.command.as_str()).collect();
        assert_eq!(commands, ["description", "date", "name"]);
        let asset = load_asset(dir.path(), &asset_id);
        assert_eq!(asset.name.as_deref(), Some("Sunset"));
        assert_eq!(asset.description.as_deref(), Some("golden hour"));
    }
}
