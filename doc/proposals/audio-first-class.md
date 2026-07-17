# Proposal: Audio as a First-Class Media Type (and the Audio Workbench Question)

Status: draft for discussion · July 2026

## Goal

Decide where the "audio processing workbench" idea belongs — a
management/ordering/overview layer over the existing audio CLI toolchain
(audio-separator, rubberband, keyfinder-cli, beat_this, sung-notes.py,
ffmpeg/sox — see the pitch-shift tutorial series) — and specifically
whether the answer is: make audio a first-class citizen in MAKI.

Ground rule carried over from the workbench discussion: **no audio
processing in MAKI, ever.** The CLI tools produce; the workbench manages,
orders, visualizes. This matches MAKI's existing stance for images
(Capture One / RAW converters process; MAKI keeps, indexes, tracks
recipes as *artifacts*, not as operations it performs).

## Why MAKI is the natural home — grounded in the code

The philosophical fit is not aspirational, it's already implemented:

- `AssetType::Audio` exists today (`src/models/asset.rs`), and the core
  data model is type-generic: one `assets` table with a type
  discriminator, content-addressed `variants` keyed by SHA-256, YAML
  sidecars as source of truth, rebuildable catalog. Audio needs **no new
  core tables** to become first-class.
- Audio files already import (the `audio` file-type group is default-on
  in `src/asset_service.rs`), and `src/preview.rs::extract_audio_metadata`
  already reads duration/bitrate/sample-rate/channels via `lofty` — but
  only to paint an info card. The data is thrown away: it never reaches
  `variant.source_metadata`, has no typed columns, no search filters.
- Shelling out to type-specific external tools with `tool_available()`
  probing is the established pattern (dcraw, ffmpeg, ffprobe in
  `src/preview.rs`). keyfinder-cli or beat_this as optional audio
  extractors would be the fourth and fifth entries in an existing list,
  not a new architecture.

So audio today is "importable file with a placeholder thumbnail" — the
gap to first-class is entirely in the type-branched periphery, exactly
where video already blazed the trail (ffprobe extraction, ffmpeg frame
grab, denormalized `video_duration`/`video_codec` columns).

## Two decisions, not one

The workbench question hides two separable decisions. Conflating them is
what makes "should it be a MAKI module?" feel unclear.

**Decision 1 — index audio properly.** Metadata extraction, typed
search, useful previews. This is pure Keeper-&-Indexer territory,
uncontroversial, valuable even if the workbench never happens (a music
library with thousands of files benefits from `maki find` on duration,
key, BPM exactly like photos benefit from ISO/lens filters).

