# Proposal: AI Auto-Tagging

Zero-shot image classification using CLIP/SigLIP embeddings for automated tag suggestions, plus visual similarity search as a bonus feature.

Referenced from [enhancements.md](enhancements.md) item 15 and [roadmap.md](roadmap.md).

---

## CLI Interface

```
dam auto-tag [--asset <id>] [--volume <label>] [--query <QUERY>]
             [--model clip|siglip] [--threshold 0.5] [--labels <file>]
             [--apply] [--json] [--log] [--time]
```

Given a configurable label vocabulary (e.g., `landscape`, `portrait`, `architecture`, `animals/birds`), the model scores each image against every label and suggests tags above the threshold. Report-only by default (`--apply` writes tags).

**Bonus feature**: With embeddings stored per-asset, `dam search --similar <asset-id>` finds visually similar images by cosine distance.

**Web UI**: "Suggest tags" button on the asset detail page. Shows suggested tags with confidence badges. Click to accept.

---

## Approach Comparison

### Option A: Embedded ONNX Runtime (`ort` crate)

Ship ONNX Runtime as a linked library. Model files downloaded on first use or bundled.

| Aspect | Impact |
|--------|--------|
| **New dependency** | `ort` crate (~v1.13+), links against `libonnxruntime` |
| **Binary size increase** | +50-150 MB for the ONNX Runtime shared library (platform-dependent). The `dam` binary itself grows ~1-2 MB for the Rust wrapper code. With `minimal-build` feature: ~30-60 MB |
| **Model files on disk** | CLIP ViT-B/32: ~340 MB (vision encoder, FP32) or ~85 MB (INT8 quantized). Text encoder: ~250 MB (FP32) or ~65 MB (INT8). Total: **150-600 MB** depending on precision. Stored in `~/.dam/models/` or catalog `models/` dir |
| **Runtime memory** | ~400-800 MB RSS during inference (model loaded + image tensors + ONNX Runtime overhead). Freed after batch completes |
| **CPU inference** | ~50-200 ms per image on modern CPU (Apple Silicon / x86-64 with AVX2). ViT-B/32 is the smallest/fastest CLIP variant |
| **Compile time** | +2-5 minutes (ONNX Runtime build or download during `cargo build`) |
| **Platform support** | macOS (arm64, x86_64), Linux (x86_64, aarch64), Windows (x86_64) -- all supported by `ort` prebuilt binaries |

### Option B: Python Subprocess

Shell out to a Python script that uses `transformers` + `onnx_clip` or `open_clip`.

| Aspect | Impact |
|--------|--------|
| **New dependency** | Python 3.8+, pip packages (`transformers`, `Pillow`, `onnxruntime`) |
| **Binary size increase** | ~0 (just a bundled .py script) |
| **Disk overhead** | Python env + packages: ~500 MB-2 GB. Model files: same as above |
| **Runtime memory** | Same model memory + Python interpreter overhead (~100 MB extra) |
| **CPU inference** | Similar speed (same ONNX Runtime underneath), but ~500 ms startup penalty per invocation |
| **User friction** | Requires Python installed. Version conflicts. Virtual env management. Not self-contained |

### Option C: External API (Ollama / Local LLaVA)

Shell out to a local vision-language model server (Ollama with LLaVA, or similar).

| Aspect | Impact |
|--------|--------|
| **New dependency** | Ollama or similar installed and running |
| **Binary size increase** | ~0 |
| **Model files** | 4-8 GB for LLaVA (managed by Ollama, not by dam) |
| **Runtime memory** | 4-8 GB (full VLM in memory) |
| **Accuracy** | Higher (natural language understanding), but slower and less predictable output format |
| **Latency** | 1-5 seconds per image on CPU |

---

## Recommendation: Option A (Embedded `ort`) with Optional Feature Flag

**Why:**
1. **Self-contained** -- no external dependencies at runtime. "It just works" after model download.
2. **Fast** -- 50-200 ms/image is practical for batch processing thousands of images.
3. **Fits the project philosophy** -- dam is a single-binary CLI tool. Adding a Python dependency undermines that.
4. **Controllable size** -- use Cargo feature flag (`--features ai`) so users who don't want it pay zero cost. The ONNX Runtime library is only linked when the feature is enabled.
5. **Model download on demand** -- model files (~150 MB quantized) are downloaded on first `dam auto-tag` invocation, not bundled in the binary.

