# MAKI QA Report — 2026-05-03

State at v4.4.14 (post-release). Baseline: 63 321 Rust LOC across ~80 files, 12 051 template LOC. Tests at this baseline: 779 lib + 249 CLI + 14 doc on standard, 886 + 273 + 14 on pro.

This report focuses on **what's left** after the substantial extraction work landed in v4.4.5 (web/routes split 6599 → 348 LOC across 13 submodules, `cli_output.rs`, `Volume::online_map`, `resolve_collection_ids`) and v4.4.13–14 (`run_faces_command` lifted, `build_search_where` decomposed, `AssetService::embed_assets` extracted, `JobRegistry` lifted).

Previous QA reports are archived under `doc/qa-report/archive/`.

---

## Findings — by severity

### HIGH

| # | Finding | Citation | Notes |
|---|---------|----------|-------|
| H1 | `run_command` is **5 921 lines** in one match | `src/main.rs:2474-8395` | 45+ command arms inline. Longest still-inline handlers: Import (~457 LOC), AutoTag (~338), Edit (~124), Describe (~199). The `run_faces_command` extraction in v4.4.5 set the pattern; uniformly applying it would shrink main.rs by ~3 000 LOC. |
| H2 | `catalog.rs` 9 200 LOC god-module | `src/catalog.rs` | Mixes asset CRUD, variant CRUD, location CRUD, recipe storage, schema migrations, denormalised-column maintenance, duplicates queries, rebuild logic. Natural cleavage planes: `catalog/{assets,variants,recipes,migrations,denorm}.rs` re-exporting through one `Catalog` impl. |
| H3 | `asset_service.rs` 8 886 LOC mixing service workflows | `src/asset_service.rs` | Section comment headers (`// ═══ X ═══`) already mark cleavage planes. Splits: `asset_service/{import,verify,sync,dedup,refresh,export,ai}.rs`. Each section is ~700–1 500 LOC. |
| H4 | `resolve_asset_id` error boilerplate copy-pasted 8+ times | `web/routes/{browse,ai,media,stacks,assets,collections}.rs` | Pattern: `catalog.resolve_asset_id(id).map_err(...)?` then `.ok_or_else(\|\| format!("no asset matching '{id}'"))`. Lift to a single helper in `web/routes/mod.rs`. Hot path — every asset mutation. |
| H5 | `spawn_blocking + match Ok(Ok)/Ok(Err)/Err` response chain repeated 40+ times | `web/routes/*.rs` | Pattern is identical: spawn, await, match on three layers, build response. A generic `async fn spawn_catalog_blocking<T: Serialize>(f) -> Response` would eliminate ~120 LOC across the routes module. |
| H6 | `main.rs` has **zero inline tests** for 8 804 LOC of CLI dispatch | `src/main.rs` | Compare: catalog.rs (121 test blocks), asset_service.rs (67), query.rs (211). The CLI integration suite (`tests/cli.rs`, 249/273 tests) covers external behaviour but not internal helpers within main. Some critical paths (CLI argument parsing edge cases, error message formatting) have no coverage. |

### MEDIUM

