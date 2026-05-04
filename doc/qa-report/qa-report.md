# MAKI QA Report — 2026-05-03

State at v4.4.14 (post-release). Baseline: 63 321 Rust LOC across ~80 files, 12 051 template LOC. Tests at this baseline: 779 lib + 249 CLI + 14 doc on standard, 886 + 273 + 14 on pro.

This report focuses on **what's left** after the substantial extraction work landed in v4.4.5 (web/routes split 6599 → 348 LOC across 13 submodules, `cli_output.rs`, `Volume::online_map`, `resolve_collection_ids`) and v4.4.13–14 (`run_faces_command` lifted, `build_search_where` decomposed, `AssetService::embed_assets` extracted, `JobRegistry` lifted).

Previous QA reports are archived under `doc/qa-report/archive/`.

## Status

- **Batch 1 (small DRY wins)**: ✅ landed in commit `6889825` (2026-05-03). Tests still 779/249/886/273. See per-item status below.
- **Batch 2 (structural splits)**: ✅ done — M1, H1 (partial), M2, H3, H2, M3+M4 across `85984f8`, `7ce8d11`, `9d24d8f`, `6262a39`, `ae3bd4d`, `6f74dec` (2026-05-03 / 2026-05-04). Tests still 779/249/886/273. Only **H1 remaining-arms** (opportunistic) and **H5 remaining-sites** (opportunistic) deferred from earlier batches.
- **Batch 3 (documentation polish)**: ✅ done in `0eb33c6` (2026-05-04). Tests still 779/249/886/273.

---

## Findings — by severity

### HIGH

| # | Finding | Citation | Notes |
|---|---------|----------|-------|
| H1 | 🟡 **PARTIAL** (`7ce8d11`, 2026-05-03) — extracted the five longest arms (Import, Tag, AutoTag, RebuildCatalog, Volume); `run_command` shrank 5921 → 4062 LOC. Remaining big arms (GeneratePreviews 281, Collection 206, Describe 197, Cleanup 193, SavedSearch 165) follow the same mechanical pattern; left for opportunistic cleanup as touched. | `src/main.rs` | — |
| H2 | ✅ **DONE** (`ae3bd4d`, 2026-05-04) — split into 17 submodules along the existing `// ═══` markers via multi-file `impl Catalog` blocks: schema, asset_crud, variant_crud, recipe_crud, volume, lookup, duplicates, recipe_query, rebuild, stats, search_builder, search_exec, facets, tags, analytics, backup, cleanup. catalog.rs went 9200 → 4524 LOC (most of which is tests). 6 cross-section helpers lifted to `pub(super)`. | `src/catalog/` | — |
| H3 | ✅ **DONE** (`6262a39`, 2026-05-04) — split into 12 submodules: `asset_service/{import,relocate,verify,sync,cleanup,volume,dedup,refresh,fix,export,ai,video}.rs`. Each is an `impl AssetService { ... }` block; no struct split, public API unchanged. asset_service.rs went 8886 → 2759 LOC (preamble + struct + ctor + free fns + tests). 3 cross-section private helpers lifted to `pub(super)`. | `src/asset_service/` | — |
| H4 | ✅ **DONE** (`6889825`) — `resolve_asset_id` boilerplate lifted into `web::routes::resolve_asset_id_or_err`; 7 sites migrated, message format unified. | `web/routes/{browse,ai,media,stacks,assets,collections}.rs` | — |
| H5 | ⚠️ **HELPER LANDED, MIGRATION OPPORTUNISTIC** (`6889825`) — `web::routes::spawn_catalog_blocking` returns `Result<T, Response>` so handlers short-circuit on `?`. 3 demo sites migrated; remaining ~100 sites left for opportunistic cleanup. Recount: 106 actual `spawn_blocking` sites across 13 files (initial 40+ estimate was low). | `web/routes/*.rs` | — |
| H6 | `main.rs` has **zero inline tests** for 8 804 LOC of CLI dispatch | `src/main.rs` | Compare: catalog.rs (121 test blocks), asset_service.rs (67), query.rs (211). The CLI integration suite (`tests/cli.rs`, 249/273 tests) covers external behaviour but not internal helpers within main. Some critical paths (CLI argument parsing edge cases, error message formatting) have no coverage. |

### MEDIUM

