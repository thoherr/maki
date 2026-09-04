# MAKI Search Filter Reference

All filters work in `maki search`, the web UI search box, and saved searches.
Filters combine with **AND** — every filter must match. Free-text tokens that don't match a filter prefix search names, filenames, descriptions, and metadata.

| Filter | Syntax | Example |
|--------|--------|---------|
| Asset type | `type:<type>` | `type:image`, `type:video` |
| Tag | `tag:<name>` | `tag:landscape`, `tag:"Fools Theater"` |
| Tag whole path | `tag:=<name>` | `tag:=Legoland` (exact value, not leaves elsewhere) |
| Tag leaf only | `tag:/<name>` | `tag:/animals\|birds` (no descendants) |
| Tag case-sensitive | `tag:^<name>` | `tag:^Landscape` (not "landscape") |
| Tag prefix anchor | `tag:\|<text>` | `tag:\|wed` (wedding, wedding-2024, ...) |
| Exclude tag | `-tag:<name>` | `-tag:rejected` |
| Format | `format:<ext>` | `format:nef`, `format:jpg` |
| Rating (exact) | `rating:<N>` | `rating:5`, `rating:0` (unrated) |
| Rating (minimum) | `rating:<N>+` | `rating:3+` |
| Rating (range) | `rating:<N>-<M>` | `rating:3-5` |
| Rating (OR) | `rating:<N>,<M>` | `rating:2,4` |
| Color label | `label:<color>` | `label:Red`, `label:Blue` |
| Camera | `camera:<text>` | `camera:fuji`, `camera:"Canon EOS R5"` |
| Lens | `lens:<text>` | `lens:56mm`, `lens:"RF 50mm"` |
| Description | `description:<text>`, `desc:<text>` | `description:sunset` |
| ISO | `iso:<N>`, `iso:<min>-<max>` | `iso:100`, `iso:100-800` |
| Focal length | `focal:<N>`, `focal:<min>-<max>` | `focal:50`, `focal:35-70` |
| Aperture | `f:<N>`, `f:<min>-<max>` | `f:2.8`, `f:1.4-2.8` |
| Width | `width:<N>`, `width:<N>+`, `width:<min>-<max>` | `width:4000+` |
| Height | `height:<N>`, `height:<N>+` | `height:2160` |
| Source metadata | `meta:<key>=<value>` | `meta:software=CaptureOne` |
| Path pattern | `path:<pattern>` (`*` = wildcard) | `path:*/2026/*/party` |
| Collection | `collection:<name>` | `collection:Favorites` |
| Date | `date:<prefix>` | `date:2026`, `date:2026-02` |
| Date from | `dateFrom:<date>` | `dateFrom:2026-01-01` |
| Date until | `dateUntil:<date>` | `dateUntil:2026-12-31` |
| Volume | `volume:<label>` | `volume:Archive`, `volume:none` |
| Asset ID | `id:<prefix>` | `id:72a0bb4b` |
| Copies | `copies:<N>`, `copies:<N>+` | `copies:1`, `copies:2+` |
| Variants | `variants:<N>+` | `variants:2+` |
| Tag count (leaves) | `tagcount:<N>`, `tagcount:<N>+`, `tagcount:<A>-<B>` | `tagcount:0` (untagged), `tagcount:5+` |
| Scattered | `scattered:<N>`, `scattered:<N>+` | `scattered:2+` (files in 2+ session roots) |
| Duration (seconds) | `duration:<N>`, `duration:<N>+`, `duration:<min>-<max>` | `duration:60`, `duration:30+` |
| Codec | `codec:<text>` | `codec:h264`, `codec:hevc` |
| Musical key | `key:<key>` | `key:Am`, `key:F#m` |
| Tempo (BPM) | `bpm:<N>`, `bpm:<N>+`, `bpm:<min>-<max>` | `bpm:100-140` |
| GPS (box) | `geo:<S>,<W>,<N>,<E>` | `geo:47.5,11.0,48.5,13.0` |
| GPS (radius) | `geo:<lat>,<lng>,<km>` | `geo:52.52,13.405,5` |
| GPS (any/none) | `geo:any`, `geo:none` | `geo:any` |
| Orphan | `orphan:true` | `orphan:true` |
| Missing files | `missing:true` | `missing:true` |
| Stale verification | `stale:<days>` | `stale:30` |
| Stacked | `stacked:true`, `stacked:false` | `stacked:true` |
| Face count | `faces:any`, `faces:none`, `faces:<N>+` | `faces:2+` |
| Person *(Pro)* | `person:<name>` | `person:Alice`, `person:Alice,Bob` (either) |
| Visual similarity *(Pro)* | `similar:<id>`, `similar:<id>:<limit>` | `similar:72a0bb4b:50` (default 40) |
| Query image *(Pro)* | `maki search --image <file>` | `maki search --image preview.jpg` |
| Similarity threshold *(Pro)* | `min_sim:<percent>` | `min_sim:90` (with `similar:`, `text:`, `--image`) |
| Text search *(Pro)* | `text:<query>`, `text:"<query>":<limit>` | `text:"sunset beach"` (quote multi-word) |
| Embedding status | `embed:any`, `embed:none` | `embed:none type:image` |