| # | Finding | Citation | Notes |
|---|---------|----------|-------|
| M1 | `web/routes/ai.rs` 1 614 LOC | `src/web/routes/ai.rs` | Mixes suggest-tags, batch auto-tag, embed, find-similar, faces/people, stroll. Other route modules already split this way (browse.rs, media.rs are big but single-domain). Split into `ai/{tags,similarity,faces,stroll}.rs`. |
| M2 | `query.rs` 6 820 LOC | `src/query.rs` | Two distinct concerns: search/filter parsing (read path) vs tag/edit/auto-group/writeback (write path). Extract `query/{search.rs, write.rs}` or two newtype wrappers. |
| M3 | `build_search_where` still 356 LOC after v4.4.5 decomposition | `src/catalog.rs:3017-3373` | Further per-filter-type extraction: text, tags, dates, numeric, custom. Each becomes a private helper returning `(clause, params, needs_join_*)`. |
| M4 | `parse_search_query` is a 241-line tokenizer with 40+ `strip_prefix` branches | `src/query.rs:404-645` | Replace the if-chain with table-driven dispatch (HashMap or const slice of `(prefix, parser)` tuples). Easier to add filter types and easier to test individual parsers. |
| M5 | One stray inline `volumes.iter().filter(is_online)` | `src/asset_service.rs:4753` | Should use `Volume::online_map()` like the seven other call sites already do. Single-line fix. |
| M6 | Inconsistent helper-function naming: `_inner` vs `_impl` | `query.rs` (multiple `*_inner`) vs `ai.rs` (`classify_impl`) | Pick one convention. `_inner` is more common; align `_impl` callers to it (or vice versa). Cosmetic but noticeable in code review. |
| M7 | 20 of 33 `src/` files lack `//!` module docs | incl. `main.rs`, `catalog.rs`, `asset_service.rs`, `query.rs`, `xmp_reader.rs`, `face_store.rs`, `preview.rs`, `config.rs` | One-or-two-sentence summary per file. Unblocks `cargo doc` legibility and makes onboarding less archaeology. |
| M8 | 81 undocumented public items in the top-three files | `catalog.rs` 32, `asset_service.rs` 29, `query.rs` 20 | Prioritise `pub fn` and `pub struct` on the public-facing API surface (`Catalog`, `QueryEngine`, `AssetService` entrypoints). |
| M9 | Large templates lack purpose comments | `templates/{browse,asset,compare,stroll,people,filter_bar_js,lightbox_js}.html` | Newer partials (`import_dialog.html`, `job_toast.html`) start with a 4–8 line HTML comment explaining what the partial is, where it's mounted, and how external code interacts with it. The old large templates have nothing. |

### LOW

| # | Finding | Citation | Notes |
|---|---------|----------|-------|
| L1 | Inconsistent error-response shape in web routes | various `web/routes/*.rs` | Three forms in active use: `Json(json!({"error": ...}))`, `(StatusCode::X, msg)`, `.into_response()` with bare strings. Standardise on one — likely `(StatusCode, Json(json!({"error": ...})))`. |
| L2 | Model-dir `~/`-expansion duplicated | `src/main.rs:4632-4635` and `src/web/routes/ai.rs::resolve_model_dir` | Same logic in two places after v4.4.13's web migration. Extract to `crate::config::expand_model_dir()` (or similar). |
| L3 | `find_catalog_root() + CatalogConfig::load()` pair in 10+ command handlers | `src/main.rs` various | Fold into a `load_config() -> Result<(PathBuf, CatalogConfig)>` helper. Tiny change, big readability win for the dispatch arms. |
| L4 | All web handlers are `async fn` that immediately `spawn_blocking` | `src/web/routes/*.rs` | No real async work happens in any handler. The current shape is safe and idiomatic for axum, but the H5 helper would also tidy this up. |

---

## Implementation plan

Three batches. The first is small and surgical (no test impact, low risk). The second is the big-payoff structural work. The third is documentation polish that can ship anytime.

### Batch 1 — Small DRY wins (~3h, single PR)

Cohesive, no public-API changes, no test impact expected.

1. **L3** — Extract `load_config() -> Result<(PathBuf, CatalogConfig)>` in `src/config.rs`. Replace the 10+ inline `find_catalog_root() + CatalogConfig::load()` pairs in `main.rs`. (~30 min)
2. **H4** — Extract `web::routes::resolve_asset_id_or_err(catalog, prefix) -> Result<String, Response>` in `web/routes/mod.rs`. Wire all 8+ call sites through it. (~45 min)
3. **H5** — Add `web::routes::spawn_catalog_blocking<T: Serialize>(f) -> Response` in `web/routes/mod.rs`. Migrate the heaviest call sites first (ai.rs has the most); leave per-route migrations to follow as opportunistic cleanup. (~1h)
4. **L2** — Move `resolve_model_dir` from `web/routes/ai.rs` to `crate::config` (or `crate::ai`); delete the inline version in `main.rs`. (~15 min)
5. **M5** — One-line fix: switch `asset_service.rs:4753` to `Volume::online_map()`. (~5 min)
6. **M6** — Pick a naming convention (`_inner`); rename `classify_impl` and any other outliers. (~15 min)

