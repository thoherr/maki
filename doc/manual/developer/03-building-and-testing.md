# Building & Testing

## Building

### Debug Build

```bash
cargo build
```

Produces an unoptimized binary at `target/debug/maki` with debug symbols and runtime checks enabled. Fast compilation, suitable for development.

### Release Build

```bash
cargo build --release
```

Produces an optimized binary at `target/release/maki`. Significantly faster runtime performance. Use this for production deployment.

### Pro Build

```bash
cargo build --features pro
cargo build --release --features pro
```

Builds the **Pro** edition with SigLIP-based zero-shot image classification, face detection/recognition, and visual similarity search. The `pro` feature enables `ai` automatically. Adds ONNX Runtime (`ort`), `ndarray`, and `tokenizers` as dependencies. The ONNX Runtime shared library is downloaded automatically during build. Without `--features pro` (or `--features ai`), these dependencies are not compiled and AI commands (`auto-tag`, `embed`, `faces`) are not available.

### Requirements

- **Rust edition**: 2021; minimum Rust **1.89** (std file locking; the bundled-SQLite stack needs 1.88). CI tracks stable.
- **Platforms**: macOS, Linux, Windows
- **SQLite**: Bundled via `rusqlite` with the `bundled` feature (no system SQLite required)

## Testing

### Run All Tests

```bash
cargo test
```

Runs approximately 693 tests total: ~465 unit tests and ~228 integration tests. With `--features pro` (or `--features ai`), adds ~41 unit tests and ~13 integration tests.

### Unit Tests Only

```bash
cargo test --lib
```

Runs the ~465 unit tests embedded in library source files (`#[cfg(test)]` modules within `src/`).

### Integration Tests Only

```bash
cargo test --test integration
```

Runs the ~228 integration tests defined in `tests/cli.rs`. These tests exercise the full system through the CLI binary and library API, using temporary catalogs and volumes.

### Run a Specific Test

```bash
cargo test test_name_pattern
```

Runs only tests whose names match the given pattern.

### Test Helpers

The integration test suite provides helper functions for setting up test catalogs:

- **`setup_search_catalog()`** -- Creates a catalog with pre-populated assets for search testing. Requires `asset.variants` to be populated before calling `catalog.insert_asset()` (because denormalized columns `best_variant_hash`, `primary_variant_format`, and `variant_count` are computed at insert time).

- **`setup_metadata_catalog()`** -- Creates a catalog with assets that have metadata (tags, ratings, descriptions, recipes) for metadata operation testing. Same requirement: variants must be populated before insert.

## Documentation

### Rust API Docs

```bash
cargo doc --no-deps --open
```

Generates HTML documentation from doc comments and opens it in your browser. The `--no-deps` flag skips building docs for third-party dependencies, which speeds up the build considerably. Output is at `target/doc/maki/`.

### PDF Manual

```bash
bash doc/manual/build-pdf.sh
```

Generates `doc/manual/maki-manual.pdf` from the 21 Markdown source files. The script concatenates all sections in order, renders mermaid diagrams to PNG, and produces a PDF with table of contents, headers/footers, and syntax-highlighted code blocks. The version number is read from `Cargo.toml`.

**Prerequisites** (not required for building or running maki itself):

- **pandoc** -- Document conversion. `brew install pandoc`
- **XeLaTeX** -- PDF typesetting with Unicode support. `brew install --cask mactex-no-gui`
- **mermaid-cli** (`mmdc`) -- Diagram rendering. `brew install mermaid-cli`

## Release Process

1. **Update documentation**: User manual, README, CHANGELOG, and any other docs affected by the release.

2. **Bump version** in `Cargo.toml`:
   ```toml
   [package]
   version = "X.Y.Z"
   ```

3. **Update lockfile**:
   ```bash
   cargo build
   ```
   This regenerates `Cargo.lock` with the new version.

4. **Run all tests — both feature matrices**:
   ```bash
   cargo test
   cargo test --features pro
   ```
   All tests must pass in both configurations before releasing.

5. **Commit**:
   ```bash
   git add -A
   git commit -m "Release vX.Y.Z -- brief description"
   ```

6. **Tag**:
   ```bash
   git tag vX.Y.Z
   ```

7. **Push**:
   ```bash
   git push origin main && git push origin vX.Y.Z
   ```
   Verify both actually landed (`git ls-remote origin` shows the tag and
   the main commit).

8. **Let the Release workflow publish.** The tag push triggers
   `.github/workflows/release.yml`, which builds all platform archives,
   creates the GitHub release, extracts the release body from the
   CHANGELOG's `## vX.Y.Z` section, and uploads all assets.
   **Do not run `gh release create` manually** — a manually created
   release races the workflow: the workflow's token cannot upload assets
   to a release it doesn't own, and the result is a release with a
   partial asset set.

9. **Verify the assets** once the workflow completes (~20 minutes):
   ```bash
   gh release view vX.Y.Z --json assets --jq '.assets[].name'
   ```
   Expect 11 assets: six platform archives (macOS arm64 / Linux x86_64 /
   Windows x86_64, each standard + pro), four PDFs (manual, cheat-sheet,
   search-filters, tagging), and `THIRD_PARTY_LICENSES.md`.

