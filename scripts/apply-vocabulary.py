#!/usr/bin/env python3
"""
Emit `maki tag rename` / `maki tag split` commands that convert flat
catalog tags into their hierarchical form per an AI vocabulary
(the `my-labels.yaml` format used by `[ai] labels`).

Use as a recurring maintenance step:

- The vocabulary grows over time as your catalog matures. Re-running
  this script after each vocabulary change catches any flat tags the
  new mappings should now move into hierarchy.
- Imports from CaptureOne / Lightroom / sidecar XMP often arrive with
  flat keywords (`dog`, `sunset`, `concert`). The script converts each
  to its canonical hierarchical home in one pass.

By default emits dry-run commands so you can review before executing.
Pipe to `sh` to run them, or pass `--apply` to emit commands that
modify the catalog directly.

Usage
-----

    # Use the active vocabulary (whatever `[ai].labels` points to,
    # or the built-in default if no labels file is configured)
    python3 scripts/apply-vocabulary.py

    # Use the built-in default vocabulary explicitly
    python3 scripts/apply-vocabulary.py --default

    # Read an explicit YAML file (e.g. test a candidate file before
    # adopting it in [ai].labels)
    python3 scripts/apply-vocabulary.py my-labels.yaml

    # Generate apply-ready commands and execute them
    python3 scripts/apply-vocabulary.py --apply | sh

    # Save commands to a file, review, then execute
    python3 scripts/apply-vocabulary.py --apply > apply.sh
    less apply.sh
    sh apply.sh

YAML format recap
-----------------

Each top-level key is a flat AI label (what SigLIP scores against).
The value is the hierarchical catalog tag(s) MAKI should apply:

    sunset:
      - subject|nature|sky
      - technique|lighting|golden hour    # one-to-many → emits `tag split`
    concert: subject|performing arts|concert   # one-to-one → emits `tag rename`
    dog: subject|animal|domestic               # one-to-one → emits `tag rename`
    landscape: landscape                       # identity → skipped (no rename needed)
    weather: null                              # null → skipped (no canonical mapping)

Notes
-----

The rename commands use the `=` prefix marker so they only match the
exact flat tag — they will NOT cascade into descendants. That keeps
existing hierarchical tags like `subject|animal|dog` untouched even
when the script renames the bare `dog`.

Tags that don't exist in your catalog are no-ops at execution time
(MAKI reports zero matches and moves on), so it's safe to emit every
mapping unconditionally. We don't filter against the catalog state
here so the script remains useful offline and against any catalog.

This script ships its own tiny parser for the MAKI vocab format
(label → string | list | null) so there's no PyYAML dependency. The
parser is intentionally restricted to the documented format — it
doesn't claim to handle arbitrary YAML.
"""

import argparse
import re
import shlex
import subprocess
import sys
from pathlib import Path


# ── Vocabulary parser ───────────────────────────────────────────────────
#
# Constrained to the MAKI vocab format only — three line shapes:
#
#   key: value                  # bare scalar or null
#   "key with spaces": value    # quoted key (single or double quotes)
#   key:                        # opens a list, followed by:
#     - item                    #   indented list entries
#     - "item with spaces"      #   quoted list items
#
# Comments start with `#`. Blank lines are ignored. A trailing comment
# after a value is supported (`key: value  # note`). The parser fails
# loudly on anything else so we don't silently misinterpret unfamiliar
# input.


_KEY_RE = re.compile(
    r"""^
    (?:"(?P<dq>[^"]+)" | '(?P<sq>[^']+)' | (?P<bare>[^:#]+?))
    \s*:\s*
    (?P<rest>.*)
    $""",
    re.VERBOSE,
)
_LIST_RE = re.compile(
    r"""^\s+-\s+
    (?:"(?P<dq>[^"]+)" | '(?P<sq>[^']+)' | (?P<bare>.+?))
    \s*(?:\#.*)?$""",
    re.VERBOSE,
)


