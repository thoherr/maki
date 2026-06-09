//! `maki volume` — volume registration and management.

use super::*;

/// Extracted body of `Commands::Volume`. Inner match dispatches to the
/// `VolumeCommands` subcommand variants.
pub fn run_volume_command(
    cmd: VolumeCommands,
    json: bool,
    log: bool,
    #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let _ = verbosity;
    match cmd {
    VolumeCommands::Add { args, purpose } => {
        // Two positional args: LABEL PATH. One arg: PATH (label derived).
        let (label, path) = if args.len() == 2 {
            (args[0].clone(), args[1].clone())
        } else {
            let path = &args[0];
            let label = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Volume")
                .to_string();
            (label, path.clone())
        };

        let catalog_root = maki::config::find_catalog_root()?;
        let registry = DeviceRegistry::new(&catalog_root);
        let parsed_purpose = if let Some(ref p) = purpose {
            Some(maki::models::VolumePurpose::parse(p).ok_or_else(|| {
                anyhow::anyhow!("invalid purpose '{}'. Valid values: media, working, archive, backup, cloud", p)
            })?)
        } else {
            None
        };
        let volume = registry.register(
            &label,
            std::path::Path::new(&path),
            maki::models::VolumeType::Local,
            parsed_purpose,
        )?;
        if cli.json {
            println!("{}", serde_json::json!({
                "id": volume.id.to_string(),
                "label": volume.label,
                "path": volume.mount_point.display().to_string(),
                "purpose": volume.purpose.as_ref().map(|p| p.as_str()),
            }));
        } else {
            println!("Registered volume '{}' ({})", volume.label, volume.id);
            println!("  Path: {}", volume.mount_point.display());
            if let Some(ref p) = volume.purpose {
                println!("  Purpose: {}", p);
            } else {
                eprintln!("  Hint: use --purpose <media|working|archive|backup|cloud> to set the volume's role");
            }
        }
        Ok(())
    }
    VolumeCommands::List { purpose, offline, online } => {
        if offline && online {
            anyhow::bail!("--offline and --online are mutually exclusive");
        }
        let purpose_filter = if let Some(ref p) = purpose {
            Some(maki::models::VolumePurpose::parse(p).ok_or_else(|| {
                anyhow::anyhow!("invalid purpose '{}'. Valid values: media, working, archive, backup, cloud", p)
            })?)
        } else {
            None
        };

        let catalog_root = maki::config::find_catalog_root()?;
        let registry = DeviceRegistry::new(&catalog_root);
        let volumes: Vec<_> = registry.list()?.into_iter().filter(|v| {
            if let Some(ref pf) = purpose_filter {
                if v.purpose.as_ref() != Some(pf) {
                    return false;
                }
            }
            if offline && v.is_online { return false; }
            if online && !v.is_online { return false; }
            true
        }).collect();

        if cli.json {
            let json_volumes: Vec<serde_json::Value> = volumes.iter().map(|v| {
                serde_json::json!({
                    "id": v.id.to_string(),
                    "label": v.label,
                    "path": v.mount_point.display().to_string(),
                    "volume_type": format!("{:?}", v.volume_type).to_lowercase(),
                    "purpose": v.purpose.as_ref().map(|p| p.as_str()),
                    "is_online": v.is_online,
                })
            }).collect();
            println!("{}", serde_json::to_string_pretty(&json_volumes)?);
        } else if volumes.is_empty() {
            if purpose.is_some() || offline || online {
                println!("No matching volumes.");
            } else {
                println!("No volumes registered.");
            }
        } else {
            for v in &volumes {
                let status = if v.is_online { "online" } else { "offline" };
                let purpose_tag = v.purpose.as_ref()
                    .map(|p| format!(" [{}]", p))
                    .unwrap_or_default();
                println!("{} ({}) [{}]{}", v.label, v.id, status, purpose_tag);
                println!("  Path: {}", v.mount_point.display());
            }
        }
        Ok(())
    }
    VolumeCommands::SetPurpose { volume, purpose } => {
        let catalog_root = maki::config::find_catalog_root()?;
        let registry = DeviceRegistry::new(&catalog_root);
        let parsed_purpose = if purpose == "none" || purpose == "clear" {
            None
        } else {
            Some(maki::models::VolumePurpose::parse(&purpose).ok_or_else(|| {
                anyhow::anyhow!("invalid purpose '{}'. Valid values: media, working, archive, backup, cloud, none", purpose)
            })?)
        };
        let vol = registry.set_purpose(&volume, parsed_purpose)?;
        // Update the SQLite cache too
        let catalog = maki::catalog::Catalog::open(&catalog_root)?;
        catalog.ensure_volume(&vol)?;
        if cli.json {
            println!("{}", serde_json::json!({
                "id": vol.id.to_string(),
                "label": vol.label,
                "purpose": vol.purpose.as_ref().map(|p| p.as_str()),
            }));
        } else if let Some(ref p) = vol.purpose {
            println!("Volume '{}' purpose set to: {}", vol.label, p);
        } else {
            println!("Volume '{}' purpose cleared.", vol.label);
        }
        Ok(())
    }
    VolumeCommands::Remove { volume, apply } => {
        let (catalog_root, config) = maki::config::load_config()?;
        let service = AssetService::new(&catalog_root, verbosity, &config.preview);

        let show_log = cli.log;
        let result = if show_log {
            use maki::asset_service::CleanupStatus;
            service.remove_volume(
                &volume,
                apply,
                |path, status, elapsed| {
                    match status {
                        CleanupStatus::Stale => {
                            let name = path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                            item_status(name, "removed", Some(elapsed));
                        }
                        CleanupStatus::LocationlessVariant => {
                            let name = path.to_str().unwrap_or("?");
                            item_status(name, "locationless variant removed", Some(elapsed));
                        }
                        CleanupStatus::OrphanedAsset => {
                            let name = path.to_str().unwrap_or("?");
                            item_status(name, "orphaned asset removed", Some(elapsed));
                        }
                        CleanupStatus::OrphanedFile => {
                            let name = path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                            item_status(name, "orphaned file removed", Some(elapsed));
                        }
                        _ => {}
                    }
                },
            )?
        } else {
            service.remove_volume(
                &volume,
                apply,
                |_, _, _| {},
            )?
        };

        if cli.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            for err in &result.errors {
                eprintln!("  {err}");
            }

            if apply {
                let mut parts = vec![
                    format!("{} locations removed", result.locations_removed),
                    format!("{} recipes removed", result.recipes_removed),
                ];
                if result.removed_assets > 0 {
                    parts.push(format!("{} orphaned assets removed", result.removed_assets));
                }
                if result.removed_previews > 0 {
                    parts.push(format!("{} orphaned previews removed", result.removed_previews));
                }
                println!("Volume '{}' removed: {}", result.volume_label, parts.join(", "));
            } else {
                let mut parts = vec![
                    format!("{} locations", result.locations),
                    format!("{} recipes", result.recipes),
                ];
                if result.orphaned_assets > 0 {
                    parts.push(format!("{} orphaned assets", result.orphaned_assets));
                }
                if result.orphaned_previews > 0 {
                    parts.push(format!("{} orphaned previews", result.orphaned_previews));
                }
                println!("Volume '{}' would remove: {}", result.volume_label, parts.join(", "));
                if result.locations > 0 || result.recipes > 0 {
                    println!("  Run with --apply to remove.");
                }
            }
        }
        Ok(())
    }
    VolumeCommands::Combine { source, target, apply } => {
        let (catalog_root, config) = maki::config::load_config()?;
        let service = AssetService::new(&catalog_root, verbosity, &config.preview);

        let show_log = cli.log;
        let result = service.combine_volume(
            &source,
            &target,
            apply,
            |asset_id, elapsed| {
                if show_log {
                    item_status(asset_id, "updated", Some(elapsed));
                }
            },
        )?;

        if cli.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            for err in &result.errors {
                eprintln!("  {err}");
            }

            if apply {
                println!(
                    "Volume '{}' combined into '{}': {} locations moved, {} recipes moved ({} assets, prefix '{}')",
                    result.source_label,
                    result.target_label,
                    result.locations_moved,
                    result.recipes_moved,
                    result.assets_affected,
                    result.path_prefix,
                );
            } else {
                println!(
                    "Would combine '{}' into '{}': {} locations, {} recipes ({} assets, prefix '{}')",
                    result.source_label,
                    result.target_label,
                    result.locations,
                    result.recipes,
                    result.assets_affected,
                    result.path_prefix,
                );
                if result.locations > 0 || result.recipes > 0 {
                    println!("  Run with --apply to combine.");
                }
            }
        }
        Ok(())
    }
    VolumeCommands::Split { source, new_label, path, purpose, apply } => {
        let (catalog_root, config) = maki::config::load_config()?;
        let service = AssetService::new(&catalog_root, verbosity, &config.preview);

        let show_log = cli.log;
        let result = service.split_volume(
            &source,
            &new_label,
            &path,
            purpose.as_deref(),
            apply,
            |asset_id, elapsed| {
                if show_log {
                    item_status(asset_id, "updated", Some(elapsed));
                }
            },
        )?;

        if cli.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            for err in &result.errors {
                eprintln!("  {err}");
            }

            if apply {
                println!(
                    "Volume '{}' split: new volume '{}' created with {} locations, {} recipes ({} assets, prefix '{}')",
                    result.source_label,
                    result.new_label,
                    result.locations_moved,
                    result.recipes_moved,
                    result.assets_affected,
                    result.path_prefix,
                );
            } else {
                println!(
                    "Would split '{}': new volume '{}' with {} locations, {} recipes ({} assets, prefix '{}')",
                    result.source_label,
                    result.new_label,
                    result.locations,
                    result.recipes,
                    result.assets_affected,
                    result.path_prefix,
                );
                if result.locations > 0 || result.recipes > 0 {
                    println!("  Run with --apply to split.");
                }
            }
        }
        Ok(())
    }
    VolumeCommands::Rename { volume, new_label } => {
        let catalog_root = maki::config::find_catalog_root()?;
        let registry = DeviceRegistry::new(&catalog_root);
        let vol = registry.resolve_volume(&volume)?;
        let old_label = vol.label.clone();

        registry.rename(&volume, &new_label)?;

        let catalog = maki::catalog::Catalog::open(&catalog_root)?;
        catalog.rename_volume(&vol.id.to_string(), &new_label)?;

        if cli.json {
            println!("{}", serde_json::json!({
                "old_label": old_label,
                "new_label": new_label,
                "volume_id": vol.id.to_string(),
            }));
        } else {
            println!("Volume '{}' renamed to '{}'", old_label, new_label);
        }
        Ok(())
    }
    }
}
