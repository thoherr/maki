//! Search by query image — find catalog assets similar to an image file
//! that is **not** in the catalog (a preview someone sent back, an export
//! whose original needs locating, a screenshot).
//!
//! Two stages, shared by `maki search --image` and the web `similar:@<token>`
//! session reference:
//!
//! 1. **Exact-copy fast path** — hash the file and look it up among the
//!    catalog's variants. A byte-identical copy is answered without loading
//!    the model.
//! 2. **Embedding search** — encode the file with the active SigLIP model
//!    (routing RAW / video through the preview generator into a scratch
//!    directory first) and rank the embedding index with
//!    [`crate::embedding_store::rank_similar`], pinning the exact match
//!    first when there is one.
//!
//! Nothing here writes to the catalog: the query file is never imported,
//! embedded, or copied into `<catalog>/previews`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::ai::SigLipModel;
use crate::catalog::Catalog;
use crate::content_store::ContentStore;
use crate::preview::PreviewGenerator;

/// Default result-set size for a query-image search (counts the pinned
/// exact match) — the same default `similar:<id>` uses.
pub const DEFAULT_LIMIT: usize = 40;

/// What the catalog knows about a query image file after the two stages.
#[derive(Debug, Clone)]
pub struct QueryImage {
    /// `sha256:<hex>` of the query file.
    pub content_hash: String,
    /// The asset owning a variant with that exact content hash, if any.
    pub exact_asset_id: Option<String>,
    /// SigLIP embedding of the query image; `None` when the model could
    /// not encode it (only tolerated when an exact match exists).
    pub embedding: Option<Vec<f32>>,
}

impl QueryImage {
    /// Rank the index for this query: exact match pinned first (score
    /// 1.0), then the embedding neighbours above `min_sim` (0.0–1.0).
    pub fn rank(
        &self,
        index: &crate::embedding_store::EmbeddingIndex,
        limit: usize,
        min_sim: f32,
    ) -> Vec<(String, f32)> {
        crate::embedding_store::rank_similar(
            index,
            self.embedding.as_deref(),
            limit,
            min_sim,
            self.exact_asset_id.as_deref(),
        )
    }
}

/// Stage 1: hash the query file and look for a byte-identical variant.
pub fn hash_and_lookup(catalog: &Catalog, path: &Path) -> Result<(String, Option<String>)> {
    if !path.is_file() {
        anyhow::bail!("query image not found: {}", path.display());
    }
    let store = ContentStore::new(&PathBuf::new());
    let hash = store
        .hash_file(path)
        .with_context(|| format!("failed to hash query image {}", path.display()))?;
    let exact = catalog.find_asset_id_by_variant(&hash)?;
    Ok((hash, exact))
}

/// A scratch directory that is removed on drop — holds the preview
/// rendition of a RAW/video query file while it is being encoded.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("maki-query-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create scratch dir {}", dir.display()))?;
        Ok(Self(dir))
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Lower-cased extension of `path` ("" when absent).
pub fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default()
}

/// Stage 2 input: a file the SigLIP preprocessor can decode. Formats the
/// `image` crate handles are returned as-is; everything else (RAW, video,
/// HEIC via the external tools) is rendered through the preview generator
/// into a scratch directory. The returned guard keeps the scratch file
/// alive for as long as the caller holds it.
fn encodable_path(
    preview_gen: &PreviewGenerator,
    path: &Path,
) -> Result<(PathBuf, Option<ScratchDir>)> {
    let ext = extension_of(path);
    if crate::ai::is_supported_image(&ext) {
        return Ok((path.to_path_buf(), None));
    }
    if !crate::preview::is_visual_format(&ext) {
        // Anything else would render as an info card — embedding that
        // would "work" and return nonsense neighbours. Name only: the web
        // upload lives in a scratch dir nobody needs to see.
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("query image");
        anyhow::bail!("'{name}' has no picture content to compare (format '{ext}')");
    }
    let scratch = ScratchDir::new()?;
    let dest = scratch.0.join(format!("query.{}", preview_gen.preview_extension()));
    match preview_gen.generate_to(&dest, path, &ext)? {
        Some(p) => Ok((p, Some(scratch))),
        None => anyhow::bail!(
            "cannot render query image {} (unsupported format '{ext}' or missing external tool)",
            path.display()
        ),
    }
}

/// Stage 2: encode the query file with the loaded model.
pub fn encode_query_image(
    model: &mut SigLipModel,
    preview_gen: &PreviewGenerator,
    path: &Path,
) -> Result<Vec<f32>> {
    let (encodable, _scratch) = encodable_path(preview_gen, path)?;
    model
        .encode_image(&encodable)
        .with_context(|| format!("failed to encode query image {}", path.display()))
}

