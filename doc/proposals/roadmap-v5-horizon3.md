# Roadmap v5 — Horizon 3

Planning document for the v5 development arc, following the v4.6/v4.7/v4.8
cycle. Successor to `roadmap-v4.6-horizons.md` (Horizons 1 and 2, both
complete).

Where the v4.6 horizons doc earned its tight themes — Horizon 1 "trust
hardening," Horizon 2 "foundation rework" — Horizon 3 as originally
sketched was a grab-bag of "v5 territory." This document imposes
structure: two arcs plus two outliers, with the items re-assessed in
light of everything Horizons 1–2 shipped (provenance, doctor, the
locked atomic write path, watch mode, the proven rebuild-from-sidecars
guarantee, the EmbeddingIndex search seam).

---

## The shape of Horizon 3

Two coherent arcs, two outliers:

| Item | Arc | Complexity | Now unblocked by | Risk |
|---|---|---|---|---|
| **Undo / edit history** | Reversibility | Med-High | provenance, doctor, atomic+locked writes | Medium, well-understood |
| **ANN embedding index** | Scale | Low-Med | the `EmbeddingIndex` seam | Low, contained |
| **Multi-machine** | Reach | **High** | rebuild-from-sidecars proven, provenance | **High — design-first** |
| **Mobile / PWA** | Reach | Medium | `serve --read-only` + auth (H1) | Low, frontend |
| **Tethered shooting** | Reach | Low-Med | watch mode (H2) | Low |
| **Print workflow** | Output | High | contact-sheet layout engine | Medium (ICC color) |

The **Reversibility** arc continues the trust line: Horizon 1 made the
data *trustworthy*, this makes every change *reversible*. The **Reach**
arc is the v5 headline — the catalog escapes the single machine.

---

## Decided forks

Three product decisions that were open when this plan was drafted, now
settled (revisit if real use argues otherwise):

1. **Undo scope: bounded LIFO undo at operation granularity**, not a
   forever audit log. `maki undo` reverts the most recent operation;
   again reverts the one before; etc. An "operation" is one
   command / web action and may touch many assets (a 500-asset tag
   rename undoes as one unit). The journal retains the last N
   operations (configurable) and prunes older. This covers the
   "oops, I just batch-edited the wrong set" case — the actual reason
   anyone wants undo — without the weight of permanent history.

2. **History journal location: a dedicated, non-authoritative
   `<catalog>/history/` log**, NOT inside the asset sidecars. It is
   neither source-of-truth (sidecars) nor derived cache (catalog.db):
   it is an independent, prunable, append-only record. It survives
   `rebuild-catalog` (which only rewrites catalog.db from sidecars),
   is not checked by `maki doctor`, and can be deleted at any time —
   losing undo capability but nothing about catalog correctness. This
   keeps it entirely out of the dual-store invariant.

3. **Multi-machine: MAKI is the reconcile layer, not the transport.**
   The user syncs `<catalog>/metadata/` (the sidecars) and the media
   volumes via whatever they already use (Syncthing, Dropbox, rsync,
   git-annex). Each machine keeps its own `catalog.db` — a derived
   cache, never synced. MAKI provides conflict detection and a
   provenance-aware merge for sidecars that two machines edited
   between syncs. This fits "sidecars are truth," reuses the proven
   rebuild path, and is dramatically less code than MAKI owning a
   sync protocol. (ANN scale work is deferred until search/clustering
   speed is a felt pain — see below.)

---

## Reversibility arc

### v4.9.x — Edit history + undo

**The structural payoff.** Beyond the user-facing undo, this finally
builds the **write-through choke point** the codebase has wanted since
the v4.5.15 YAML/SQLite divergence bug. Today metadata mutations are
scattered across dozens of sites; `maki doctor` verifies consistency
*after the fact*. A single `AssetWriter`-style path that every mutation
flows through makes the dual-store update atomic-by-construction *and*
gives undo its recording point for free. The QA assessment that opened
this whole cycle flagged this refactor first; undo is the forcing
function that justifies it.

**Placement.** The choke point lives at the `QueryEngine` mutation
layer (`set_rating_inner`, `tag_inner`, `set_*`, `batch_*`), not in the
CLI command layer — so CLI, web, and shell edits are all journaled and
write-through-consistent for free (the web handlers already call these
methods).

**Journal model.**
- `<catalog>/history/` holds one file per operation
  (`<timestamp>-<opid>.json`), written atomically (temp + rename).
- Each operation: id, timestamp, command label, human summary, source
  (cli/web/shell), and a list of per-asset deltas. Each delta stores
  the full `before` and `after` asset state (a sidecar is small;
  storing the whole prior state is robust and lets `history` show a
  diff and `undo` restore exactly).
- Undo of an operation, per touched asset: verify the asset's *current*
  state still equals the recorded `after` (a later edit may have
  touched it) → restore `before` through the same write-through path
  (sidecar + catalog + denormalized columns + XMP write-back) →
  mark the operation undone. Assets that changed since are reported
  and skipped unless `--force`.
- LIFO: `maki undo` reverts the newest not-yet-undone operation.
  Marking-undone is a move to `history/undone/`. Redo is a noted
  follow-on (re-apply newest undone op), not in the first release.
- Config `[history]`: `enabled` (default true), `max_operations`
  (prune oldest beyond N, default ~200).

