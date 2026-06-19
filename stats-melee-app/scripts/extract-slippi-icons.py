#!/usr/bin/env python3
"""Extract Melee character + stage icons from a local Slippi Launcher install.

The art is Nintendo IP shipped *inside* the Slippi Launcher's Electron bundle.
This script only copies files you already have on disk from your own Slippi
install into ``stats-melee-app/assets/`` (gitignored by default), where the app
picks them up automatically. Nothing is downloaded and nothing leaves your
machine.

Pure standard-library Python 3.8+ — no Node, no ``npx @electron/asar``, no pip
installs. It reads the Launcher's ``app.asar`` archive directly.

Usage
-----
    python3 stats-melee-app/scripts/extract-slippi-icons.py

Options
    --asar PATH   Path to the Launcher's app.asar (auto-detected if omitted).
    --out  DIR    Output assets dir (defaults to ../assets next to this script).
    SLIPPI_ASAR   Env var, same as --asar.

How it works
------------
Slippi's renderer bundle holds a webpack ``require.context`` mapping
``./<extId>/<color>/stock.png`` (characters, keyed by EXTERNAL character id) and
``./<stageId>.png`` (stages) to numeric module ids, plus asset modules of the
form ``<id>(n,r,e){"use strict";n.exports=e.p+"<hash>.png"}``. We resolve
keys -> module ids -> hashed filenames, then pull those PNG entries straight out
of the asar and write them under the names the app expects (the CHARACTERS /
STAGES tables in ``stats-melee/src/gamedata.rs``).

Our character_id is the INTERNAL id (from .slp metadata), so we map
internal -> external before indexing Slippi's stock folders.
"""

import argparse
import json
import os
import re
import struct
import sys
from pathlib import Path

# internal character id (our CHARACTERS table) -> external id (Slippi folders)
INT_TO_EXT = [8, 2, 0, 1, 4, 5, 6, 19, 11, 12, 14, 14, 13, 16, 17,
              15, 10, 7, 9, 18, 21, 22, 20, 24, 3, 25, 23]
CHAR_NAMES = ["Mario", "Fox", "CaptainFalcon", "DonkeyKong", "Kirby", "Bowser",
              "Link", "Sheik", "Ness", "Peach", "Popo", "Nana", "Pikachu",
              "Samus", "Yoshi", "Jigglypuff", "Mewtwo", "Luigi", "Marth",
              "Zelda", "YoungLink", "DrMario", "Falco", "Pichu", "GameAndWatch",
              "Ganondorf", "Roy"]
STAGE_NAMES = ["Dummy", "Test", "FountainOfDreams", "PokemonStadium",
               "PrincessPeachsCastle", "KongoJungle", "Brinstar", "Corneria",
               "YoshisStory", "Onett", "MuteCity", "RainbowCruise",
               "JungleJapes", "GreatBay", "HyruleTemple", "BrinstarDepths",
               "YoshisIsland", "GreenGreens", "Fourside", "MushroomKingdomI",
               "MushroomKingdomII", "Akaneia", "Venom", "PokeFloats", "BigBlue",
               "IcicleMountain", "Icetop", "FlatZone", "DreamLandN64",
               "YoshisIslandN64", "KongoJungleN64", "Battlefield",
               "FinalDestination"]

# Asset module:  <id>(n,r,e){"use strict";n.exports=e.p+"<hash>.png"}
MOD_TO_FILE_RE = re.compile(
    r'(\d{2,7})\(\w,\w,(\w)\)\{"use strict";\w\.exports=\2\.p\+"([0-9a-f]+\.png)"\}'
)
# require.context entries: "./<extId>/<color>/stock.png":<moduleId>
CHAR_CTX_RE = re.compile(r'"\./(\d+)/(\d+)/stock\.png":(\d+)')
# require.context entries: "./<stageId>.png":<moduleId>
STAGE_CTX_RE = re.compile(r'"\./(\d+)\.png":(\d+)')


class Asar:
    """Minimal read-only reader for Electron's asar archive format."""

    def __init__(self, path: Path):
        self.path = path
        with open(path, "rb") as f:
            # 4x uint32 LE preamble: [4, header_size, header_obj_size, json_size]
            _, header_size, _, json_size = struct.unpack("<4I", f.read(16))
            header = f.read(json_size).decode("utf-8", "replace")
        self.tree = json.loads(header)
        # File payloads begin right after the 8-byte size prefix + header pickle.
        self.data_offset = 8 + header_size

    def _node(self, parts):
        node = self.tree
        for p in parts:
            node = node["files"][p]
        return node

    def listdir(self, *parts):
        """Names directly under the given directory path components."""
        return list(self._node(parts).get("files", {}).keys())

    def read(self, *parts) -> bytes:
        """Raw bytes of the file at the given path components."""
        node = self._node(parts)
        off = self.data_offset + int(node["offset"])
        with open(self.path, "rb") as f:
            f.seek(off)
            return f.read(int(node["size"]))


