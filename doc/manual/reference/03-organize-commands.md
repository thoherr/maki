# Organize Commands

Commands for curating static collections and managing saved searches (smart albums).

---

## dam collection create

### NAME

dam-collection-create -- create a new collection

### SYNOPSIS

```
dam [GLOBAL FLAGS] collection create <NAME> [--description <TEXT>]
```

Alias: `dam col create`

### DESCRIPTION

Creates a new empty collection. Collections are manually curated lists of asset IDs, similar to static albums in photo management tools. They are backed by SQLite tables for fast queries and a `collections.yaml` file at the catalog root for persistence across `rebuild-catalog`.

Collection names must be unique. Attempting to create a collection with an existing name produces an error.

### ARGUMENTS

**NAME** (required)
: The name for the new collection.

### OPTIONS

**--description \<TEXT\>**
: An optional description for the collection.

`--json` outputs the created collection's details.

### EXAMPLES

Create a simple collection:

```bash
dam collection create "Best of 2026"
```

Create a collection with a description:

```bash
dam col create "Wedding Portfolio" --description "Final selects for client delivery"
```

Create with JSON output:

```bash
dam col create "Travel" --json
```

### SEE ALSO

[collection add](#dam-collection-add) -- add assets to a collection.
[collection show](#dam-collection-show) -- view collection contents.
[search](04-retrieve-commands.md#dam-search) -- `collection:` filter for searching within a collection.

---

## dam collection list

### NAME

dam-collection-list -- list all collections

### SYNOPSIS

```
dam [GLOBAL FLAGS] collection list
```

Alias: `dam col list`

### DESCRIPTION

Lists all collections in the catalog, showing each collection's name, description (if any), and the number of assets it contains.

### ARGUMENTS

None.

### OPTIONS

This command only accepts [global flags](00-cli-conventions.md#global-flags).

`--json` outputs an array of collection objects.

### EXAMPLES

List all collections:

```bash
dam collection list
```

List collections as JSON and extract names:

```bash
dam col list --json | jq '.[].name'
```

Count total collections:

```bash
dam col list --json | jq 'length'
```

### SEE ALSO

[collection create](#dam-collection-create) -- create a new collection.
[collection show](#dam-collection-show) -- view a specific collection's contents.

---

## dam collection show

### NAME

dam-collection-show -- show the contents of a collection

### SYNOPSIS

```
dam [GLOBAL FLAGS] collection show <NAME> [--format <FMT>]
```

Alias: `dam col show`

### DESCRIPTION

Displays the assets belonging to a collection. Output format can be customized using the same format presets and template syntax as `dam search`.

### ARGUMENTS

**NAME** (required)
: The name of the collection to display.

### OPTIONS

**--format \<FMT\>**
: Output format. Presets: `ids`, `short` (default), `full`, `json`. Custom templates use `{placeholder}` syntax (e.g., `'{id}\t{name}'`).

### EXAMPLES

Show a collection's contents:

```bash
dam collection show "Best of 2026"
```

Get just the asset IDs for piping:

```bash
dam col show "Wedding Portfolio" --format ids
```

Show full details including tags:

```bash
dam col show "Travel" --format full
```

Export collection as JSON:

```bash
dam col show "Favorites" --format json | jq '.[].id'
```

### SEE ALSO

[collection add](#dam-collection-add) -- add assets to the collection.
[collection remove](#dam-collection-remove) -- remove assets from the collection.
[search](04-retrieve-commands.md#dam-search) -- `collection:` filter for searching within collections.

---

## dam collection add

### NAME

dam-collection-add -- add assets to a collection

### SYNOPSIS

```
dam [GLOBAL FLAGS] collection add <NAME> <ASSET_IDS...>
```

Alias: `dam col add`

### DESCRIPTION

Adds one or more assets to an existing collection. Asset IDs that are already in the collection are silently ignored (no duplicates are created).

Supports stdin piping for integration with `dam search -q` and shell scripting.

### ARGUMENTS

**NAME** (required)
: The name of the collection to add assets to.

**ASSET_IDS** (required)
: One or more asset IDs to add. Also accepts IDs from stdin.

### OPTIONS

This command only accepts [global flags](00-cli-conventions.md#global-flags).

### EXAMPLES

Add specific assets to a collection:

```bash
dam collection add "Favorites" a1b2c3d4-... e5f67890-...
```

Pipe search results into a collection:

```bash
dam search -q "rating:5 tag:travel" | xargs dam col add "Travel Best"
```

Add all 5-star landscape photos to a collection:

```bash
dam search -q "rating:5 tag:landscape" | xargs dam col add "Portfolio"
```

Add assets from a saved search:

```bash
dam ss run "Recent Imports" --format ids | xargs dam col add "Review Queue"
```

### SEE ALSO

[collection remove](#dam-collection-remove) -- remove assets from a collection.
[collection show](#dam-collection-show) -- view collection contents.
[search](04-retrieve-commands.md#dam-search) -- find assets to add.

---

## dam collection remove

### NAME

dam-collection-remove -- remove assets from a collection

### SYNOPSIS

```
dam [GLOBAL FLAGS] collection remove <NAME> <ASSET_IDS...>
```

Alias: `dam col remove`

### DESCRIPTION

Removes one or more assets from a collection. The assets themselves are not deleted -- only their membership in the collection is removed. Asset IDs not present in the collection are silently ignored.

### ARGUMENTS

**NAME** (required)
: The name of the collection to remove assets from.

**ASSET_IDS** (required)
: One or more asset IDs to remove.

### OPTIONS

This command only accepts [global flags](00-cli-conventions.md#global-flags).

### EXAMPLES

Remove a single asset from a collection:

```bash
dam collection remove "Favorites" a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

Remove multiple assets:

```bash
dam col remove "Review Queue" a1b2c3d4-... e5f67890-...
```

Remove all assets with a certain label from a collection:

```bash
dam search -q "collection:Portfolio label:Red" --format ids | xargs dam col remove "Portfolio"
```

### SEE ALSO

[collection add](#dam-collection-add) -- add assets to a collection.
[collection delete](#dam-collection-delete) -- delete the entire collection.

---

## dam collection delete

### NAME

dam-collection-delete -- delete a collection

### SYNOPSIS

```
dam [GLOBAL FLAGS] collection delete <NAME>
```

Alias: `dam col delete`

### DESCRIPTION

Deletes a collection entirely. This removes the collection record and all its membership entries. The assets themselves are not affected -- only the collection is removed.

### ARGUMENTS

**NAME** (required)
: The name of the collection to delete.

### OPTIONS

This command only accepts [global flags](00-cli-conventions.md#global-flags).

### EXAMPLES

Delete a collection:

```bash
dam collection delete "Old Review Queue"
```

Delete using the alias:

```bash
dam col delete "Temporary"
```

Delete with JSON confirmation:

```bash
dam col delete "Drafts" --json
```

### SEE ALSO

[collection create](#dam-collection-create) -- create a new collection.
[collection list](#dam-collection-list) -- list all collections.

---

## dam saved-search save

### NAME

dam-saved-search-save -- save a search query with a name

### SYNOPSIS

```
dam [GLOBAL FLAGS] saved-search save <NAME> <QUERY> [--sort <SORT>]
```

Alias: `dam ss save`

### DESCRIPTION

Saves a search query under a name for later re-use. Saved searches are stored in `searches.toml` at the catalog root and function as smart albums -- the results update dynamically as the catalog changes.

If a saved search with the same name already exists, it is replaced.

Saved searches appear as clickable chips in the web UI browse page and can be executed from the CLI with `dam saved-search run`.

### ARGUMENTS

**NAME** (required)
: A name for the saved search.

**QUERY** (required)
: The search query string, using the same syntax as `dam search`.

### OPTIONS

**--sort \<SORT\>**
: Sort order for results. Values: `date_desc` (default), `date_asc`, `name_asc`, `name_desc`, `size_asc`, `size_desc`.

`--json` outputs the saved search entry.

### EXAMPLES

Save a search for highly-rated landscapes:

```bash
dam saved-search save "Best Landscapes" "tag:landscape rating:4+"
```

Save with a custom sort order:

```bash
dam ss save "Recent Videos" "type:video" --sort date_desc
```

Save a search using quoted filter values:

```bash
dam ss save "Canon Portraits" 'camera:"Canon EOS R5" tag:portrait'
```

Save a path-scoped search:

```bash
dam ss save "February Shoot" "path:Capture/2026-02"
```

### SEE ALSO

[saved-search run](#dam-saved-search-run) -- execute a saved search.
[saved-search list](#dam-saved-search-list) -- list all saved searches.
[saved-search delete](#dam-saved-search-delete) -- delete a saved search.
[search](04-retrieve-commands.md#dam-search) -- query syntax reference.

---

## dam saved-search list

### NAME

dam-saved-search-list -- list all saved searches

### SYNOPSIS

```
dam [GLOBAL FLAGS] saved-search list
```

Alias: `dam ss list`

### DESCRIPTION

Lists all saved searches stored in the catalog, showing each search's name, query, and sort order.

### ARGUMENTS

None.

### OPTIONS

This command only accepts [global flags](00-cli-conventions.md#global-flags).

`--json` outputs an array of saved search objects.

### EXAMPLES

List all saved searches:

```bash
dam saved-search list
```

List as JSON:

```bash
dam ss list --json
```

Count saved searches:

```bash
dam ss list --json | jq 'length'
```

### SEE ALSO

[saved-search save](#dam-saved-search-save) -- create or update a saved search.
[saved-search run](#dam-saved-search-run) -- execute a saved search.

---

## dam saved-search run

### NAME

dam-saved-search-run -- execute a saved search and display results

### SYNOPSIS

```
dam [GLOBAL FLAGS] saved-search run <NAME> [--format <FMT>]
```

Alias: `dam ss run`

### DESCRIPTION

Executes a previously saved search by name and displays the results. The stored query is run against the current state of the catalog, so results reflect any changes since the search was saved.

The sort order saved with the search is applied. Output format can be overridden with `--format`.

### ARGUMENTS

**NAME** (required)
: The name of the saved search to execute.

### OPTIONS

**--format \<FMT\>**
: Output format. Presets: `ids`, `short` (default), `full`, `json`. Custom templates use `{placeholder}` syntax.

### EXAMPLES

Run a saved search:

```bash
dam saved-search run "Best Landscapes"
```

Run and get just IDs for piping:

```bash
dam ss run "Recent Videos" --format ids
```

Run a saved search and add results to a collection:

```bash
dam ss run "Best Landscapes" --format ids | xargs dam col add "Portfolio"
```

Run with JSON output:

```bash
dam ss run "Canon Portraits" --format json | jq '.[].id'
```

### SEE ALSO

[saved-search save](#dam-saved-search-save) -- create or update a saved search.
[collection add](03-organize-commands.md#dam-collection-add) -- add search results to a collection.

---

## dam saved-search delete

### NAME

dam-saved-search-delete -- delete a saved search

### SYNOPSIS

```
dam [GLOBAL FLAGS] saved-search delete <NAME>
```

Alias: `dam ss delete`

### DESCRIPTION

Deletes a saved search by name. The search is removed from `searches.toml`. This does not affect any assets or collections.

### ARGUMENTS

**NAME** (required)
: The name of the saved search to delete.

### OPTIONS

This command only accepts [global flags](00-cli-conventions.md#global-flags).

### EXAMPLES

Delete a saved search:

```bash
dam saved-search delete "Old Query"
```

Delete using the alias:

```bash
dam ss delete "Temporary Search"
```

Delete with JSON confirmation:

```bash
dam ss delete "Drafts" --json
```

### SEE ALSO

[saved-search save](#dam-saved-search-save) -- create a new saved search.
[saved-search list](#dam-saved-search-list) -- list all saved searches.

---

Previous: [Ingest Commands](02-ingest-commands.md) -- `import`, `tag`, `edit`, `group`, `auto-group`.
Next: [Retrieve Commands](04-retrieve-commands.md) -- `search`, `show`, `duplicates`, `stats`, `serve`.
