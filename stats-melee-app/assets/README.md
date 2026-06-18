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

These tools (Slippipedia, slippi.gg, the Slippi Launcher) all use stock
icons ripped from Melee — Nintendo's art, redistributed by the community.
A common source is the Slippi Launcher's open-source asset folder or a
community stock-icon pack. Bringing those into this repo is the repo
owner's call; this folder is `.gitignore`-friendly if you'd rather keep
the art untracked.
