# Proposal: Future Enhancements

Longer-term feature ideas for DAM, carried forward from the completed [Photo Workflow Integration proposal](proposal-photo-workflow-integration.md).

---

## 1. Watch Mode

```
dam watch [PATHS...] [--volume <label>]
```

File system watcher (via `notify` crate) that auto-imports/syncs when files change. Useful for monitoring a CaptureOne session's output folder during an active editing session.

**Use cases:**
- Leave `dam watch /Volumes/PhotosDrive/Sessions/2026-02-23/Capture/` running while shooting tethered — new RAW files are imported automatically
- Monitor an export folder — processed TIFFs/JPEGs are picked up and grouped with their RAW originals
- Detect recipe modifications (XMP/COS) and refresh metadata in real time

**Design considerations:**
- Should debounce events (files are often written in stages)
- Needs to handle volume mount/unmount gracefully
- Could optionally trigger preview generation on new imports
- Consider whether to run as foreground process or background daemon

---

## 2. Export Command

```
dam export <query> --target <path> [--format <preset>] [--include-sidecars]
```

Export matching assets to a directory, optionally with sidecars. Useful for preparing files for delivery or for feeding into another tool.

**Use cases:**
- `dam export "rating:5 tag:portfolio" --target /tmp/delivery/` — gather best-of selections for client delivery
- `dam export "collection:Print" --target /Volumes/USB/ --include-sidecars` — export with XMP/COS sidecars for handoff to another workstation
- `dam export "tag:instagram" --target ~/Export/ --format flat` — flat directory (no subdirectories) for social media upload

**Design considerations:**
- Copy vs. symlink options
- Directory structure preservation (mirror source paths vs. flat)
- Filename conflict resolution (hash suffix, sequence number)
- Whether to export only the primary variant or all variants
- Dry-run mode for preview
