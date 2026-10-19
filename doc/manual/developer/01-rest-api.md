# REST API Reference

The `maki serve` command starts an Axum-based web server (default `127.0.0.1:8080`) that exposes HTML page routes, htmx fragment endpoints, and JSON APIs. This chapter documents every route registered in `build_router` (`src/web/mod.rs`); the handlers live in `src/web/routes/`.

*Last reconciled with the router: v4.12.2 (2026-10-19).*

Base URL: `http://127.0.0.1:8080` (configurable via `--bind` and `--port`, or `[serve]` in `maki.toml`).

## Conventions

- **Connections**: SQLite access runs in `spawn_blocking`; each handler checks a connection out of the pre-opened `CatalogPool` (`state.catalog()`, an RAII `PooledCatalog` returned on drop). Nothing migrates at serve time -- the CLI verifies the schema version before the server starts.
- **Request bodies**: single-asset edit endpoints take `application/x-www-form-urlencoded` (they are htmx targets) and return HTML fragments; everything else takes and returns `application/json` unless stated otherwise.
- **Asset IDs**: path parameters accept a full UUID or a unique prefix.
- **Errors**: handlers that fail return a plain-text body `Error: <message>` with status 500; validation errors return 400 where the handler checks input explicitly (noted per endpoint). A few AI endpoints return `{"error": "..."}` JSON instead.
- **Read-only mode** (`--read-only` or `[serve] read_only`): every request whose method is not `GET`/`HEAD` is rejected with 403 -- except `POST /api/query-image`, which writes nothing.
- **Basic auth** (`[serve] username`/`password`, or `MAKI_SERVE_PASSWORD`): applies to every route including previews and SSE streams; a missing or wrong credential yields 401 with `WWW-Authenticate: Basic realm="MAKI"`.
- **Feature gates**: routes marked *(ai)* exist only in builds with `--features ai` (which `--features pro` includes). `POST /api/asset/{id}/writeback`, `vlm-describe`, `/api/batch/describe`, `/api/maintain/writeback` and `/api/maintain/sync-metadata` are registered in every build; the web UI surfaces them only in Pro builds (the `pro` flag in `/api/build-info`).
- **`HX-Trigger: pending-changed`**: the tag, rating, description, name and label fragment responses carry this header so the detail page can refresh its recipes block (pending-writeback markers).

### Shared browse filter parameters {#browse-params}

The browse page and every endpoint that mirrors its result set (`/api/search`, `/api/all-ids`, `/api/page-ids`, `/api/facets`, `/api/calendar`, `/api/map`, and `filters` in `/api/batch/export`) accept the same query parameters (`SearchParams` in `src/web/routes/mod.rs`):

| Parameter    | Default     | Description |
|--------------|-------------|-------------|
| `q`          | `""`        | Free text and filter tokens, same syntax as `maki search` (see the [Search Filter Reference](../reference/06-search-filters.md)) |
| `type`       | `""`        | Asset type (`image`, `video`, `audio`, ...) |
| `tag`        | `""`        | Comma-separated chip list, ANDed; a leading `-` on an entry excludes it; `=`/`/`/`^` markers as in the `tag:` filter |
| `format`     | `""`        | File format; comma-separated for OR (`nef,cr3`) |
| `volume`     | `""`        | Volume ID |
| `rating`     | `""`        | Numeric filter (`5`, `3+`) |
| `label`      | `""`        | Color label name; `none` selects unlabeled assets |
| `collection` | `""`        | Collection name |
| `path`       | `""`        | Path prefix; an absolute path under a volume mount point is normalized and pins that volume |
| `person`     | `""`        | Comma-separated person names *(ai)* |
| `sort`       | `date_desc` | `date_desc`, `date_asc`, `name_asc`, `name_desc`, `size_asc`, `size_desc`, `similarity_desc`, `similarity_asc` |
| `page`       | `1`         | Page number (`[serve] per_page` results per page, default 60) |
| `stacks`     | `1`         | `1` collapses stacks to their pick; `0` shows every member |
| `nodefault`  | --          | `1` disables the `[browse] default_filter` from `maki.toml` |

A `similar:<id>` or `similar:@<token>` query renders as one score-sorted page; with no explicit `sort` the effective sort becomes `similarity_desc`. `/api/calendar` and `/api/map` do not apply the AI text-query and `similar:` filters.

---

## Pages