\newpage

## Combining Filters

**AND** — repeat `tag:` (or `meta:`) to require all values:

    tag:landscape tag:sunset          both tags required

Repeating any other filter ORs the values (`type:image type:video` = `type:image,video`).

**OR** — comma within a single filter:

    tag:alice,bob                     either tag matches
    format:nef,cr3                    NEF or CR3
    label:Red,Orange                  Red or Orange

**NOT** — dash prefix excludes matches (`tag`, `type`, `format`, `label`, `volume`, `camera`, `lens`, `description`, `path`, `collection`, `person`, and free text):

    -tag:rejected                     exclude rejected
    -format:xmp                       exclude XMP files
    -type:other                       exclude "other" type

**Combined:**

    type:image,video -tag:rejected rating:3+

**Hierarchical tags** — `tag:animals` matches `animals`, `animals|birds`, `animals|birds|eagles`. Markers: `tag:=x` whole path only, `tag:/x` leaf only, `tag:^x` case-sensitive, `tag:|x` prefix anchor.

**Numeric filters** — all support: exact (`3`), minimum (`3+`), range (`3-5`), OR (`2,4`), OR+min (`2,4+`).

## Path Normalization

`path:` auto-normalizes in the CLI: `~` expands to home, `./` and `../` resolve relative to cwd, absolute paths matching a volume mount are stripped to volume-relative form with `volume:` implicitly applied.

## Output Formats

| Flag | Output |
|------|--------|
| *(default)* | One line per result: ID, name (or filename), type, format, date |
| `--format ids` or `-q` | One UUID per line (for scripting) |
| `--format full` | Default + tags and description |
| `--format json` or `--json` | JSON array |
| `--format '{id}\t{name}'` | Custom template |

**Placeholders:** `{id}`, `{short_id}`, `{name}`, `{filename}`, `{type}`, `{format}`, `{date}`, `{tags}`, `{description}`, `{hash}`, `{label}`, `{similarity}`, `{exact}` *(Pro, similarity searches)*

## Common Recipes

```
maki search "rating:4+ type:image"                    best images
maki search "date:2026-03 tag:landscape"              March landscapes
maki search "format:nef -tag:processed"               unprocessed RAWs
maki search "copies:1 volume:Photos"                  single-copy files
maki search "stale:90"                                not verified in 90 days
maki search "orphan:true"                             assets with no files
maki search "faces:any person:Alice rating:4+"        Alice's best shots
maki search "similar:72a0bb4b rating:3+ min_sim:80"   similar + curated (Pro)
maki search "text:\"sunset on the beach\""            semantic search (Pro)
maki search --image preview.jpg "min_sim:90"           find the original (Pro)
```

## Sort Options

| Value | Description |
|-------|-------------|
| `date_desc` | Newest first *(default)* |
| `date_asc` | Oldest first |
| `name_desc` | Name Z→A |
| `name_asc` | Name A→Z |
| `size_desc` | Largest first |
| `size_asc` | Smallest first |
| `similarity_desc` | Most similar first *(Pro, default for `similar:`/`--image` views)* |
| `similarity_asc` | Least similar first *(Pro)* |

Sort applies in the web UI and to saved searches (`maki saved-search save NAME QUERY --sort name_asc`). `maki search` has no `--sort` flag: output is newest-first, or by score for `similar:` / `--image`.