10. **Per-minor cleanup**: only the latest patch of each minor line
    keeps a GitHub release (all tags are kept). After verifying, delete
    the previous *published* release of the same minor:
    ```bash
    gh release delete vX.Y.<previous> --yes --cleanup-tag=false
    ```
    Check `gh release list` first — pick the previous release that was
    actually published, not just the previous version number.

## Dependencies

### Rust Crates

| Crate | Purpose |
|-------|---------|
| `clap` | CLI argument parsing with derive macros |
| `sha2` | SHA-256 content hashing |
| `serde` / `serde_json` / `serde_norway` | Serialization for JSON output, YAML sidecars |
| `rusqlite` | SQLite database access (bundled) |
| `kamadak-exif` | EXIF metadata extraction from images |
| `quick-xml` | XMP/XML parsing and manipulation |
| `regex` | Pattern matching in query parsing and XMP processing |
| `image` | Image decoding, resizing, and encoding for previews |
| `imageproc` | Text rendering on info card previews |
| `ab_glyph` | Font loading for info card text (embedded DejaVu Sans) |
| `lofty` | Audio metadata extraction (duration, bitrate, sample rate, channels, embedded tags) |
| `uuid` | UUID v4 generation (asset IDs) and v5 (deterministic IDs) |
| `axum` | HTTP web framework for the `serve` command |
| `askama` | Compile-time HTML template engine |
| `tokio` | Async runtime for the web server |
| `tower-http` | Static file serving middleware (`ServeDir` for previews) |
| `toml` / `toml_edit` | Configuration file parsing (`maki.toml`, `searches.toml`); comment-preserving Settings save |
| `schemars` | JSON Schema generation for the web Settings form |
| `printpdf` | Contact-sheet PDF assembly |
| `zip` | Web export ZIP archives |
| `rustyline` | Interactive `maki shell` REPL (line editing, completion) |
| `glob-match` | Filename glob matching for import exclusion patterns |
| `chrono` | Date/time handling with serde support |
| `anyhow` / `thiserror` | Error handling |
| `ort` | ONNX Runtime bindings for AI inference (optional, `ai` feature) |
| `ndarray` | N-dimensional array operations for tensor manipulation (optional, `ai` feature) |
| `tokenizers` | HuggingFace tokenizer for SentencePiece text encoding (optional, `ai` feature) |

### Dev Dependencies

| Crate | Purpose |
|-------|---------|
| `assert_cmd` | CLI binary testing (running `maki` as a subprocess) |
| `predicates` | Assertion helpers for CLI output matching |
| `tempfile` | Temporary directories for test isolation |
| `tower` / `http-body-util` | In-process axum router test harness (no socket) |
| `proptest` | Property-based round-trip tests for the XMP reader/writer |

### External Tools (Highly Recommended)

These tools are not Rust dependencies but are invoked as subprocesses for specific preview generation tasks. Their absence does not prevent the application from running; missing tools result in info card fallback previews.

- **dcraw** or **LibRaw** (`dcraw_emu`) -- RAW image preview extraction. Used to decode camera-native formats (NEF, ARW, CR2, CR3, etc.) into RGB data for thumbnail generation. LibRaw's `dcraw_emu` is preferred when available.

- **ffmpeg** / **ffprobe** -- Video thumbnail extraction, audio waveform previews (`showwavespic`), and video metadata (duration, codec, resolution, framerate via `ffprobe`, which ships in the ffmpeg package).

- **curl** -- Model file download for AI auto-tagging *(Pro)* and VLM image descriptions (`maki describe`). Used to download ONNX model files from HuggingFace. Available by default on macOS and most Linux distributions.

- **keyfinder-cli** / **beat_this** *(optional)* -- External audio analyzers for `maki audio analyze` (musical key and BPM detection). Commands configurable via `[audio]` in `maki.toml`; see the [Configuration Reference](../reference/08-configuration.md).

**Install on macOS** (Homebrew):

```bash
brew install libraw ffmpeg curl
```

**Install on Linux** (package manager):

```bash
# Debian/Ubuntu
sudo apt install libraw-bin ffmpeg curl

# Fedora
sudo dnf install LibRaw ffmpeg curl
```

**Install on Windows** (winget or scoop):

```powershell
# winget
winget install LibRaw.LibRaw Gyan.FFmpeg cURL.cURL

# scoop
scoop install libraw ffmpeg curl
```

To check if these tools are available:

```bash
# macOS / Linux
which dcraw_emu || which dcraw
which ffmpeg
which curl
```

```powershell
# Windows (PowerShell)
Get-Command dcraw_emu -ErrorAction SilentlyContinue
Get-Command ffmpeg -ErrorAction SilentlyContinue
Get-Command curl -ErrorAction SilentlyContinue
```

Preview generation silently falls back to info cards (metadata display images) when these tools are missing. Use `maki generate-previews --debug` to see external tool invocations and errors.