| # | Finding | Citation | Notes |
|---|---------|----------|-------|
| M1 | ✅ **DONE** (`85984f8`, 2026-05-03) — split into `web/routes/ai/{mod,tags,embed,similarity,faces,stroll}.rs`. Shared `resolve_model_dir` / `resolve_labels` helpers stay in `mod.rs`. | `src/web/routes/ai/` | — |
| M2 | ✅ **DONE** (`9d24d8f`, 2026-05-03) — extracted the parsing layer into `query/parse.rs` (date parser, ParsedSearch + impls, query tokenizer, parse_search_query 245-LOC dispatcher, NumericFilter, normalize_path_for_search). Public API unchanged via `pub use parse::*;`. query.rs went 6820 → 6028 LOC. The further search-impl/write-impl split the report originally suggested can follow if the file grows again. | `src/query/parse.rs` | — |
| M3 | ✅ **DONE** (`6f74dec`, 2026-05-04) — extracted 6 per-filter helpers (`add_text_filters`, `add_format_filter`, `add_volume_filter`, `add_path_filter`, `add_date_filters`, `add_geo_filters`) following the existing `add_like_filter` shape. `build_search_where` shrank 357 → 205 LOC (-43%). | `src/catalog/search_builder.rs` | — |
| M4 | ✅ **DONE** (`6f74dec`, 2026-05-04) — replaced 30 of 40+ if/else branches with four lookup tables: `SIMPLE_FILTERS`, `NUMERIC_FILTERS`, `STRING_FILTERS`, `BOOLEAN_TOKENS`. parse_search_query shrank 242 → 186 LOC (-23%). New filter of those shapes is one table line. | `src/query/parse.rs` | — |
| M5 | ❌ **WITHDRAWN** (`6889825`) — re-inspection showed the flagged site builds a `Vec<&Volume>` for sequential iteration, not a `HashMap`; `online_map()` returns the wrong shape. The original code is correct as-is. | `src/asset_service.rs:4753` | — |
| M6 | ✅ **DONE** (`6889825`) — `classify_impl` renamed to `classify_inner` (4 refs in `ai.rs`). Codebase now uniformly uses `_inner` for private helpers. | `src/ai.rs` | — |
| M7 | ✅ **DONE** (`0eb33c6`, 2026-05-04) — added `//!` module docs to 29 source files (lib.rs, main.rs, the splitted catalog/asset_service/query roots, models/, web/, plus all the standalone single-file modules). Submodules already had docs from the batch-2 split commits. | `src/**/*.rs` | — |
| M8 | ✅ **DONE** (`0eb33c6`, 2026-05-04) — recount after batch-2 splits: only 7 actually-undocumented top-three items remained (split surfaced most pub items into submodules that already had docs). All 7 now documented (`Catalog::open`, `SearchSort::from_str`, `FileStatus`, `AssetService` + `::new`, `QueryEngine` + `::new`). | `src/{catalog,asset_service,query}.rs` | — |
| M9 | ✅ **DONE** (`0eb33c6`, 2026-05-04) — added 4–8 line purpose comments to 17 templates: the seven the report flagged plus tags, collections, duplicates, saved_searches, backup, analytics, stats, results, volumes, lightbox. Pattern matches `import_dialog.html` / `job_toast.html`. | `templates/*.html` | — |

### LOW

| # | Finding | Citation | Notes |
|---|---------|----------|-------|
| L1 | ⏳ **FOLDED INTO H5** — the inconsistent shapes live in the same `spawn_blocking + match Ok/Ok/Err` chains that `spawn_catalog_blocking` standardises when sites are migrated. Tracking with H5's opportunistic carryover; separate mechanical pass would be busywork. | various `web/routes/*.rs` | — |
| L2 | ✅ **DONE** (`6889825`) — `crate::config::resolve_model_dir(model_dir_root, model_id)` is now the single source of truth; `web::routes::ai::resolve_model_dir` is a one-line delegate; 3 inline `~/`-expansion blocks in `main.rs` removed. | `src/config.rs`, `src/web/routes/ai.rs`, `src/main.rs` | — |
| L3 | ✅ **DONE** (`6889825`) — `config::load_config()` returns `(PathBuf, CatalogConfig)`. Replaced the inline pair in **27** command handlers (initial 10+ estimate was conservative). | `src/main.rs` | — |
| L4 | All web handlers are `async fn` that immediately `spawn_blocking` | `src/web/routes/*.rs` | No real async work happens in any handler. The current shape is safe and idiomatic for axum, but the H5 helper would also tidy this up. |

---

## Implementation plan

Three batches. The first is small and surgical (no test impact, low risk). The second is the big-payoff structural work. The third is documentation polish that can ship anytime.

### Batch 1 — Small DRY wins ✅ DONE (`6889825`, 2026-05-03)

Cohesive, no public-API changes, no test impact. Tests stayed at 779 + 249 / 886 + 273.

