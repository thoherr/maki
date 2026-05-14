#!/usr/bin/env bash
set -euo pipefail

# Build the MAKI Tagging Quick Guide poster as a 1-page A3 landscape PDF.
# Prerequisites: xelatex (e.g. via MacTeX or texlive-xetex)
#
# Usage:
#   bash doc/quickref/build-tagging-pdf.sh   # from repo root
#   bash build-tagging-pdf.sh                # from doc/quickref/

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml")

cd "$SCRIPT_DIR"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
sed "s/__VERSION__/$VERSION/g" tagging.tex > "$TMP/tagging.tex"

# Add doc/images/ to xelatex's image search path so all referenced
# assets (header icon, wordmark, etc.) resolve without per-file copies.
export TEXINPUTS="$REPO_ROOT/doc/images:$TMP:"

echo "Building Tagging Quick Guide PDF (v$VERSION)..."
( cd "$TMP" && xelatex -interaction=nonstopmode tagging.tex ) > /dev/null
cp "$TMP/tagging.pdf" "$SCRIPT_DIR/tagging.pdf"
echo "PDF generated: $SCRIPT_DIR/tagging.pdf"
