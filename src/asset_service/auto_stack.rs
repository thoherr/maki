//! `auto-stack` section of `AssetService` — discover natural visual clusters
//! across the catalog via stored SigLIP embeddings and propose/create stacks.
//!
//! Phase 3 of the similarity-browse proposal (Phases 1–2 shipped as the
//! `similar:` search filter and the stroll page). Clustering is greedy
//! connected-components over the similarity graph: instead of an O(N²)
//! similarity matrix, each candidate queries its top-K neighbours in the
//! in-memory `EmbeddingIndex` and an edge exists when similarity >= threshold
//! and both endpoints are candidates. Union-find merges the edges into
//! components; components with >= min_size members become proposed stacks.

use super::*;

use crate::embedding_store::{EmbeddingIndex, EmbeddingStore};
use crate::stack::StackStore;

/// Number of nearest neighbours queried per candidate when building the
/// similarity graph. Connectivity is transitive (union-find), so clusters
/// larger than K still form through neighbour chains.
const TOP_K: usize = 20;

/// One member of a proposed cluster.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AutoStackMember {
    pub asset_id: String,
    pub name: Option<String>,
    pub rating: Option<u8>,
}

/// One proposed (or created) stack.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AutoStackCluster {
    /// Asset ID of the pick (stack position 0): highest rating first,
    /// then earliest created_at, then lowest asset ID.
    pub pick: String,
    /// Ordered members — pick first, rest by the same criteria.
    pub members: Vec<AutoStackMember>,
    /// Lowest edge similarity inside the cluster (0.0–1.0).
    pub min_similarity: f32,
    /// Average edge similarity inside the cluster (0.0–1.0).
    pub avg_similarity: f32,
    /// Stack ID when `--apply` created the stack; `None` on dry run.
    pub stack_id: Option<String>,
}

/// Result of a `maki auto-stack` run.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AutoStackResult {
    /// Unstacked assets with a stored embedding (after scope filtering).
    pub candidates: u32,
    pub clusters: Vec<AutoStackCluster>,
    /// Total assets across all proposed clusters.
    pub assets_clustered: u32,
    /// Stacks actually created (0 on dry run).
    pub stacks_created: u32,
    /// Similarity threshold in percent (50–99).
    pub threshold: u32,
    /// Minimum cluster size.
    pub min_size: usize,
    pub dry_run: bool,
    /// Human-readable warnings (e.g. one giant component — threshold too low).
    pub warnings: Vec<String>,
}

/// Candidate metadata loaded from the assets table.
struct Candidate {
    id: String,
    name: Option<String>,
    rating: Option<u8>,
    created_at: String,
}

/// Union-find with path compression and union by size.
struct UnionFind {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self { parent: (0..n).collect(), size: vec![1; n] }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (mut ra, mut rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        if self.size[ra] < self.size[rb] {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parent[rb] = ra;
        self.size[ra] += self.size[rb];
    }
}

impl AssetService {
    /// Cluster unstacked embedded assets by visual similarity and
    /// propose (dry run) or create (`apply`) stacks.
    ///
    /// `query`/`asset` narrow the candidate set (standard scope pattern);
    /// `None`/`None` scans the whole catalog. Uses the stored embeddings
    /// for `model_id` — no model inference happens, so the model files
    /// don't need to be downloaded.
    pub fn auto_stack(
        &self,
        query: Option<&str>,
        asset: Option<&str>,
        model_id: &str,
        threshold_pct: u32,
        min_size: usize,
        apply: bool,
        on_cluster: impl Fn(usize, &AutoStackCluster),
    ) -> Result<AutoStackResult> {
        let spec = crate::ai::get_model_spec(model_id).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown model: {model_id}. Run 'maki auto-tag --list-models' to see available models."
            )
        })?;
        let catalog = Catalog::open(&self.catalog_root)?;
        let engine = crate::query::QueryEngine::new(&self.catalog_root);
        let scope = engine.resolve_scope(query, asset, &[])?;
        auto_stack_catalog(
            &catalog,
            &self.catalog_root,
            scope.as_ref(),
            model_id,
            spec.embedding_dim,
            threshold_pct,
            min_size,
            apply,
            on_cluster,
        )
    }
}

