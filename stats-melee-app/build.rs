//! Build script: render the procedural app icon into the per-OS form each
//! platform's packaging needs, dropping the result next to the built binary
//! (the profile dir) where the release workflow can find it.
//!
//! - Windows: encode a multi-size `.ico` and embed it in the `.exe` (so
//!   Explorer / taskbar / shortcuts show it), plus a copy at `app.ico`.
//! - macOS:   write an `AppIcon.iconset/` for `iconutil` to turn into the
//!   `.app` bundle's `.icns`.
//! - Linux:   write a single `stats-melee.png` for the `.desktop` launcher.
//!
//! Icon pixels come from the same [`app_icon::diamond_rgba`] the runtime window
//! icon uses, so every form stays in sync from one source.

#[path = "src/app_icon.rs"]
#[allow(dead_code)] // not every helper is used on every host
mod app_icon;

use std::path::Path;

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    // The profile dir (e.g. target/<triple>/release) sits three levels up from
    // OUT_DIR's hashed path and is where the built binary lands.
    let profile_dir = Path::new(&out_dir).ancestors().nth(3).map(Path::to_path_buf);

    #[cfg(windows)]
    embed_windows_icon(&out_dir, profile_dir.as_deref());

    #[cfg(target_os = "macos")]
    if let Some(dir) = profile_dir.as_deref() {
        write_macos_iconset(dir);
    }

    #[cfg(target_os = "linux")]
    if let Some(dir) = profile_dir.as_deref() {
        write_png(&dir.join("stats-melee.png"), 256);
    }

    let _ = (&out_dir, &profile_dir); // keep both "used" on every host
}

/// Encode `size`×`size` RGBA as a PNG file. Used to assemble the macOS iconset
/// and the Linux launcher icon.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn write_png(path: &Path, size: u32) {
    let rgba = app_icon::diamond_rgba(size as usize);
    let file = std::fs::File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("png header")
        .write_image_data(&rgba)
        .expect("png data");
}

/// Write the standard macOS `AppIcon.iconset/` layout (each logical size plus
/// its `@2x` retina variant) so CI can `iconutil -c icns` it.
#[cfg(target_os = "macos")]
fn write_macos_iconset(profile_dir: &Path) {
    let iconset = profile_dir.join("AppIcon.iconset");
    std::fs::create_dir_all(&iconset).expect("mkdir AppIcon.iconset");
    // (base size, filename) — `@2x` entries are just the doubled pixel size.
    for (px, name) in [
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ] {
        write_png(&iconset.join(name), px);
    }
}

#[cfg(windows)]
fn embed_windows_icon(out_dir: &str, profile_dir: Option<&Path>) {
    let ico_path = Path::new(out_dir).join("app.ico");

    // Multi-resolution .ico so small sizes (taskbar, Explorer list) stay crisp
    // instead of being downscaled from 256.
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in &[16u32, 24, 32, 48, 64, 128, 256] {
        let rgba = app_icon::diamond_rgba(size as usize);
        let image = ico::IconImage::from_rgba_data(size, size, rgba);
        dir.add_entry(ico::IconDirEntry::encode(&image).expect("encode .ico entry"));
    }
    let file = std::fs::File::create(&ico_path).expect("create app.ico");
    dir.write(file).expect("write app.ico");

    // Copy to a stable spot next to the .exe for any external tooling.
    if let Some(profile) = profile_dir {
        let _ = std::fs::copy(&ico_path, profile.join("app.ico"));
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico_path.to_str().expect("utf-8 OUT_DIR path"));
    res.compile().expect("embed Windows icon resource");
}
