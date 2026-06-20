//! Rip Melee character + stage icons out of a local Slippi Launcher install.
//!
//! The art is Nintendo IP shipped *inside* the Slippi Launcher's Electron
//! bundle. We never download or redistribute it — [`extract_to`] only copies
//! files already on the user's disk (from their own Slippi install) into the
//! app's writable assets dir, where [`crate::icons`] picks them up. The app
//! calls this once on first launch (see `StatsMeleeApp::new`); missing Slippi
//! or any parse failure is non-fatal and just leaves the drawn-badge fallback
//! in place.
//!
//! ## How it works
//!
//! Slippi's renderer bundle holds a webpack `require.context` mapping
//! `./<extId>/<color>/stock.png` (characters, keyed by EXTERNAL character id)
//! and `./<stageId>.png` (stages) to numeric module ids, plus asset modules of
//! the form `<id>(n,r,e){"use strict";n.exports=e.p+"<hash>.png"}`. We resolve
//! keys → module ids → hashed filenames and pull those PNGs straight out of the
//! `app.asar` archive, writing them under the names the app expects (the
//! [`CHARACTERS`] / [`STAGES`] tables). Our character id is the INTERNAL id, so
//! we map internal → external before indexing Slippi's stock folders.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use stats_melee::gamedata::{CHARACTERS, STAGES};

/// Internal character id (our [`CHARACTERS`] order) → external id (the folder
/// names inside Slippi's stock-icon `require.context`). Covers the 27 playable
/// slots; ids past this (Master Hand, etc.) have no stock icon.
const INT_TO_EXT: [u32; 27] = [
    8, 2, 0, 1, 4, 5, 6, 19, 11, 12, 14, 14, 13, 16, 17, 15, 10, 7, 9, 18, 21, 22, 20, 24, 3, 25,
    23,
];

/// Minimal read-only reader for Electron's `asar` archive format.
struct Asar {
    path: PathBuf,
    tree: serde_json::Value,
    data_offset: u64,
}

impl Asar {
    fn open(path: &Path) -> Result<Self> {
        let mut f = fs::File::open(path)?;
        // 4× little-endian u32 preamble: [4, header_size, header_obj_size, json_size].
        let mut pre = [0u8; 16];
        f.read_exact(&mut pre)?;
        let header_size = u32::from_le_bytes([pre[4], pre[5], pre[6], pre[7]]) as u64;
        let json_size = u32::from_le_bytes([pre[12], pre[13], pre[14], pre[15]]) as usize;
        let mut buf = vec![0u8; json_size];
        f.read_exact(&mut buf)?;
        let tree = serde_json::from_slice(&buf)?;
        Ok(Self {
            path: path.to_path_buf(),
            // File payloads begin right after the 8-byte size prefix + header.
            data_offset: 8 + header_size,
            tree,
        })
    }

    fn node(&self, parts: &[&str]) -> Option<&serde_json::Value> {
        let mut node = &self.tree;
        for p in parts {
            node = node.get("files")?.get(p)?;
        }
        Some(node)
    }

    /// File / directory names directly under the given path components.
    fn listdir(&self, parts: &[&str]) -> Vec<String> {
        self.node(parts)
            .and_then(|n| n.get("files"))
            .and_then(|f| f.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Raw bytes of the file at the given path components.
    fn read(&self, parts: &[&str]) -> Option<Vec<u8>> {
        let node = self.node(parts)?;
        let off: u64 = node.get("offset")?.as_str()?.parse().ok()?;
        let size = node.get("size")?.as_u64()? as usize;
        let mut f = fs::File::open(&self.path).ok()?;
        f.seek(SeekFrom::Start(self.data_offset + off)).ok()?;
        let mut buf = vec![0u8; size];
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    }
}

/// Best-effort per-OS locations of the Slippi Launcher's `app.asar`.
fn asar_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        let base = Path::new("/Applications/Slippi Launcher.app/Contents/Resources");
        out.push(base.join("app.asar"));
        out.push(base.join("app-arm64.asar"));
        out.push(base.join("app-x64.asar"));
    } else if cfg!(target_os = "windows") {
        // electron-builder's NSIS installer defaults to a per-user install
        // under %LOCALAPPDATA%\Programs; a machine-wide (admin) install lands
        // in Program Files instead. Cover both, and pair each `resources` dir
        // with the multi-arch asar names electron may emit.
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            for dir in ["Slippi Launcher", "slippi-launcher"] {
                roots.push(Path::new(&local).join("Programs").join(dir).join("resources"));
            }
        }
        for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
            if let Ok(pf) = std::env::var(var) {
                roots.push(Path::new(&pf).join("Slippi Launcher").join("resources"));
            }
        }
        for root in roots {
            for name in ["app.asar", "app-x64.asar", "app-arm64.asar"] {
                out.push(root.join(name));
            }
        }
    } else if let Ok(home) = std::env::var("HOME") {
        out.push(
            Path::new(&home)
                .join(".local/share/Slippi Launcher/resources/app.asar"),
        );
        out.push(PathBuf::from("/opt/Slippi Launcher/resources/app.asar"));
    }
    out
}

