# Proposal: Interactive REPL

## Motivation

Every `dam` invocation repeats the same startup work: locate catalog root, load `dam.toml`, open SQLite with pragmas, check schema version, construct services. For interactive workflows — browsing results, editing tags, checking stats — this overhead adds up.

A REPL (read-eval-print loop) keeps state alive between commands, giving instant response for the second command onward. It also enables workflow features that aren't possible with one-shot invocations: persistent search results, command history, tab completion, and session context.

## Design

### Entry Point

```
dam repl
```

Starts an interactive session in the current catalog. Displays a prompt, accepts any `dam` subcommand without the `dam` prefix:

```
dam> search "tag:landscape rating:4+"
  12 assets found
dam> edit --rating 5 abc12345
dam> search "tag:portrait date:2024"
  8 assets found
dam> show abc12345
dam> stats
dam> quit
```

### Cached State

Modeled after the existing `AppState` from `dam serve`:

| State | Lifecycle | Notes |
|-------|-----------|-------|
| `catalog_root` | Session | Found once at startup |
| `CatalogConfig` | Session | Reloaded on explicit `reload` command |
| `DeviceRegistry` | Session, invalidated on volume mutations | Volume add/remove/combine refreshes |
| Preview/AI/VLM config | Session | Derived from `CatalogConfig` |
| Catalog (SQLite) | Per-command | Fresh `Catalog::open_fast()` each command, same as web server |

Per-command catalog opens are cheap (~1ms with pragmas) and avoid stale-connection issues. This matches the proven `dam serve` pattern.

### Command Parsing

Clap supports `try_parse_from(args)` which takes an iterator of strings. The REPL loop:

1. Read a line from the user (via rustyline)
2. Shell-split into tokens (handle quotes, escapes)
3. Prepend `"dam"` as argv[0]
4. Call `Cli::try_parse_from(tokens)`
5. Execute the command using the cached state
6. Print result, loop

Parse errors are displayed without exiting. Commands like `init`, `migrate`, and `serve` are rejected inside the REPL (they don't make sense in an interactive session).

### REPL-Only Commands

| Command | Description |
|---------|-------------|
| `quit` / `exit` / Ctrl-D | End the session |
| `reload` | Re-read `dam.toml` and refresh cached config |
| `help` | Show available commands (delegates to clap `--help`) |

### Readline Features

Using `rustyline` crate (~4K lines, stable, MIT):

- **Command history** — persisted to `~/.dam_history` (or catalog `.dam/history`)
- **Tab completion** — complete subcommand names, `--flags`, volume labels, tag names (from cached dropdown data)
- **Line editing** — Emacs-style keybindings (Ctrl-A/E/K/W), Vi mode optional
- **Multi-line** — not needed; commands are single-line

### Output Handling

- Normal text output goes to stdout as usual
- `--json` works per-command (e.g., `search --json "tag:landscape"`)
- `--log` and `--debug` can be set as session defaults via REPL-only `set` command
- `--time` works per-command

### Excluded Commands

These are blocked inside the REPL with a clear message:

- `init` — creates a new catalog; meaningless inside an existing one
- `migrate` — schema migration should be run standalone
- `serve` — starts a long-running web server; conflicts with the REPL's own loop
- `repl` — no nesting

## Implementation

### Phase 1 — Basic REPL

- Add `rustyline` dependency
- New `Commands::Repl` variant (no arguments)
- REPL loop: readline -> parse -> dispatch -> print
- Cache `catalog_root` and `CatalogConfig`
- Command history (in-memory)
- Block excluded commands

### Phase 2 — Completion & History

- Persist history to file
- Tab completion for subcommand names and flags
- Tab completion for volume labels and tag names (from catalog queries, cached)

### Phase 3 — Session Context (Optional)

- `set` command for session-wide defaults (`set --log`, `set --debug`, `set --json`)
- Last search results available as `_` for piping: `edit --rating 5 _` applies to all results from the previous search
- Prompt customization showing catalog name

## Dependencies

| Crate | Purpose | Size |
|-------|---------|------|
| `rustyline` | Line editing, history, completion | ~4K lines, stable |

Alternative: `reedline` (Nushell's editor) — better Unicode, heavier (~30K lines). `rustyline` is the pragmatic choice.

## Complexity

**Phase 1:** Low. The command dispatcher already exists as a single `match` block in `main.rs`. Wrapping it in a loop with `try_parse_from` is mechanical.

**Phase 2:** Low-Medium. Rustyline's `Completer` trait is straightforward; the hard part is keeping completion data fresh after mutations.

**Phase 3:** Medium. Session context and result piping require threading state through the dispatcher.

## Trade-offs

**Pros:**
- Near-instant command execution after first startup
- Command history and tab completion for efficient workflows
- Natural fit for exploratory sessions (search, inspect, edit, repeat)
- Mirrors the `sqlite3` / `python` / `psql` interactive experience

**Cons:**
- New dependency (rustyline)
- Commands must be careful about process-level side effects (changing working directory, signal handlers)
- `--json` output in a REPL is slightly awkward (but useful for scripting with `expect`)
- Catalog changes from external `dam` invocations won't be visible without `reload`

## Not In Scope

- TUI / ncurses interface — the REPL is text-based, not a full terminal UI
- Concurrent command execution — commands run sequentially
- Remote REPL / network protocol — local only