1. ✅ **L3** — `config::load_config()` extracted; **27** paired call sites in `main.rs` migrated.
2. ✅ **H4** — `web::routes::resolve_asset_id_or_err` lifted; **7** sites migrated; "no asset found matching '{prefix}'" message unified.
3. ⚠️ **H5** — `web::routes::spawn_catalog_blocking` helper landed (returns `Result<T, Response>` so handlers can `?`-short-circuit). **3 demonstration sites migrated** (`volumes_page`, `assign_face`, `unassign_face`); remaining ~100 sites left for opportunistic cleanup as touched. Recount: 106 actual sites across 13 files (initial 40+ estimate was low).
4. ✅ **L2** — `crate::config::resolve_model_dir(root, model_id)` is the single source of truth; web helper delegates; 3 inline `main.rs` blocks removed.
5. ❌ **M5** — Withdrawn after re-inspection; flagged site builds `Vec<&Volume>` for sequential iteration, not a `HashMap` for lookups, so `online_map()` returns the wrong shape. Code is correct as-is.
6. ✅ **M6** — `classify_impl` → `classify_inner`. Codebase now uniformly uses `_inner` for private helpers.

Net diff: +153 / −142 LOC across 9 files.

### Batch 2 — Structural splits (separate PRs, larger)

Each item is its own PR — they're independent of each other. Order by pain-relief: `main.rs` first because every code review touches it.

1. ✅ **H1** (PARTIAL, `7ce8d11`) — extracted Import, Tag, AutoTag, RebuildCatalog, Volume; `run_command` 5921 → 4062 LOC. Remaining big arms left for opportunistic cleanup.
2. ✅ **M1** (`85984f8`) — `web/routes/ai/` directory module: tags, embed, similarity, faces, stroll.
3. ✅ **H3** (`6262a39`) — split into 12 submodules along the existing `// ═══ X ═══` markers via multi-file `impl AssetService` blocks. asset_service.rs 8886 → 2759 LOC. Three cross-section helpers lifted to `pub(super)`.
4. ✅ **H2** (`ae3bd4d`) — split into 17 submodules along the existing `// ═══` markers, same multi-file `impl Catalog` pattern as H3. catalog.rs 9200 → 4524 LOC (≈3.6 kLOC of that is tests). Six cross-section helpers lifted to `pub(super)`.
5. ✅ **M2** (`9d24d8f`) — extracted the parsing layer into `query/parse.rs` (~800 LOC). The original plan suggested search/write split on the impl block; the cleaner cleavage turned out to be parsing (DB-free) vs everything else (DB-bound). Public API unchanged via `pub use parse::*;`. Search-impl/write-impl split can follow if query.rs grows again.
6. ✅ **M3 + M4** (`6f74dec`) — `build_search_where` 357 → 205 LOC via 6 new per-filter helpers; `parse_search_query` 242 → 186 LOC via four lookup tables (SIMPLE_FILTERS / NUMERIC_FILTERS / STRING_FILTERS / BOOLEAN_TOKENS).

**Batch 2 fully landed across 2026-05-03 / 2026-05-04.** Only opportunistic carryovers remain: H1's smaller match arms (Describe, GeneratePreviews, Cleanup, Collection, SavedSearch, etc.) and H5's remaining ~100 `spawn_blocking` sites.

### Batch 3 — Documentation polish ✅ DONE (`0eb33c6`, 2026-05-04)

1. ✅ **M7** — `//!` module docs added to 29 source files.
2. ✅ **M8** — recount-after-splits dropped 81 → 7 undoc items in top three; all 7 documented.
3. ✅ **M9** — 17 templates got leading purpose comments.
4. ⏳ **L1** — folded into H5's opportunistic carryover (same `spawn_blocking` chains that `spawn_catalog_blocking` standardises).

### Not addressed

- **H6** (no inline tests in `main.rs`): leaving for now. The CLI integration suite covers external behaviour, which is what matters for a CLI tool. Inline tests would mostly duplicate what `tests/cli.rs` already exercises. Revisit only if a regression slips through that an inline test would have caught.

---

## Top 5 priorities (afternoon batch)

If time-boxed to a single afternoon, the maximum-payoff sequence is:

1. **Batch 1 entirely** (~3h) — six small DRY wins, one commit, no risk.
2. **H1** (Batch 2 #1, ~2–3h) — extract main.rs command handlers; alone shrinks the most-edited file in the repo by a third or more.

Everything in Batch 1 is independent, so the afternoon doesn't need to pick a stopping point — fold in as many as time allows. H1 is one focused session.

Estimated total: 5–6h for the highest-impact ~70% of the punch-list. The remaining structural splits (H2, H3) and documentation pass (Batch 3) are best as separate sessions.
