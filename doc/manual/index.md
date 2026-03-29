# MAKI User Manual

**MAKI** is a command-line digital asset manager built in Rust, designed for photographers and media professionals who manage large collections across multiple storage devices.

This manual is organized into three sections:

## User Guide

Workflow-oriented guides that walk you through common tasks.

1. [Overview & Concepts](user-guide/01-overview.md) — Data model, architecture, and the round-trip workflow
2. [Setup](user-guide/02-setup.md) — Installation, initialization, volumes, and configuration
3. [Ingesting Assets](user-guide/03-ingest.md) — Importing files, auto-grouping, metadata extraction, and previews
4. [Organizing Assets](user-guide/04-organize.md) — Tags, editing, grouping, collections, and saved searches
5. [Browsing & Searching](user-guide/05-browse-and-search.md) — CLI search, filters, output formats, and statistics
6. [Web UI](user-guide/06-web-ui.md) — Browser interface, batch operations, and keyboard navigation
7. [Maintenance](user-guide/07-maintenance.md) — Verification, sync, refresh, cleanup, and relocation
8. [Scripting](user-guide/08-scripting.md) — Shell and Python scripting patterns, workflow automation
9. [Interactive Shell](user-guide/09-shell.md) — Variables, tab completion, script files, and session management
10. [Organizing & Culling](user-guide/10-organizing-and-culling.md) — Rating vs. curation, default filters, and workflow patterns
11. [The Archive Lifecycle](user-guide/11-archive-lifecycle.md) — Storage strategy, backup workflows, and long-term library management

## Reference Guide

Man-page style documentation for every command, filter, and configuration option.

- [CLI Conventions](reference/00-cli-conventions.md) — Global flags, scripting patterns, exit codes
- [Setup Commands](reference/01-setup-commands.md) — `init`, `volume add`, `volume list`, `volume combine`, `volume remove`
- [Ingest Commands](reference/02-ingest-commands.md) — `import`, `delete`, `tag`, `edit`, `group`, `split`, `auto-group`, `auto-tag`, `embed`, `describe`
- [Organize Commands](reference/03-organize-commands.md) — `collection`, `saved-search`, `stack`, `faces`
- [Retrieve Commands](reference/04-retrieve-commands.md) — `search`, `show`, `preview`, `export`, `contact-sheet`, `duplicates`, `stats`, `backup-status`, `serve`, `shell`
- [Maintain Commands](reference/05-maintain-commands.md) — `verify`, `sync`, `refresh`, `sync-metadata`, `writeback`, `cleanup`, `dedup`, `relocate`, `update-location`, `generate-previews`, `fix-roles`, `fix-dates`, `fix-recipes`, `create-sidecars`, `rebuild-catalog`, `migrate`
- [Search Filters](reference/06-search-filters.md) — Complete filter syntax reference
- [Format Templates](reference/07-format-templates.md) — Output format presets, custom templates, placeholders
- [Configuration](reference/08-configuration.md) — `maki.toml` reference
- [Data Model](reference/09-data-model.md) — Asset, Variant, Recipe, Volume, and FileLocation entities
- [VLM Model Guide](reference/10-vlm-models.md) — Vision-language models for `maki describe`: tested models, backends, hardware guide

\newpage

## Developer Guide

Technical documentation for integrators and contributors.

1. [REST API](developer/01-rest-api.md) — Complete web API documentation
2. [Module Reference](developer/02-module-reference.md) — Rust module overview and dependency graph
3. [Building & Testing](developer/03-building-and-testing.md) — Build commands, tests, and release process

---

**Version**: v4.2.2 | **Source**: [GitHub](https://github.com/thoherr/maki) | **License**: Apache-2.0