**Recommended model**: CLIP ViT-B/32 (OpenAI) via Qdrant's split ONNX exports (huggingface.co/Qdrant/clip-ViT-B-32-vision).

- Best ratio of speed to accuracy for a tagging use case
- Well-tested ONNX exports available
- 512-dimensional embeddings (compact for storage)
- INT8 quantized variant available (~85 MB vision encoder)
- SigLIP is more memory-efficient and slightly better accuracy, but fewer ready-made ONNX exports and less ecosystem tooling in Rust

---

## Implementation Plan

### New Crate Dependencies

```toml
[dependencies]
ort = { version = "1.13", optional = true, default-features = false, features = ["download-binaries"] }
ndarray = { version = "0.16", optional = true }  # tensor manipulation

[features]
default = []
ai = ["ort", "ndarray"]
```

### New Files (~1500-2000 lines total)

| File | Lines | Purpose |
|------|-------|---------|
| `src/clip.rs` | ~400 | CLIP model loading, image preprocessing (resize/crop/normalize to 224x224, RGB f32 tensor), text tokenization (BPE), inference, cosine similarity |
| `src/embedding_store.rs` | ~200 | SQLite table `embeddings(variant_hash TEXT PK, embedding BLOB, model TEXT)`. Store/retrieve 512-dim f32 vectors. Cosine distance query for similarity search |
| `src/auto_tagger.rs` | ~300 | Orchestration: load label vocabulary, encode labels (cached), encode each image, compute similarities, threshold, return suggestions. Callback-based progress |
| `src/model_manager.rs` | ~200 | Download model files from HuggingFace on first use, verify SHA-256, cache in `~/.dam/models/` or catalog-relative `models/`. Version/integrity tracking |
| `src/asset_service.rs` | ~150 | New `auto_tag()` method + `AutoTagResult` struct |
| `src/main.rs` | ~80 | CLI registration + handler for `auto-tag` and `search --similar` |
| `tests/cli.rs` | ~200 | Integration tests (mocked model or small test model) |

### Schema Changes

```sql
CREATE TABLE IF NOT EXISTS embeddings (
    variant_hash TEXT PRIMARY KEY,
    embedding BLOB NOT NULL,      -- 512 x f32 = 2048 bytes per asset
    model TEXT NOT NULL DEFAULT 'clip-vit-b32'
);
CREATE INDEX idx_embeddings_model ON embeddings(model);
```

Storage overhead: ~2 KB per asset. For 100,000 assets: ~200 MB in SQLite.

### Image Preprocessing Pipeline (Pure Rust, No New Deps)

The existing `image` crate already handles loading/resizing. Preprocessing for CLIP:

1. Resize to 224x224 (center crop)
2. Convert to RGB f32 [0,1]
3. Normalize with CLIP means `[0.48145466, 0.4578275, 0.40821073]` and stds `[0.26862954, 0.26130258, 0.27577711]`
4. Reshape to `[1, 3, 224, 224]` NCHW tensor

### Text Tokenization

CLIP uses BPE tokenization. Options:

- **Embed a minimal BPE tokenizer** (~300 lines of Rust + ~1 MB vocab file) -- self-contained
- **Use `tokenizers` crate** from HuggingFace -- well-tested but adds ~5 MB to binary

Recommendation: embed minimal BPE. Label vocabularies are short (50-200 labels), so performance doesn't matter.

---

## Total Overhead Summary

| Metric | Without `--features ai` | With `--features ai` |
|--------|------------------------|---------------------|
| **Binary size** | 15 MB (unchanged) | ~65-165 MB (+50-150 MB for ONNX Runtime) |
| **Disk (models)** | 0 | ~150 MB (quantized) to ~600 MB (FP32), downloaded on first use |
| **Disk (embeddings)** | 0 | ~2 KB/asset (200 MB for 100K assets) |
| **RAM at rest** | unchanged | unchanged (models not loaded until needed) |
| **RAM during inference** | n/a | ~400-800 MB |
| **CPU during inference** | n/a | ~50-200 ms/image (one core saturated) |
| **Compile time** | ~90s | ~120-180s (+30-90s for ONNX Runtime download/link) |

---

## Effort Estimate

