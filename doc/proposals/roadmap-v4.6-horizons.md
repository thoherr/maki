# Roadmap v4.6.x ff. — Trust Hardening, Foundation Rework, v5 Horizons

> **Status: ✅ COMPLETE (historical record).** Horizon 1 shipped as
> **v4.6.0** and Horizon 2 as **v4.7.0** (core) + **v4.8.0** / **v4.8.1**
> (completion). Both horizons are fully released; see the Completed list
> in `roadmap.md` and `CHANGELOG.md` for per-version detail. The v5 arc
> that follows is `roadmap-v5-horizon3.md`. This document is retained as
> the original plan for the cycle.

Planning document for the development cycle following v4.5.17. Derived
from the June 2026 QA loop (code metrics, DRY, SRP, and documentation
audits) plus a gap analysis of the v4.5.x bug history.

This document complements `roadmap.md` (the living tier list): items
here are sequenced into **horizons** — coherent groups with a theme and
an ordering rationale — rather than ranked individually. Tier-1 items
from `roadmap.md` are slotted into horizons where they have
prerequisites.

---

## The meta-observation driving this plan

MAKI's three longest bug sagas all trace back to the same two roots:

1. **String-level XMP writing.** The runaway-escape train
   (v4.5.14–v4.5.17: `&amp;amp;…` escalation, namespace-prefix
   misses, dual `hierarchicalSubject` blocks) lives entirely in the
   regex-edit-in-place functions of `xmp_reader.rs`.
