#!/usr/bin/env python3
"""Download a deduplicated, stratified sample of erickfm/slippi-public-dataset-v3.7.

The full dataset is ~174,000 file entries / ~523 GB, but only ~95,000 of those
entries are distinct games: every replay is filed under *both* players'
characters, so a Fox-vs-Falco match exists at `FOX/<name>.slp` and
`FALCO/<name>.slp`. Those two copies are byte-identical (verified: same LFS
sha256), which is what makes this script cheap — the Hub's tree API hands back
a sha256 for every file, so duplicates can be collapsed *before* anything is
downloaded.

Three phases, each cached on disk so you can re-run the later ones freely:

  index   list the whole repo tree      -> tree.jsonl   (~174k rows, no payload)
  select  dedup + stratify + sample     -> selection.tsv
  fetch   download just those files     -> <out>/<CHARACTER>/<name>.slp

The output layout is `root/<subdir>/*.slp`, which is exactly what
`parse_new_replays` walks, so `<out>` can be handed straight to stats-melee.

    python3 scripts/hf_slippi_sample.py --count 5000 --out ~/melee-corpus

Needs `pip install "huggingface_hub[hf_xet]"`. No auth: the dataset is CC0.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import random
import re
import sys
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

REPO = "erickfm/slippi-public-dataset-v3.7"

# Two filename conventions coexist in this dataset. Both name exactly two
# players, which is the only thing we read them for:
#   "10_23_35 Bowser + Peach (BF).slp"
#   "20200101 - HNC 4 - PM 0737 - Marth (Default) vs Bowser (Red) - Battlefield.slp"
#   "Peach vs Falcon [FoD] Game_001DBC7CDA54_20200307T172646.slp"
# But 43,226 of the 94,514 unique games (46%) are plain default-named Slippi
# files — "Game_20190505T061220.slp" — which name no players at all. They are
# ordinary 1v1 replays; the name simply carries no information. Filtering them
# out would drop nearly half the corpus and skew it by era (most are 2019),
# which is why --require-known-names is opt-in rather than the default.
NAME_PATTERNS = (
    re.compile(r"^\d\d_\d\d_\d\d .+ \+ .+ \(.+\)\.slp$"),
    re.compile(r"^\d{8} - .+ - (?:AM|PM) \d{4} - .+ vs .+ - .+\.slp$"),
    re.compile(r"^.+ vs .+ \[.+\] Game_.+\.slp$"),
)


def looks_like_singles(filename: str) -> bool:
    return any(p.match(filename) for p in NAME_PATTERNS)


# ---------------------------------------------------------------- index -----

def phase_index(cache: Path) -> Path:
    """Walk the repo tree once and record (path, size, sha256) per file."""
    out = cache / "tree.jsonl"
    if out.exists():
        print(f"index: reusing {out} ({sum(1 for _ in out.open()):,} rows)")
        return out

    from huggingface_hub import HfApi

    print(f"index: listing {REPO} (~174k files, takes a few minutes)...")
    api = HfApi()
    n = 0
    tmp = out.with_suffix(".partial")
    with tmp.open("w") as fh:
        for item in api.list_repo_tree(REPO, repo_type="dataset", recursive=True):
            # Directories come back too; only blobs carry a size.
            path = getattr(item, "path", None)
            if path is None or not path.endswith(".slp"):
                continue
            lfs = getattr(item, "lfs", None)
            fh.write(json.dumps({
                "path": path,
                "size": getattr(item, "size", 0) or 0,
                # Non-LFS files would have no sha256; fall back to the path so
                # they stay distinct rather than collapsing into one bucket.
                "sha256": getattr(lfs, "sha256", None) if lfs else None,
            }) + "\n")
            n += 1
            if n % 20000 == 0:
                print(f"  {n:,} files...")
    tmp.rename(out)
    print(f"index: wrote {n:,} file entries to {out}")
    return out


# --------------------------------------------------------------- select -----

def phase_select(cache: Path, tree: Path, count: int, cap_frac: float,
                 seed: int, require_singles: bool, max_gb: float) -> Path:
    """Collapse duplicates, then sample across characters."""
    out = cache / "selection.tsv"

    rows = [json.loads(line) for line in tree.open()]
    print(f"select: {len(rows):,} file entries")

    # Collapse the two-copies-per-game duplication. Keep the copy under the
    # alphabetically-first character folder so runs are reproducible.
    by_game: dict[str, dict] = {}
    for r in rows:
        key = r["sha256"] or r["path"]
        prev = by_game.get(key)
        if prev is None or r["path"] < prev["path"]:
            by_game[key] = r
    games = list(by_game.values())
    print(f"select: {len(games):,} unique games after sha256 dedup "
          f"({len(rows) - len(games):,} duplicate copies dropped)")

    unparsed = sum(1 for g in games if not looks_like_singles(Path(g["path"]).name))
    print(f"select: {unparsed:,} games are default-named / unrecognized "
          f"({'excluded' if require_singles else 'kept'})")
    if require_singles:
        games = [g for g in games if looks_like_singles(Path(g["path"]).name)]

    # Group by the character folder the kept copy lives under. This is
    # authoritative (the folder *is* the label), unlike parsing the filename.
    by_char: dict[str, list[dict]] = collections.defaultdict(list)
    for g in games:
        by_char[g["path"].split("/", 1)[0]].append(g)

    rng = random.Random(seed)
    for bucket in by_char.values():
        bucket.sort(key=lambda g: g["path"])
        rng.shuffle(bucket)

    # Proportional sampling would return an almost pure Fox/Falco/Marth
    # corpus (Fox alone is ~45k of 174k entries). Capping each character at
    # `cap_frac` of the target keeps the long tail — Bowser, Pichu, Kirby,
    # Ice Climbers — represented, which is what actually exercises unusual
    # parser paths.
    cap = max(1, int(count * cap_frac))
    picked: list[dict] = []
    round_no = 0
    while len(picked) < count:
        added = 0
        for char in sorted(by_char):
            bucket = by_char[char]
            taken = sum(1 for g in picked if g["path"].startswith(char + "/"))
            if round_no < len(bucket) and taken < cap:
                picked.append(bucket[round_no])
                added += 1
                if len(picked) >= count:
                    break
        if added == 0:
            print(f"select: exhausted the corpus at {len(picked):,} games")
            break
        round_no += 1

    # Hard ceiling: trim the sample until it fits, whatever --count asked for.
    budget = int(max_gb * 1e9)
    if sum(g["size"] for g in picked) > budget:
        kept, running = [], 0
        for g in picked:
            if running + g["size"] > budget:
                continue
            kept.append(g)
            running += g["size"]
        print(f"select: --max-gb {max_gb} trimmed {len(picked):,} -> {len(kept):,} games")
        picked = kept

    total_bytes = sum(g["size"] for g in picked)
    dist = collections.Counter(g["path"].split("/", 1)[0] for g in picked)
    print(f"select: {len(picked):,} games, {total_bytes / 1e9:.1f} GB")
    print("select: top characters " + ", ".join(
        f"{c}={n}" for c, n in dist.most_common(6)))

    with out.open("w") as fh:
        for g in picked:
            fh.write(f"{g['path']}\t{g['sha256'] or ''}\t{g['size']}\n")
    print(f"select: wrote {out}")
    return out


# ---------------------------------------------------------------- fetch -----

def phase_fetch(selection: Path, out_dir: Path, workers: int, dry_run: bool,
                max_gb: float) -> None:
    entries = [line.rstrip("\n").split("\t") for line in selection.open()]
    total_bytes = sum(int(e[2]) for e in entries)
    if total_bytes > max_gb * 1e9:
        sys.exit(f"fetch: selection is {total_bytes / 1e9:.1f} GB, over the "
                 f"--max-gb {max_gb} ceiling; re-run select with a lower --count")
    print(f"fetch: {len(entries):,} files, {total_bytes / 1e9:.1f} GB -> {out_dir}")

    free = os.statvfs(out_dir.parent if not out_dir.exists() else out_dir)
    free_bytes = free.f_bavail * free.f_frsize
    if free_bytes < total_bytes * 1.1:
        sys.exit(f"fetch: need ~{total_bytes / 1e9:.1f} GB but only "
                 f"{free_bytes / 1e9:.1f} GB free")
    if dry_run:
        print("fetch: --dry-run, stopping here")
        return

    from huggingface_hub import hf_hub_download

    # Filenames are only a timestamp + matchup + stage, so two genuinely
    # different games from different events can share one. Suffix on collision
    # rather than silently overwriting.
    seen: set[str] = set()
    plan: list[tuple[str, Path]] = []
    for path, sha, _size in entries:
        char, name = path.split("/", 1)
        name = Path(name).name
        dest_dir = out_dir / char
        stem, ext = os.path.splitext(name)
        candidate = name
        if str(dest_dir / candidate) in seen:
            candidate = f"{stem}_{(sha or '')[:8]}{ext}"
        seen.add(str(dest_dir / candidate))
        plan.append((path, dest_dir / candidate))

    out_dir.mkdir(parents=True, exist_ok=True)
    for char in {p.parent for _, p in plan}:
        char.mkdir(parents=True, exist_ok=True)

    done = failed = 0

    def grab(repo_path: str, dest: Path) -> None:
        if dest.exists():
            return
        tmp = hf_hub_download(REPO, repo_path, repo_type="dataset")
        # hf_hub_download returns a cache path; hard-link into place so the
        # corpus costs one copy on disk, not two.
        try:
            os.link(tmp, dest)
        except OSError:
            import shutil
            shutil.copy2(tmp, dest)

    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = {pool.submit(grab, rp, d): rp for rp, d in plan}
        for fut in as_completed(futures):
            try:
                fut.result()
                done += 1
            except Exception as exc:  # noqa: BLE001 - report and keep going
                failed += 1
                print(f"  failed {futures[fut]}: {exc}", file=sys.stderr)
            if (done + failed) % 250 == 0:
                print(f"  {done + failed:,}/{len(plan):,}")

    print(f"fetch: {done:,} downloaded, {failed:,} failed -> {out_dir}")
    print("fetch: the Hub cache under ~/.cache/huggingface still holds a copy; "
          "`hf cache delete` (or rm -rf) reclaims it once you're happy.")


# ----------------------------------------------------------------- main -----

def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, required=True, help="corpus destination")
    ap.add_argument("--count", type=int, default=5000, help="unique games to fetch")
    ap.add_argument("--cache", type=Path, default=Path(".hf-slippi-cache"),
                    help="where tree.jsonl / selection.tsv live")
    ap.add_argument("--cap-frac", type=float, default=0.12,
                    help="max share of the sample any one character may take")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--workers", type=int, default=12)
    ap.add_argument("--max-gb", type=float, default=20.0,
                    help="hard ceiling on total download size (default 20 GB)")
    ap.add_argument("--require-known-names", action="store_true",
                    help="keep only files whose name names both players; drops "
                         "~46%% of the corpus (default-named Game_*.slp files)")
    ap.add_argument("--dry-run", action="store_true",
                    help="index + select, report the download size, stop")
    args = ap.parse_args()

    args.out = args.out.expanduser()
    args.cache = args.cache.expanduser()
    args.cache.mkdir(parents=True, exist_ok=True)

    tree = phase_index(args.cache)
    selection = phase_select(args.cache, tree, args.count, args.cap_frac,
                             args.seed, args.require_known_names, args.max_gb)
    phase_fetch(selection, args.out, args.workers, args.dry_run, args.max_gb)


if __name__ == "__main__":
    main()
