#!/usr/bin/env bash
set -euo pipefail

# Build the MAKI Search Filter Reference card as a 2-page A4 PDF.
# Prerequisites: xelatex (e.g. via MacTeX or texlive-xetex)
#
# Usage:
#   bash doc/quickref/build-search-filters-pdf.sh   # from repo root
#   bash build-search-filters-pdf.sh                # from doc/quickref/

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Version is extracted from Cargo.toml so the quickref header always
# matches the binary the user is running. The committed .tex file
# carries `MAKI v__VERSION__` as a placeholder; we substitute into a
# temp copy before invoking xelatex.
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml")

cd "$SCRIPT_DIR"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
sed "s/__VERSION__/$VERSION/g" search-filters.tex > "$TMP/search-filters.tex"

# Add doc/images/ to xelatex's image search path so all referenced
# assets (header icon, wordmark, etc.) resolve without per-file copies.
export TEXINPUTS="$REPO_ROOT/doc/images:$TMP:"

echo "Building Search Filter Reference PDF (v$VERSION)..."
( cd "$TMP" && xelatex -interaction=nonstopmode search-filters.tex ) > /dev/null
cp "$TMP/search-filters.pdf" "$SCRIPT_DIR/search-filters.pdf"
echo "PDF generated: $SCRIPT_DIR/search-filters.pdf"
