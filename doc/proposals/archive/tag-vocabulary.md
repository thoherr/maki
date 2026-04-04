# Proposal: Tag Vocabulary File

A predefined tag hierarchy that guides autocomplete and prevents vocabulary drift, even for tags that haven't been used on any asset yet.

**Date:** 2026-04-04

**Status:** Complete. All 4 phases implemented (v4.3.6).

---

## Problem

MAKI's tag autocomplete only suggests tags that already exist on at least one asset. When a photographer plans a structured vocabulary (as recommended in the Tagging Guide), the system can't guide them toward planned-but-unused tags. This leads to:

1. **Vocabulary drift** — new tags are invented on the fly instead of following the planned structure
2. **Inconsistency** — `musician` vs `subject|performing arts|concert|musician` depending on whether the user remembers the hierarchy
3. **Lost intent** — the planned structure exists only in a document or in the user's memory

---

## Proposed Solution

A `vocabulary.yaml` file in the catalog root that defines the planned tag hierarchy. MAKI reads it at startup and merges its entries into autocomplete suggestions alongside tags from actual usage.

### File format

A nested YAML tree where keys are hierarchy nodes and values are either sub-trees (maps), leaf lists (arrays), or null (leaf node):

```yaml
# vocabulary.yaml — planned tag hierarchy
#
# Nodes can be maps (sub-tree), arrays (list of leaves), or empty (leaf).
# This file defines the vocabulary skeleton — actual tags are stored per-asset.

subject:
  nature:
    - landscape
    - flora
    - sky
    - water
  animal:
    - mammal
    - bird
    - reptile
    - invertebrate
    - aquatic
    - domestic
  urban:
    - architecture
    - street
    - transport
  person:
    - portrait
    - group
    - activity
  performing arts:
    - concert
    - theatre
    - dance
  event:
    - festival
    - exhibition
    - wedding
    - workshop
    - sports event
  object:
    - food
    - instrument
  concept:
    - travel
    - fashion
    - documentary
    - abstract

location:
  # Structure: location > country > region > city > venue
  # Actual values are proper nouns, added as needed

person:
  family:
  friend:
  artist:
    - musician
    - actor
    - model
  public figure:

technique:
  style:
    - black and white
    - high key
    - low key
    - infrared
  exposure:
    - long exposure
    - double exposure
    - HDR
  lighting:
    - natural light
    - flash
    - studio
    - golden hour
    - blue hour
    - stage lighting
  composition:
    - minimalist
    - symmetry
    - leading lines
  effect:
    - bokeh
    - motion blur
    - silhouette
    - reflection

project:
  # Project entries are personal — add as needed
```

### Parsing rules

The tree is flattened to pipe-separated paths:

- `subject:` → `subject`
- `subject: nature:` → `subject|nature`
- `subject: nature: [landscape]` → `subject|nature|landscape`
- `location:` with no children → `location` (just the root)
- Comments and empty maps are ignored

A node with `#` comment-only children is treated as a structural placeholder (shown in autocomplete as a category, but no leaf values predefined).

### How it integrates

**Autocomplete**: when building the tag suggestion list, merge vocabulary entries with actual tags from the catalog. Vocabulary-only entries (no assets use them yet) appear in autocomplete with a visual distinction (e.g., dimmed or with a "planned" indicator).

**Tags page**: if "show empty categories" is enabled (config toggle), the tree view shows vocabulary entries with zero count. This gives a complete picture of the planned hierarchy.

**Tag add**: when adding a tag that matches a vocabulary entry, everything works as normal. When adding a tag NOT in the vocabulary, no warning — the vocabulary is guidance, not enforcement.

**Export**: `maki tag export-vocabulary` generates `vocabulary.yaml` from the current catalog's tag tree. This captures the organic vocabulary that has grown from usage, providing a starting point for cleanup and planning.

**Import**: the file is read-only from MAKI's perspective — the user edits it in any text editor. No import command needed beyond placing the file in the catalog root.

---

## Implementation Plan

### Phase 1: Read vocabulary file (low effort)

1. At startup (and on `reload` in shell), read `vocabulary.yaml` from catalog root if it exists
2. Flatten the tree to a list of pipe-separated tag paths
3. Merge with actual tags for autocomplete (CLI tab completion and web UI suggestions)

### Phase 2: Web UI integration (medium effort)

1. Autocomplete shows vocabulary entries alongside actual tags, with visual distinction for unused ones
2. Tags page: config option `[tags] show_empty = false` to show/hide unused vocabulary nodes in the tree view
3. Asset detail tag input also uses the merged list

### Phase 3: Export command (low effort)

1. `maki tag export-vocabulary` generates `vocabulary.yaml` from the current tag tree
2. Groups tags into a nested tree structure (reverse of flattening)
3. Useful as a starting point when transitioning from organic tagging to a planned vocabulary

### Phase 4: Default vocabulary (low effort)

1. `maki init` optionally creates a starter `vocabulary.yaml` based on the Tagging Guide's recommended structure
2. Flag: `maki init --vocabulary` or interactive prompt

---

## Design Decisions

### Why YAML tree, not flat list?

The flat format (`subject|nature|landscape` per line) is simpler to parse but harder to read and edit. The tree format:
- Mirrors how people think about hierarchies
- Is easy to scan for gaps
- Supports comments for documentation
- Collapses naturally in editors with folding

### Why not enforce the vocabulary?

Enforcement (rejecting tags not in the vocabulary) would:
- Break import from C1/LR (which may use tags outside the vocabulary)
- Frustrate users who need a one-off tag for a specific project
- Require an escape hatch (`--force`) that undermines the enforcement

Guidance (suggesting + visual cues) achieves 90% of the benefit without the friction.

### Why not store the vocabulary in the catalog database?

The vocabulary is a planning artifact, not derived data. It should be:
- Human-editable in any text editor
- Diffable in git (for catalog backup)
- Sharable between catalogs (copy the file)
- Independent of the SQLite cache (survives `rebuild-catalog`)

### Relationship to `vocabulary.yaml` and git backup

The vocabulary file is tracked by git (not in `.gitignore`), so it's versioned alongside the metadata sidecars. Changes to the planned vocabulary are visible in the git history.

---

## Open Questions

1. **Should the vocabulary distinguish "structural nodes" from "usable tags"?** For example, `subject` is a category header — you'd never tag an image just `subject`. But `landscape` is a usable leaf. The tree format naturally implies this (maps are structural, arrays/leaves are usable), but should the UI enforce it?

2. **Should vocabulary entries appear in `maki stats --tags`?** If so, with zero count (showing planned vs. actual usage)? Or only in the tags page tree view?

3. **Should the web UI have an in-browser vocabulary editor?** Or is the YAML file sufficient for the target audience (technically capable photographers)?

4. **Naming**: `vocabulary.yaml` vs `tags.yaml` vs `tag-vocabulary.yaml`? `vocabulary.yaml` is clear and follows the established naming pattern.
