//! Creation of `.xmp` sidecar files for variants that have no XMP recipe
//! yet. Shared by `maki create-sidecars`, `maki relocate --create-sidecars`
//! and the `[writeback] create_sidecars` auto-create path, so the naming
//! rule (`[writeback] sidecar_naming`), the "already has a sidecar" checks
//! and the recipe registration exist exactly once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use uuid::Uuid;

use crate::catalog::Catalog;
use crate::config::SidecarNaming;
use crate::content_store::ContentStore;
use crate::models::recipe::{Recipe, RecipeType};
use crate::models::{Asset, FileLocation};

/// Per-file outcome reported through the `on_file` callback of
/// [`create_missing_sidecars`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarStatus {
    /// File written and registered as a recipe.
    Created,
    /// Report-only run: the file would be created.
    WouldCreate,
    /// A file already exists at the sidecar path but the catalog does not
    /// know it (typically written by Capture One / Lightroom and not yet
    /// imported). Left untouched — `maki import` attaches it.
    ExistsUnregistered,
}

/// Counters returned by [`create_missing_sidecars`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SidecarCreation {
    /// Files created (or, in report-only mode, that would be created).
    pub created: usize,
    /// Sidecar paths that already hold an unregistered file.
    pub exists_unregistered: usize,
}

/// Whether an asset carries any metadata worth materialising in a sidecar.
pub fn asset_has_sidecar_metadata(asset: &Asset) -> bool {
    !asset.tags.is_empty()
        || asset.rating.is_some()
        || asset.color_label.is_some()
        || asset.description.is_some()
}

/// Create `.xmp` sidecars for every variant location of `asset` on an
/// online volume that has no XMP recipe yet.
///
/// - `online` maps volume id → mount point for the volumes to consider.
/// - `volume_filter` restricts creation to one volume.
/// - `naming` decides between `DSC.xmp` and `DSC.NEF.xmp`.
/// - `apply = false` only reports.
///
/// Skips, per location: variants that already have an `.xmp` recipe on
/// that volume; sidecar paths already registered as a recipe of this
/// asset (a RAW+JPEG pair shares one `DSC.xmp` under the `stem` scheme,
/// so the second variant must not create a duplicate); and paths where
/// an unregistered file already exists on disk (never overwritten).
///
/// Created recipes are inserted into the catalog and pushed onto
/// `asset.recipes`; the caller saves the YAML sidecar when
/// `SidecarCreation::created > 0` and `apply` was set.
#[allow(clippy::too_many_arguments)]
pub fn create_missing_sidecars(
    asset: &mut Asset,
    catalog: &Catalog,
    content_store: &ContentStore,
    online: &HashMap<Uuid, PathBuf>,
    volume_filter: Option<Uuid>,
    naming: SidecarNaming,
    apply: bool,
    mut on_file: impl FnMut(&Path, SidecarStatus),
) -> Result<SidecarCreation> {
    let mut outcome = SidecarCreation::default();
    if !asset_has_sidecar_metadata(asset) {
        return Ok(outcome);
    }

    // (variant hash, volume id, sidecar relative path, absolute path)
    let mut candidates: Vec<(String, Uuid, PathBuf, PathBuf)> = Vec::new();
    let mut claimed: HashSet<(Uuid, PathBuf)> = HashSet::new();

    for variant in &asset.variants {
        for loc in &variant.locations {
            if volume_filter.is_some_and(|vf| vf != loc.volume_id) {
                continue;
            }
            let Some(mount_point) = online.get(&loc.volume_id) else {
                continue;
            };
            let xmp_relative = naming.sidecar_path(&loc.relative_path);

            let blocked = asset.recipes.iter().any(|r| {
                r.location.volume_id == loc.volume_id
                    && (r.location.relative_path == xmp_relative
                        || (r.variant_hash == variant.content_hash && is_xmp_recipe(r)))
            });
            if blocked {
                continue;
            }
            if !claimed.insert((loc.volume_id, xmp_relative.clone())) {
                continue;
            }
            let xmp_path = mount_point.join(&xmp_relative);
            candidates.push((variant.content_hash.clone(), loc.volume_id, xmp_relative, xmp_path));
        }
    }

    for (variant_hash, volume_id, xmp_relative, xmp_path) in candidates {
        if xmp_path.exists() {
            outcome.exists_unregistered += 1;
            on_file(&xmp_path, SidecarStatus::ExistsUnregistered);
            continue;
        }
        if !apply {
            outcome.created += 1;
            on_file(&xmp_path, SidecarStatus::WouldCreate);
            continue;
        }

        if let Some(parent) = xmp_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let xmp_content = crate::xmp_reader::create_xmp(
            &asset.tags,
            asset.rating,
            asset.color_label.as_deref(),
            asset.description.as_deref(),
        );
        std::fs::write(&xmp_path, &xmp_content)?;
        let xmp_hash = content_store.hash_file(&xmp_path)?;

        let recipe = Recipe {
            id: Uuid::new_v4(),
            variant_hash,
            software: "MAKI".to_string(),
            recipe_type: RecipeType::Sidecar,
            content_hash: xmp_hash,
            location: FileLocation {
                volume_id,
                relative_path: xmp_relative,
                verified_at: None,
            },
            pending_writeback: false,
        };
        catalog.insert_recipe(&recipe)?;
        asset.recipes.push(recipe);
        outcome.created += 1;
        on_file(&xmp_path, SidecarStatus::Created);
    }

    Ok(outcome)
}

fn is_xmp_recipe(recipe: &Recipe) -> bool {
    recipe
        .location
        .relative_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
}
