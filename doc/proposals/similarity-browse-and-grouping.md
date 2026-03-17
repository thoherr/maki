# Proposal: Similarity Browse and Grouping

## Motivation

The SigLIP embedding-based similarity search works very well for finding visually related images. Currently it's used in stroll navigation and the detail page's "similar images" section, but there's no easy way to go from "these images are similar" to "let me process them" (tag, group, cull).

Key use case: burst shots and near-duplicates. A photographer shoots 15 similar frames, wants to keep the best 2-3 rated, and either group the rest under a hero shot or tag them as `rest` so they don't clutter browsing.

## Phase 1: Browse with Similarity Scores

**Goal:** Navigate from the detail page to a browse view filtered by similarity, with scores visible and a threshold control.

- **"Browse similar" link** on the detail page → navigates to browse with `similar:<asset_id>` query pre-filled.
- **Similarity score on browse cards** — when a `similar:` query is active, display the similarity percentage on each card (small overlay or badge). The cosine similarity is already computed; it needs to flow through SearchRow → AssetCard → template.
- **Minimum similarity filter** — new `min_sim:` search prefix (e.g. `similar:abc123 min_sim:0.9`). Currently `similar_limit` caps the result count; a threshold filter is more natural for "show me everything above 90%."
- **Sort by similarity** — when `similar:` is active, default sort to similarity descending. Add `similarity` as a sort option.

From the browse grid, existing batch tools (tag, group, delete) handle the rest.

## Phase 2: Group by Similarity (Targeted)

**Goal:** One-click grouping of similar images around a hero shot.

- **"Group similar" button** on the detail page — finds all assets above a configurable threshold and groups them under the current asset as primary.
- **Batch version** — select multiple in browse, group around the highest-rated one.
- Threshold configurable via `[ai] similarity_group_threshold` in `maki.toml` (default ~0.85).

## Phase 3: Auto-Cluster by Similarity

**Goal:** Discover natural visual clusters across the entire catalog.

- `maki auto-group --by similarity --threshold 0.85` — scan all embedded assets, find groups where pairwise similarity exceeds threshold, propose as groups.
- Similar to face clustering (DBSCAN or greedy connected-components), but over whole-image embeddings.
- `--dry-run` for review before applying.
- Computationally O(n²) pairwise, but can be optimized with approximate nearest neighbors or batched cosine similarity over the embedding matrix.

## Overlap with Default Filter / Culling

This feature complements the `[browse] default_filter` and `rest` tag workflow:

1. Browse similar → select the burst shots you don't need
2. Tag them as `rest` (they disappear from default browsing)
3. Or group them under the hero shot (they collapse in browse via stack collapsing)

Grouping + stack collapse is arguably cleaner than tagging for bursts, because the images stay associated with their best variant and can be expanded when needed.

## Implementation Notes

- Similarity scores come from `EmbeddingStore::find_similar()` which returns `Vec<(String, f32)>` (asset_id, cosine similarity).
- The `similar:` filter is already parsed in `query.rs` behind `#[cfg(feature = "ai")]`.
- Current flow: `similar:` → resolved in `catalog.rs` `search_assets()` via `EmbeddingStore` → returns asset IDs → filtered as IN clause. The similarity score is discarded at this point — Phase 1 needs to preserve it through to the template.
- Browse cards are rendered from `AssetCard` in `templates.rs` — add an optional `similarity: Option<f32>` field.
