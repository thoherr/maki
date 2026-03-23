#!/usr/bin/env bash
set -euo pipefail

# Build the MAKI Search Filter Reference card as a 2-page A4 PDF.
# Prerequisites: xelatex (e.g. via MacTeX or texlive-xetex)
#
# Usage:
#   bash doc/manual/build-search-filters-pdf.sh    # from repo root
#   bash build-search-filters-pdf.sh               # from doc/manual/

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

cd "$SCRIPT_DIR"
echo "Building Search Filter Reference PDF..."
xelatex -interaction=nonstopmode search-filters.tex > /dev/null 2>&1
echo "PDF generated: $SCRIPT_DIR/search-filters.pdf"
