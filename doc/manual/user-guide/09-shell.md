# Interactive Shell

MAKI includes an interactive shell that keeps catalog state alive between commands, giving near-instant response times and enabling multi-step workflows with named variables, session defaults, tab completion, and script files. Instead of paying the startup cost of locating the catalog, loading configuration, and opening SQLite on every invocation, the shell does this once and reuses it for the entire session.

This chapter covers interactive use, variables, scripting, and practical workflows. For one-shot CLI scripting with bash or Python, see [Scripting](08-scripting.md).

---

## Starting the Shell

### Interactive mode

Launch the shell from within a catalog directory:

```bash
maki shell
```

Output:

```
maki shell v4.14.0 — type 'help' or 'quit' to exit
photos>
```

The prompt shows the catalog name (the directory basename). Type any maki command without the `maki` prefix.

### Run a script file

Execute a `.maki` script file and exit:

```bash
maki shell post-import.maki
```

### Run a single command

Run one command and exit, useful in shell scripts or aliases:

```bash
maki shell -c 'search "rating:5 tag:landscape"'
```

The `-c` mode shares the same parsing and variable support as interactive mode, but exits after processing the command string.

---

## Basic Usage

Inside the shell, type any maki subcommand directly. There is no need to type `maki` before each command:

```
photos> stats
photos> search "tag:portrait rating:4+"
a1b2c3d4  DSC_1234 [image] (nef) — 2026-01-15T10:30:00
e5f6a7b8  DSC_5678 [image] (nef) — 2026-01-14T16:45:00
...

42 result(s)
photos> show abc12345
photos> edit --rating 5 abc12345
```

### Blank lines and comments

Blank lines are silently skipped. Lines starting with `#` are treated as comments:

```
photos> # This is a comment
photos>
photos> stats
```

### History

The shell saves command history to `.maki/shell_history` inside your catalog directory. Use the up/down arrow keys to recall previous commands. History persists across sessions.

### Keyboard shortcuts

| Key | Action |
|-----|--------|
| Up/Down | Navigate command history |
| Ctrl-C | Cancel the current line (does not exit) |
| Ctrl-D | Exit the shell (same as `quit`) |
| Ctrl-A | Move cursor to start of line |
| Ctrl-E | Move cursor to end of line |
| Ctrl-K | Delete from cursor to end of line |
| Ctrl-W | Delete word before cursor |
| Tab | Trigger tab completion |

---

## Variables

The shell supports named variables that hold lists of asset IDs. Variables let you capture command results and reuse them in subsequent commands without re-running queries.

### The implicit `_` variable

`search` stores the IDs of its results in the implicit `_` variable (it is the only command that does; `duplicates`, `show` and the others leave `_` unchanged). Use `_` in the next command to refer to those results:

```
photos> search "tag:landscape rating:4+"
a1b2c3d4  DSC_1234 [image] (nef) — 2026-01-15T10:30:00
...

12 result(s)
photos> edit --rating 5 _
Name: DSC_1234
Description: (none)
Rating: ★★★★★ (5/5)
Label: (none)
Date: 2026-01-15
...
photos> tag _ portfolio
Added tags: portfolio
Tags: landscape, portfolio
...
```

`edit` and `tag` take one asset, so the shell runs them once per ID and each run prints its own summary.

The `_` variable is updated after every `search` that returns at least one asset. It only expands when it appears as a standalone token -- underscores inside words like `my_tag` or `_foo` are left alone.

### Named variables

Store results in a named variable with the `$name = command` syntax (the search listing itself, omitted here, is printed as usual):

```
photos> $picks = search "rating:5 date:2024"
  38 assets → $picks
photos> $untagged = search "tagcount:0 type:image"
  142 assets → $untagged
```

Use variables in any command by referencing `$name`:

```
photos> tag $untagged needs-review
Added tags: needs-review
Tags: needs-review
...
photos> export $picks /tmp/best
Export complete: 38 files copied
  Total size: 1.2 GB
```

You can also copy the last result into a named variable:

```
photos> search "rating:5"
...

38 result(s)
photos> $best = _
  38 assets → $best
```

### Variable rules