Run full test suite, single commit.

### Batch 2 — Structural splits (separate PRs, larger)

Each item is its own PR — they're independent of each other. Order by pain-relief: `main.rs` first because every code review touches it.

1. **H1** — Extract `main.rs` command handlers, one `fn run_X_command(...)` per arm. (~2–3h, mechanical, very low risk because each handler is self-contained.) Pattern is already in the codebase from `run_faces_command`. Start with the longest five (Import, AutoTag, Edit, Describe, Embed) — those alone shrink `main.rs` by ~1 500 LOC. Continue in a follow-up if energy allows.
2. **M1** — Split `web/routes/ai.rs` 1 614 LOC into `ai/{tags,similarity,faces,stroll}.rs` plus `ai/mod.rs` re-exporting. (~1h, mostly file moves and `mod.rs` edits.)
3. **H3** — Split `asset_service.rs` along its existing `// ═══ X ═══` section markers into `asset_service/{import,verify,sync,dedup,refresh,export,ai}.rs`. (~4–6h, more disruption because shared private helpers will need to be lifted to a module-internal `mod common` or be kept on the main impl.) Recommended approach: create the submodules as `impl AssetService { ... }` blocks across files, no struct split — Rust supports multi-file `impl`. This minimises the code churn and keeps the public API identical.
4. **H2** — Split `catalog.rs` along the same plan as H3. (~4–6h.) Same multi-file-`impl` strategy.
5. **M2** — Split `query.rs` into `query/{search,write}.rs`. (~2h.) Tighter cleavage than catalog/service: search and write paths share little.
6. **M3 + M4** — Further decomposition of `build_search_where` and `parse_search_query`. (~2h together.) Best done after M2 so the work happens inside the new `query/search.rs`.

H1, M1, M2 are good candidates for a single afternoon (each ~1–2h, all mechanical). H3 and H2 should each be their own focused session.

### Batch 3 — Documentation polish (~2h, low priority)

1. **M7** — Add `//!` module docs to all 20 source files lacking them. One or two sentences each. Mechanical pass with `Grep` for `^//!` to find gaps. (~45 min)
2. **M8** — Doc the top-three files' undocumented public items (81 total). Prioritise return types and entrypoint methods; skip trivial accessors. (~1h)
3. **M9** — Add 4–8 line purpose comments to the seven large templates. Pattern: what the template is, where it's mounted, what external JS APIs it exposes. (~30 min)
4. **L1** — Standardise web error-response shape on `(StatusCode, Json(json!({"error": ...})))`. Mostly mechanical search-and-replace. (~30 min)

This batch can land in any order; each item is independent and self-contained.

### Not addressed

- **H6** (no inline tests in `main.rs`): leaving for now. The CLI integration suite covers external behaviour, which is what matters for a CLI tool. Inline tests would mostly duplicate what `tests/cli.rs` already exercises. Revisit only if a regression slips through that an inline test would have caught.

---

## Top 5 priorities (afternoon batch)

If time-boxed to a single afternoon, the maximum-payoff sequence is:

1. **Batch 1 entirely** (~3h) — six small DRY wins, one commit, no risk.
2. **H1** (Batch 2 #1, ~2–3h) — extract main.rs command handlers; alone shrinks the most-edited file in the repo by a third or more.

Everything in Batch 1 is independent, so the afternoon doesn't need to pick a stopping point — fold in as many as time allows. H1 is one focused session.

Estimated total: 5–6h for the highest-impact ~70% of the punch-list. The remaining structural splits (H2, H3) and documentation pass (Batch 3) are best as separate sessions.
