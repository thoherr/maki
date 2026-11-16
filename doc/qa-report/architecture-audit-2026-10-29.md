# MAKI Architecture and Design Audit — 2026-10-29

State at **v4.12.3** (commit `b4cdb59`), 84,000 lines of Rust in 120 files,
15,800 lines of templates, 13,400 lines of integration tests.

Scope: the whole `src/` tree, templates, build and CI configuration,
checked against single-responsibility, don't-repeat-yourself, separation
of concerns, dependency direction, invariants (dual storage, denormalized
columns), error handling, consistency across the three front ends (CLI,
web, shell), cross-platform discipline, feature gating, and test
architecture. Method: measurement first (lines, longest functions, call
counts), then reading of the hot spots, by four independent read-only
passes with disjoint scopes (domain and storage; services; interfaces;
cross-cutting), consolidated here. Every finding cites evidence; style
nits that clippy already reports are excluded.

One finding was a defect and is fixed in the same change as this report
(see F1). Everything else is a proposal; nothing else was changed.

## Status

- Phase 0 (quick wins, clippy gate): done, commit `a1192c6` (2026-10-31).
- Phase 1 (correctness): done 2026-11-01 — `Catalog::with_tx` around every
  multi-row writer, `run_migrations() -> Result` with per-version
  transactions, schema v13 (faces/people/embeddings/stacks/collections
  DDL feature-independent), `resync_asset_from_sidecar`, store-owned
  atomic YAML persistence (`FaceStore`/`StackStore`/`CollectionStore::open`),
  `MakiError` with exit codes 3/4 and HTTP mapping, doctor exit status.
  Residue carried to Phase 2: `Catalog::delete_asset_rows` removes
  collection memberships SQLite-only (`Catalog` has no root to persist
  `collections.yaml`); a store mutator that joins an enclosing
  transaction writes its YAML before the outer COMMIT; the stack web
  routes still answer 400 for "already stacked" (web test fixes it).
- Phase 2 (structure): done, committed 2026-11-16 — pure `models` (`models::{format,
  numeric_filter, geo}`), `QueryEngine` split into per-concern files with
  one connection per engine (`with_catalog`, `search_in`, web pool feeds
  the engine), `mutate_asset` behind every setter, service functions
  phase-split behind request structs and typed events, web describe /
  generate-previews / browse endpoints as adapters over the service,
  `maki.toml` read once per invocation, `features` module. The Phase 1
  residue on collection membership and the stack 409 is closed; still
  open: a store mutator that joins an enclosing transaction writes its
  YAML before the outer COMMIT. Exit criteria re-measured below.
- Phase 3: open.

## Verdict in one paragraph

The intended three-layer picture (interface, core library, storage) is
real and mostly honoured: the storage layer is free of CLI concepts, the
`--json` stdout contract is clean, the sidecar-first write order holds in
every single-asset mutator, migrations are version-guarded, the dual
stores are self-healing, and the three front ends share the engine for
tags, search resolution, export and maintenance jobs. The code's problems
are the ones that accumulate in a long-lived single-developer project
that grew by feature: two 5–6k-line modules that each do several jobs,
multi-row catalog writes that run without a transaction, configuration
re-parsed from disk inside per-asset loops, a handful of rules that exist
as two or three copies (one of which had already diverged), a dependency
cycle between the two service modules, two web-side loops that
re-implement service logic, and 12,700 lines of inline JavaScript with
twelve copies of the same escape helper. None of this is a rewrite. The
plan below is four phases of mostly mechanical, individually releasable
steps, ordered so that correctness fixes come first and the structural
moves are protected by the existing 1,500 tests.

## Metrics