/// Render a small thumbnail of the query file as a `data:` URL (JPEG,
/// longest edge `max_edge`) for the web query pill. RAW/video files are
/// routed through the preview generator like the encoder input.
pub fn thumbnail_data_url(
    preview_gen: &PreviewGenerator,
    path: &Path,
    max_edge: u32,
) -> Result<String> {
    let (decodable, _scratch) = encodable_path(preview_gen, path)?;
    let img = image::ImageReader::open(&decodable)?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("failed to decode {}", decodable.display()))?;
    let thumb = img.thumbnail(max_edge, max_edge);
    let mut bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut bytes);
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 80);
    thumb.write_with_encoder(encoder)?;
    Ok(format!("data:image/jpeg;base64,{}", base64_encode(&bytes)))
}

/// Minimal standard-alphabet base64 encoder (RFC 4648, with padding) —
/// the web layer already has one for Basic-Auth; a thumbnail is not worth
/// a crate dependency either.
pub fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Both stages with a model that may or may not be available.
///
/// `encode` is called only after the hash lookup and is expected to load
/// the model lazily and encode `path` (see [`encode_query_image`]). Its
/// failure is tolerated when an exact match exists — the primary question
/// is answered; the message is returned as a warning for the caller to
/// surface — and fatal otherwise.
pub fn resolve_query_image(
    catalog: &Catalog,
    path: &Path,
    encode: impl FnOnce(&Path) -> Result<Vec<f32>>,
) -> Result<(QueryImage, Option<String>)> {
    let (content_hash, exact_asset_id) = hash_and_lookup(catalog, path)?;
    let (embedding, warning) = match encode(path) {
        Ok(emb) => (Some(emb), None),
        Err(e) if exact_asset_id.is_some() => (None, Some(format!("{e:#}"))),
        Err(e) => return Err(e),
    };
    Ok((QueryImage { content_hash, exact_asset_id, embedding }, warning))
}

// ─── Web query-image sessions ───────────────────────────────────────────

/// One uploaded query image, referenced from the browse URL as
/// `similar:@<token>` so the whole search pipeline (browse, select-all,
/// facets, export) resolves it like `similar:<id>`.
#[derive(Debug, Clone)]
pub struct QueryImageSession {
    pub query: QueryImage,
    pub filename: String,
    /// `data:image/jpeg;base64,…` thumbnail for the query pill.
    pub thumbnail: String,
    pub created: Instant,
}

/// TTL-evicted, capped store of uploaded query images. Lives in the web
/// `AppState`; nothing is persisted — a restart just means a re-drop.
#[derive(Debug)]
pub struct QueryImageSessions {
    map: HashMap<String, QueryImageSession>,
    ttl: Duration,
    cap: usize,
}

impl Default for QueryImageSessions {
    fn default() -> Self {
        Self::new(Duration::from_secs(60 * 60), 64)
    }
}

impl QueryImageSessions {
    pub fn new(ttl: Duration, cap: usize) -> Self {
        Self { map: HashMap::new(), ttl, cap }
    }