**Decision 2 — model the derivation graph.** A practice-track project is
a *tree of derived content*: song → stems → pitch-shifted stems → mixes,
clicks, LRC files. Managing that tree (what exists, what produced it,
what's stale) is the workbench's actual job, and it is the part where
MAKI's current concepts genuinely don't fit yet — this needs a real
choice, made below.

## Decision 1: audio indexing (recommended, low risk)

Concrete shape, citing the touch points:

- **Extraction**: add the audio branch in
  `src/asset_service/import.rs` — route `lofty` output into
  `variant.source_metadata` and the sidecar, like ffprobe does for
  video. (Optional refactor while there: pull the per-type `if/else`
  into a `MetadataExtractor` trait; three media types is the point where
  the branching starts to smell. Not a prerequisite.)
- **Typed columns**: migration adding `duration_seconds` (shared —
  fold `video_duration` into it), `sample_rate`, `channels`, `bit_rate`,
  and two *musical* fields: `audio_key`, `audio_bpm`. The `video_*`
  columns on `assets` are the precedent; generalizing `duration` fixes
  the precedent's wart instead of copying it.
- **Musical metadata is just another extractor.** keyfinder-cli and
  beat_this are optional shelled tools (probe, warn, skip) that fill
  `audio_key`/`audio_bpm` — the audio analog of "similarity data,
  tagging, indexing". The caveats from the find-your-key guide apply
  (key detection is only trustworthy on full mixes, not on voice
  recordings), so these belong behind an explicit
  `maki audio analyze`-style opt-in per folder/volume, not silently in
  every import.
- **Previews**: waveform PNG via ffmpeg (`showwavespic`) or
  `audiowaveform`, same shell-out-and-cache pattern as video frame
  grabs; keep the current info card as the overlay. Spectrograms
  (`showspectrumpic`) are a cheap second variant behind the existing
  `preview_variant` mechanism.
- **Web/UI**: `<audio>` players in the asset detail view (htmx-friendly,
  no JS framework needed), duration/key/BPM in filter UI and FTS.

None of this touches the asset/variant/stack/tag semantics. Tags stay
human/semantic (genre, project, "vocal-lesson"); technical and musical
facts live in typed fields where they can be filtered and sorted.

## Decision 2: the derivation graph — three options

The question from the discussion: are MAKI's variants/stacks the right
level for "several pitch shifts of the same song"? Looking at the actual
semantics, mostly no:

- **Variant grouping** means *renditions of the same logical content*,
  grouped at import by filename stem (RAW+JPEG+XMP). A −8st playback
  mix is different content with different bytes, a different musical
  key, and a parameterized derivation history; stem-grouping wouldn't
  even cluster it with its source. `VariantRole::Processed` is
  spiritually close but carries no parameters and no parent link beyond
  "same asset".
- **Stacks** group *independent* assets by human judgment
  (burst/same-scene). Deriveds are not independent and the grouping is
  mechanical, not curatorial.

So derived audio wants a third relationship that MAKI doesn't have:
**a derivation edge** — this asset was produced from that asset by this
tool with these parameters. Options:

### Option A: stretch variants

Model deriveds as `VariantRole::Processed` variants of the song asset,
parameters stuffed into `source_metadata`.

*For*: zero schema work. *Against*: breaks variant semantics ("best
variant" scoring, stem grouping, `preview_variant` all assume
renditions); a click track or LRC as a "variant of the song" is a
category error; parameters as untyped blob. **Not recommended** — this
is the choice that would quietly corrupt a clean model.

### Option B: derivation edges in core

New concept: `derived_from` (parent asset id, tool, parameters, source
content hash at derivation time) on assets, in sidecar + catalog.
Deriveds stay first-class assets (they are: you tag them, export them,
play them), plus a typed edge. Generalizes beyond audio — video proxies
and image exports have the same shape — and enables "stale" detection
(parent hash changed since derivation).

*For*: the honest model; queryable ("all deriveds of X", "everything
stale"); fits sidecar-first and the deterministic UUID-v5 scheme.
*Against*: a core schema migration + sidecar schema addition + UI in a
91k-LOC stable codebase, justified so far by one media type's workflows.

### Option C: project manifest sidecar, outside core

A per-project `song.yaml` (or `project.yaml`) manifest — written by the
*render scripts/recipes*, next to the files, listing the tree: source,
deriveds, tool + parameters each. MAKI core indexes the files as plain
assets; a workbench view reads the manifest for grouping, provenance
display and staleness. Core schema untouched.

*For*: zero core risk; source-of-truth-in-files matches MAKI's
architecture; the manifest format can iterate at workbench speed, not
migration speed; works even for files MAKI hasn't imported yet.
*Against*: two grouping mechanisms in the UI (manifest projects vs.
collections); provenance invisible to `maki find` until promoted.

### Recommendation: C now, B when proven

Start with the manifest (C): it's reversible, it forces the derivation
vocabulary (tool, params, parent) to stabilize against real use — a few
songs through the find-your-key and click-track pipelines will bend the
schema in ways worth learning cheaply. When the manifest format has
survived a season, promote it to core derivation edges (B) with a
migration that ingests existing manifests. A is rejected outright.

This mirrors how MAKI already treats external editors' recipes: track
the artifact (the sidecar/manifest), don't own the operation.

## Where the workbench UI lives

With Decisions 1+2 made, the "workbench" shrinks to: a song/project
dashboard view over indexed assets + manifests, with players,
visualizations (waveform, pitch histogram, tempo curve — all from data
the CLI tools emit), and links. That is a **MAKI web module**: axum
routes + askama templates + htmx, exactly the existing stack, reusing
auth, players, previews, jobs/SSE.

Considered and set aside:

- **Separate workbench app** on MAKI's REST API: keeps core pristine,
  but duplicates UI plumbing and splits the user across two web UIs for
  one hard drive of media. The REST API keeps this door open if audio
  churn ever threatens core stability — it's a fallback, not the plan.
- **Static song-report generator** (CLI → HTML per project): still a
  fine week-one experiment to discover which visualizations matter, and
  its templates would migrate into the module. Optional stepping stone,
  not a destination.

**Does the workbench *run* the pipelines?** Not initially. The render
commands stay manual CLI (they are documented, verified, and fast to
invoke); the manifest + filesystem watcher (`src/asset_service/watch.rs`)
picks results up. The ephemeral web `JobRegistry` is not a durable job
queue, and building one is real scope — defer until manual invocation
demonstrably hurts. "Run recipe" buttons are a later convenience, not
the foundation. (This also keeps the no-processing rule structurally
true: MAKI never even spawns the producing process in phase one.)

## Phasing

1. **Index** (Decision 1): extraction branch, typed columns +
   `duration` generalization, waveform previews, filters, players.
   Value independent of everything below.
2. **Projects** (Decision 2, option C): manifest format, written by the
   render scripts from the tutorial series; workbench dashboard route
   rendering project trees with provenance and staleness.
3. **Promote** (option B) once the manifest schema has stabilized:
   derivation edges in core, `maki find --derived-from`, stale queries.
4. **Horizons**, explicitly out of scope now: audio similarity
   embeddings (CLAP — the audio sibling of the SigLIP setup in
   `src/ai.rs`, behind the `ai` feature gate), chromaprint
   fingerprinting for fuzzy duplicate detection (same song, different
   encoding — the audio analog of a need `duplicates.rs` can't meet
   with exact hashes), durable job execution.

## Open questions

- Project identity: is a "song project" a folder convention, a manifest
  location, or a collection? (Current folders like
  `BackingTracks/<song>/` suggest folder-as-project with the manifest
  at its root.)
- How much musical metadata belongs in *typed columns* vs.
  `source_metadata` blob? (Key and BPM earn columns; sung-note
  histograms probably stay JSON artifacts referenced by the manifest.)
- Should `maki audio analyze` results write back into the audio files'
  own tags (ID3/Vorbis) for portability, the way XMP write-back works
  for images — or stay sidecar-only?
- Naming: "workbench" as a view ("Projects"?) inside MAKI's web UI.