| Measure | Value |
|---|---|
| Rust source / test lines | 84,000 total; `catalog.rs` is 82 % tests, `query.rs` 49 %, `xmp_reader.rs` 57 % |
| Files over 3,000 lines | `query.rs` 6,402, `catalog.rs` 5,465, `xmp_reader.rs` 3,426, `main.rs` 3,294 |
| Functions over 150 lines (non-test) | 63, of which 13 over 300; largest `run_command` 943, `import_with_callback` 736, `run_faces_command` 633, `run_tag_command` 572, `run_migrations` 442 |
| `Catalog::open` call sites | 130 (`query.rs` alone 42, one per method; `open_fast` is an alias) |
| Transactions in the codebase | 1 (`variant_crud.rs` hash migration) |
| `CatalogConfig::load` sites | 33, of which 15 inside library layers; all but one `unwrap_or_default()` |
| `Catalog::conn()` uses outside `src/catalog` | about 190 in 29 files (41 of the 44 web uses are store constructors) |
| `eprintln!` in library code (not CLI) | 134; no logging facade; `log: bool` threaded through 96 signatures |
| `#[allow(dead_code)] struct Ctx { json, log }` copies | 41 of 49 CLI handlers |
| Error-to-status by substring match | 10 sites in 8 blocks; 86 handlers default to 500; `thiserror` declared, 0 uses |
| `unwrap()` / `expect()` / `let _ =` outside tests | 72 / 14 / 231 (64 of the `let _` are in `run_migrations`) |
| Feature-gate attributes | `ai` 118, `pro` 27, `cfg!(feature)` 18; two cfg-duplicated 15-argument functions in `web/mod.rs` |
| Raw `spawn_blocking` vs `spawn_catalog_blocking` in web | 57 vs 54 |
| Inline JavaScript | 12,700 lines in templates, nothing in `static/` but vendored libraries; `browse.html` 3,571 |
| Duplicated JS helpers | 12 escape helpers, 3 toasts, 3 path-autocomplete widgets, 3 colour-label maps, 124 raw `fetch` calls |
| Public API | 727 `pub fn` / 340 `pub` types vs 24 `pub(crate)` items; no re-export surface |
| CI | 3 OS × {default, ai, pro}; `ai-gpu` built only in release; no clippy, no fmt, no `rust-version` |

## Findings

Severity reflects user-visible risk, not code taste. Effort: S under a
day, M a few days, L a sprint. Each entry names the tests that protect
the change.

### A. Correctness and invariants

**F1 [high, fixed in this change] Preview scoring existed in three copies, one diverged.**
`catalog/asset_crud.rs` `update_asset_preview_variant` recomputed
`best_variant_hash` in SQL with the pre-v4.12.3 weights (no `alternate`
score, format bonus 50), so clearing a preview override picked the RAW
again until the next full insert. A third copy lives in the v1 migration
backfill (`schema.rs`). Fixed: the method now stores only the override
and the caller derives the column through the one Rust rule
(`update_denormalized_variant_columns`); regression test
`clearing_preview_override_recomputes_with_the_shared_rule`. The v1
backfill copy is left as historical SQL; it runs only on catalogs older
than schema v1.

**F2 [high] Multi-row catalog writes run without a transaction.**
`QueryEngine::group` (`query.rs` ~941–981) issues eleven statements:
save sidecar, insert target, per donor update variants and roles, four
`let _ =` deletes with errors discarded, raw `DELETE FROM faces`, delete
sidecar, delete asset. `group_by_asset_ids` is the same 140 lines again.
`relocate` inserts catalog locations per plan entry and saves the sidecar
only after the loop, the inverse of the documented sidecar-first order.
`reimport_metadata_inner` toggles `PRAGMA foreign_keys` around a closure
instead of using a transaction. `tag_rename`, `delete_assets`,
`remove_volume`, `sync`, and the per-asset `rebuild-catalog` path have
the same shape. A failure mid-sequence leaves rows that `doctor` must
find later. Refactor: `Catalog::with_tx(|c| …)` over
`unchecked_transaction()`; in each writer compute the in-memory `Asset`
first, save the sidecar, then apply all rows inside one transaction;
promote the swallowed deletes to `?`. Effort M. Risk medium (foreign-key
ordering). Protected by `group_*`, `split_*`, `relocate_*` unit tests
and the doctor CLI tests.

**F3 [high] Migrations swallow failures and stamp the version anyway.**
`run_migrations()` returns `()`, every statement is `let _ =` (64 sites),
and `SCHEMA_VERSION` is written unconditionally at the end. The idiom is
justified for `ALTER TABLE ADD COLUMN` idempotency, not for the v11
rename, the FTS5 table creation, or the v12 backfill: a failed step is
recorded as migrated and `doctor` then trusts a half-migrated database.
Refactor: `run_migrations() -> Result<()>`, one `migrate_vN` per version
inside a transaction, `add_column_if_missing` via `pragma_table_info`,
stamp only on success, `maki migrate` names the failing step. Effort M.
Risk: previously hidden failures surface, which is the point. Protected
by `initialize_is_idempotent` and the v11/v12 migration tests.