/// The first existing Slippi `app.asar`, if any install is present.
pub fn find_asar() -> Option<PathBuf> {
    asar_candidates().into_iter().find(|p| p.is_file())
}

/// Resolve a user-supplied Slippi *Launcher* path (from Settings) to its
/// `app.asar`. Auto-discovery covers the standard install locations; this is
/// the manual escape hatch for non-standard installs the probes miss.
///
/// Accepts whatever a file picker is likely to return: the `app.asar` file
/// itself, a `.app` bundle (macOS), the install directory, or its `resources`
/// subfolder. Returns `None` if no `.asar` can be found under it.
pub fn resolve_launcher_override(path: &Path) -> Option<PathBuf> {
    // Already pointing straight at an `.asar` file — take it as-is.
    if path.is_file() {
        return (path.extension().is_some_and(|e| e == "asar")).then(|| path.to_path_buf());
    }
    if !path.is_dir() {
        return None;
    }
    // Roots that might hold the asar, in priority order: the dir itself, a
    // macOS `.app`'s Contents/Resources, and an install dir's `resources`.
    let roots = [
        path.to_path_buf(),
        path.join("Contents").join("Resources"),
        path.join("resources"),
    ];
    for root in roots {
        for name in ["app.asar", "app-arm64.asar", "app-x64.asar"] {
            let cand = root.join(name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Pull `count` leading ASCII digits off the front of `s`.
fn leading_digits(s: &str) -> &str {
    let end = s
        .as_bytes()
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    &s[..end]
}

/// Scan one JS chunk, accumulating the three webpack maps. Hand-rolled (no
/// regex dep): anchor on small literal landmarks and read digits off each side.
fn parse_chunk(
    src: &str,
    mod_to_file: &mut HashMap<String, String>,
    char_ctx: &mut HashMap<String, String>,
    stage_ctx: &mut HashMap<String, String>,
) {
    let bytes = src.as_bytes();

    // Asset modules: `<id>(a,b,c){"use strict";W.exports=Z.p+"<hash>.png"}`.
    const ANCHOR: &str = "){\"use strict\";";
    for (pos, _) in src.match_indices(ANCHOR) {
        // Module id: walk left past the param list to its `(`, then take the
        // digits in front of it. Bounded so a stray anchor can't run away.
        let mut p = pos;
        while p > 0 && bytes[p - 1] != b'(' && pos - p < 40 {
            p -= 1;
        }
        if p == 0 || bytes[p - 1] != b'(' {
            continue;
        }
        let lparen = p - 1;
        let mut s = lparen;
        while s > 0 && bytes[s - 1].is_ascii_digit() {
            s -= 1;
        }
        let module_id = &src[s..lparen];
        if module_id.is_empty() {
            continue;
        }
        // Forward: the `e.p+"<hash>.png"}` must sit right after the anchor.
        let rest = &src[pos + ANCHOR.len()..];
        let Some(pp) = rest.find(".p+\"") else { continue };
        if pp > 24 {
            continue; // too far → not this module's exports line
        }
        let after = &rest[pp + 4..];
        let Some(end) = after.find(".png\"") else {
            continue;
        };
        let hash = &after[..end];
        if !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            mod_to_file.insert(module_id.to_string(), format!("{hash}.png"));
        }
    }

    // Character context: `"./<ext>/<color>/stock.png":<mod>`.
    const STOCK: &str = "/stock.png\":";
    for (pos, _) in src.match_indices(STOCK) {
        // color (digits before STOCK), then ext (digits before the next `/`).
        let mut c = pos;
        while c > 0 && bytes[c - 1].is_ascii_digit() {
            c -= 1;
        }
        let color = &src[c..pos];
        if color.is_empty() || c == 0 || bytes[c - 1] != b'/' {
            continue;
        }
        let mut e = c - 1; // at the `/` before ext
        while e > 0 && bytes[e - 1].is_ascii_digit() {
            e -= 1;
        }
        let ext = &src[e..c - 1];
        if ext.is_empty() {
            continue;
        }
        let module_id = leading_digits(&src[pos + STOCK.len()..]);
        if !module_id.is_empty() {
            char_ctx.insert(format!("{ext}/{color}"), module_id.to_string());
        }
    }

    // Stage context: `"./<stageId>.png":<mod>` — distinguished from the stock
    // entries (which read `stock.png`) by the digits immediately before `.png`.
    const PNG: &str = ".png\":";
    for (pos, _) in src.match_indices(PNG) {
        let mut k = pos;
        while k > 0 && bytes[k - 1].is_ascii_digit() {
            k -= 1;
        }
        if k == pos {
            continue; // no digits before `.png` → it's stock.png, handled above
        }
        // Require the `"./` root so we only catch the stage require.context.
        if !(k >= 3 && bytes[k - 1] == b'/' && bytes[k - 2] == b'.' && bytes[k - 3] == b'"') {
            continue;
        }
        let stage = &src[k..pos];
        let module_id = leading_digits(&src[pos + PNG.len()..]);
        if !module_id.is_empty() {
            stage_ctx.insert(stage.to_string(), module_id.to_string());
        }
    }
}

/// Extract character + stage icons from the local Slippi install into
/// `dest/characters` and `dest/stages`, named to match [`CHARACTERS`] /
/// [`STAGES`]. Returns `(characters, stages)` written. Errors if no Slippi
/// install is found or its bundle layout is unrecognized.
///
/// `launcher_override` is the optional manual path from Settings; when set and
/// resolvable it wins over auto-discovery, otherwise we fall back to the
/// standard per-OS install locations.
pub fn extract_to(dest: &Path, launcher_override: Option<&Path>) -> Result<(usize, usize)> {
    let asar_path = launcher_override
        .and_then(resolve_launcher_override)
        .or_else(find_asar)
        .ok_or_else(|| anyhow!("no Slippi Launcher install found"))?;
    let arc = Asar::open(&asar_path)?;

    let renderer = arc.listdir(&["dist", "renderer"]);
    if renderer.is_empty() {
        bail!("unexpected asar layout: no dist/renderer (Slippi version mismatch?)");
    }

    // The context map and the asset modules aren't guaranteed to share a chunk,
    // and chunk filenames change between releases, so scan every JS chunk.
    let mut mod_to_file = HashMap::new();
    let mut char_ctx = HashMap::new();
    let mut stage_ctx = HashMap::new();
    for name in &renderer {
        if !name.ends_with(".js") {
            continue;
        }
        if let Some(bytes) = arc.read(&["dist", "renderer", name]) {
            let src = String::from_utf8_lossy(&bytes);
            parse_chunk(&src, &mut mod_to_file, &mut char_ctx, &mut stage_ctx);
        }
    }
    if char_ctx.is_empty() && stage_ctx.is_empty() {
        bail!("no stock-icon context in the renderer bundle (Slippi asset layout changed)");
    }

    let chars_dir = dest.join("characters");
    let stages_dir = dest.join("stages");
    fs::create_dir_all(&chars_dir)?;
    fs::create_dir_all(&stages_dir)?;

    let copy = |module_id: &str, dest: PathBuf| -> bool {
        match mod_to_file.get(module_id) {
            Some(file) => match arc.read(&["dist", "renderer", file]) {
                Some(data) => fs::write(dest, data).is_ok(),
                None => false,
            },
            None => false,
        }
    };

    let mut n_char = 0;
    for (internal, ext) in INT_TO_EXT.iter().enumerate() {
        let Some(name) = CHARACTERS.get(internal) else {
            continue;
        };
        if let Some(module_id) = char_ctx.get(&format!("{ext}/0")) {
            // color 0 = neutral costume
            if copy(module_id, chars_dir.join(format!("{name}.png"))) {
                n_char += 1;
            }
        }
    }

    let mut n_stage = 0;
    for (stage_id, module_id) in &stage_ctx {
        let Ok(idx) = stage_id.parse::<usize>() else {
            continue;
        };
        let Some(name) = STAGES.get(idx) else { continue };
        if *name == "Dummy" || *name == "Test" {
            continue;
        }
        if copy(module_id, stages_dir.join(format!("{name}.png"))) {
            n_stage += 1;
        }
    }

    Ok((n_char, n_stage))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercises the real extractor against the local Slippi install when one is
    // present; silently skips otherwise (e.g. in CI) so it never flakes.
    #[test]
    fn extracts_from_local_slippi_if_present() {
        if find_asar().is_none() {
            eprintln!("skipping: no local Slippi Launcher install");
            return;
        }
        let tmp = std::env::temp_dir().join(format!("stats-melee-icontest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let (chars, stages) = extract_to(&tmp, None).expect("extraction should succeed");
        assert!(chars >= 20, "expected most character icons, got {chars}");
        assert!(stages >= 15, "expected most stage icons, got {stages}");
        // A known icon should be a real PNG.
        let falco = fs::read(tmp.join("characters/Falco.png")).expect("Falco.png written");
        assert_eq!(&falco[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn override_resolves_asar_from_install_dir_or_file() {
        let root =
            std::env::temp_dir().join(format!("stats-melee-asar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);

        // A bare install dir with `resources/app.asar` inside resolves to it.
        let resources = root.join("Slippi Launcher").join("resources");
        fs::create_dir_all(&resources).expect("mkdir");
        let asar = resources.join("app.asar");
        fs::write(&asar, b"x").expect("write asar");
        assert_eq!(
            resolve_launcher_override(&root.join("Slippi Launcher")),
            Some(asar.clone())
        );

        // Pointing straight at the `.asar` file returns it unchanged.
        assert_eq!(resolve_launcher_override(&asar), Some(asar));

        // A directory with no asar (and a non-asar file) resolves to None.
        let empty = root.join("empty");
        fs::create_dir_all(&empty).expect("mkdir");
        assert_eq!(resolve_launcher_override(&empty), None);
        let stray = root.join("notes.txt");
        fs::write(&stray, b"x").expect("write");
        assert_eq!(resolve_launcher_override(&stray), None);

        let _ = fs::remove_dir_all(&root);
    }
}