- Names start with `$`, followed by letters, digits, or underscores (`$picks`, `$session_2024`, `$batch1`)
- Variables hold asset ID lists -- they are not re-evaluated queries
- Variables persist for the session and are cleared on `reload`
- Expanding an undefined variable leaves it as-is in the command
- **Position does not matter.** A variable is taken out of the command line and its asset IDs are put where the command expects them, so `tag portfolio $picks` and `tag $picks portfolio` produce the same result:
  - Single-asset commands (`tag`, `edit`, `show`, `split`, `update-location`, `preview`) receive the ID right after the command name and are run once per ID.
  - `export` and `contact-sheet` receive the IDs as an `id:… id:…` query in their first position (`export $picks /tmp/best`).
  - Every other command gets the IDs appended at the end of the argument list in a single run (`delete --apply $picks`). This only works for commands that accept a list of asset IDs; commands that take a search query or paths instead (`auto-group`, `generate-previews`, ...) reject or ignore them.

### Listing and removing variables

Use `vars` to see all defined variables and their counts:

```
photos> vars
  Session defaults: --log
  _ = 38 assets
  $picks = 38 assets
  $untagged = 142 assets
```

Remove a variable with `unset`:

```
photos> unset $untagged
  Removed $untagged
```

---

## Session Defaults

Use `set` to add flags that are automatically injected into every subsequent command. This is useful when you want `--log` or `--json` output for an entire session without typing it each time.

### Setting defaults

```
photos> set --log
  Session default: --log
  Active defaults: --log
photos> set --json
  Session default: --json
  Active defaults: --json --log
```

The settable flags are: `--json`, `--log`, `--verbose`, `--debug`, and `--time`.

After setting `--log`, every command will include progress output automatically:

```
photos> set --log
photos> generate-previews /Volumes/Photos/Capture/2026-02-22
  DSC_1234.NEF — generated (212ms)
  DSC_1235.NEF — generated (198ms)
  ...
```

(`generate-previews` takes files or directories, not a search query; without arguments it covers the whole catalog and skips assets that already have a preview.)

### Removing defaults

```
photos> unset --log
  Removed session default: --log
```

Session defaults are also cleared by `reload`.

**Tip:** If a session default conflicts with what a particular command needs, you do not need to unset it first. The shell will not inject a flag that already appears in the command line.

---

## Tab Completion

The shell provides context-aware tab completion powered by rustyline. Press Tab at any point to see suggestions.

### What gets completed

| Context | Completes | Example |
|---------|-----------|---------|
| First word | Subcommand names and built-in commands | `sea` -> `search` |
| `--` prefix | Common flags | `--j` -> `--json` |
| `$` prefix | Defined variable names | `$pi` -> `$picks` |
| `tag:` prefix | Tag names from the catalog | `tag:land` -> `tag:landscape` |
| `volume:` prefix | Volume labels from the catalog | `volume:Ph` -> `volume:Photos` |
| After `--volume` | Volume labels | `--volume Ph` -> `--volume Photos` |

Volume labels containing spaces are automatically quoted in completions:

```
photos> search volume:<Tab>
volume:Photos  volume:"External SSD"  volume:Archive
```

Completion data is loaded from the catalog when the shell starts. Use `reload` to refresh it after adding new tags or volumes.

---

## Script Files

A `.maki` script file is a sequence of commands, one per line. Scripts support the same syntax as interactive mode: variables, comments, blank lines, and `source`.

### Basic script format

```bash
# post-import.maki -- run after each shoot day
import /Volumes/Cards/DCIM --log
auto-group
generate-previews --log
auto-tag "" --apply --log
describe "type:image" --apply --log
stats
```

`auto-tag` needs a scope; the empty query `""` means all assets. `describe` skips assets that already have a description unless `--force` is given, so a broad scope only touches the undescribed ones.

Execute it:

```bash
maki shell post-import.maki
```

### Variables in scripts

Variables work across lines, making multi-step workflows clean:

```bash
# curate-and-export.maki
auto-group "path:Capture/2026-02-22"
generate-previews /Volumes/Photos/Capture/2026-02-22 --log
$picks = search "path:Capture/2026-02-22 rating:4+"
tag $picks selects
export $picks /tmp/selects
stats
```

### The `--strict` flag

By default, scripts continue after errors. Use `--strict` to stop on the first failure:

```bash
maki shell --strict critical-workflow.maki
```

In `--strict` mode, the shell exits with code 1 on any error. Without `--strict`, errors are printed with file and line number but execution continues:

