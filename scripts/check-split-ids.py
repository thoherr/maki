#!/usr/bin/env python3
"""
Check for assets with mismatched IDs from pre-fix maki split.

Before v4.3.7, split used the wrong UUID namespace (NAMESPACE_URL instead
of DAM_NAMESPACE), creating asset IDs that don't match what import would
produce. This script scans sidecar YAML files directly (fast, no maki
subprocess per asset) and reports mismatches.

Usage:
    python3 scripts/check-split-ids.py                    # scan and report
    python3 scripts/check-split-ids.py --fix              # fix mismatched IDs
    python3 scripts/check-split-ids.py --catalog /path    # specify catalog root

The fix renames the sidecar file and updates the asset ID inside it.
After fixing, run 'maki rebuild-catalog' to regenerate SQLite.
"""

import argparse
import os
import sqlite3
import subprocess
import re
import sys
import uuid

# The MAKI namespace UUID (must match DAM_NAMESPACE in src/models/asset.rs)
DAM_NAMESPACE = uuid.UUID(bytes=bytes([
    0x8a, 0x3b, 0x7e, 0x01, 0x4f, 0xd2, 0x4a, 0x6b,
    0x9c, 0x1d, 0xe7, 0x5a, 0x0b, 0xf3, 0x28, 0x4c,
]))

# The wrong namespace that was used by split before the fix
NAMESPACE_URL = uuid.NAMESPACE_URL


def expected_id(content_hash):
    """Compute the correct asset ID for a given content hash."""
    return uuid.uuid5(DAM_NAMESPACE, content_hash)


def find_catalog_root():
    """Walk up from cwd looking for maki.toml."""
    path = os.getcwd()
    while path != "/":
        if os.path.exists(os.path.join(path, "maki.toml")):
            return path
        path = os.path.dirname(path)
    return None


def scan_sidecars(metadata_dir):
    """Scan all sidecar YAML files and check asset ID consistency."""
    mismatches = []
    checked = 0
    skipped = 0

    for shard in sorted(os.listdir(metadata_dir)):
        shard_path = os.path.join(metadata_dir, shard)
        if not os.path.isdir(shard_path):
            continue
        for filename in sorted(os.listdir(shard_path)):
            if not filename.endswith(".yaml"):
                continue
            filepath = os.path.join(shard_path, filename)
            try:
                with open(filepath, "r") as f:
                    content = f.read()
            except Exception:
                skipped += 1
                continue

            checked += 1

            # Extract asset ID from filename (uuid.yaml)
            file_id = filename[:-5]  # strip .yaml

            # Extract first variant's content_hash from YAML
            # Look for content_hash under variants:
            match = re.search(r"variants:\s*\n-\s*content_hash:\s*(\S+)", content)
            if not match:
                skipped += 1
                continue

            first_hash = match.group(1)
            correct_id = str(expected_id(first_hash))

            if file_id != correct_id:
                # Check if it matches the wrong namespace (confirming it's a split issue)
                wrong_id = str(uuid.uuid5(NAMESPACE_URL, first_hash.encode() if isinstance(first_hash, str) else first_hash))

                mismatches.append({
                    "file": filepath,
                    "current_id": file_id,
                    "correct_id": correct_id,
                    "first_hash": first_hash,
                    "is_split_bug": file_id == wrong_id,
                    "shard": shard,
                })

            if checked % 10000 == 0:
                print(f"  Scanned {checked} assets...", file=sys.stderr)

    return checked, skipped, mismatches


def fix_mismatch(entry):
    """Fix a mismatched asset by renaming the sidecar and updating the ID inside."""
    old_path = entry["file"]
    old_id = entry["current_id"]
    new_id = entry["correct_id"]
    new_shard = new_id[:2]

    # Read content
    with open(old_path, "r") as f:
        content = f.read()

    # Replace the asset ID in the YAML content
    new_content = content.replace(f"id: {old_id}", f"id: {new_id}")

    # Also update asset_id references in variants
    new_content = new_content.replace(f"asset_id: {old_id}", f"asset_id: {new_id}")

    # Determine new file path
    metadata_dir = os.path.dirname(os.path.dirname(old_path))
    new_shard_dir = os.path.join(metadata_dir, new_shard)
    os.makedirs(new_shard_dir, exist_ok=True)
    new_path = os.path.join(new_shard_dir, f"{new_id}.yaml")

    # Write new file
    with open(new_path, "w") as f:
        f.write(new_content)

    # Remove old file
    if new_path != old_path:
        os.remove(old_path)

    return new_path