def _strip_trailing_comment(s: str) -> str:
    """Remove `  # ...` trailing comments, respecting quoting."""
    in_dq = False
    in_sq = False
    for i, ch in enumerate(s):
        if ch == '"' and not in_sq:
            in_dq = not in_dq
        elif ch == "'" and not in_dq:
            in_sq = not in_sq
        elif ch == "#" and not in_dq and not in_sq:
            return s[:i].rstrip()
    return s.rstrip()


def _parse_scalar(raw: str) -> str | None:
    """Parse a YAML scalar to a Python value.

    Handles the subset we care about: bare strings, double/single
    quoted strings, and null (`null`, `~`, or empty).
    """
    s = _strip_trailing_comment(raw).strip()
    if not s or s == "null" or s == "~" or s == "Null" or s == "NULL":
        return None
    if s.startswith('"') and s.endswith('"') and len(s) >= 2:
        return s[1:-1]
    if s.startswith("'") and s.endswith("'") and len(s) >= 2:
        return s[1:-1]
    return s


def parse_vocab(text: str) -> dict:
    """Parse the MAKI vocab YAML format into a dict.

    Returns label → str | list[str] | None.
    """
    out: dict[str, object] = {}
    lines = text.splitlines()
    i = 0
    current_key: str | None = None
    while i < len(lines):
        raw = lines[i]
        # Drop blank lines and pure-comment lines.
        if not raw.strip() or raw.lstrip().startswith("#"):
            i += 1
            continue

        if raw.startswith((" ", "\t")):
            # Indented line → must be a list entry for current_key.
            m = _LIST_RE.match(raw)
            if not m or current_key is None:
                raise ValueError(
                    f"line {i + 1}: unexpected indented line "
                    f"(must be a list `- item` after a `key:` opener):\n  {raw!r}"
                )
            item = m.group("dq") or m.group("sq") or (m.group("bare") or "").strip()
            existing = out.get(current_key)
            if existing is None:
                out[current_key] = [item]
            elif isinstance(existing, list):
                existing.append(item)
            else:
                raise ValueError(
                    f"line {i + 1}: list entry for {current_key!r} but "
                    f"the key already has a scalar value"
                )
            i += 1
            continue

        # Top-level key line.
        m = _KEY_RE.match(raw)
        if not m:
            raise ValueError(
                f"line {i + 1}: cannot parse vocabulary line:\n  {raw!r}"
            )
        key = m.group("dq") or m.group("sq") or (m.group("bare") or "").strip()
        rest = m.group("rest") or ""
        rest_stripped = _strip_trailing_comment(rest).strip()
        if rest_stripped == "":
            # `key:` with nothing after → list opener (the next lines
            # provide its entries) or null (if no indented lines follow).
            # We tentatively treat as null; the indented-line path above
            # will overwrite when it sees the first `- item`.
            out[key] = None
            current_key = key
        else:
            out[key] = _parse_scalar(rest_stripped)
            current_key = key
        i += 1

    return out


# ── Vocab loading ───────────────────────────────────────────────────────


def load_vocab(path: Path | None, use_default: bool) -> dict:
    """Load the AI vocabulary.

    Resolution order:
      1. Explicit `path` → read YAML file directly.
      2. Otherwise → invoke `maki ai export-vocabulary [--default]` and
         parse its stdout.
    """
    if path is not None:
        with open(path, encoding="utf-8") as f:
            return parse_vocab(f.read())

    args = ["maki", "ai", "export-vocabulary"]
    if use_default:
        args.append("--default")
    try:
        result = subprocess.run(args, capture_output=True, text=True, check=False)
    except FileNotFoundError:
        print(
            "error: `maki` not found on PATH. Either install MAKI or "
            "pass a vocabulary file path explicitly.",
            file=sys.stderr,
        )
        sys.exit(2)
    if result.returncode != 0:
        print(
            f"error: `maki ai export-vocabulary` failed:\n"
            f"{result.stderr.strip()}",
            file=sys.stderr,
        )
        sys.exit(2)
    return parse_vocab(result.stdout)


# ── Command emission ────────────────────────────────────────────────────