def default_asar_candidates():
    """Best-effort per-OS locations of the Slippi Launcher's app.asar."""
    cands = []
    if sys.platform == "darwin":
        base = Path("/Applications/Slippi Launcher.app/Contents/Resources")
        cands += [base / "app.asar", base / "app-arm64.asar", base / "app-x64.asar"]
    elif sys.platform == "win32":
        local = os.environ.get("LOCALAPPDATA", "")
        if local:
            for d in ("slippi-launcher", "Slippi Launcher"):
                cands.append(Path(local) / "Programs" / d / "resources" / "app.asar")
    else:  # linux / other
        home = Path.home()
        # If the user extracted the AppImage (or installed a .deb), the asar
        # lands under a resources/ dir. These are guesses; pass --asar if wrong.
        cands += [
            home / ".local/share/Slippi Launcher/resources/app.asar",
            Path("/opt/Slippi Launcher/resources/app.asar"),
        ]
    return cands


def find_asar(explicit):
    if explicit:
        p = Path(explicit).expanduser()
        if not p.is_file():
            sys.exit(f"asar not found: {p}")
        return p
    for c in default_asar_candidates():
        if c.is_file():
            return c
    hint = "  " + "\n  ".join(str(c) for c in default_asar_candidates())
    sys.exit(
        "Couldn't find the Slippi Launcher's app.asar automatically. Looked in:\n"
        f"{hint}\n"
        "Pass --asar /path/to/app.asar (or set SLIPPI_ASAR)."
    )


def main():
    ap = argparse.ArgumentParser(description="Rip Melee icons from a local Slippi install.")
    ap.add_argument("--asar", default=os.environ.get("SLIPPI_ASAR"),
                    help="Path to the Slippi Launcher app.asar (auto-detected if omitted).")
    ap.add_argument("--out", default=str(Path(__file__).resolve().parent.parent / "assets"),
                    help="Output assets dir (default: ../assets next to this script).")
    args = ap.parse_args()

    asar = find_asar(args.asar)
    print(f"reading {asar}")
    arc = Asar(asar)

    try:
        renderer_files = arc.listdir("dist", "renderer")
    except KeyError:
        sys.exit("unexpected asar layout: no dist/renderer (Slippi version mismatch?)")

    # Accumulate the webpack maps across every JS chunk — the context map and
    # the asset modules aren't guaranteed to live in the same chunk, and the
    # chunk filename changes between Slippi releases.
    mod_to_file, char_ctx, stage_ctx = {}, {}, {}
    for name in renderer_files:
        if not name.endswith(".js"):
            continue
        src = arc.read("dist", "renderer", name).decode("utf-8", "replace")
        for mod, _bref, fname in MOD_TO_FILE_RE.findall(src):
            mod_to_file[mod] = fname
        for ext_id, color, mod in CHAR_CTX_RE.findall(src):
            char_ctx[f"{ext_id}/{color}"] = mod
        for stage_id, mod in STAGE_CTX_RE.findall(src):
            stage_ctx[stage_id] = mod

    if not char_ctx and not stage_ctx:
        sys.exit("found no stock-icon context in the renderer bundle "
                 "(Slippi changed its asset layout — script needs updating).")

    out = Path(args.out)
    (out / "characters").mkdir(parents=True, exist_ok=True)
    (out / "stages").mkdir(parents=True, exist_ok=True)

    def emit(module_id, dest: Path, label) -> bool:
        fname = mod_to_file.get(module_id)
        if not fname:
            print(f"  ! no file for module {module_id} ({label})")
            return False
        try:
            data = arc.read("dist", "renderer", fname)
        except KeyError:
            print(f"  ! missing asar entry {fname} ({label})")
            return False
        dest.write_bytes(data)
        return True

    n_char = 0
    for internal, name in enumerate(CHAR_NAMES):
        mod = char_ctx.get(f"{INT_TO_EXT[internal]}/0")  # color 0 = neutral costume
        if mod and emit(mod, out / "characters" / f"{name}.png", name):
            n_char += 1

    n_stage = 0
    for stage_id, mod in stage_ctx.items():
        idx = int(stage_id)
        if idx >= len(STAGE_NAMES):
            continue
        name = STAGE_NAMES[idx]
        if name in ("Dummy", "Test"):
            continue
        if emit(mod, out / "stages" / f"{name}.png", name):
            n_stage += 1

    print(f"copied {n_char} character icons, {n_stage} stage icons -> {out}")


if __name__ == "__main__":
    main()