**Scope of v4.9.0** (kept shippable and manually testable):
- Field-level metadata edits: rating, label, description, name, date,
  and tag add/remove/clear (single + batch). These are the
  highest-frequency edits and already funnel through the
  `BatchContext` `*_inner` family — the natural seam.
- `maki undo [--dry-run] [--force]` and `maki history [<asset>]
  [--limit N] [--json]`.

**Follow-ons (v4.9.x):**
- Operation-level undo for tag rename / split / delete / fix-unicode
  (multi-asset, already operation-shaped).
- Structural ops (group / split / stack / merge) — these snapshot
  catalog *structure*, not just fields, and need their own design.
- Web: an undo toast after batch edits + a per-asset history panel
  (the journal data is already there; this is UI).
- Redo.

**Explicitly NOT undoable** (they have their own recovery): `import`
(re-import is idempotent; the inverse is `delete`), `delete`
(already trash-backed), preview/embedding/face generation
(regenerable).

### ANN embedding index — slot in when felt

The brute-force in-memory scan behind `similar:`, the stroll page, and
`auto-stack` all go through one `EmbeddingIndex` abstraction — the seam
an HNSW (e.g. `usearch`) or quantized-vector backend slots behind with
no caller changes. This only matters at hundreds of thousands of
embedded assets. **Deferred until it is a felt pain** (text-to-image
latency, large `auto-stack` runs) rather than scheduled. When triggered,
it is a contained, low-risk perf release.

---

## Reach arc — v5.0 headline

### Multi-machine (proposal-doc-gated)

The version-bump-worthy feature, and the one item that needs its own
design proposal + an explicit go/no-go **before any code** — guessing
the topology wrong is expensive.

Working design (per decided fork #3): the **shared-sidecars,
per-machine-cache, external-transport, MAKI-reconciles** topology.

- Each machine: own `catalog.db` (cache, never synced), shared
  `metadata/` + volumes via the user's transport.
- After a sync, a machine refreshes its cache (`rebuild-catalog` /
  incremental doctor) — already proven to reconstruct everything.
- Conflicts are sidecar-level: two machines edit asset X between syncs,
  the transport produces a conflict artifact (Syncthing
  `.sync-conflict-*`, git merge markers, etc.). MAKI detects these and
  offers a **provenance-aware three-way merge** — user tags beat
  machine tags, ratings/labels surface as explicit conflicts like
  `sync-metadata` already does for XMP.

Deliverables: bless + document the topology; a conflict-artifact
detector; a provenance-aware sidecar merge tool; making the
post-sync cache refresh cheap (incremental). **Proposal doc first.**

### Mobile / PWA

Depends on nothing new (read-only + basic auth shipped in H1; the
in-process axum test harness locks the API side). Pure frontend:
responsive grid, touch/swipe lightbox, collapsible filter bar, a PWA
manifest + service worker for installable, offline-tolerant browsing.
Low risk, high "show clients photos on the iPad" value. Interleaves
with the Reach arc.

### Tethered shooting

Watch mode (H2) did the hard part. Tethered = watch + lower latency +
auto-open the imported asset in the web UI; CaptureOne hot-folder as
the primary case. Niche but high-delight for the target user (a
working photographer). Low-medium.

---

## Output outlier

### Print workflow

`maki print` / a web print button: single- and multi-image page
layouts with margins and ICC color profiles. The contact-sheet layout
engine already exists; color management is the genuinely hard, narrow
part. Smallest audience — last, or only if asked for.

---

## Proposed release map

| Release | Contents | Status |
|---|---|---|
| **v4.9.0** | Edit-history journal + write-through choke point; `maki undo` / `maki history` for field edits | ✅ shipped 2026-06-23 |
| **v4.9.3** | Audio phase 1 (outlier — see `audio-first-class.md`), schema v11 | ✅ shipped 2026-08-15 |
| **v4.10.0** | Search by query image (outlier, demand-driven — see `query-image-search.md`) | ✅ shipped 2026-09-03 |
| **v4.10.x** | Undo for tag rename/split/delete; structural-op undo; web undo toast + history panel; (redo) | next |
| **v5.0.0** | Multi-machine (after its proposal doc + go/no-go); the Reach headline | proposal-gated |
| **v5.x** | Mobile/PWA, tethered — interleaved | planned |
| **when felt** | ANN index (scale-triggered) | deferred |
| **if asked** | Print | deferred |

Progress: the Reversibility arc's foundation (the write-through choke
point) and the `maki undo` / `maki history` commands landed in v4.9.0.
The journal is built as the planned `<catalog>/history/` log with
bounded LIFO undo at operation granularity — both decided forks held.
What remains in the arc: extending undo past field edits (tag
rename/split/delete, then structural ops), redo, and the web UI.

Sequencing rationale: reversibility first — it is the natural
continuation of the trust arc, the lowest-risk highest-value item, and
it pays for itself structurally (the write-through refactor) before the
undo UX even lands. Multi-machine is the headline but is design-gated.
ANN and print are demand-driven, not scheduled.

---

## Deliberate non-goals (still)

- MAKI owning a sync transport (decided fork #3 — reconcile, don't
  transport).
- A forever audit log (decided fork #1 — bounded LIFO undo).
- Undo of import/delete (they have their own recovery paths).
- Real-time collaborative editing / a server-of-record model — MAKI
  stays a local-first tool whose source of truth is text on disk.