def shell_quote(s: str) -> str:
    """`shlex.quote()` with one extra paranoia: always quote tokens that
    start with `=`.

    zsh's `EQUALS` option (default on, default shell on macOS) treats
    an unquoted token starting with `=` as a command lookup — `=cat`
    expands to `/bin/cat`, and the whole tag-rename invocation falls
    apart. POSIX-shell-aware `shlex.quote()` doesn't escape `=`
    because POSIX shells don't treat it specially, so we have to
    force-quote here to stay portable across zsh and sh.
    """
    q = shlex.quote(s)
    if q == s and s.startswith("="):
        return f"'{s}'"
    return q


def is_identity(label: str, target: str) -> bool:
    """A mapping that leaves the tag flat (no rename needed).

    Only literal identity (target == label) is treated as no-op here.
    Hierarchical promotion (`landscape` → `subject|landscape`) IS
    emitted as a rename — that's the whole point of the script.
    """
    return target == label


def emit_commands(vocab: dict, apply: bool) -> list[str]:
    """Walk the vocabulary and yield shell commands.

    Each emitted line is a single self-contained `maki tag …` invocation
    safe to execute via `sh`. Rename commands use the `=` marker so they
    only match the exact flat tag — descendants stay intact.
    """
    suffix = " --apply" if apply else ""
    cmds: list[str] = []

    for label, target in vocab.items():
        # Skip explicit null mappings — these say "leave the flat label
        # in place, no canonical hierarchical home for it (yet)".
        if target is None:
            continue

        if isinstance(target, str):
            if is_identity(label, target):
                continue
            # `=` constrains the rename to the whole-path match — won't
            # touch `Foo|child` tags that happen to share the leaf name.
            cmds.append(
                f"maki tag rename {shell_quote('=' + label)} "
                f"{shlex.quote(target)}{suffix}"
            )
        elif isinstance(target, list):
            targets = [str(t) for t in target if t]
            if not targets:
                continue
            if len(targets) == 1 and is_identity(label, targets[0]):
                continue
            quoted_targets = " ".join(shlex.quote(t) for t in targets)
            cmds.append(
                f"maki tag split {shell_quote('=' + label)} "
                f"{quoted_targets}{suffix}"
            )
        else:
            print(
                f"warning: skipping {label!r} — value has unsupported type "
                f"{type(target).__name__} (expected string, list, or null)",
                file=sys.stderr,
            )

    return cmds


# ── Entry point ─────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Emit `maki tag rename` / `maki tag split` commands that "
            "convert flat catalog tags into their hierarchical form per "
            "an AI vocabulary."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="See the top of this file for full usage details and YAML format.",
    )
    parser.add_argument(
        "vocab",
        nargs="?",
        type=Path,
        help=(
            "Optional path to a YAML vocabulary file. If omitted, the "
            "active vocabulary is fetched via `maki ai export-vocabulary`."
        ),
    )
    parser.add_argument(
        "--default",
        action="store_true",
        help=(
            "Use the built-in default vocabulary (via "
            "`maki ai export-vocabulary --default`). "
            "Ignored when VOCAB is set."
        ),
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="Emit commands with --apply so they execute when run, "
        "rather than dry-run commands.",
    )
    args = parser.parse_args()

    if args.vocab and args.default:
        print(
            "warning: --default is ignored when an explicit VOCAB file is given.",
            file=sys.stderr,
        )

    try:
        vocab = load_vocab(args.vocab, args.default)
    except ValueError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    if not isinstance(vocab, dict):
        print(
            "error: vocabulary did not parse to a mapping "
            "(top level must be label → tag(s)).",
            file=sys.stderr,
        )
        return 2

    cmds = emit_commands(vocab, args.apply)
    if not cmds:
        print(
            "# No rename or split commands needed — every vocabulary "
            "entry is identity or null.",
            file=sys.stderr,
        )
        return 0

    header_mode = "apply" if args.apply else "dry-run"
    print(f"# Generated by apply-vocabulary.py ({header_mode} mode)")
    print(f"# {len(cmds)} command(s). Pipe to `sh` to execute.")
    if not args.apply:
        print(
            "# These commands run in dry-run mode (no --apply). "
            "Re-run with --apply to emit live commands."
        )
    for c in cmds:
        print(c)
    return 0


if __name__ == "__main__":
    sys.exit(main())