2. **Convention-enforced consistency.** The dual-store invariant
   ("every write path updates YAML *and* SQLite, plus the denormalized
   columns") is a checklist in CLAUDE.md, not a mechanism. The
   v4.5.15 `insert_recipe` divergence (one omitted column → silently
   stale pending flags) is the canonical failure of this class.

Horizon 1 hardens what users trust us with (their files and metadata);
Horizon 2 fixes the two roots before building the next feature wave on
top of them.

---

## Horizon 1 — Trust hardening (v4.6.x)

Small, independent, high-safety items. Each is roughly one release in
the established cadence. None blocks any other; suggested order puts
shared infrastructure (test harness) early and pure additions later.

### 1. Fix the select-all / text-query trap

`all_ids_api` and `page_ids_api` (the "select all matching" /
keyboard-selection backends) ignore `text:` and `similar:` filters —
only `browse_page` and `search_api` run the AI filter pipeline. A user
can text-search, hit select-all, and batch-tag/-rate/-delete the
**unfiltered** result set.

**Scope:**
- Wire `ResolvedSearch::resolve_ai_filters` into `all_ids_api` and
  `page_ids_api` so selection endpoints see exactly what the grid
  shows
- Audit `facets_api`, `calendar_api`, `map_api` for the same mismatch;
  decide deliberately per endpoint (facets arguably *should* reflect
  the text-filtered set; calendar/map may stay as-is) and document
  the decision in `ResolvedSearch`'s rustdoc

**Complexity:** Low. The shared `ResolvedSearch` struct (built in the
QA loop) makes this a per-endpoint one-liner plus tests.

### 2. Web test harness + mutation endpoint coverage

The web layer (~5 kLOC of routes, templates, jobs) has zero automated
tests; releases rely on manual verification (v4.5.16 shipped untested
for exactly this reason). After three web-layer refactors in the QA
loop, this is the largest risk concentration in the codebase.

**Scope:**
- Test harness: build the axum `Router` against a temp catalog and
  drive it with `tower::ServiceExt::oneshot` — no listening socket,
  no separate process
- Cover the mutation endpoints first (rating, tags, description,
  label, batch ops): assert status codes, fragment rendering, the
  `HX-Trigger: pending-changed` header, and the dual-store write
  (YAML + SQLite both updated — doubles as a consistency regression
  net)
- A few read endpoints (browse page, search API, asset page) as
  smoke tests
- Goal is the harness + exemplary coverage, not exhaustive coverage;
  later features (read-only mode, doctor) write their tests against it

**Complexity:** Medium. The harness itself is small; the value is in
choosing assertions that lock real contracts.

### 3. `serve --read-only` + basic auth

`maki serve` has no trust boundary. Default bind is localhost, but
`--bind 0.0.0.0` exposes every write endpoint — including ones that
delete files from disk (duplicates page, dedup resolve) — to the LAN
with no authentication. Previously filed under "Mobile & Tablet
Browsing" as a convenience; reframed here as a safety feature and
shipped independently of any CSS work.

**Scope:**
- `--read-only` flag + `[serve] read_only` config: middleware guard
  rejecting all mutating routes (405 with a clear message); UI hides
  edit affordances when the build-info endpoint reports read-only
- Optional basic auth: `[serve] username` / `password` (password
  accepted via env var too, to keep it out of maki.toml if desired)
- A startup warning when binding to a non-loopback address with
  neither read-only nor auth enabled
- Tests via the new harness (guard matrix: read-only × auth ×
  endpoint class)

**Complexity:** Medium. Mostly route classification + middleware;
the endpoint inventory must be complete (a missed write route is a
hole, so derive the allowlist from the router table, not by hand).

### 4. Trash / quarantine for destructive operations

`cleanup --apply`, `dedup`, `delete`, volume remove, and the web
duplicates page all `fs::remove_file` outright. Undo/Edit-History
(roadmap Tier 3) is the full answer but High complexity; a trash layer
is ~20% of the effort for ~80% of the safety value, and can ship now.

**Scope:**
- Deleting operations move media files to
  `<catalog_root>/.trash/<date>/…` (preserving relative paths)
  instead of unlinking; sidecar-only and derived files (previews,
  embedding binaries) keep hard-deleting — they are regenerable
- `maki trash list` / `maki trash restore <id|path>` /
  `maki trash empty [--older-than 30d]`
- `[trash] enabled = true` (default on), `retention_days` for the
  `maki status` hint ("N files in trash older than retention →
  maki trash empty")
- `--no-trash` escape hatch on deleting commands for the
  disk-pressure case
- Cross-device moves: same-volume rename when possible, copy+delete
  fallback (trash lives on the catalog volume; document the
  implication for huge files on other volumes — possibly a per-volume
  `.maki-trash` instead, decide during design)

**Complexity:** Medium. The mechanics are simple; the design decision
that needs care is trash location for files on offline-capable media
volumes.

### 5. `maki doctor` — consistency checker

Make the dual-store invariant *verifiable* instead of assumed. This is
both a user-facing trust feature ("prove my metadata is consistent")
and the regression net for the v4.5.15 bug class.

**Scope:**
- Compare, per asset: YAML sidecar ↔ SQLite row field-by-field
  (tags, rating, label, description, dates, recipe pending flags,
  variant/recipe sets) and recomputed ↔ stored denormalized columns
  (`best_variant_hash`, `variant_count`, `face_count`,
  `leaf_tag_count`, …)
- Orphan detection both directions: sidecars without catalog rows,
  catalog rows without sidecars, variants without locations
- `--hashes` opt-in pass: re-hash recipe files on online volumes
  against stored content hashes (overlaps `maki verify` — doctor
  delegates or links rather than duplicating)
- Report mode by default; `--repair` rebuilds the SQLite side from
  YAML for flagged assets (sidecars are the source of truth, so
  repair is always "rebuild derived state", never "edit sidecars")
- `--sample N` for a fast probabilistic check; full scan for the
  nightly/cron use case; `--json` for tooling
- `maki status` gains a one-line hint when doctor hasn't been run
  recently (mirrors the verify-staleness pattern)

**Complexity:** Medium-High. The field-by-field comparison is
mechanical but must be kept in lockstep with the schema —
worth generating the comparison from one shared field list so schema
additions can't silently skip the checker.

### 6. Sidecar write locking

`maki serve` + a CLI command writing the same sidecar concurrently is
last-writer-wins with no detection. SQLite WAL protects the DB; the
YAML files have nothing.

**Scope:**
- Advisory lock (single lock file in catalog root, `fs2`/`flock`)
  acquired around metadata write operations; CLI commands and the
  web server share it
- Held per-operation (not per-process) so serve + CLI interleave
  safely instead of excluding each other
- Clear error/retry message when the lock is contended beyond a
  timeout

**Complexity:** Low-Medium. The subtlety is granularity — coarse
enough to be correct, fine enough that a long import doesn't freeze
the web UI's metadata edits (likely: lock per asset-save, not per
command).

---

## Horizon 2 — Foundation rework (v4.7–v4.8)

Fix the two root causes, then build the features that depend on them.

### XMP writer rework (prerequisite for IPTC/EXIF write-back)

Replace the regex-edit-in-place update functions in `xmp_reader.rs`
with a quick-xml-based parse → modify → render pipeline that keeps the
byte-stability guarantee for untouched regions. The reader is already
quick-xml and already correct; the writer is where every escape/
namespace bug lived. Add property-based round-trip tests (proptest) —
they would have caught the `&amp;amp;` escalation on day one. Only
after this lands should **IPTC/EXIF Write-Back** (roadmap Tier 1,
High complexity) be attempted: binary-format metadata surgery on top
of string-surgery foundations would compound the risk.

### Tag provenance model

Tags become `(value, source)` where source ∈ {user, xmp-import,
auto-tag, vlm} (+ optional confidence). Sidecar schema addition with
migration; `Vec<String>` stays as the compatibility view. Unlocks:
re-running auto-tag with a better model replacing only machine tags;
principled sync-metadata conflict resolution (replaces the
sidecar-wins special-casing); UI distinction between human curation
and machine suggestion. Prerequisite for Undo/History done properly.

### FTS5 free-text search

Free text currently runs LIKE scans. An FTS5 index over
name/filename/description/source-metadata raises the comfortable
catalog ceiling from ~100k toward 1M assets. Contained change:
index maintenance in the write paths + a query-builder branch.

### Unified search pipeline

One `SearchService` consumed by both CLI and web. The web layer's
`ResolvedSearch` (QA loop) was the consolidation step; this is the
unification step, eliminating the remaining CLI-vs-web semantic drift
(the `EmptyFilterPolicy` split becomes an explicit, documented
parameter instead of an accident of history). Also migrate the
`media.rs` export-zip enrichment cousin.

### Carried over from roadmap.md Tier 1 (no new prerequisites)

- **Watch Mode** — fits anywhere in this horizon
- **Auto-Stack by Similarity** *(Pro)* — fits anywhere
- **GPU embeddings for Linux/Windows** *(Pro)* — packaging-bound

---

## Horizon 3 — v5 territory

- **Undo / edit history** — cheap(er) once provenance + doctor exist:
  history rows are provenance deltas, and doctor already knows how to
  rebuild derived state
- **Multi-machine story** — catalog replication or a first-class
  "rebuild everywhere from sidecars" workflow; collections/stacks/
  faces already export to YAML, so the gap is orchestration + conflict
  handling, not data model
- **ANN embedding index** — replace the brute-force in-memory scan
  (HNSW via usearch or quantized vectors) when catalog size demands it
- **Mobile/PWA** — builds on read-only + auth from Horizon 1
- **Tethered shooting** — builds on Watch Mode
- **Print workflow** — unchanged from roadmap.md Tier 3

---

## Deliberate non-goals (for now)

- `#![warn(missing_docs)]` — would land ~60 warnings concentrated in
  web templates/routes; do it as its own pass or not at all
- Tag storage normalization (tag table with IDs instead of
  `a|b|c` strings) — provenance can be added without it; revisit only
  if FTS5 + provenance prove insufficient
- Plugin/extension API — no demonstrated need
