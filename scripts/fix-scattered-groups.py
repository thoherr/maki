#!/usr/bin/env python3
"""
Fix accidentally over-grouped assets by splitting them based on directory structure.

Finds assets with scattered variants (files in different directories) and splits
them so each directory-group becomes its own asset. After splitting, reimports
metadata and re-groups by filename stem within each directory.

Usage:
    # Preview what would be split (dry run)
    python3 scripts/fix-scattered-groups.py --min-scattered 4

    # Preview with day-level grouping (default is month-level)
    python3 scripts/fix-scattered-groups.py --min-scattered 4 --depth 3

    # Apply the fixes
    python3 scripts/fix-scattered-groups.py --min-scattered 4 --apply

    # Process a specific asset
    python3 scripts/fix-scattered-groups.py --asset a1b2c3d4

    # Start with the worst offenders, review, then widen
    python3 scripts/fix-scattered-groups.py --min-scattered 10 --apply
    python3 scripts/fix-scattered-groups.py --min-scattered 4 --apply
"""

import argparse
import json
import os
import subprocess
import sys
from collections import defaultdict
from pathlib import PurePosixPath


def maki_json(*args):
    """Run a maki command with --json and return parsed output."""
    cmd = ["maki", "--json"] + list(args)
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        print(f"  ERROR: maki {' '.join(args)}: {result.stderr.strip()}", file=sys.stderr)
        return None
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        print(f"  ERROR: failed to parse JSON from: maki {' '.join(args)}", file=sys.stderr)
        return None


def maki_ids(*args):
    """Run a maki search with -q and return a list of asset IDs."""
    cmd = ["maki", "search", "-q"] + list(args)
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        return []
    return [line.strip() for line in result.stdout.strip().splitlines() if line.strip()]


def maki_run(*args):
    """Run a maki command and return (success, stdout, stderr)."""
    cmd = ["maki"] + list(args)
    result = subprocess.run(cmd, capture_output=True, text=True)
    return result.returncode == 0, result.stdout.strip(), result.stderr.strip()


def dir_at_depth(path, depth):
    """Extract directory prefix at a given depth from a relative path.

    depth=2: yyyy/yyyy-mm       (month level)
    depth=3: yyyy/yyyy-mm/yyyy-mm-dd  (day level)

    Example: Pictures/Masters/2019/2019-11/2019-11-30/Selects/file.RAF
    After stripping the volume-relative prefix, the year/month/day structure
    is detected by looking for YYYY patterns.
    """
    parts = PurePosixPath(path).parts[:-1]  # remove filename
    # Find the first YYYY-like component (4 digits starting with 19 or 20)
    year_idx = None
    for i, part in enumerate(parts):
        if len(part) == 4 and part.isdigit() and part[:2] in ("19", "20"):
            year_idx = i
            break

    if year_idx is not None and year_idx + depth <= len(parts):
        return "/".join(parts[:year_idx + depth])

    # Fallback: use first N directory components
    if len(parts) >= depth:
        return "/".join(parts[:depth])
    return "/".join(parts) if parts else "(root)"


def analyze_asset(asset_id, depth):
    """Analyze an asset and return variant groups by directory."""
    details = maki_json("show", asset_id)
    if not details or "variants" not in details:
        return None, None

    # Group variants by directory prefix
    groups = defaultdict(list)
    for variant in details["variants"]:
        content_hash = variant["content_hash"]
        for loc in variant.get("locations", []):
            path = loc.get("relative_path", "")
            dir_key = dir_at_depth(path, depth)
            groups[dir_key].append({
                "content_hash": content_hash,
                "filename": variant["original_filename"],
                "format": variant["format"],
                "path": path,
            })

    return details, dict(groups)