| Phase | Effort |
|-------|--------|
| Model manager + download | 1 day |
| CLIP preprocessing + inference | 2 days |
| BPE tokenizer | 1 day |
| Embedding store (SQLite) | 0.5 day |
| Auto-tagger orchestration | 1 day |
| CLI + handler | 0.5 day |
| `--similar` search integration | 1 day |
| Web UI (suggest tags button, similar-images link) | 1 day |
| Tests (unit + integration) | 1 day |
| Documentation | 0.5 day |
| **Total** | **~9-10 days** |

---

## Pros and Cons

### Pros

- **Biggest UX win remaining** -- manual tagging is the #1 friction point in any DAM
- **Even 70% accuracy saves massive time** -- user accepts/rejects rather than typing
- **Visual similarity search** is a natural bonus -- "find photos like this one" with near-zero extra effort
- **Feature flag keeps it optional** -- users who don't want AI pay nothing
- **No cloud dependency** -- all inference runs locally, no privacy concerns
- **Hierarchical tag support already exists** -- suggested tags can use the existing `/`-separated hierarchy
- **XMP write-back already exists** -- accepted suggestions flow back to CaptureOne/Lightroom automatically

### Cons

- **Large optional dependency** -- ONNX Runtime adds 50-150 MB to binary when enabled
- **Model download friction** -- first-time users must download ~150 MB (quantized) before it works
- **Accuracy ceiling** -- CLIP zero-shot tops out at ~70-80% on general photography categories. Niche subjects (specific bird species, architectural styles) need fine-tuning
- **No GPU acceleration on macOS by default** -- `ort` supports CoreML EP but requires additional build configuration. CPU-only is still fast enough for batch processing
- **BPE tokenizer is CLIP-specific** -- switching to SigLIP later would require a different tokenizer
- **Embedding storage grows linearly** -- 2 KB/asset is modest but adds up in very large catalogs (500K assets = 1 GB)
- **Compile-time complexity** -- ONNX Runtime linking can be finicky on some platforms, especially cross-compilation

---

## Configuration

```toml
[ai]
model = "clip-vit-b32"          # or "clip-vit-b32-int8" for quantized
labels = "labels.txt"           # custom label vocabulary file (one per line)
threshold = 0.25                # minimum confidence to suggest
model_dir = "~/.dam/models"     # where to cache downloaded models
```

Default label vocabulary (~100 common photography categories) would be embedded in the binary, with `labels.txt` as an override.

---

## Edge Cases

- **No preview available**: fall back to loading the original file via `image` crate (slower but works)
- **Non-image assets** (video, audio, documents): skip with warning, or extract a frame for video (already done for preview generation via ffmpeg)
- **Offline volumes**: use existing preview JPEGs (800px or 2560px smart previews) -- no need for originals
- **Model not downloaded**: prompt user to run `dam auto-tag --download` or auto-download with confirmation
- **Label vocabulary empty**: use built-in default (~100 photography categories)
- **Threshold too low**: warn if suggesting >20 tags per asset (likely noise)

---

## Alternative: Start with Option C, Migrate to A

A lower-effort first step (2-3 days) would be to shell out to Ollama's API (`POST http://localhost:11434/api/generate` with a vision model) for tag suggestions. This avoids all the ONNX/model complexity but requires Ollama installed. Could serve as a prototype to validate the UX before investing in the embedded approach.

---

## References

- [pykeio/ort -- Rust ONNX Runtime bindings](https://github.com/pykeio/ort)
- [ort documentation](https://ort.pyke.io/)
- [CLIP ViT-B/32 ONNX models (sayantan47)](https://huggingface.co/sayantan47/clip-vit-b32-onnx)
- [Qdrant CLIP ViT-B/32 vision encoder](https://huggingface.co/Qdrant/clip-ViT-B-32-vision)
- [Qdrant CLIP ViT-B/32 text encoder](https://huggingface.co/Qdrant/clip-ViT-B-32-text)
- [SigLIP vs CLIP memory comparison](https://github.com/mlfoundations/open_clip/discussions/872)
- [SigLIP 2 announcement](https://huggingface.co/blog/siglip2)
- [ONNX Runtime shared library size discussion](https://github.com/microsoft/onnxruntime/issues/6160)
- [onnx_clip -- lightweight CLIP without PyTorch](https://github.com/lakeraai/onnx_clip)
- [CLIP-ONNX -- 3x speedup library](https://github.com/Lednik7/CLIP-ONNX)
