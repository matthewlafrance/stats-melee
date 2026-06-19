# Icon assets

Drop character / stage icon PNGs here and the replay library + viewer
pick them up automatically — no code change. Until a file is present, the
app draws a tinted abbreviation badge as a fallback, so the UI works with
an empty or partial set.

## Layout & naming

```
assets/
  characters/<Name>.png
  stages/<Name>.png
```

`<Name>` must exactly match the internal name (CamelCase, no spaces) from
`stats-melee/src/gamedata.rs`. Square PNGs render best (they're drawn at
~18 px square in the table).

### Characters (`assets/characters/`)
Mario, Fox, CaptainFalcon, DonkeyKong, Kirby, Bowser, Link, Sheik, Ness,
Peach, Popo, Nana, Pikachu, Samus, Yoshi, Jigglypuff, Mewtwo, Luigi,
Marth, Zelda, YoungLink, DrMario, Falco, Pichu, GameAndWatch, Ganondorf,
Roy

e.g. `assets/characters/CaptainFalcon.png`, `assets/characters/Falco.png`

### Stages (`assets/stages/`)
FountainOfDreams, PokemonStadium, YoshisStory, DreamLandN64, Battlefield,
FinalDestination (and any other stage name from the `STAGES` table)

e.g. `assets/stages/Battlefield.png`

## Where to get them

The app rips them for you. On first launch it pulls the stock icons +
stage art straight out of your local **Slippi Launcher** install (the
`app.asar` bundle) into its data dir — see `src/slippi_icons.rs`. No
network, no redistribution; you only ever touch your own copy.

To populate *this* source-tree folder instead (for `cargo run` dev), run
the standalone extractor, which does the same thing:

```sh
python3 scripts/extract-slippi-icons.py        # auto-detects Slippi
```

These are Nintendo's art, so the PNGs stay `.gitignore`d by default —
bringing them into the repo is the repo owner's call.