def main():
    parser = argparse.ArgumentParser(description="Check/fix asset IDs from pre-fix split")
    parser.add_argument("--catalog", help="Catalog root (default: auto-detect)")
    parser.add_argument("--fix", action="store_true", help="Fix mismatched IDs")
    parser.add_argument("--only-split-bugs", action="store_true",
                        help="Only fix split-bug mismatches (skip other causes)")
    args = parser.parse_args()

    catalog_root = args.catalog or find_catalog_root()
    if not catalog_root:
        print("Error: no maki catalog found", file=sys.stderr)
        sys.exit(1)

    metadata_dir = os.path.join(catalog_root, "metadata")
    if not os.path.isdir(metadata_dir):
        print(f"Error: no metadata directory in {catalog_root}", file=sys.stderr)
        sys.exit(1)

    print(f"Scanning {metadata_dir}...")
    checked, skipped, mismatches = scan_sidecars(metadata_dir)

    print(f"\nScanned {checked} assets, skipped {skipped}")

    if not mismatches:
        print("No mismatched IDs found. All assets are consistent.")
        return

    split_bugs = [m for m in mismatches if m["is_split_bug"]]
    other = [m for m in mismatches if not m["is_split_bug"]]

    print(f"\nFound {len(mismatches)} mismatch(es):")
    if split_bugs:
        print(f"  {len(split_bugs)} from split bug (wrong namespace)")
    if other:
        print(f"  {len(other)} from other causes")

    for m in mismatches[:20]:  # show first 20
        tag = " [split bug]" if m["is_split_bug"] else ""
        print(f"  {m['current_id'][:8]} → {m['correct_id'][:8]}  hash={m['first_hash'][:30]}{tag}")
    if len(mismatches) > 20:
        print(f"  ... and {len(mismatches) - 20} more")

    if not args.fix:
        print(f"\nDry run. Run with --fix to rename sidecar files and update SQLite.")
        return

    to_fix = split_bugs if args.only_split_bugs else mismatches
    print(f"\nFixing {len(to_fix)} mismatched ID(s)...")

    # Phase 1: Fix sidecar YAML files
    fixed_entries = []
    for m in to_fix:
        try:
            new_path = fix_mismatch(m)
            fixed_entries.append(m)
            if len(fixed_entries) <= 10 or len(fixed_entries) % 100 == 0:
                print(f"  Fixed sidecar {m['current_id'][:8]} → {m['correct_id'][:8]}")
        except Exception as e:
            print(f"  ERROR fixing {m['current_id'][:8]}: {e}", file=sys.stderr)

    if not fixed_entries:
        print("No fixes applied.")
        return

    # Phase 2: Update SQLite per-asset (no full rebuild needed)
    #
    # Strategy for each asset:
    # 1. Rename old asset row to new ID directly in SQLite
    # 2. Update variant asset_id references
    # 3. Call refresh --reimport to fully resync from sidecar
    db_path = os.path.join(catalog_root, "catalog.db")
    if not os.path.exists(db_path):
        print(f"\nNo catalog.db found — run: maki rebuild-catalog")
        return

    print(f"\nUpdating SQLite for {len(fixed_entries)} asset(s)...")
    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA foreign_keys = OFF")

    updated = 0
    skipped_conflict = 0
    for m in fixed_entries:
        old_id = m["current_id"]
        new_id = m["correct_id"]
        try:
            # Check if the target ID already exists as a different asset
            row = conn.execute("SELECT id FROM assets WHERE id = ?", (new_id,)).fetchone()
            if row:
                # Target ID exists — the split-off asset should be merged back.
                # Delete the wrong-ID asset; its variants will be re-associated
                # by reimport after we fix the sidecar.
                print(f"  {old_id[:8]} → {new_id[:8]}: target exists, deleting old asset")
                # Delete dependents of old asset
                old_variants = [r[0] for r in conn.execute(
                    "SELECT content_hash FROM variants WHERE asset_id = ?", (old_id,)).fetchall()]
                for vh in old_variants:
                    conn.execute("DELETE FROM recipes WHERE variant_hash = ?", (vh,))
                    conn.execute("DELETE FROM file_locations WHERE content_hash = ?", (vh,))
                conn.execute("DELETE FROM variants WHERE asset_id = ?", (old_id,))
                conn.execute("DELETE FROM embeddings WHERE asset_id = ?", (old_id,))
                conn.execute("DELETE FROM faces WHERE asset_id = ?", (old_id,))
                conn.execute("DELETE FROM collection_assets WHERE asset_id = ?", (old_id,))
                conn.execute("DELETE FROM assets WHERE id = ?", (old_id,))
                updated += 1
                continue

            # No conflict — rename the asset ID across all tables
            # Delete old embeddings first (has unique constraint on asset_id+model)
            conn.execute("DELETE FROM embeddings WHERE asset_id = ?", (old_id,))
            conn.execute("UPDATE assets SET id = ? WHERE id = ?", (new_id, old_id))
            conn.execute("UPDATE variants SET asset_id = ? WHERE asset_id = ?", (new_id, old_id))
            conn.execute("UPDATE faces SET asset_id = ? WHERE asset_id = ?", (new_id, old_id))
            conn.execute("UPDATE collection_assets SET asset_id = ? WHERE asset_id = ?", (new_id, old_id))
            updated += 1
        except Exception as e:
            print(f"  ERROR updating {old_id[:8]}: {e}", file=sys.stderr)

    conn.commit()
    conn.execute("PRAGMA foreign_keys = ON")
    conn.close()
    print(f"  Updated {updated} asset ID(s) in SQLite")
    if skipped_conflict > 0:
        print(f"  Skipped {skipped_conflict} with target ID conflicts")

    # Phase 3: Reimport to fully resync each asset from sidecar
    print(f"\nReimporting {updated} asset(s) from sidecars...")
    reimported = 0
    for m in fixed_entries:
        new_id = m["correct_id"]
        result = subprocess.run(
            ["maki", "refresh", "--reimport", "--asset", new_id],
            capture_output=True, text=True,
        )
        if result.returncode == 0:
            reimported += 1
        else:
            print(f"  WARNING: reimport {new_id[:8]} failed: {result.stderr.strip()}")

    print(f"\nDone. {reimported}/{updated} asset(s) fully resynced. No rebuild needed.")


if __name__ == "__main__":
    main()