```
critical-workflow.maki:3: Error: volume "Offline" is not mounted
```

### The `source` command

Use `source` inside the shell (or another script) to run a script file inline, sharing the current session state:

```
photos> source post-import.maki
```

Variables defined in the sourced script are available after it finishes. Paths are resolved relative to the catalog root directory, or you can use absolute paths:

```
photos> source /path/to/shared/cleanup.maki
```

---

## Session Management

### `reload`

Re-reads `maki.toml` configuration, refreshes tab completion data (tags and volumes), and clears all variables and session defaults:

```
photos> reload
  Reloaded config, cleared variables and session defaults.
```

Use this after making external changes -- editing `maki.toml`, importing from another terminal, or adding volumes.

### `help`

Prints a quick reference of shell syntax, variable usage, session defaults, and built-in commands:

```
photos> help
```

### `quit` / `exit`

End the session. Ctrl-D also exits. Command history is saved automatically on exit.

---

## Quote Handling

The shell uses smart quote handling that lets search filter syntax pass through naturally. The rule is simple:

- **Quotes at the start of a token** are grouping quotes -- they are stripped, and their content is treated as a single argument (standard shell behavior).
- **Quotes that appear mid-token** are preserved as part of the value (syntax quotes).

This means search filters with quoted values work without extra escaping:

```
photos> search text:"woman with glasses"
```

The shell passes this on as one token, `text:"woman with glasses"`. The quotes around `woman with glasses` are preserved because they start mid-token (after `text:`), so the search engine receives them intact.

Compare with grouping quotes, which wrap the entire token from the start:

```
photos> search "tag:landscape rating:4+"
```

Here the outer quotes are stripped (they start at the beginning of the token), and their content reaches `search` as one argument: the query `tag:landscape rating:4+`. This matters because `search` (like every command that takes a query) accepts exactly one query argument. Typing `search tag:landscape rating:4+` without quotes produces two arguments and fails with `unexpected argument 'rating:4+' found`.

To combine a quoted value with other filters, group the whole query in single quotes and keep the double quotes inside:

```
photos> search 'type:image camera:"NIKON Z 9" rating:4+'
```

The outer single quotes are stripped; the inner double quotes are kept, so the query arrives as `type:image camera:"NIKON Z 9" rating:4+`.

**Tip:** A query that consists of a single filter needs no outer quotes: where the standard CLI wants `maki search 'text:"query"'`, inside the shell you can type `search text:"query"`.

---

## Tilde Expansion

The shell expands `~` to your home directory (`$HOME` on Unix, `%USERPROFILE%` on Windows) in any token:

```
photos> export $picks ~/Desktop/delivery
Export complete: 38 files copied
  Total size: 1.2 GB
photos> export "rating:5" ~/exports/portfolio.zip --zip
Export complete: 12 files archived
  Total size: 410.5 MB
  Written to: /Users/alice/exports/portfolio.zip
```

Only `~` at the start of a token is expanded — `~` appearing elsewhere (e.g. inside a search query) is left as-is.

---

## Built-in Commands

Most commands in the shell are maki subcommands run directly. A few commands are **built-ins** handled specially by the shell:

| Command | Purpose |
|---------|---------|
| `help` | Shell help and syntax reference |
| `reload` | Re-read config, clear variables and defaults |
| `set` / `unset` | Manage session defaults and variables |
| `source` | Run a script file inline |
| `vars` | List defined variables |
| `quit` / `exit` | End the session |

### `export` and `preview` with variables

`export` and `preview` are ordinary maki commands, not built-ins; the shell only decides where variable IDs go.

`export` takes a query or a variable in its first position, plus all standard export flags:

```
photos> export <query|$var> <target> [--layout flat|mirror] [--all-variants]
        [--include-sidecars] [--dry-run] [--overwrite] [--symlink] [--zip]
```

When a variable expands to multiple asset IDs, they are turned into one `id:… id:…` query, so all assets are exported in a single operation:

```
photos> $picks = search "rating:5 tag:landscape"
  24 assets → $picks
photos> export $picks ~/Desktop/landscapes
Export complete: 24 files copied
  Total size: 980.4 MB
photos> export $picks ~/Desktop/landscapes.zip --zip
Export complete: 24 files archived
  Total size: 980.4 MB
  Written to: /Users/alice/Desktop/landscapes.zip
```

