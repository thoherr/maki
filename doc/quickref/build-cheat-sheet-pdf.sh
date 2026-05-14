#!/usr/bin/env bash
set -euo pipefail

# Build the MAKI Cheat Sheet as a 2-page A4 reference card.
# Prerequisites: xelatex (e.g. via MacTeX or texlive-xetex)
#
# Usage:
#   bash doc/quickref/build-cheat-sheet-pdf.sh   # from repo root
#   bash build-cheat-sheet-pdf.sh                # from doc/quickref/

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml")

cd "$SCRIPT_DIR"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
sed "s/__VERSION__/$VERSION/g" cheat-sheet.tex > "$TMP/cheat-sheet.tex"

# Add doc/images/ to xelatex's image search path so all referenced
# assets (header icon, wordmark, etc.) resolve without per-file copies.
export TEXINPUTS="$REPO_ROOT/doc/images:$TMP:"

echo "Building Cheat Sheet PDF (v$VERSION)..."
( cd "$TMP" && xelatex -interaction=nonstopmode cheat-sheet.tex ) > /dev/null
cp "$TMP/cheat-sheet.pdf" "$SCRIPT_DIR/cheat-sheet.pdf"
echo "PDF generated: $SCRIPT_DIR/cheat-sheet.pdf"
