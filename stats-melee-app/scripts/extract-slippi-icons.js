#!/usr/bin/env node
// Extract character stock icons + stage art from a local Slippi Launcher
// install into stats-melee-app/assets/. The art is Nintendo IP shipped
// inside Slippi's Electron bundle; this script only copies files you
// already have locally. The PNGs are gitignored by default.
//
// Usage:
//   1. npx --yes @electron/asar extract \
//        "/Applications/Slippi Launcher.app/Contents/Resources/app-arm64.asar" \
//        /tmp/slippi-asar-extract
//   2. node stats-melee-app/scripts/extract-slippi-icons.js
//
// How it works: Slippi's renderer bundle (dist/renderer/901.renderer.js)
// holds a webpack require.context mapping `./<extId>/<color>/stock.png`
// (characters, keyed by EXTERNAL character id) and `./<stageId>.png`
// (stages) to numeric module ids, plus asset modules of the form
// `<id>(n,r,e){"use strict";n.exports=e.p+"<hash>.png"}`. We resolve
// keys -> module ids -> hashed filenames and copy them out under names
// our app expects (CHARACTERS / STAGES from stats-melee/src/gamedata.rs).
//
// NB: our character_id is the INTERNAL id (from .slp metadata), so we map
// internal -> external before indexing Slippi's stock folders.

const fs = require("fs");
const path = require("path");

const SRC_DIR = process.env.SLIPPI_ASAR || "/tmp/slippi-asar-extract/dist/renderer";
const REPO = path.resolve(__dirname, "..", "assets");
const JS = path.join(SRC_DIR, "901.renderer.js");

const js = fs.readFileSync(JS, "utf8");

const modToFile = new Map();
for (const m of js.matchAll(/(\d{2,7})\(\w,\w,(\w)\)\{"use strict";\w\.exports=\2\.p\+"([0-9a-f]+\.png)"\}/g)) {
  modToFile.set(m[1], m[3]);
}
const charCtx = new Map();
for (const m of js.matchAll(/"\.\/(\d+)\/(\d+)\/stock\.png":(\d+)/g)) {
  charCtx.set(`${m[1]}/${m[2]}`, m[3]);
}
const stageCtx = new Map();
for (const m of js.matchAll(/"\.\/(\d+)\.png":(\d+)/g)) {
  stageCtx.set(m[1], m[2]);
}

// internal character id (our CHARACTERS table) -> external id (Slippi folders)
const INT_TO_EXT = [8,2,0,1,4,5,6,19,11,12,14,14,13,16,17,15,10,7,9,18,21,22,20,24,3,25,23];
const CHAR_NAMES = ["Mario","Fox","CaptainFalcon","DonkeyKong","Kirby","Bowser","Link","Sheik","Ness","Peach","Popo","Nana","Pikachu","Samus","Yoshi","Jigglypuff","Mewtwo","Luigi","Marth","Zelda","YoungLink","DrMario","Falco","Pichu","GameAndWatch","Ganondorf","Roy"];
const STAGE_NAMES = ["Dummy","Test","FountainOfDreams","PokemonStadium","PrincessPeachsCastle","KongoJungle","Brinstar","Corneria","YoshisStory","Onett","MuteCity","RainbowCruise","JungleJapes","GreatBay","HyruleTemple","BrinstarDepths","YoshisIsland","GreenGreens","Fourside","MushroomKingdomI","MushroomKingdomII","Akaneia","Venom","PokeFloats","BigBlue","IcicleMountain","Icetop","FlatZone","DreamLandN64","YoshisIslandN64","KongoJungleN64","Battlefield","FinalDestination"];

function copy(moduleId, destPath, label) {
  const file = modToFile.get(moduleId);
  if (!file) { console.warn(`  ! no file for module ${moduleId} (${label})`); return false; }
  const src = path.join(SRC_DIR, file);
  if (!fs.existsSync(src)) { console.warn(`  ! missing src ${file} (${label})`); return false; }
  fs.copyFileSync(src, destPath);
  return true;
}

fs.mkdirSync(path.join(REPO, "characters"), { recursive: true });
fs.mkdirSync(path.join(REPO, "stages"), { recursive: true });

let nChar = 0, nStage = 0;
for (let internal = 0; internal < CHAR_NAMES.length; internal++) {
  const mod = charCtx.get(`${INT_TO_EXT[internal]}/0`); // color 0 = neutral costume
  if (mod && copy(mod, path.join(REPO, "characters", `${CHAR_NAMES[internal]}.png`), CHAR_NAMES[internal])) nChar++;
}
for (const [stageId, mod] of stageCtx) {
  const name = STAGE_NAMES[parseInt(stageId, 10)];
  if (!name || name === "Dummy" || name === "Test") continue;
  if (copy(mod, path.join(REPO, "stages", `${name}.png`), name)) nStage++;
}
console.log(`copied ${nChar} character icons, ${nStage} stage icons -> ${REPO}`);