`preview` opens an asset's preview image in the operating system's default image viewer. It takes one asset, so `preview $picks` (or `preview _`) runs once per ID and opens one viewer window per asset.

---

## Practical Examples

### Curate and export workflow

Browse a recent shoot, refine selections, and export:

```
photos> $candidates = search "date:2024-06 type:image rating:3+"
  287 assets → $candidates
photos> $portraits = search "date:2024-06 tag:portrait rating:4+"
  42 assets → $portraits
photos> tag $portraits june-selects
photos> export $portraits /tmp/june-portraits
Export complete: 42 files copied
```

### Post-import script

Save this as `post-import.maki` and run after every import:

```bash
# post-import.maki
auto-group
generate-previews --log
auto-tag "tagcount:0 type:image" --apply --log
describe "type:image" --apply --log
stats
```

`auto-group` without `--apply` only reports what it would merge; add `--apply` once you trust the result. `generate-previews` skips assets that already have a preview, and `describe` skips assets that already have a description, so the script only works on what is new.

Run it:

```bash
maki shell post-import.maki
```

Or source it during an interactive session:

```
photos> import /Volumes/Cards/DCIM --log
photos> source post-import.maki
```

### Audit workflow

Investigate and fix grouping issues:

```
photos> set --log
photos> $scattered = search "scattered:2 variants:3+"
  15 assets → $scattered
photos> show $scattered
  ... review each asset ...
photos> split abc12345 def678 ghi901
photos> auto-group "scattered:2 variants:3+"
```

### Batch tagging with variables

Tag assets from multiple searches without re-querying:

```
photos> $landscapes = search "tag:landscape rating:4+"
  85 assets → $landscapes
photos> $portraits = search "tag:portrait rating:4+"
  42 assets → $portraits
photos> tag $landscapes portfolio
photos> tag $portraits portfolio
photos> $portfolio = search "tag:portfolio"
  127 assets → $portfolio
photos> export $portfolio /tmp/portfolio
```

### Quick one-liners with `-c`

Use `-c` mode for cron jobs or shell aliases. The command string is split into commands at line breaks only; `;` is not a separator, so put each command on its own line:

```bash
# Daily stats check
maki shell -c 'stats'

# Export one day's picks
maki shell -c '$picks = search "date:2026-02-22 rating:4+"
export $picks /tmp/daily'

# Verify with logging
maki shell -c 'set --log
verify'
```

---

## Blocked Commands

Four commands are blocked inside the shell because they conflict with the shell session:

| Command | Reason |
|---------|--------|
| `init` | Creates a new catalog; the shell is already inside one |
| `migrate` | Schema migration is run standalone (`maki migrate`). Migrations never run implicitly: when the catalog schema is older than the binary, `maki shell` (like every other command) exits with a hint to run `maki migrate` first |
| `serve` | Starts a long-running web server that conflicts with the shell loop |
| `shell` | No nesting; use `source` to run script files instead |

Attempting to run a blocked command prints a clear message:

```
photos> serve
'serve' cannot be used inside the shell.
```

---

## Tips

**The shell prompt tells you what is in memory.** When named variables are defined, the prompt shows their counts in brackets:

```
photos [picks=38 untagged=142]> tag $untagged review
```

**Variables are snapshots, not live queries.** If you run `$picks = search "rating:5"` and then rate more assets, `$picks` still holds the original results. Run the assignment again to refresh.

**Use `set --time` to profile commands.** This is handy when exploring performance differences between queries during an interactive session.

**Scripts are composable.** A master script can `source` other scripts, building complex workflows from reusable pieces:

```bash
# nightly.maki
source post-import.maki
source cleanup.maki
source verify.maki
```

**Ctrl-C is safe.** It cancels the current input line but does not exit the shell or lose your variables. Use Ctrl-D or `quit` to exit.

---

## Related Topics

- [Scripting](08-scripting.md) -- bash and Python scripting with `maki --json` and `jq`
- [Browsing & Searching](05-browse-and-search.md) -- search syntax and output format options
- [CLI Conventions](../reference/00-cli-conventions.md) -- global flags, exit codes, and output conventions
- [Search Filters Reference](../reference/06-search-filters.md) -- all available search filters
- [Configuration Reference](../reference/08-configuration.md) -- `maki.toml` settings including `[import]` and `[serve]` sections
