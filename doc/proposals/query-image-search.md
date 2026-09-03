# Proposal: Search by Query Image (reverse image lookup)

**Status:** ✅ shipped in v4.10.0 (2026-09-03) as designed below; the batch "find my originals" and persistence questions remain open.

## Goal

Find catalog assets visually similar to an image file that is **not** in the catalog — a preview or downsized copy someone sent back, an export whose original needs locating, a screenshot, a frame from a contact sheet. Today `similar:<id>` requires the reference to be a cataloged asset, so the only workaround is importing the query file, which pollutes the catalog with a throwaway.

The typical question is "which original does this preview belong to?", so the feature must surface an exact or near-exact match with high confidence, and degrade gracefully into ordinary "looks like this" browsing.

Pro-only (`ai` feature), like `similar:` and `text:`.

## Why this is mostly plumbing — grounded in the code

Everything expensive already exists:

- `SigLipModel::encode_image(path)` (`src/ai.rs`) encodes any decodable image file into an L2-normalized embedding. It is what `maki embed` runs per asset; nothing ties it to a catalog asset.
- `EmbeddingIndex::search(query, limit, exclude)` (`src/embedding_store.rs`) takes an arbitrary vector.
- The `text:` filter is precisely this feature with the text encoder swapped in: a query that is not an asset gets encoded, searched against the index, and its scored ID list is installed into `SearchOptions` so every other filter ANDs with it. See `src/query.rs` (`text_query` block) and `ResolvedSearch` in `src/web/routes/mod.rs`.
- The browse grid already renders similarity views (single page, `similarity_desc` sort, score badge per card) whenever `ResolvedSearch::has_similarity()` is true.

What is missing is (1) a shared helper that turns *a file path* into an embedding, (2) a CLI entry point, (3) a web upload endpoint plus a way to reference the uploaded query from the browse URL, and (4) the UI around it.

## Behavior

1. **Exact-copy fast path.** Hash the query file (`ContentStore::hash_file`) and look it up via `Catalog::find_asset_id_by_variant`. A byte-identical copy of a cataloged variant is answered without loading the model. The hit is reported as an exact match (score 100%, flagged `exact: true` in JSON).
2. **Embedding search.** Otherwise encode the query image and search the index for the configured `[ai] model`. Results are the top N (default 40, same as `similar:`) sorted by similarity, with `min_sim:` honored. All other filters combine with AND, exactly as `similar:` does.
3. **RAW / HEIC / other non-`image`-crate formats** are routed through `PreviewGenerator` into a scratch directory first, since `encode_image` only decodes JPEG/PNG/TIFF/WebP/BMP/GIF (`ai::is_supported_image`). The scratch file is removed afterwards; nothing is written under `<catalog>/previews`.
4. **No catalog mutation.** The query image is never imported, embedded, or stored. `serve --read-only` allows the feature.

Score expectations (to verify on a real catalog before choosing any default threshold): a resized copy or maki-generated preview of an original should score very close to 100%, clearly separated from unrelated images; a re-cropped or heavily edited derivative lands lower. The UI therefore shows the score on every card and does **not** apply a default `min_sim` — the gradient is the information.

## CLI

```bash
maki search --image ~/Downloads/IMG_4711_preview.jpg
maki search --image scan.tif min_sim:90 --json
maki search --image frame.png tag:events|wedding rating:3+
```

A **flag**, not a `image:<path>` query token: Windows paths contain a colon (`C:/…`), which collides with the `token:value:limit` grammar the parser uses for `similar:` and `text:`. The flag sets `opts.similar_asset_ids` through the same path the `similar:` block in `QueryEngine::search` uses, so `min_sim:`, `--format`, `--json`, sort and every other filter work unchanged. JSON output carries `similarity` per hit and `exact_match` when the fast path fired. `maki shell` inherits the flag.

Errors follow the existing messages: unknown model, no embeddings for the model (`Run maki embed first`), unsupported/undecodable query file, missing dcraw for a RAW query.

## Web UI

The entry point must be reachable **without selecting anything in the catalog** — the batch toolbar only appears with a selection, so it is the wrong home. The feature lives in the always-visible search row of the browse page:

