# stats-melee

A desktop stats tracker for your **Super Smash Bros. Melee** Slippi replays.
Point it at your `.slp` folder and it ingests every game into a local
database, then gives you a searchable replay library, per-match breakdowns,
career analytics, and one-click playback in Slippi Dolphin.

100% native Rust ([egui](https://github.com/emilk/egui) UI, bundled SQLite) —
one binary, no web runtime, no telemetry. Your replays and stats never leave
your machine.

> **Not affiliated with Nintendo or Project Slippi.** This is an independent,
> fan-made tool. *Super Smash Bros. Melee* is a trademark of Nintendo. You must
> supply your own legally-obtained game disc image and your own replays.

## Features

- **Replay library** — every ingested game in a sortable table, with
  structured filters (your character, opponent character, stage, win/loss,
  played/added date, opponent tag).
- **Per-match view** — head-to-head comparison of both players plus a combat
  timeline (openings, neutral wins, edge-guards, damage/opening, and more).
- **Analytics** — win-rate breakdowns by character, opponent, and stage,
  filterable across every dimension.
- **Career** — lifetime totals (matches, record, playtime, stocks taken/lost),
  favorite character / stage / matchup, and advanced aggregates.
- **Watch in Slippi** — launches the replay straight into Slippi Dolphin
  playback.

## Requirements

- **macOS, Windows, or Linux.**
- Your own **Slippi `.slp` replays** (from Slippi netplay or local recording).
- **To watch replays:** [Slippi Dolphin](https://slippi.gg) installed. The app
  auto-detects the Slippi Launcher's `playback` build on all three OSes (you can
  also set the binary path manually in Settings). A Dolphin installed via the
  Slippi Launcher already knows your Melee ISO, so playback just works; the
  **Melee ISO** field in Settings is optional, only needed as an override or for
  a Dolphin with no default ISO. Browsing, ingesting, and all the stats need
  none of this.

## Install

Grab the download for your OS from the [Releases](../../releases) page. Each OS
has an easy "app" download and a portable archive (the executable, this README,
the license, and an `assets/` folder — see [Icons](#icons) below).

- **Windows** — run **`stats-melee-Setup-<version>.exe`**. It installs per-user
  (no admin) and creates Start Menu + Desktop shortcuts; uninstall from *Add or
  remove programs*. Prefer no installer? Use the `…-windows-msvc.zip` and run
  `stats-melee-app.exe` directly. Either way SmartScreen may warn on first run
  (the app is unsigned): **More info → Run anyway**.
- **macOS** — open the **`.dmg`** and drag **stats-melee** to Applications.
  Because the app is unsigned, the first launch needs a right-click → **Open**
  (or *System Settings → Privacy & Security → Open Anyway*) to get past
  Gatekeeper. A portable `…-apple-darwin.tar.gz` with the raw binary is also
  available. Pick `aarch64` for Apple Silicon, `x86_64` for Intel Macs.
- **Linux** — unpack the `…-linux-gnu.tar.gz`. Run `./stats-melee-app`
  directly, or run **`sh install.sh`** to add it to your application menu with
  its icon (installs under `~/.local`, no root; `sh uninstall.sh` removes it).
  Requires a desktop with the XDG desktop portal (for file dialogs), standard
  on modern distros.

## Build from source

Needs a recent stable Rust toolchain.

```sh
git clone <your-fork-url> stats-melee
cd stats-melee
cargo run --release -p stats-melee-app
```

On **Linux** the GUI stack needs a few system libraries at build time:

```sh
sudo apt-get install -y libxcb-render0-dev libxcb-shape0-dev \
    libxcb-xfixes0-dev libxkbcommon-dev libssl-dev
```

(No GTK is required — file dialogs use the XDG Desktop Portal backend.)

## Icons

**On first launch the app automatically rips character + stage icons from your
local Slippi Launcher install** into its data dir, so the library and viewer
show real Melee art. The art is Nintendo's, so **nothing ships in the
download** — you get it from the copy of Slippi you already have. No Slippi
installed? The app falls back to tinted abbreviation badges, so the UI works
fine either way.

To refresh after a Slippi update, hit **Re-extract from Slippi** in Settings.
If your Slippi Launcher is installed somewhere the auto-detect misses, point the
**Slippi Launcher path** field in Settings at the install folder (or its
`app.asar`) and re-extract. From a source checkout you can also run the
standalone extractor, which writes into `stats-melee-app/assets/`:

```sh
python3 stats-melee-app/scripts/extract-slippi-icons.py   # --asar / --out optional
```

See [`stats-melee-app/assets/README.md`](stats-melee-app/assets/README.md) for
the naming scheme if you'd rather drop in your own icon pack.

## Data & config locations

The app stores its database and config in your OS's standard app-data
directories (resolved via the `directories` crate), e.g. on macOS under
`~/Library/Application Support/dev.slippi.stats-melee/`. Nothing is written
outside your user profile.

## License

[MIT](LICENSE) © Matthew Lafrance