All page routes return `text/html`. Pages that carry filters accept the [shared browse parameters](#browse-params).

| Route | Description |
|-------|-------------|
| `GET /` | Browse page. With an `HX-Request` header only the results grid fragment is returned. Response carries `Cache-Control: no-store`. |
| `GET /asset/{id}` | Asset detail page. Optional `prev` / `next` query parameters carry the neighbouring asset IDs from the grid. 404 with an HTML body if the asset does not exist. |
| `GET /compare?ids=a,b[,c,d]` | Side-by-side comparison of 2 to 4 assets; 400 for any other count, 404 if an ID is unknown. |
| `GET /tags` | Hierarchical tag tree with own and leaf counts. |
| `GET /stats` | Catalog statistics. |
| `GET /analytics` | Analytics dashboard. |
| `GET /backup` | Backup coverage (non-media volumes). |
| `GET /volumes` | Volume management. |
| `GET /collections` | Collections. |
| `GET /saved-searches` | Saved searches management. |
| `GET /duplicates?mode=all\|same\|cross&volume&format&path` | Duplicate file groups; `mode` defaults to `all`. |
| `GET /people` *(ai)* | People gallery. |
| `GET /stroll?id&q&n&mode&skip&cross_session` *(ai)* | Visual exploration page; without `id` a random embedded asset is the centre. Parameters as for `/api/stroll/neighbors`. 404 when no embeddings exist. |

```bash
curl "http://localhost:8080/?q=sunset&rating=4%2B&sort=date_desc&page=2"
curl -H "HX-Request: true" "http://localhost:8080/?tag=landscape"      # grid fragment only
```

---

## Browse and search

### `GET /api/search`

Same parameters as `GET /`. With an `HX-Request` header it returns the results fragment (`text/html`); without one it redirects (303) to `/?<same query string>` so a bookmarked API URL lands on the page.

### `GET /api/all-ids`

Ordered asset IDs of the whole result set (select-all across pages, remembered-filter restore). Honours the AI text-query and `similar:` filters.

```json
{"ids": ["550e8400-...", "..."], "total": 487, "total_pages": 9, "per_page": 60}
```

### `GET /api/page-ids`

IDs of one page, in grid order (lightbox and detail-page navigation). Extra parameter `nocount=1` (or `true`) skips the count query: `total_pages` is then `null` and `page` is not clamped to the last page.

```json
{"ids": ["..."], "page": 2, "total_pages": 9}
```

### `GET /api/facets`

Facet counts for the sidebar under the current filters (`FacetCounts`). Tuples serialize as arrays.

```json
{"total": 487,
 "ratings": [[5, 12], [null, 300]],
 "labels": [["Red", 4], [null, 480]],
 "formats": [["nef", 200], ["jpg", 287]],
 "volumes": [["<volume-id>", "Photos2024", 487]],
 "tags": [["landscape", 120]],
 "years": [["2024", 487]],
 "geotagged": 42}
```

### `GET /api/paths?q&volume&limit`

Path completions for the filter bar: the distinct next segments under prefix `q` in `file_locations.relative_path`. Directory completions end in `/`. `limit` defaults to 100 (maximum 100). An absolute path that matches a volume mount point is normalized and scoped to that volume; a `q` containing `*` returns `[]`.

```json
["2024/", "2024-backup/", "notes.txt"]
```

### `GET /api/calendar?year=2026`

Per-day counts for the heatmap. `year` defaults to the current year; the other [browse parameters](#browse-params) apply (except `sort`/`page`).

```json
{"year": 2026, "counts": {"2026-03-01": 12, "2026-03-02": 3}, "years": [2019, 2024, 2026]}
```

### `GET /api/map?limit=10000`

Markers for geotagged assets matching the filters (default `limit` 10000).

```json
{"markers": [{"id": "...", "lat": 47.9, "lng": 11.5, "preview": "/preview/ab/ab12....jpg",
              "name": "DSC_0001.NEF", "rating": 4, "label": null}],
 "total": 42, "truncated": false}
```

### `GET /api/tags`

All tags with counts as `[name, count]` pairs, merged with the planned entries of `vocabulary.yaml` at count 0 (autocomplete source).

```json
[["event|festival", 12], ["landscape", 120], ["planned|unused", 0]]
```

### `GET /api/stats`

```json
{"assets": 1234, "variants": 2100, "recipes": 900, "total_size": 123456789}
```

### `GET /api/build-info`

Compile-time features and serve-time defaults the JavaScript layer needs.

```json
{"ai": true, "pro": false, "slideshow_seconds": 5, "slideshow_loop": true,
 "remember_latest_filter": false, "import_smart_previews": false,
 "import_embeddings": false, "import_descriptions": false, "read_only": false}
```

---

## Asset

Path parameter `{id}` is the asset UUID or a unique prefix.

### Metadata edits (form-encoded, HTML fragment responses)

| Method and path | Form field | Notes |
|-----------------|------------|-------|
| `POST /api/asset/{id}/tags` | `tags` (comma-separated; `>` is converted to the `\|` hierarchy separator) | Returns the tags fragment with provenance badges |
| `DELETE /api/asset/{id}/tags?tag=<tag>` | query parameter `tag` | Removes one tag |
| `POST /api/asset/{id}/tags/clear` | -- | Removes every tag |
| `PUT /api/asset/{id}/rating` | `rating` (1-5; `0` or absent clears) | |
| `PUT /api/asset/{id}/description` | `description` (empty clears) | |
| `PUT /api/asset/{id}/name` | `name` (empty clears; fragment falls back to the original filename) | |
| `PUT /api/asset/{id}/label` | `label` (`Red`, `Orange`, `Yellow`, `Green`, `Blue`, `Pink`, `Purple`, case-insensitive; empty clears) | Unknown names are rejected (500) |
| `PUT /api/asset/{id}/date` | `date` (`YYYY`, `YYYY-MM`, `YYYY-MM-DD`, or RFC 3339) | No `HX-Trigger` header; invalid input is a 500 |

All of these except `date` add `HX-Trigger: pending-changed`.

```bash
curl -X POST -d "tags=landscape,sunset" http://localhost:8080/api/asset/550e8400/tags
curl -X DELETE "http://localhost:8080/api/asset/550e8400/tags?tag=sunset"
curl -X PUT -d "rating=5" http://localhost:8080/api/asset/550e8400/rating
```

### Preview and variants

| Method and path | Body | Response |
|-----------------|------|----------|
| `POST /api/asset/{id}/preview` | -- | Preview fragment (HTML); regenerates preview and smart preview from an online source. Offline sources and tool failures are reported inside the fragment, not as 500 |
| `POST /api/asset/{id}/rotate` | -- | Preview fragment; cycles rotation 0, 90, 180, 270 degrees and regenerates |
| `POST /api/asset/{id}/preview-variant` | JSON `{"content_hash": "sha256:..."}` (omit or `null` to clear) | `{"ok": true}` |
| `POST /api/asset/{id}/variant-role` | JSON `{"content_hash", "role"}` | `{"ok": true, "role": "alternate"}` |
| `POST /api/asset/{id}/reimport-metadata` | -- | Tags fragment (HTML) after clearing and re-extracting metadata from the variant files |
| `GET /api/asset/{id}/recipes-fragment` | -- | Recipes block (HTML) with current pending-writeback markers |
| `POST /api/asset/{id}/split` | JSON `{"variant_hashes": ["sha256:..."]}` | `{"source_id": "...", "new_assets": ["..."]}` |

### Writeback and VLM

| Method and path | Body | Response |
|-----------------|------|----------|
| `POST /api/asset/{id}/writeback` | -- | `{"written": 1, "skipped": 0, "failed": 0}` -- flushes this asset's pending XMP changes |
| `POST /api/asset/{id}/vlm-describe` | JSON `{"mode": "describe\|tags\|both", "model": "..."}` (both optional, defaults from `[vlm]`) | `{"description": "...", "tags": ["..."]}` |

### Stacks (per asset)

| Method and path | Body | Response |
|-----------------|------|----------|
| `PUT /api/asset/{id}/stack-pick` | -- | `{"pick": "<id>"}` |
| `DELETE /api/asset/{id}/stack` | -- | `{"status": "dissolved"}` |
| `POST /api/asset/{id}/stack-add` | JSON `{"asset_ids": [...]}` | `{"added": 2}` -- adds the listed assets to the stack of `{id}` |

Stack errors (asset not in a stack, fewer than two members) return 400.

### AI per asset *(ai)*

| Method and path | Body | Response |
|-----------------|------|----------|
| `POST /api/asset/{id}/suggest-tags` | -- | `[{"tag": "subject\|vehicle\|car", "confidence": 0.42, "existing": false, "source_label": "car"}]` (`source_label` only for non-identity vocabulary mappings) |
| `POST /api/asset/{id}/similar` | -- | 20 nearest neighbours: `[{"asset_id", "similarity", "preview_url", "name"}]`; computes and stores the embedding first if missing |
| `POST /api/asset/{id}/stack-similar` | JSON `{"threshold": 85, "limit": 40}` (both optional, percent) | `{"stack_id", "member_count", "threshold"}`; 400 `{"error": "..."}` if already stacked or nothing above the threshold |
| `POST /api/asset/{id}/embed` | -- | `{"embedded": 1, "skipped": 0, "errors": []}` (synchronous); 500 if the model is not downloaded |
| `GET /api/asset/{id}/faces` | -- | `[{"face_id", "confidence", "bbox": [x, y, w, h], "person_id", "person_name", "crop_url"}]` |
| `POST /api/asset/{id}/detect-faces` | -- | `{"succeeded": 1, "faces_detected": 2, "errors": []}` (synchronous); 500 `{"error"}` if face models are missing |

---

## Batch

All batch endpoints take JSON with `asset_ids` and return JSON. Edit-style endpoints respond with

```json
{"succeeded": 10, "failed": 1, "errors": [{"asset_id": "...", "error": "..."}]}
```

Job-style endpoints respond immediately with `{"job_id": "<uuid>", "status": "started"}`; progress and the summary arrive through the [jobs API](#jobs).

| Method and path | Body (besides `asset_ids`) | Response |
|-----------------|----------------------------|----------|
| `PUT /api/batch/rating` | `rating` (1-5, `0`/`null` clears) | batch result |
| `POST /api/batch/tags` | `tags` (array), `remove` (bool, required) | batch result |
| `PUT /api/batch/label` | `label` (name or `null`) | batch result |
| `POST /api/batch/collection` | `collection` (name) | `{"added": 3, "collection": "..."}` |
| `DELETE /api/batch/collection` | `collection` | `{"removed": 3, "collection": "..."}` |
| `POST /api/batch/auto-group` | -- | `{"groups_merged", "donors_removed", "variants_moved"}` |
| `POST /api/batch/group` | `target_id` (optional) | `{"target_id", "variants_moved", "donors_removed"}` |
| `POST /api/batch/stack` | -- | `{"stack_id", "member_count"}`; 400 on error |
| `DELETE /api/batch/stack` | -- | `{"removed": 2}` |
| `POST /api/batch/delete` | `remove_files` (bool, default false) | `{"deleted", "files_removed", "files_trashed", "previews_removed", "not_found", "errors"}`; files go through the trash unless `[trash]` is disabled |
| `POST /api/batch/describe` | `mode`, `model` (optional) | job; terminal event `{"phase": "describe", "succeeded", "failed", "descriptions_set", "tags_applied", "errors", "done": true}` |
| `POST /api/batch/auto-tag` *(ai)* | -- | job; terminal event `{"phase": "auto_tag", "succeeded", "failed", "tags_applied", "errors", "done": true}` |
| `POST /api/batch/embed` *(ai)* | -- | job; terminal event `{"phase": "embed", "embedded", "skipped", "errors", "done": true}`; 500 before starting if the model is not downloaded |
| `POST /api/batch/detect-faces` *(ai)* | -- | job; progress events carry `faces` per asset; terminal event `{"phase": "detect_faces", "succeeded", "faces_detected", "errors", "done": true}` |

The batch jobs do not have a one-per-kind conflict gate; progress events look like `{"phase", "done": false, "processed", "total", "asset", "status"}`.

```bash
curl -X POST -H "Content-Type: application/json" \
  -d '{"asset_ids":["550e8400","6ba7b810"],"tags":["event|festival"],"remove":false}' \
  http://localhost:8080/api/batch/tags
```

### `POST /api/batch/export` -- ZIP download

```json
{"asset_ids": ["..."],
 "filters": {"q": "tag:landscape", "stacks": "1"},
 "layout": "flat",
 "source": "originals",
 "all_variants": false,
 "include_sidecars": false}
```

- `asset_ids` wins when non-empty; otherwise `filters` (the [browse parameters](#browse-params) as a JSON object) is resolved through the same pipeline as the grid, so "export all" matches what is shown. An unresolvable collection or person exports nothing.
- `layout`: `flat` (default) or `mirror`; `source`: `originals` (default), `previews` or `smart`.
- Response: `application/zip`, `Content-Disposition: attachment; filename="maki-export-<n>-assets.zip"`, plus the headers `X-Maki-Exported`, `X-Maki-Skipped` and `X-Maki-Skipped-Offline` so a partial archive can be flagged. 400 when nothing matched, 500 when no file could be exported.

---

## Tags

| Method and path | Body | Response |
|-----------------|------|----------|
| `POST /api/tag/rename` | `{"old_tag", "new_tag", "apply": false}` | `{"matched", "renamed", "removed", "skipped", "dry_run"}` |
| `POST /api/tag/split` | `{"old_tag", "new_tags": [...], "keep": false, "apply": false}` | `{"matched", "split", "skipped", "dry_run"}` |
| `POST /api/tag/delete` | `{"tag", "apply": false}` | `{"matched", "removed", "skipped", "dry_run"}` |

Without `apply: true` all three are dry runs. Empty names return 400. The `=` / `/` / `^` markers of the `tag:` filter are accepted in `old_tag` and `tag`.

### `GET /api/tags/export-vocabulary`

Downloads the tag hierarchy as a vocabulary file (mirrors `maki tag export-vocabulary`).

| Parameter | Description |
|-----------|-------------|
| `format`  | `yaml` (default), `text`, `json`; anything else is 400 |
| `counts=1` | Annotate entries with asset counts (YAML comments; JSON always carries counts; ignored for text) |
| `prune=1` | Drop planned-but-unused `vocabulary.yaml` entries |
| `default=1` | Export the built-in default vocabulary instead of the catalog's |

Response: `Content-Disposition: attachment; filename="vocabulary.<ext>"`.

---

## Stacks

| Method and path | Response |
|-----------------|----------|
| `GET /api/stack/{stack_id}/members` | Ordered member cards: `[{"asset_id", "name", "asset_type", "format", "date", "preview_url", "rating", "label", "variant_count", "stack_id", "stack_count", "preview_rotation", "face_count"}]` |

Creation, dissolve, pick and add live under [Asset](#asset) and [Batch](#batch).

---

## Collections

| Method and path | Body | Response |
|-----------------|------|----------|
| `GET /api/collections` | -- | `[{"id", "name", "description", "asset_count", "created_at"}]` |
| `POST /api/collections` | `{"name", "description"}` | 201 `{"id", "name", "description"}`; 409 if the name exists; 500 for an empty name |

Membership changes go through `POST`/`DELETE /api/batch/collection`.

---

## Saved searches

Saved searches live in `searches.toml`. Objects have the shape `{"name", "query", "sort", "favorite"}`; `query` is omitted when empty, `sort` when unset, `favorite` when false.

| Method and path | Body | Response |
|-----------------|------|----------|
| `GET /api/saved-searches` | -- | array of saved-search objects |
| `POST /api/saved-searches` | `{"name", "query", "sort", "favorite"}` (`sort`, `favorite` optional) | `{"status": "saved", "name"}` -- creates or replaces by name |
| `DELETE /api/saved-searches/{name}` | -- | `{"status": "deleted", "name"}` |
| `PUT /api/saved-searches/{name}/favorite` | `{"favorite": true}` | `{"status": "updated", "name", "favorite"}` |
| `PUT /api/saved-searches/{name}/rename` | `{"new_name"}` | `{"status": "renamed", "old_name", "new_name"}`; 409 if `new_name` exists |

An unknown `{name}` returns 404.

---

## Volumes

| Method and path | Body | Response |
|-----------------|------|----------|
| `GET /api/volumes` | -- | `[{"id", "label", "mount_point", "volume_type", "purpose", "is_online"}]` |
| `POST /api/volumes` | `{"path", "label", "purpose"}` (`purpose` optional: `media`, `working`, `archive`, `backup`, `cloud`) | 201 `{"id", "label"}`; 409 if already registered; 400 for a missing path, empty label or bad purpose |
| `PUT /api/volumes/{id}/rename` | `{"label"}` | `{"ok": true}` |
| `PUT /api/volumes/{id}/purpose` | `{"purpose"}` (`null` or empty clears) | `{"ok": true}` |
| `DELETE /api/volumes/{id}` | -- | `{"label", "locations_removed", "recipes_removed", "assets_removed"}` |

### `GET /api/volumes/{id}/browse?prefix&limit&hidden&filter`

Lists filesystem entries under `<mount point>/<prefix>` for the import dialog (paths not yet in the catalog). `limit` defaults to 50 (maximum 500), `hidden=1` includes dotfiles, `filter` is `all` (default), `dirs` or `files`. Directories sort first.

```json
{"prefix": "2026", "entries": [{"name": "04", "is_dir": true, "relative_path": "2026/04"}]}
```

403 when the path escapes the mount point, 404 when it does not exist or is not a directory, 400 when the volume is offline.

---

## Duplicates

| Method and path | Body | Response |
|-----------------|------|----------|
| `POST /api/dedup/resolve` | `{"min_copies", "volume", "format", "path", "prefer", "dry_run"}` (all optional; `prefer` falls back to `[dedup] prefer`) | `DedupResult`: `{"duplicates_found", "locations_to_remove", "locations_removed", "files_deleted", "files_trashed", "recipes_removed", "bytes_freed", "dry_run", "errors"}` |
| `DELETE /api/dedup/location` | `{"content_hash", "volume_id", "relative_path"}` | `{"removed": true, "recipes_removed": 1}` -- trashes the file and co-located recipes, updates catalog and sidecar; 400 when it is the last copy or the volume is offline |

---

## Import

### `POST /api/import`

Starts an import job (409 if an import -- including a dry run -- is already running).

```json
{"volume_id": "<uuid or label>", "subfolder": "2026/04", "profile": "wedding",
 "tags": ["event|wedding"], "auto_group": true, "smart": false,
 "embed": false, "describe": false, "dry_run": false}
```

`embed` is honoured only on `ai` builds and `describe` only on Pro builds; both default to the `[import]` config. Response `{"job_id", "status": "started"}`. Progress events:

- `{"phase": "<label>", "done": false, "status": "phase_started"}` / `"phase_skipped"` (with `message`)
- `{"phase": "import", "done": false, "file", "status": "imported|location|skipped|recipe", "imported", "skipped", "locations_added", "recipes"}`
- `{"phase": "embed", ...}` *(ai)* and `{"phase": "describe", ...}` *(pro)* with running counters

Terminal event (also the dry-run summary):

```json
{"dry_run": false, "imported": 120, "locations_added": 3, "skipped": 10,
 "recipes_attached": 80, "recipes_updated": 0, "previews_generated": 120,
 "sidecars_created": 0, "new_asset_ids": ["..."],
 "embedded": 118, "embeddings_skipped": 2, "done": true}
```

(`embedded`/`embeddings_skipped` appear on `ai` builds when embeddings ran, `described`/`descriptions_skipped` on Pro builds.)

### `GET /api/import/profiles`

```json
{"profiles": ["default", "wedding"]}
```

---

## Jobs {#jobs}

Every long-running operation (import, the batch jobs, all `/api/maintain/*` operations) is registered in the `JobRegistry` (`src/web/jobs.rs`). Job kinds: `import`, `embed`, `auto_tag`, `detect_faces`, `describe`, `writeback`, `sync_metadata`, `verify`, `generate_previews`, `sync`, `refresh`, `cleanup`, `suggest_tags_review`, `apply_tags`.

| Method and path | Response |
|-----------------|----------|
| `GET /api/jobs` | `{"running": 1, "jobs": [{"id", "kind", "kind_label", "started_at", "completed", "progress"}]}` -- newest first, running and recently completed |
| `GET /api/jobs/{id}` | one entry of the same shape; 404 if unknown |
| `GET /api/jobs/{id}/progress` | `text/event-stream`: replays the ring buffer of recent events, then streams live ones. The terminal event always carries `"done": true`; a failed job's terminal event is `{"error": "...", "done": true}` |
| `GET /api/jobs/{id}/result` | structured result payload (used by `suggest-tags-review`): 200 JSON, 204 when the job set none, 425 while still running, 404 if unknown |

`progress` is the most recent event. Clients close the `EventSource` on `done: true`.

```bash
curl -N http://localhost:8080/api/jobs/<job_id>/progress
```

---

## Maintain

Each endpoint launches a job and returns `{"job_id", "status": "started"}`; a second job of the same kind while one is running returns 409 (different kinds run in parallel). Progress events are `{"processed", "status", "asset"}` (generate-previews: `{"processed", "total", "asset"}`); the terminal event carries the summary below plus `"done": true`.

| Method and path | Body (JSON; booleans default to false) | Terminal event fields |
|-----------------|----------------------------------------|-----------------------|
| `POST /api/maintain/writeback` | `{"query", "volume", "all", "force", "mirror_tags", "dry_run"}`; 400 when `mirror_tags` without `all`; `[writeback] mirror_tags` in config ORs in | `written`, `already_in_sync`, `skipped`, `failed`, `errors`, `dry_run`, `skipped_offline_volumes` |
| `POST /api/maintain/sync-metadata` | `{"volume", "media", "dry_run"}` | `inbound`, `outbound`, `unchanged`, `skipped`, `conflicts`, `media_refreshed`, `errors`, `dry_run` |
| `POST /api/maintain/verify` | `{"volume", "max_age_days"}` | `verified`, `failed`, `modified`, `skipped`, `skipped_recent`, `errors` |
| `POST /api/maintain/generate-previews` | `{"volume", "asset", "smart", "force", "upgrade"}` | `generated`, `skipped`, `failed`, `upgraded`, `errors`, `offline_blockers` |
| `POST /api/maintain/sync` | `{"volume" (required label), "path", "apply", "remove_stale"}`; 400 when `remove_stale` without `apply`; `path` must stay inside the mount | `unchanged`, `moved`, `new_files`, `modified`, `missing`, `stale_removed`, `orphaned_cleaned`, `locationless_after`, `errors` |
| `POST /api/maintain/refresh` | `{"volume", "media", "dry_run"}` | `unchanged`, `refreshed`, `missing`, `skipped`, `errors`, `dry_run` |
| `POST /api/maintain/cleanup` | `{"volume", "path", "apply"}` | `checked`, `stale`, `removed`, `skipped_offline`, `locationless_variants`, `removed_variants`, `orphaned_assets`, `removed_assets`, `orphaned_previews`, `removed_previews`, `orphaned_smart_previews`, `removed_smart_previews`, `orphaned_embeddings`, `removed_embeddings`, `orphaned_face_files`, `removed_face_files`, `errors`, `skipped_global_passes`, `applied` |
| `POST /api/maintain/suggest-tags-review` *(ai)* | `{"asset_ids", "threshold"}` (`threshold` defaults to half of `[ai] threshold`) | `{"phase": "suggest_tags_review", "processed", "total", "tag_count"}`; the aggregated payload is fetched from `/api/jobs/{id}/result` |

`volume` is a volume label in these endpoints. The suggest-tags-review result payload:

```json
{"asset_count": 50, "processed": 50, "skipped": 2, "errors": [], "threshold_used": 0.1,
 "tags": [{"tag": "subject|vehicle|car", "asset_count": 7, "mean_confidence": 0.3, "max_confidence": 0.6,
           "candidates": [{"asset_id": "...", "confidence": 0.6, "source_label": "car", "existing": false}]}],
 "per_asset": {"<asset_id>": [{"tag", "confidence", "existing", "source_label"}]},
 "assets": {"<asset_id>": {"preview_url": "/preview/ab/....jpg", "name": "DSC_0001.NEF"}}}
```

---

## Config

| Method and path | Body | Response |
|-----------------|------|----------|
| `GET /api/config` | -- | `{"config": <CatalogConfig as JSON>, "raw_toml": "<file contents>", "path": "/.../maki.toml"}` |
| `POST /api/config` | `{"config": <CatalogConfig as JSON>}` | `{"ok": true, "restart_required": false, "backup_path": "/.../maki.toml.bak"}`; 400 when deserialization or `validate()` fails |
| `GET /api/config/schema` | -- | JSON Schema (draft 2020-12) of `CatalogConfig`, generated with `schemars` |

Saving copies the previous file to `maki.toml.bak` and writes through `toml_edit`, so comments and formatting survive; only changed keys are touched. Keys that are not part of `CatalogConfig` are silently dropped by deserialization (no `deny_unknown_fields`).

---

## OS integration

| Method and path | Body | Response |
|-----------------|------|----------|
| `POST /api/open-location` | `{"volume_id", "relative_path"}` | `{"ok": true}` -- reveals the file in the OS file manager; 400 if the volume is offline or the file is missing |
| `POST /api/open-terminal` | `{"volume_id", "relative_path"}` | `{"ok": true}` -- opens a terminal in the file's directory |

Both act on the machine running `maki serve`.

---

## AI *(ai)*

Besides the per-asset and batch AI endpoints listed above:

### Query image (search by image)

| Method and path | Request | Response |
|-----------------|---------|----------|
| `POST /api/query-image` | raw image bytes as the body (`Content-Type: image/*`, optional `X-Maki-Filename` header, percent-encoded); own body limit of 64 MiB; allowed in read-only mode | `{"token", "filename", "exact_match_id", "embedded", "warning", "thumbnail"}` -- `thumbnail` is a `data:image/jpeg;base64,...` URL; 400 for an empty body, 422 when the file cannot be used |
| `GET /api/query-image/{token}` | -- | same shape (`warning` null); 404 once the session (1 h TTL) has expired |

The browse pipeline references the session as `similar:@<token>` in `q`.

```bash
curl -X POST -H "Content-Type: image/jpeg" -H "X-Maki-Filename: query.jpg" \
  --data-binary @query.jpg http://localhost:8080/api/query-image
```

### Faces and people

| Method and path | Body | Response |
|-----------------|------|----------|
| `PUT /api/faces/{face_id}/assign` | `{"person_id"}` | `{"ok": true}`; 400 without `person_id` |
| `DELETE /api/faces/{face_id}/unassign` | -- | `{"ok": true}` |
| `DELETE /api/faces/{face_id}` | -- | `{"ok": true}` -- deletes the face record, crop and embedding |
| `POST /api/faces/cluster` | none | `{"people_created", "faces_assigned", "singletons_skipped"}` -- clusters unassigned faces with `[ai] face_cluster_threshold` / `face_min_confidence` |
| `GET /api/people` | -- | `[{"id", "name", "face_count", "representative_face_id"}]` |
| `POST /api/people` | `{"name"}` | `{"id", "name"}`; 400 `{"error"}` for an empty name |
| `PUT /api/people/{id}/name` | `{"name"}` | `{"ok": true}` |
| `POST /api/people/{id}/merge` | `{"source_id"}` or `{"source_ids": [...]}` | `{"ok": true, "faces_moved": 12, "people_merged": 2}`; 400 without sources |
| `GET /api/people/merge-suggestions?threshold=0.4&limit=20` | -- | `{"suggestions": [{"similarity", "a": {"id", "name", "face_count", "crop_url", "named"}, "b": {...}}]}` |
| `DELETE /api/people/{id}` | -- | `{"ok": true}` -- faces become unassigned |

Face crops are served from `/face/{prefix}/{face_id}.jpg`.

### `GET /api/stroll/neighbors`

| Parameter | Default | Description |
|-----------|---------|-------------|
| `id` | required (400 if missing) | centre asset |
| `q` | -- | search query restricting the neighbour pool (combined with `[browse] default_filter`) |
| `n` | `[serve] stroll_neighbors` (12) | neighbour count, clamped to 5 .. `stroll_neighbors_max` (25) |
| `mode` | `nearest` | `nearest`, `explore` (skip `skip` ranked neighbours first), `discover` (random sample from a pool of `stroll_discover_pool`) |
| `skip` | 0 | offset for `explore` |
| `cross_session` | false | exclude assets from the centre's own shooting session |

```json
{"center": {"asset_id", "name", "preview_url", "smart_preview_url", "rating", "color_label", "format", "created_at"},
 "neighbors": [{"asset_id", "name", "preview_url", "similarity", "rating", "color_label"}]}
```

404 with a message when the asset or its embedding cannot be resolved.

---

## Media and static files

| Route | Description |
|-------|-------------|
| `GET /preview/{prefix}/{hash}.{ext}` | Thumbnails from `<catalog>/previews/` (`tower-http` `ServeDir`); `ext` follows `[preview] format` |
| `GET /smart-preview/{prefix}/{file}` | Smart previews from `<catalog>/smart-previews/`; generated on demand from an online source when `[preview] generate_on_demand` is set, otherwise 404 when missing. `Cache-Control: public, max-age=86400` |
| `GET /video/{hash}` / `GET /audio/{hash}` | Streams the original file of the variant with that content hash (hex without the `sha256:` prefix) from an online volume. Supports `Range` requests (206, `Accept-Ranges: bytes`); 404 when offline, 416 for an unsatisfiable range. One handler serves both; the two paths only differ in what the URL says |
| `GET /face/{prefix}/{face_id}.jpg` *(ai)* | Face crops from `<catalog>/faces/` |
| `GET /favicon.ico`, `GET /static/favicon.ico`, `GET /static/maki-icon.svg`, `GET /static/htmx.min.js`, `GET /static/style.css`, `GET /static/leaflet.min.js`, `GET /static/leaflet.css`, `GET /static/leaflet.markercluster.min.js`, `GET /static/MarkerCluster.css`, `GET /static/MarkerCluster.Default.css`, `GET /static/images/marker-icon.png`, `GET /static/images/marker-icon-2x.png`, `GET /static/images/marker-shadow.png`, `GET /static/images/layers.png`, `GET /static/images/layers-2x.png` | Assets embedded at compile time |

Responses under `/preview/`, `/smart-preview/` and `/face/` get `Cache-Control: no-cache` from the logging middleware so rotated or regenerated previews revalidate (unchanged files answer 304).

---

## Status codes

| Code | Where |
|------|-------|
| 200 | Success |
| 201 | `POST /api/collections`, `POST /api/volumes` |
| 204 | `GET /api/jobs/{id}/result` when the job produced no payload |
| 206 | Range responses from `/video/{hash}` and `/audio/{hash}` |
| 303 | `GET /api/search` without `HX-Request` |
| 400 | Explicit validation failures (empty names, bad combinations such as `mirror_tags` without `all`, compare with fewer than 2 IDs, stack errors) |
| 401 | Basic auth required or wrong |
| 403 | Read-only mode (non-GET request), volume browse escaping the mount point |
| 404 | Unknown asset page, job, query-image token, stroll centre, missing media file |
| 409 | Duplicate collection / volume / saved-search name; a job of the same kind already running |
| 416 | Unsatisfiable `Range` |
| 422 | Unusable query image |
| 425 | `GET /api/jobs/{id}/result` while the job is running |
| 500 | Catalog or processing error (`Error: ...` plain text) |

---

Next: [Module Reference](02-module-reference.md) -- source layout and the responsibilities of each module.