def main():
    parser = argparse.ArgumentParser(
        description="Fix accidentally over-grouped assets by splitting on directory structure"
    )
    parser.add_argument("--min-scattered", type=int, default=4,
                        help="Minimum scattered level to process (default: 4)")
    parser.add_argument("--depth", type=int, default=2,
                        help="Directory depth for grouping: 2=month, 3=day (default: 2)")
    parser.add_argument("--asset", type=str,
                        help="Process a specific asset ID instead of searching")
    parser.add_argument("--apply", action="store_true",
                        help="Actually perform splits (default: dry run)")
    parser.add_argument("--skip-reimport", action="store_true",
                        help="Skip metadata reimport after split")
    parser.add_argument("--skip-regroup", action="store_true",
                        help="Skip auto-group after split")
    parser.add_argument("--limit", type=int, default=0,
                        help="Process at most N assets (0 = unlimited)")
    args = parser.parse_args()

    # Find affected assets
    if args.asset:
        asset_ids = [args.asset]
    else:
        query = f"scattered:{args.min_scattered}+"
        print(f"Searching for assets with {query}...")
        asset_ids = maki_ids(query)
        print(f"Found {len(asset_ids)} asset(s)")

    if not asset_ids:
        print("No assets to process.")
        return

    if args.limit > 0:
        asset_ids = asset_ids[:args.limit]
        print(f"Processing first {args.limit} asset(s)")

    # Phase 1: Analyze
    print(f"\n{'=' * 60}")
    print(f"{'DRY RUN' if not args.apply else 'APPLYING'} — depth={args.depth} ({'month' if args.depth == 2 else 'day' if args.depth == 3 else f'{args.depth} levels'})")
    print(f"{'=' * 60}\n")

    total_splits = 0
    split_plan = []

    for i, asset_id in enumerate(asset_ids):
        short_id = asset_id[:8]
        details, groups = analyze_asset(asset_id, args.depth)
        if not details or not groups:
            print(f"  [{i+1}/{len(asset_ids)}] {short_id} — skipped (could not load)")
            continue

        name = details.get("name") or details["variants"][0]["original_filename"]
        total_variants = len(details["variants"])

        if len(groups) <= 1:
            # All variants in same directory — nothing to split
            continue

        print(f"  [{i+1}/{len(asset_ids)}] {short_id} ({name}) — {total_variants} variants in {len(groups)} directory groups:")
        for dir_key, variants in sorted(groups.items()):
            hashes = [v["content_hash"] for v in variants]
            files = [f"{v['filename']} ({v['format']})" for v in variants]
            print(f"    {dir_key}/")
            for f in files:
                print(f"      {f}")

        # Determine which group to keep with the original asset.
        # The asset ID is derived from a specific variant's hash (UUID v5).
        # We must keep the group containing that variant to avoid splitting
        # away the identity variant. Find it by checking which variant hash
        # was used to generate the asset UUID.
        #
        # Since we can't easily recompute UUID v5 in Python without the
        # namespace, we use a safe heuristic: keep the group containing
        # the FIRST variant listed (variants[0] is typically the original
        # that created the asset).
        first_hash = details["variants"][0]["content_hash"]
        keep_dir = None
        for dir_key, variants in groups.items():
            if any(v["content_hash"] == first_hash for v in variants):
                keep_dir = dir_key
                break
        if keep_dir is None:
            # Fallback: keep the largest group
            keep_dir = max(groups.items(), key=lambda x: len(x[1]))[0]

        keep_variants = groups[keep_dir]
        split_groups = [(d, v) for d, v in groups.items() if d != keep_dir]

        print(f"    → Keep {len(keep_variants)} variant(s) in {keep_dir}/ (contains identity variant)")
        for dir_key, variants in split_groups:
            hashes = list(set(v["content_hash"] for v in variants))
            print(f"    → Split {len(variants)} variant(s) from {dir_key}/")
            split_plan.append({
                "asset_id": asset_id,
                "split_hashes": hashes,
                "dir": dir_key,
            })
            total_splits += 1
        print()

    print(f"{'=' * 60}")
    print(f"Summary: {total_splits} split(s) across {len(asset_ids)} asset(s)")

    if not args.apply:
        print("Dry run — no changes made. Run with --apply to execute.")
        return

    if total_splits == 0:
        print("Nothing to split.")
        return

    # Phase 2: Execute splits
    print(f"\nExecuting {total_splits} split(s)...\n")

    new_asset_ids = []
    for entry in split_plan:
        asset_id = entry["asset_id"]
        hashes = entry["split_hashes"]
        short_id = asset_id[:8]

        # maki split <asset-id> <hash1> <hash2> ...
        result = maki_json("split", asset_id, *hashes)
        if result:
            new_ids = result.get("new_asset_ids", [])
            print(f"  Split {short_id}: {len(hashes)} variant(s) from {entry['dir']}/")
            for nid in new_ids:
                print(f"    New asset: {nid[:8]}")
                new_asset_ids.append(nid)
            new_asset_ids.append(asset_id)  # reimport the source too
        else:
            # Fallback: try without --json
            ok, stdout, stderr = maki_run("split", asset_id, *hashes)
            if ok:
                print(f"  Split {short_id}: {len(hashes)} variant(s) from {entry['dir']}/")
                new_asset_ids.append(asset_id)
            else:
                print(f"  FAILED split {short_id}: {stderr}")

    # Phase 3: Reimport metadata
    if not args.skip_reimport and new_asset_ids:
        print(f"\nReimporting metadata for {len(new_asset_ids)} affected asset(s)...")
        # Also reimport the newly created assets — find them by searching
        # for recently modified assets (the split creates new ones)
        for aid in set(new_asset_ids):
            ok, stdout, stderr = maki_run("refresh", "--reimport", "--asset", aid)
            if ok:
                print(f"  Reimported {aid[:8]}")
            else:
                print(f"  FAILED reimport {aid[:8]}: {stderr}")

    # Phase 4: Re-group by stem
    # TODO: THIS IS TOTALLY WRONG, SINCE IT REGROUPS THE WHOLE CATALOG SIMPLY BY STEM
    # We have to regroup by the found directory names that we splitted....
    #if not args.skip_regroup:
    #    print(f"\nRe-grouping by filename stem...")
    #    ok, stdout, stderr = maki_run("auto-group", "--apply")
    #    if ok:
    #        print(f"  {stdout}")
    #    else:
    #        print(f"  Auto-group: {stderr}")

    print("\nDone.")
    print("Review the results in the web UI and run 'maki generate-previews --upgrade' if needed.")


if __name__ == "__main__":
    main()