**F4 [high] Face and embedding tables live outside the versioned schema.**
`schema.rs` creates them only under `cfg(feature = "ai")` and only inside
the `current < 1` block, so a catalog initialised by a standard build
never gets them and the v6 `ALTER TABLE faces` silently fails. About 20
lazy `let _ = FaceStore::initialize(...)` calls at use sites compensate,
`embedding_store.rs` runs its own unversioned primary-key migration by
sniffing `pragma table_info`, and the `embeddings` DDL exists twice.
Refactor: move all DDL into `schema.rs` as feature-independent versioned
blocks (v13), delete the lazy initialisers. Effort M. Protected by
`initialize_creates_all_tables` and the face/embedding store tests.

**F5 [high] Configuration is re-parsed from disk inside hot paths, with a silent fallback.**
33 `CatalogConfig::load` sites, 15 in library code: once per asset in
every writeback-enabled batch (`write_back_field_to_xmp_inner` via
`writeback_config()`, so a `tag rename` over 10k assets parses `maki.toml`
10k times), up to three times per `search_impl` call, per edit in
`HistoryStore::from_config` and `Trash::from_config`, mid-function in
`relocate`, per request in several web routes. All but one use
`unwrap_or_default()`, so a syntax error in `maki.toml` makes those paths
run with defaults while the command entry already loaded the real values:
two configurations in one process. Refactor: load once at the three
entry points (`main`, `web::serve`, `shell::run`), hold `Arc<CatalogConfig>`
in `AssetService`, `QueryEngine`, `AppState` and `BatchContext`; stores
take `&HistoryConfig` / `&TrashConfig`. Effort M. Risk low. Protected by
`config.rs` tests and the web harness, which already injects a config.

**F6 [medium] Stack, collection and face dual-writes are caller-enforced and non-atomic.**
`StackStore` mutators write SQLite only; persisting the YAML side is a
separate `export_all` + `save_yaml` repeated at 19 call sites, and for
faces `let _ = face_store.save_all_yaml(...)` at 16 sites (8 web, 8 CLI)
with errors dropped, which contradicts the dual-storage rule for the
source-of-truth side. Six YAML files are written with bare
`std::fs::write`, unlike `MetadataStore::save` (temp + rename + lock).
Refactor: a shared `yaml_file::{load_or_default, save_atomic}`; the
stores own `persist()` and end each mutator with it inside a
transaction. Effort M. Protected by stack/collection unit tests and the
web harness stack routes.

**F7 [medium] Four copies of "rebuild an asset's rows from its sidecar", with four behaviours.**
`doctor::rebuild_asset_rows`, `reimport_metadata_inner`, `restore_asset`
(undo; does not re-insert variants or locations, so an undo after a
variant change leaves stale rows), and the per-asset `rebuild-catalog`
path each delete and re-insert a different subset in a different order.
Refactor: `Catalog::resync_asset_from_sidecar(&Asset)` in `asset_crud.rs`,
transactional, used by all four. Effort S. Protected by doctor, undo and
rebuild tests.

**F8 [medium] Swallowed write failures on the dual-store path.**
`merge_variant_metadata` returns "written" after `.ok()`-ing both stores;
embedding writes drop the `.bin` result; stack and collection rebuild
import swallow `UPDATE` errors, so a rebuild can silently lose stack
membership; 34 `let _ = catalog.|store.` sites in the services. Refactor:
return `Result`, surface through the result structs' existing `errors`
field, and make the deletes inside F2's transactions `?`. Effort S.

**F9 [medium] Two implementations of the session-root rule.**
`catalog.rs` registers a SQL function `session_root` that re-implements
`query::find_session_root` ("same logic as auto-group"). Refactor: move
the rule to a `session_root` module and have the SQL function call it.
Effort S.

**F10 [low] Panics on degenerate data and poisoned locks.**
`face_store.rs` and `embedding_store.rs` sort similarities with
`partial_cmp().unwrap()` (a NaN embedding panics a web request; use
`f32::total_cmp`); 26 `RwLock`/`Mutex` `.unwrap()`s on `AppState` mean one
panicking job poisons every later request; `lookup.rs` has a `panic!` and
an `unwrap` on read paths and `filter_map(|r| r.ok())` drops malformed
rows silently. Effort S.