- **"Find by image" button** in `.search-row` (`templates/filter_bar.html`), directly after the `q` input, next to the Filters toggle. Rendered only when `ai_enabled`. Opens a native file picker.
- **Drag-and-drop** of a file onto the search row or onto the results grid. A dashed drop-highlight on the search row signals the target while dragging (`dragenter`/`dragover` on `.browse-main`).
- Optional third route: paste an image from the clipboard while the browse page has focus (`paste` event with an image item). Cheap once the upload code exists; can ship later.

After the upload completes, the page navigates to `/?q=similar:@<token>` (plus whatever other filters were active). The browse page then shows:

- A **query pill** at the left of the saved-searches row (same row as the "Restored from last session" pill): the query thumbnail, the filename, "exact match found" when the fast path hit, and an `× Clear` that drops the `similar:@…` token and returns to the previous filter.
- The existing similarity grid: single page, sorted by score, score badge on each card. Tag chips, people, path, rating and label filters keep working because the token is resolved exactly like `similar:<id>`.
- Exact-match hits are pinned first with a distinct badge.

Reloading the page restores the pill from the token in the URL as long as the server still holds the query session (see below). An expired token shows a small "query image expired — drop it again" notice and clears the token.

The asset detail page and lightbox need no changes.

## Implementation plan

1. **Shared helper** `AssetService::query_image_embedding(path, config) -> Result<QueryImage>` in `src/asset_service/ai.rs`. Returns `{ content_hash, exact_asset_id: Option<String>, embedding: Vec<f32>, model_id }`. Handles the hash fast path, RAW→preview into a scratch dir, and model loading via `config::resolve_model_dir` / `SigLipModel::load_with_provider`. Web callers pass the already-loaded `state.ai_model` to avoid a reload.
2. **Extract the index search** duplicated at `src/query.rs` (`similar:` block) and `src/web/routes/mod.rs::resolve_similar_filter` into one `resolve_similar_ids(index, query_emb, limit, min_sim, exclude, prepend)` helper. Both existing callers and both new paths use it.
3. **CLI flag** `--image <path>` on `search` in `src/main.rs`; `QueryEngine::search` gains a `query_embedding: Option<&[f32]>` input that feeds `opts.similar_asset_ids`. Mutually exclusive with `similar:` in the same query (error, not silent precedence).
4. **Web upload** `POST /api/query-image`: raw request body as `axum::body::Bytes` with the `Content-Type` header naming the image type, written to a scratch file under the catalog's temp area. No multipart dependency (axum is built without that feature; the import dialog sends server-side paths as JSON, so this is the first browser→server file transfer). Raise `DefaultBodyLimit` on this route only (≈50 MB). Returns `{ token, thumbnail (data URL), filename, exact_match_id }`.
5. **Query session store** on `AppState`: `query_images: Mutex<HashMap<String, QueryImageSession>>` holding embedding, thumbnail, filename, exact-match ID, created-at. TTL eviction (e.g. 1 h, swept on insert), hard cap on entries. `resolve_similar_filter` gets a branch: `similar:@<token>` → look up the session instead of an asset. `GET /api/query-image/{token}` returns the pill data for reload; 404 when expired.
6. **Templates/JS:** button + drop target + paste in `filter_bar.html` / `filter_bar_js.html`, query pill in `browse.html`, badge for exact hits in `results.html`. Feature-gated by `ai_enabled` like the Embed controls.
7. **Tests:** helper unit tests (hash fast path, unsupported format, RAW routed through preview when dcraw present, skipped otherwise); web harness test for upload → token → browse resolution → expiry; CLI test with a fixture JPEG against a catalog embedded with a stub. Run both standard and pro matrices.
8. **Docs:** new `--image` section in `doc/manual/reference/06-search-filters.md` (next to `similar:`), a "Find the original of a preview" workflow in user-guide chapter 12 Visual Discovery, the web-page filter matrix row, `doc/specification.md`, CHANGELOG, roadmap Completed line.

## Effort and risk

Small to medium: one to two days. The only design decision with reach is the web session store; the `@token` form keeps every existing filter combination without touching the search core or the URL scheme. Risk is low — no catalog writes, no schema change, feature-gated.

## Open questions

- Should `--image` accept a directory or several files and return one result set per file (batch "find my originals")? Natural follow-on once the helper exists; out of scope for v1.
- Persist query sessions across `serve` restarts? No — a re-drop costs one click.
- Clipboard paste in v1 or later? Recommend v1 if the drop handler is already there; it is ~15 lines.