/// Catalog-level auto-stack: cluster candidates, optionally create stacks.
///
/// Candidates are assets that (a) have a stored embedding for `model_id`
/// with the expected dimension, (b) are not already in a stack
/// (`stack_id IS NULL` — existing stacks are never disturbed), and
/// (c) match `scope` when given.
#[allow(clippy::too_many_arguments)]
pub fn auto_stack_catalog(
    catalog: &Catalog,
    catalog_root: &Path,
    scope: Option<&HashSet<String>>,
    model_id: &str,
    embedding_dim: usize,
    threshold_pct: u32,
    min_size: usize,
    apply: bool,
    on_cluster: impl Fn(usize, &AutoStackCluster),
) -> Result<AutoStackResult> {
    if !(50..=99).contains(&threshold_pct) {
        bail!(
            "threshold must be between 50 and 99 (percent) — values below 50 \
             produce degenerate giant clusters"
        );
    }
    if min_size < 2 {
        bail!("minimum cluster size is 2 (a stack requires at least 2 assets)");
    }
    let threshold = threshold_pct as f32 / 100.0;

    let _ = EmbeddingStore::initialize(catalog.conn());
    let emb_store = EmbeddingStore::new(catalog.conn());

    // Candidates: unstacked assets with a stored embedding for this model.
    let mut candidates: Vec<Candidate> = {
        let mut stmt = catalog.conn().prepare(
            "SELECT a.id, a.name, a.rating, a.created_at
             FROM assets a
             JOIN embeddings e ON e.asset_id = a.id AND e.model = ?1
             WHERE a.stack_id IS NULL
             ORDER BY a.id",
        )?;
        let rows = stmt.query_map(rusqlite::params![model_id], |row| {
            Ok(Candidate {
                id: row.get(0)?,
                name: row.get(1)?,
                rating: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    if let Some(scope) = scope {
        candidates.retain(|c| scope.contains(&c.id));
    }

    // Per-candidate embeddings (query vectors). Drop candidates whose stored
    // embedding has an unexpected dimension — the index skips them too.
    let mut embeddings: HashMap<String, Vec<f32>> = emb_store
        .all_embeddings_for_model(model_id)?
        .into_iter()
        .filter(|(_, emb)| emb.len() == embedding_dim)
        .collect();
    candidates.retain(|c| embeddings.contains_key(&c.id));

    let mut result = AutoStackResult {
        candidates: candidates.len() as u32,
        clusters: Vec::new(),
        assets_clustered: 0,
        stacks_created: 0,
        threshold: threshold_pct,
        min_size,
        dry_run: !apply,
        warnings: Vec::new(),
    };
    if candidates.len() < 2 {
        return Ok(result);
    }

    // Similarity graph: top-K neighbour search per candidate, edge when
    // similarity >= threshold and both endpoints are candidates.
    let index = EmbeddingIndex::load(catalog.conn(), model_id, embedding_dim)?;
    let id_to_idx: HashMap<&str, usize> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();

    let mut uf = UnionFind::new(candidates.len());
    let mut edge_sims: HashMap<(usize, usize), f32> = HashMap::new();
    for (i, cand) in candidates.iter().enumerate() {
        let emb = &embeddings[&cand.id];
        for (other_id, sim) in index.search(emb, TOP_K, Some(&cand.id)) {
            if sim < threshold {
                break; // results are sorted by similarity descending
            }
            if let Some(&j) = id_to_idx.get(other_id.as_str()) {
                edge_sims.entry((i.min(j), i.max(j))).or_insert(sim);
                uf.union(i, j);
            }
        }
    }
    embeddings.clear();

    // Connected components.
    let mut components: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..candidates.len() {
        let root = uf.find(i);
        components.entry(root).or_default().push(i);
    }

    // Per-component edge similarity stats: (min, sum, count).
    let mut edge_stats: HashMap<usize, (f32, f32, u32)> = HashMap::new();
    for (&(a, _), &sim) in &edge_sims {
        let root = uf.find(a);
        let entry = edge_stats.entry(root).or_insert((f32::MAX, 0.0, 0));
        entry.0 = entry.0.min(sim);
        entry.1 += sim;
        entry.2 += 1;
    }

    // Safety: when one component swallows more than half of all candidates,
    // the threshold is likely too low.
    let largest = components.values().map(|m| m.len()).max().unwrap_or(0);
    if largest >= 2 && largest * 2 > candidates.len() {
        result.warnings.push(format!(
            "largest cluster contains {largest} of {} candidates — threshold {threshold_pct}% is likely too low",
            candidates.len()
        ));
    }

    // Build clusters from components with >= min_size members.
    let mut clusters: Vec<AutoStackCluster> = Vec::new();
    for (root, member_idxs) in components {
        if member_idxs.len() < min_size {
            continue;
        }
        let mut members: Vec<&Candidate> =
            member_idxs.iter().map(|&i| &candidates[i]).collect();
        // Pick selection: highest rating, then earliest created_at, then lowest ID.
        members.sort_by(|a, b| {
            b.rating
                .unwrap_or(0)
                .cmp(&a.rating.unwrap_or(0))
                .then_with(|| a.created_at.cmp(&b.created_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        let (min_sim, sum_sim, edge_count) =
            edge_stats.get(&root).copied().unwrap_or((0.0, 0.0, 0));
        clusters.push(AutoStackCluster {
            pick: members[0].id.clone(),
            members: members
                .into_iter()
                .map(|c| AutoStackMember {
                    asset_id: c.id.clone(),
                    name: c.name.clone(),
                    rating: c.rating,
                })
                .collect(),
            min_similarity: if edge_count > 0 { min_sim } else { 0.0 },
            avg_similarity: if edge_count > 0 { sum_sim / edge_count as f32 } else { 0.0 },
            stack_id: None,
        });
    }
    // Deterministic ordering: largest cluster first, then by pick ID.
    clusters.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then_with(|| a.pick.cmp(&b.pick))
    });

    // Apply: one stack per cluster, member order = pick first.
    if apply {
        let store = StackStore::new(catalog.conn());
        for cluster in &mut clusters {
            let ids: Vec<String> =
                cluster.members.iter().map(|m| m.asset_id.clone()).collect();
            let stack = store.create(&ids)?;
            cluster.stack_id = Some(stack.id.to_string());
            result.stacks_created += 1;
        }
        let yaml = store.export_all()?;
        crate::stack::save_yaml(catalog_root, &yaml)?;
    }

    for (i, cluster) in clusters.iter().enumerate() {
        result.assets_clustered += cluster.members.len() as u32;
        on_cluster(i, cluster);
    }
    result.clusters = clusters;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Asset, AssetType, Variant, VariantRole};

    const DIM: usize = 4;
    const MODEL: &str = "test-model";

    /// L2-normalize a vector so dot product == cosine similarity.
    fn unit(v: [f32; DIM]) -> Vec<f32> {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / norm).collect()
    }

    fn add_asset(
        catalog: &Catalog,
        hash: &str,
        rating: Option<u8>,
        created: &str,
    ) -> String {
        let mut asset = Asset::new(AssetType::Image, &format!("sha256:{hash}"));
        asset.name = Some(format!("img-{hash}"));
        asset.rating = rating;
        asset.created_at = created.parse().unwrap();
        let variant = Variant {
            content_hash: format!("sha256:{hash}"),
            asset_id: asset.id,
            role: VariantRole::Original,
            format: "jpg".to_string(),
            file_size: 100,
            original_filename: format!("{hash}.jpg"),
            source_metadata: Default::default(),
            locations: vec![],
        };
        asset.variants.push(variant.clone());
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&variant).unwrap();
        asset.id.to_string()
    }

    fn setup() -> Catalog {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();
        EmbeddingStore::initialize(catalog.conn()).unwrap();
        catalog
    }

    /// Two tight clusters + a singleton + an already-stacked asset.
    /// Returns (catalog, cluster_a_ids, cluster_b_ids, singleton_id, stacked_id).
    fn setup_two_clusters() -> (Catalog, Vec<String>, Vec<String>, String, String) {
        let catalog = setup();
        let emb_store = EmbeddingStore::new(catalog.conn());

        // Cluster A: three near-identical vectors around the x axis.
        let a1 = add_asset(&catalog, "a1", None, "2026-01-03T00:00:00Z");
        let a2 = add_asset(&catalog, "a2", None, "2026-01-01T00:00:00Z");
        let a3 = add_asset(&catalog, "a3", None, "2026-01-02T00:00:00Z");
        emb_store.store(&a1, &unit([1.0, 0.0, 0.0, 0.0]), MODEL).unwrap();
        emb_store.store(&a2, &unit([0.999, 0.04, 0.0, 0.0]), MODEL).unwrap();
        emb_store.store(&a3, &unit([0.999, 0.0, 0.04, 0.0]), MODEL).unwrap();

        // Cluster B: two near-identical vectors around the y axis.
        let b1 = add_asset(&catalog, "b1", None, "2026-02-01T00:00:00Z");
        let b2 = add_asset(&catalog, "b2", None, "2026-02-02T00:00:00Z");
        emb_store.store(&b1, &unit([0.0, 1.0, 0.0, 0.0]), MODEL).unwrap();
        emb_store.store(&b2, &unit([0.04, 0.999, 0.0, 0.0]), MODEL).unwrap();

        // Singleton: orthogonal to both clusters.
        let s1 = add_asset(&catalog, "s1", None, "2026-03-01T00:00:00Z");
        emb_store.store(&s1, &unit([0.0, 0.0, 1.0, 0.0]), MODEL).unwrap();

        // Already-stacked asset, visually identical to cluster A — must be
        // excluded from candidacy (never disturb existing stacks).
        let x1 = add_asset(&catalog, "x1", None, "2026-01-01T00:00:00Z");
        emb_store.store(&x1, &unit([1.0, 0.01, 0.0, 0.0]), MODEL).unwrap();
        catalog
            .conn()
            .execute(
                "UPDATE assets SET stack_id = 'existing-stack', stack_position = 0 WHERE id = ?1",
                rusqlite::params![x1],
            )
            .unwrap();

        let mut a = vec![a1, a2, a3];
        a.sort();
        let mut b = vec![b1, b2];
        b.sort();
        (catalog, a, b, s1, x1)
    }

    fn run(
        catalog: &Catalog,
        root: &Path,
        scope: Option<&HashSet<String>>,
        threshold: u32,
        min_size: usize,
        apply: bool,
    ) -> AutoStackResult {
        auto_stack_catalog(catalog, root, scope, MODEL, DIM, threshold, min_size, apply, |_, _| {})
            .unwrap()
    }

    #[test]
    fn clusters_two_groups_excludes_singleton_and_stacked() {
        let (catalog, a, b, s1, x1) = setup_two_clusters();
        let tmp = tempfile::tempdir().unwrap();

        let result = run(&catalog, tmp.path(), None, 85, 2, false);

        // 6 candidates: 3 + 2 + singleton; the stacked asset is excluded.
        assert_eq!(result.candidates, 6);
        assert!(result.dry_run);
        assert_eq!(result.stacks_created, 0);
        assert_eq!(result.clusters.len(), 2);
        assert_eq!(result.assets_clustered, 5);

        // Largest cluster first.
        let mut got_a: Vec<String> = result.clusters[0]
            .members
            .iter()
            .map(|m| m.asset_id.clone())
            .collect();
        got_a.sort();
        assert_eq!(got_a, a);
        let mut got_b: Vec<String> = result.clusters[1]
            .members
            .iter()
            .map(|m| m.asset_id.clone())
            .collect();
        got_b.sort();
        assert_eq!(got_b, b);

        // Neither the singleton nor the stacked asset appears anywhere.
        for c in &result.clusters {
            assert!(c.members.iter().all(|m| m.asset_id != s1 && m.asset_id != x1));
        }

        // Similarity stats are sane for tight clusters.
        for c in &result.clusters {
            assert!(c.min_similarity >= 0.85, "min {}", c.min_similarity);
            assert!(c.avg_similarity >= c.min_similarity);
            assert!(c.avg_similarity <= 1.0);
        }
    }

    #[test]
    fn pick_prefers_rating_then_created_then_id() {
        let catalog = setup();
        let emb_store = EmbeddingStore::new(catalog.conn());
        let tmp = tempfile::tempdir().unwrap();

        // p2 has the highest rating → pick despite later created_at.
        let p1 = add_asset(&catalog, "p1", Some(3), "2026-01-01T00:00:00Z");
        let p2 = add_asset(&catalog, "p2", Some(5), "2026-01-05T00:00:00Z");
        let p3 = add_asset(&catalog, "p3", None, "2026-01-02T00:00:00Z");
        emb_store.store(&p1, &unit([1.0, 0.0, 0.0, 0.0]), MODEL).unwrap();
        emb_store.store(&p2, &unit([0.999, 0.04, 0.0, 0.0]), MODEL).unwrap();
        emb_store.store(&p3, &unit([0.999, 0.0, 0.04, 0.0]), MODEL).unwrap();

        let result = run(&catalog, tmp.path(), None, 85, 2, false);
        assert_eq!(result.clusters.len(), 1);
        let cluster = &result.clusters[0];
        assert_eq!(cluster.pick, p2);
        assert_eq!(cluster.members[0].asset_id, p2);
        // Tie on rating (None == None) → earlier created_at wins second slot.
        assert_eq!(cluster.members[1].asset_id, p1);
        assert_eq!(cluster.members[2].asset_id, p3);
    }

    #[test]
    fn pick_ties_broken_by_lowest_asset_id() {
        let catalog = setup();
        let emb_store = EmbeddingStore::new(catalog.conn());
        let tmp = tempfile::tempdir().unwrap();

        // Identical rating and created_at → lowest asset ID wins.
        let q1 = add_asset(&catalog, "q1", Some(2), "2026-01-01T00:00:00Z");
        let q2 = add_asset(&catalog, "q2", Some(2), "2026-01-01T00:00:00Z");
        emb_store.store(&q1, &unit([1.0, 0.0, 0.0, 0.0]), MODEL).unwrap();
        emb_store.store(&q2, &unit([0.999, 0.04, 0.0, 0.0]), MODEL).unwrap();

        let result = run(&catalog, tmp.path(), None, 85, 2, false);
        assert_eq!(result.clusters.len(), 1);
        let expected = std::cmp::min(q1.clone(), q2.clone());
        assert_eq!(result.clusters[0].pick, expected);
    }

    #[test]
    fn min_size_filters_small_components() {
        let (catalog, a, _b, _s1, _x1) = setup_two_clusters();
        let tmp = tempfile::tempdir().unwrap();

        let result = run(&catalog, tmp.path(), None, 85, 3, false);
        // Only the 3-member cluster survives min_size 3.
        assert_eq!(result.clusters.len(), 1);
        assert_eq!(result.clusters[0].members.len(), 3);
        let mut got: Vec<String> = result.clusters[0]
            .members
            .iter()
            .map(|m| m.asset_id.clone())
            .collect();
        got.sort();
        assert_eq!(got, a);
    }

    #[test]
    fn threshold_gates_edges() {
        let catalog = setup();
        let emb_store = EmbeddingStore::new(catalog.conn());
        let tmp = tempfile::tempdir().unwrap();

        // Two vectors with similarity ~0.926 (cos of larger angle).
        let t1 = add_asset(&catalog, "t1", None, "2026-01-01T00:00:00Z");
        let t2 = add_asset(&catalog, "t2", None, "2026-01-02T00:00:00Z");
        emb_store.store(&t1, &unit([1.0, 0.0, 0.0, 0.0]), MODEL).unwrap();
        emb_store.store(&t2, &unit([0.926, 0.377, 0.0, 0.0]), MODEL).unwrap();

        // Below the pair's similarity → clustered.
        let low = run(&catalog, tmp.path(), None, 90, 2, false);
        assert_eq!(low.clusters.len(), 1);

        // Above the pair's similarity → no clusters.
        let high = run(&catalog, tmp.path(), None, 95, 2, false);
        assert!(high.clusters.is_empty());
    }

    #[test]
    fn threshold_below_50_rejected() {
        let (catalog, ..) = setup_two_clusters();
        let tmp = tempfile::tempdir().unwrap();
        let err = auto_stack_catalog(
            &catalog, tmp.path(), None, MODEL, DIM, 49, 2, false, |_, _| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("threshold"), "{err}");
    }

    #[test]
    fn min_size_below_2_rejected() {
        let (catalog, ..) = setup_two_clusters();
        let tmp = tempfile::tempdir().unwrap();
        let err = auto_stack_catalog(
            &catalog, tmp.path(), None, MODEL, DIM, 85, 1, false, |_, _| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("cluster size"), "{err}");
    }

    #[test]
    fn scope_narrows_candidates() {
        let (catalog, a, _b, _s1, _x1) = setup_two_clusters();
        let tmp = tempfile::tempdir().unwrap();

        let scope: HashSet<String> = a.iter().cloned().collect();
        let result = run(&catalog, tmp.path(), Some(&scope), 85, 2, false);
        assert_eq!(result.candidates, 3);
        assert_eq!(result.clusters.len(), 1);
        let mut got: Vec<String> = result.clusters[0]
            .members
            .iter()
            .map(|m| m.asset_id.clone())
            .collect();
        got.sort();
        assert_eq!(got, a);
    }

    #[test]
    fn warns_when_one_cluster_swallows_majority() {
        let catalog = setup();
        let emb_store = EmbeddingStore::new(catalog.conn());
        let tmp = tempfile::tempdir().unwrap();

        // Four near-identical vectors — single component = all candidates.
        for (i, hash) in ["w1", "w2", "w3", "w4"].iter().enumerate() {
            let id = add_asset(&catalog, hash, None, "2026-01-01T00:00:00Z");
            let e = unit([1.0, 0.01 * i as f32, 0.0, 0.0]);
            emb_store.store(&id, &e, MODEL).unwrap();
        }

        let result = run(&catalog, tmp.path(), None, 85, 2, false);
        assert_eq!(result.clusters.len(), 1);
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("likely too low"), "{}", result.warnings[0]);
    }

    #[test]
    fn apply_creates_stacks_and_yaml() {
        let (catalog, a, b, _s1, _x1) = setup_two_clusters();
        let tmp = tempfile::tempdir().unwrap();

        let result = run(&catalog, tmp.path(), None, 85, 2, true);
        assert!(!result.dry_run);
        assert_eq!(result.stacks_created, 2);
        assert!(result.clusters.iter().all(|c| c.stack_id.is_some()));

        // Stacks exist in the StackStore and members carry stack_id with
        // the pick at position 0.
        let store = StackStore::new(catalog.conn());
        for cluster in &result.clusters {
            let (stack_id, members) = store
                .stack_for_asset(&cluster.pick)
                .unwrap()
                .expect("pick must be stacked");
            assert_eq!(Some(stack_id), cluster.stack_id.clone());
            assert_eq!(members[0], cluster.pick);
            let mut got = members.clone();
            got.sort();
            let mut expected: Vec<String> =
                cluster.members.iter().map(|m| m.asset_id.clone()).collect();
            expected.sort();
            assert_eq!(got, expected);
        }
        // Both clusters got distinct stacks covering A and B.
        let all_stacked: HashSet<String> = result
            .clusters
            .iter()
            .flat_map(|c| c.members.iter().map(|m| m.asset_id.clone()))
            .collect();
        for id in a.iter().chain(b.iter()) {
            assert!(all_stacked.contains(id));
        }

        // stacks.yaml exported next to the catalog root.
        let yaml = crate::stack::load_yaml(tmp.path()).unwrap();
        assert_eq!(yaml.stacks.len(), 2);
    }
}