### B. Structure and responsibilities

**F11 [high] Dependency cycles and upward edges break the layer model.**
Models import storage and services (`models/variant.rs` uses
`catalog::VariantDetails`, `asset_service::is_raw_extension`,
`exif_reader::parse_dms_string`) although `models/mod.rs` promises "pure
data"; storage imports the query parser (`catalog.rs` uses
`query::NumericFilter`); `metadata_store` calls `asset_service::normalize_rating`;
`config` builds a `vlm` client type; and `asset_service` and `query` form
a cycle (eight `QueryEngine::new` inside services, four service helpers
called from the engine, one made public as `apply_xmp_data_pub` solely to
cross the cycle). Refactor: move the pure format helpers
(`is_raw_extension`, `determine_asset_type`, `normalize_rating`) and
`NumericFilter` into `models`; move `VariantDetails` next to `Variant`;
inject the DMS parser; then break the service cycle by moving the
mutation half of `QueryEngine` behind one editor (see F12). Effort S for
the model moves, L for the cycle. Compiler-checked; re-exports keep
paths.

**F12 [high] `QueryEngine` is a god object that opens a connection per call.**
6,402 lines, 55 methods: search, seven tag operations, group/split/
auto-group, nine field setters, reimport, stack-from-tag, undo restore,
journaling, and writeback through sub-modules. The struct holds only
`catalog_root`, so every method opens `Catalog`, `MetadataStore` and
`DeviceRegistry` afresh (42 opens; each open re-runs five pragmas and
registers SQL functions), and the web's `query_engine()` bypasses the
connection pool for 32 callers. Seventeen methods hand-roll the same
"load sidecar, mutate, save, insert, denormalize" sequence; the five
field setters are 25 identical lines each and three of them bypass
`BatchContext`, so they cannot take part in batch undo. Refactor in
steps: (1) split into `impl QueryEngine` blocks per concern
(`metadata_ops.rs`, `grouping.rs`, existing `writeback*.rs`) exactly as
`AssetService` already does, a pure move; (2) `mutate_asset(ctx, prefix,
f: FnOnce(&mut Asset) -> Result<bool>)` and
`Catalog::resolve_asset_id_required` to replace the 17 sequences and 16
copies of the "no asset found matching" error; (3) a `MutationContext`
(today's `BatchContext` plus config, online volumes, content store)
received by every mutator instead of opening handles; (4)
`QueryEngine::with_catalog(&Catalog)` and `search_in(&Catalog, query)` so
services and the web pool pass their connection. Effort M for 1–2, L for
3–4. Protected by the 234 tests in `query.rs`.

**F13 [medium] Service entry points of 250–740 lines that mix scan, decide and write.**
`import_with_callback` 736 lines with two near-identical recipe branches,
two identical preview blocks, 23 hand-threaded counters and five
`expect()`s that re-derive known invariants; `sync` 357, `cleanup` 327,
`describe_inner` 314 with 13 parameters and a hand-rolled thread scope,
`refresh` 305, `relocate` 299 (already commented as "Phase 1/2/2.5/3"
but not split), `writeback_process` 294 with 14 parameters, `verify`,
`dedup`, `sync_metadata`, `search_impl` all over 250. Refactor: lift the
existing phase comments into private functions returning plain data
(`plan_relocation() -> Vec<FileCopyPlan>`, `apply_relocation(plan)`),
extract `attach_recipe` and `generate_previews_for` from import, model
"who owns this recipe" as an enum so the `expect`s disappear, and give
the long parameter lists request structs. Effort M per function. Risk
low; every command has CLI tests.

**F14 [medium] Four copies of "online volume map" in three shapes, and 24 result structs with the same counters.**
`HashMap<Uuid, PathBuf>` built by hand in nine places (one private
`load_online_volumes` already exists but maps registry errors to "all
offline"); label maps duplicated; `apply: bool` on 23 functions versus
`dry_run: bool` on 30, sometimes both on one function; four public
wrappers over `auto_group_inner`. Refactor: `DeviceRegistry::online_map()`
and `label_map()`; one `WriteMode` enum; an `AutoGroupScope` enum.
Effort S.

**F15 [medium] Catalog internals are reachable everywhere.**
`Catalog::conn()` is used about 190 times outside the storage layer;
raw `UPDATE assets` exists in `query.rs` and `stack.rs`; three web
handlers and the `Migrate` dispatch arm embed SQL. The "every write path
updates the denormalized columns" rule cannot be audited while SQL is
scattered. Refactor: lift each query to a named `Catalog` method, then
make `conn()` `pub(crate)` for the sub-store constructors only. Effort L,
incremental per file.

**F16 [low] Section files that do not match their names, and dead API.**
`catalog/rebuild.rs` holds location queries, `catalog/tags.rs` holds
volume listing and stats while `stats.rs` is 71 lines, `facets.rs` holds
a migration backfill. Verified-dead items: `open_fast` alias,
`search_assets`, `update_best_variant_hash`, two volume counters, a
recipe-verification stat, `sync_to_catalog`, two face-store helpers,
`read_embedding_binary`, two registry stubs that `bail!("not yet
implemented")`, `ContentStore` constructor and `ingest` arguments that are
ignored, 12 `#[allow(dead_code)] fn is_default_*`. Effort S.

**F17 [low] Enum-to-string at SQL boundaries via `format!("{:?}").to_lowercase()`.**
`RecipeType::EmbeddedExport` becomes `embeddedexport` in SQLite but
`embedded_export` in YAML; `VariantRole` is mapped three different ways.
Latent today. Refactor: `as_str()` / `FromStr` on the four enums, as
`VolumePurpose` already has. Effort S.

### C. Interfaces: CLI, web, shell, front end

**F18 [high] Two web loops re-implement service logic the CLI gets from the service.**
`media.rs::batch_vlm_describe_inner` (261 lines) re-implements the VLM
batch loop that `AssetService::describe` owns, with its own tag logic
and a `join().unwrap()` that lets a worker panic kill the job silently;
`routes/maintain.rs::run_generate_previews` re-implements the
preview-generation loop of `commands/maintain.rs` and silently lacks the
type and path filters the CLI has. `browse_page` and `search_api` share
95 identical lines, and `export_zip`, `all_ids_api`, `page_ids_api`
repeat the first half again. Refactor: extend `AssetService::describe`
with an id-list scope and tag option; `AssetService::generate_previews(
scope, options, on_item)`; `run_browse_search()` returning one
`BrowseResult` that both browse handlers render. Effort M. Risk medium
(behaviour alignment is a feature change on the web side). Protected by
the web harness and the CLI describe/preview tests.

**F19 [high] No error model: statuses by substring, exit codes always 1.**
`thiserror` is declared and unused. The web maps error text to status at
ten sites (`"no asset found"`, `"already"`, `"UNIQUE constraint"`,
`"cannot remove"`), 86 handlers default to 500 with a plain-text body
while eight return JSON and two HTML; the CLI exits 1 for everything and
`doctor` exits 0 after reporting issues although the component
specification says the report "drives the exit status". Refactor: one
`MakiError` enum (`NotFound`, `Offline`, `Conflict`, `InvalidInput`,
`Io`, `Sqlite`) raised at the engine, `anyhow` kept at the edges via
`downcast_ref`, an `IntoResponse` that negotiates HTML versus JSON on
`HX-Request`, an exit-code table in `main`. Do it incrementally starting
with not-found and offline. Effort M. Protected by the status-code
assertions in the web harness and `.code()` assertions in the CLI tests.

**F20 [medium] The CLI dispatcher still holds business logic, and each handler re-plumbs the global flags.**
`run_command` is 943 lines with a 145-line `Search` arm (four renderers
and a "forgot quotes" heuristic), `Init` writing files, and `Migrate`
running raw SQL plus a sidecar rating-repair loop with `let _ =`. 41 of
49 handlers open with an `#[allow(dead_code)] struct Ctx { json, log }`
shim; json, log and verbosity are passed positionally in varying order;
output is 74 hand-built `json!` next to 58 serialised structs; the
106-line `print_custom_help` is hand-maintained parallel to clap (44
listed commands versus 53 variants). The shell re-implements `export`
and `preview` by hand with a stale flag list. Refactor: `CliContext {
json, log, verbosity }` as the last parameter everywhere and an
`emit(&T, || human)` helper; `#[derive(Args)]` structs per command so
arms become one call; move `Search` rendering to `commands/search.rs`;
generate help from `Cli::command()`; route the two shell builtins through
the executor. Effort M, mostly mechanical. Protected by the CLI suite.

**F21 [medium] Half-finished helper migrations in the web layer.**
57 raw `spawn_blocking` with the triple `Ok(Ok(` match beside 54
`spawn_catalog_blocking` (the QA report of May 2026 counted 106 to
migrate); `launch_job` used by the seven maintain endpoints while six
AI and import endpoints hand-roll register, spawn, terminal event and
mark-done; the embedding index lazy-load copied five times and the SigLIP
model load four times, one of which ignores `[ai] execution_provider`;
`AppState::new` and `serve` exist twice behind `cfg` with 15 positional
parameters each. Refactor: finish the helper migration (pairs with F19),
`launch_job` with `LaunchOptions`, `AppState::embedding_index()` and
`with_siglip()`, a `ServeOptions` struct. Effort S–M. Protected by the
web harness, which builds both feature variants.

**F22 [medium] 12,700 lines of inline JavaScript with no module structure.**
`browse.html` alone is 3,571 lines in twelve IIFEs coupled through
globals, with three different `getFilterParams` and two `refreshResults`
on the same page; twelve escape helpers, three toasts, three
path-autocomplete widgets, two SSE attach routines and 124 raw `fetch`
calls across templates; the colour-label vocabulary (names, hex, keys)
exists in at least eleven places across Rust, Askama and JS; the filter
bar re-implements the server's query quoting rule, which Rust already
owns and tests. Refactor in order: (1) `static/maki-core.js` with
escape, toast, `api.{get,post,put,del}`, dialog, path-autocomplete and
job-stream helpers loaded from `base.html`; (2) one `COLOR_LABELS`
constant in `models` rendered by Askama and injected once as
`window.MAKI.labels`; (3) move `browse.html` sections into
`static/browse/*.js` one module per release, leaving a data island.
Effort S, S, L. Risk medium for (3): template interpolations inside JS
and no JS tests; the web harness covers endpoints only.

### D. Cross-cutting

**F23 [medium] Output and logging have no facade.**
134 `eprintln!` in library modules behind `if verbosity.verbose`, with
`log: bool` threaded through 96 signatures; services format CLI-shaped
lines (`"{rel_path} — written"`) and also push the same strings through
an ad-hoc `callback: Option<&dyn Fn(&str, &str)>` for the web; the server
inherits CLI stderr output. Refactor: the `log` crate with a `Verbosity`
adapter in `main`, and for progress an event enum per operation
(`WritebackEvent::{Written, SkippedOffline, Failed, Warning}`) as
`workflow.rs::ImportEvent` already does, with the CLI and web adapters
printing or streaming. Effort M. Protected by the CLI tests' stderr
assertions, which fix the wording.

**F24 [medium] Cross-platform path discipline is a convention, not a type.**
The canonical forward-slash form is `FileLocation::relative_path_str()`
(5 callers), but `verify.rs` and `volume.rs` pass `to_string_lossy()`
straight into catalog lookups (on Windows a backslash key misses, so the
`max_age_days` skip never fires), and ten sites do an ad-hoc
`.replace('\\', "/")`. The platform opener exists three times with
divergent command tables. Refactor: `path_util::forward_slashes(&Path)`,
a clippy `disallowed_methods` entry for `to_string_lossy` in the storage
and service layers, and one `platform` module for open, reveal, terminal
and URL. Effort S. Protected by the existing `cfg(windows)` parser tests,
to be extended.

**F25 [medium] Feature gates leak into generic data and control flow.**
`ParsedSearch` has five `ai`-gated fields and gated arms in `merge_from`
and the parser, so the parser differs between builds; `workflow.rs` gates
enum variants, result fields and phases; `search_impl` carries seven
gated locals. Refactor: keep fields unconditional (an `Option` costs
nothing) and gate only the implementation behind one stub module that
`bail!`s, the pattern `search_impl` already uses at its top; a
`features` module with `AI`, `PRO` constants and `edition()`. Effort S.
Protected by CI, which builds all three feature sets.

**F26 [medium] Test architecture: one 13,000-line black-box file and tests far from their code.**
`tests/cli.rs` is a single file of 344 subprocess tests; `catalog.rs`
embeds 4,500 test lines for code that lives in 17 section files holding
one test between them; 17 of the 20 `AssetService` operations have no
unit test and are exercised only through the binary, because each opens
its own catalog and prints to stderr; 25 `setup_*`/`seed_*` helpers with
no shared module (CLAUDE.md names `setup_search_catalog()` as if shared;
it is private). Gaps in kind: no Windows-path tests outside the parser,
no concurrency test for the pool against a writer, no migration-from-old
fixture. Refactor: `tests/cli/{search,tag,stack,…}.rs` under one
`main.rs`; a `#[cfg(test)] pub mod test_support` with temp-catalog and
asset builders; per-section test modules; after F12 step 3, service
operations become unit-testable through the context. Effort M,
mechanical.

**F27 [low] Build and public-API hygiene.**
No `rust-version` (the documented floor is unenforced), no
`[profile.release]` (`lto`, `codegen-units = 1`, `strip` would shrink
the eleven release assets), no `[lints]`, no clippy or fmt step in CI,
`ai-gpu` never tested, a duplicated `sha2` dev-dependency, `thiserror`
unused. 727 public functions and no re-export surface although `lib.rs`
claims one; either commit to a public API (`pub use` surface,
`pub(crate)` internals such as `conn()` and `apply_xmp_data_pub`) or mark
the crate `publish = false`. Effort S.

**F28 [low] Documentation describes the intended layering, not the measured one.**
CLAUDE.md and the architecture overview show the three-layer picture
without the service cycle, the model-to-storage edges, the per-call
connection opens, or the config re-loads; the component specification
claims doctor's report drives the exit status (it does not); CLAUDE.md's
"Test Helpers" section names private functions. To be corrected as the
corresponding findings close, with F19 (doctor exit code) first.

## Verified sound

Decisions checked and found good, so that the picture is fair:

- Storage layer free of CLI concepts (`Verbosity` has zero hits in
  catalog, models, metadata store, content store, device registry); no
  stdout output outside the command layer, so `--json` is safe.
- Sidecar-first order in every single-asset mutator; `MetadataStore::save`
  atomic with temp + rename and a cross-process lock; `tag_sources`
  self-heal; `insert_asset` as an upsert that avoids cascading deletes;
  `DerivedMediaColumns` shared by insert and refresh (since v4.12.2).
- Version-guarded migrations with FTS-trigger-aware guards; the v12
  trigger narrowed to `UPDATE OF id, tags`; `migrate_variant_hash` in one
  deferred-FK transaction; `TagIndexPlan` memoised per connection.
- `BatchContext` with an optional recorder: one journal operation per
  batch, `before` cloned only when recording, non-fatal journaling.
  History journal isolated from both stores.
- `ResolvedFilterIds::resolve` shared by CLI and web; tag normalisation,
  colour-label validation, export, writeback, sync, verify, refresh and
  cleanup all single-sourced in the engine or service; read-only mode
  enforced once in middleware; `HX-Trigger` built in one place.
- `workflow.rs::ImportEvent` is the right event-callback shape; `xmp_reader`
  is large but single-purpose with 73 tests plus property tests;
  `JobRegistry` and `CatalogPool` are clean abstractions.
- `FileLocation` serialises forward slashes on every write path;
  `deny.toml` is tight; CI covers three platforms and three feature sets.

## Improvement plan

Four phases. Each step is releasable on its own and lands as its own
commit with the protecting tests named above; the measured counts in the
metrics table are the exit criteria. Behaviour-preserving steps carry no
changelog entry beyond "internal"; the ones marked (behaviour) do.

### Phase 0 — quick wins, no behaviour change (1–2 days total)

1. Config injection: `Arc<CatalogConfig>` in `AssetService`, `QueryEngine`,
   `AppState`, `BatchContext`; delete the 15 library-side loads (F5).
2. `DeviceRegistry::online_map()` / `label_map()`; `WriteMode` and
   `AutoGroupScope` enums; `Catalog::resolve_asset_id_required` (F14, F12.2).
3. `CliContext` and `emit()`; delete the 41 `Ctx` shims; generate help
   from clap (F20, partial).
4. `AppState::embedding_index()` / `with_siglip()`; `ServeOptions`;
   `launch_job` for the six hand-rolled jobs (F21).
5. `static/maki-core.js` and the single `COLOR_LABELS` source (F22.1–2).
6. `path_util::forward_slashes`, the `platform` module, clippy
   `disallowed_methods` (F24).
7. Dead-code removal, section-file moves, enum `as_str()` (F16, F17).
8. `Cargo.toml`: `rust-version`, `[profile.release]`, `[lints]`, drop
   `thiserror` or start using it in Phase 1; CI: `cargo clippy -D warnings`
   and `cargo fmt --check`, an `ai-gpu` build step (F27).

### Phase 1 — correctness (behaviour) (3–5 days)

1. `Catalog::with_tx`; transactions around `group`, `split`, `relocate`,
   `tag_rename`, `delete_assets`, `remove_volume`, reimport; promote the
   swallowed deletes (F2, F8).
2. `run_migrations() -> Result`, per-version transactions, stamp on
   success; face and embedding DDL into schema v13 (F3, F4).
3. `Catalog::resync_asset_from_sidecar` replacing the four copies (F7).
4. `yaml_file::save_atomic`; stores own their persistence; faces,
   stacks, collections dual-write encapsulated (F6).
5. `total_cmp`, poison-tolerant locks, `lookup.rs` panic and silent-drop
   removal (F10); `session_root` single implementation (F9).
6. Doctor exit code and the first typed errors (`NotFound`, `Offline`)
   with `IntoResponse` and the exit-code table (F19, first slice).

### Phase 2 — structure (2–3 weeks, one module per release)

1. Pure `models`: move format helpers, `NumericFilter`, `VariantDetails`;
   inject the DMS parser (F11, first half).
2. Split `QueryEngine` into per-concern `impl` blocks; `mutate_asset`;
   collapse the five setters and `edit` (F12.1–2).
3. `MutationContext`; `QueryEngine::with_catalog`, `search_in`; the web
   pool feeds the engine; break the `asset_service` / `query` cycle
   (F12.3–4, F11 second half).
4. Phase-split the long service functions, starting with `import`,
   `relocate`, `sync`, `cleanup`; request structs for the 13- and
   14-parameter functions (F13).
5. Web loops onto the service: `describe`, `generate_previews`,
   `run_browse_search`; finish the `spawn_catalog_blocking` migration;
   complete the typed error model (F18, F21, F19).
6. Event enums for writeback and sync; the `log` facade; delete
   `log: bool` and the ad-hoc callbacks (F23). Feature-gate consolidation
   (F25).

### Phase 3 — strategic (ongoing)

1. `browse.html` into `static/browse/*.js` modules, one per release,
   behind the Phase 0 core helpers (F22.3).
2. Test architecture: split `tests/cli.rs`, shared `test_support`,
   per-section unit tests, unit tests for the twenty service operations
   now that they take a context; Windows-path and pool-concurrency tests
   (F26).
3. `Catalog::conn()` inventory and `pub(crate)`; the public-API decision
   (F15, F27).
4. Documentation update of CLAUDE.md and the architecture overview to the
   measured layering, as each phase closes (F28).

### Exit criteria (re-measured after Phase 2, 2026-11-16)

| Measure | At audit | Target | After Phase 2 |
|---|---|---|---|
| Transactions around multi-row writes | 1 | every writer in F2 | done (Phase 1) |
| `CatalogConfig::load` in library code | 15 | 0 | 0 outside `config.rs`, the web config editor and test-only constructors |
| `Catalog::open` in `query.rs` | 42 | 1 | 1 |
| Functions over 300 lines | 13 | 0 | 0 |
| Copies of the preview rule / session-root rule / resync | 3 / 2 / 4 | 1 / 1 / 1 | 1 / 1 / 1 |
| Error-to-status substring sites | 10 | 0 | 0 |
| `Ctx` shims in CLI handlers | 41 | 0 | 0 |
| Raw `spawn_blocking` in web | 57 | 0 | 3, each commented (job runner, the blocking helper, ZIP body streamer) |
| Duplicated JS escape helpers / colour-label maps | 12 / 11 | 1 / 1 | 1 / 1 (Phase 0) |
| Upward `use crate::` edges from models and catalog | 5 | 0 | 0 |