    /// Store a session and return its token (32 hex chars — safe inside
    /// the `similar:@<token>[:<limit>]` query grammar).
    pub fn insert(&mut self, session: QueryImageSession) -> String {
        self.sweep();
        if self.map.len() >= self.cap {
            // Evict the oldest to stay under the cap.
            if let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, s)| s.created)
                .map(|(k, _)| k.clone())
            {
                self.map.remove(&oldest);
            }
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.map.insert(token.clone(), session);
        token
    }

    /// Look up a live session; expired ones read as absent.
    pub fn get(&self, token: &str) -> Option<&QueryImageSession> {
        self.map
            .get(token)
            .filter(|s| s.created.elapsed() < self.ttl)
    }

    pub fn remove(&mut self, token: &str) -> Option<QueryImageSession> {
        self.map.remove(token)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn sweep(&mut self) {
        let ttl = self.ttl;
        self.map.retain(|_, s| s.created.elapsed() < ttl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(created: Instant) -> QueryImageSession {
        QueryImageSession {
            query: QueryImage {
                content_hash: "sha256:x".to_string(),
                exact_asset_id: None,
                embedding: Some(vec![1.0, 0.0]),
            },
            filename: "q.jpg".to_string(),
            thumbnail: String::new(),
            created,
        }
    }

    #[test]
    fn sessions_roundtrip_and_expire() {
        let mut s = QueryImageSessions::new(Duration::from_secs(60), 8);
        let tok = s.insert(session(Instant::now()));
        assert_eq!(tok.len(), 32);
        assert!(s.get(&tok).is_some());
        assert!(s.get("nope").is_none());

        let old = s.insert(session(Instant::now() - Duration::from_secs(120)));
        assert!(s.get(&old).is_none(), "expired session reads as absent");
        // The next insert sweeps it out for real.
        let _ = s.insert(session(Instant::now()));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn sessions_cap_evicts_oldest() {
        let mut s = QueryImageSessions::new(Duration::from_secs(3600), 2);
        let a = s.insert(session(Instant::now() - Duration::from_secs(30)));
        let b = s.insert(session(Instant::now() - Duration::from_secs(10)));
        let c = s.insert(session(Instant::now()));
        assert_eq!(s.len(), 2);
        assert!(s.get(&a).is_none(), "oldest evicted");
        assert!(s.get(&b).is_some());
        assert!(s.get(&c).is_some());
    }

    #[test]
    fn query_image_rank_pins_exact_match() {
        let idx = crate::embedding_store::EmbeddingIndex::from_rows(2, vec![
            ("a".to_string(), vec![1.0, 0.0]),
            ("b".to_string(), vec![0.0, 1.0]),
        ]);
        let q = QueryImage {
            content_hash: "sha256:x".to_string(),
            exact_asset_id: Some("b".to_string()),
            embedding: Some(vec![1.0, 0.0]),
        };
        let r = q.rank(&idx, 10, 0.0);
        assert_eq!(r[0], ("b".to_string(), 1.0));
        assert_eq!(r[1].0, "a");
        assert_eq!(r.len(), 2);

        let no_emb = QueryImage { embedding: None, ..q.clone() };
        assert_eq!(no_emb.rank(&idx, 10, 0.0), vec![("b".to_string(), 1.0)]);
    }

    #[test]
    fn hash_and_lookup_finds_exact_variant() {
        use crate::models::{Asset, AssetType, Variant, VariantRole};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("q.bin");
        std::fs::write(&file, b"query bytes").unwrap();
        let hash = ContentStore::new(&PathBuf::new()).hash_file(&file).unwrap();

        let catalog = Catalog::open(dir.path()).unwrap();
        catalog.initialize().unwrap();
        let mut asset = Asset::new(AssetType::Image, &hash);
        asset.variants.push(Variant {
            content_hash: hash.clone(),
            asset_id: asset.id,
            role: VariantRole::Original,
            format: "jpg".to_string(),
            file_size: 11,
            original_filename: "q.jpg".to_string(),
            source_metadata: Default::default(),
            locations: vec![],
        });
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&asset.variants[0]).unwrap();

        let (h, exact) = hash_and_lookup(&catalog, &file).unwrap();
        assert_eq!(h, hash);
        assert_eq!(exact.as_deref(), Some(asset.id.to_string().as_str()));

        let other = dir.path().join("other.bin");
        std::fs::write(&other, b"different").unwrap();
        let (_, exact) = hash_and_lookup(&catalog, &other).unwrap();
        assert!(exact.is_none());

        assert!(hash_and_lookup(&catalog, &dir.path().join("missing.jpg")).is_err());
    }

    #[test]
    fn resolve_tolerates_model_failure_only_with_exact_match() {
        use crate::models::{Asset, AssetType, Variant, VariantRole};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("q.bin");
        std::fs::write(&file, b"query bytes").unwrap();
        let hash = ContentStore::new(&PathBuf::new()).hash_file(&file).unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        catalog.initialize().unwrap();
        let failing = |_p: &Path| -> Result<Vec<f32>> { anyhow::bail!("model not downloaded") };

        // No exact match + no model → error
        assert!(resolve_query_image(&catalog, &file, failing).is_err());

        let mut asset = Asset::new(AssetType::Image, &hash);
        asset.variants.push(Variant {
            content_hash: hash.clone(),
            asset_id: asset.id,
            role: VariantRole::Original,
            format: "jpg".to_string(),
            file_size: 11,
            original_filename: "q.jpg".to_string(),
            source_metadata: Default::default(),
            locations: vec![],
        });
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&asset.variants[0]).unwrap();

        // Exact match + no model → tolerated with a warning
        let (q, warning) = resolve_query_image(&catalog, &file, failing).unwrap();
        assert_eq!(q.exact_asset_id.as_deref(), Some(asset.id.to_string().as_str()));
        assert!(q.embedding.is_none());
        assert!(warning.unwrap().contains("model not downloaded"));

        // Working encoder → embedding present
        let (q, warning) =
            resolve_query_image(&catalog, &file, |_p: &Path| Ok(vec![0.6, 0.8])).unwrap();
        assert_eq!(q.embedding, Some(vec![0.6, 0.8]));
        assert!(warning.is_none());
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }
}
