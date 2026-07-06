use anyhow::{Context, bail};
use resvg::{tiny_skia, usvg};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const APP_ICON_SVG: &str = include_str!("../../assets/app-icon.svg");
const ICONSET_DIR: &str = "target/macos-app/CodexSwitch.iconset";
const ICNS_PATH: &str = "target/macos-app/AppIcon.icns";
const ICON_SIZES: &[IconSize] = &[
    IconSize::new("icon_16x16.png", "icp4", 16),
    IconSize::new("icon_16x16@2x.png", "ic11", 32),
    IconSize::new("icon_32x32.png", "icp5", 32),
    IconSize::new("icon_32x32@2x.png", "ic12", 64),
    IconSize::new("icon_128x128.png", "ic07", 128),
    IconSize::new("icon_128x128@2x.png", "ic13", 256),
    IconSize::new("icon_256x256.png", "ic08", 256),
    IconSize::new("icon_256x256@2x.png", "ic14", 512),
    IconSize::new("icon_512x512.png", "ic09", 512),
    IconSize::new("icon_512x512@2x.png", "ic10", 1024),
];

fn main() -> anyhow::Result<()> {
    let iconset_dir = PathBuf::from(ICONSET_DIR);
    let icns_path = PathBuf::from(ICNS_PATH);
    if iconset_dir.exists() {
        fs::remove_dir_all(&iconset_dir)
            .with_context(|| format!("failed to remove {}", iconset_dir.display()))?;
    }
    fs::create_dir_all(&iconset_dir)
        .with_context(|| format!("failed to create {}", iconset_dir.display()))?;

    let tree = usvg::Tree::from_data(APP_ICON_SVG.as_bytes(), &usvg::Options::default())
        .context("failed to parse app icon svg")?;
    let mut chunks = Vec::with_capacity(ICON_SIZES.len());
    for icon_size in ICON_SIZES {
        let png_path = iconset_dir.join(icon_size.file_name);
        let png = render_png(&tree, icon_size.size, &png_path)?;
        chunks.push((icon_size.icns_type, png));
    }
    write_icns(&icns_path, &chunks)?;
    println!("created {}", iconset_dir.display());
    println!("created {}", icns_path.display());
    Ok(())
}

#[derive(Clone, Copy)]
struct IconSize {
    file_name: &'static str,
    icns_type: &'static str,
    size: u32,
}

impl IconSize {
    const fn new(file_name: &'static str, icns_type: &'static str, size: u32) -> Self {
        Self {
            file_name,
            icns_type,
            size,
        }
    }
}

fn render_png(tree: &usvg::Tree, size: u32, path: &Path) -> anyhow::Result<Vec<u8>> {
    let svg_size = tree.size();
    let scale_x = size as f32 / svg_size.width();
    let scale_y = size as f32 / svg_size.height();
    if !scale_x.is_finite() || !scale_y.is_finite() {
        bail!("invalid svg size for icon rendering");
    }

    let mut pixmap =
        tiny_skia::Pixmap::new(size, size).context("failed to create icon pixmap")?;
    let mut pixmap_mut = pixmap.as_mut();
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale_x, scale_y),
        &mut pixmap_mut,
    );
    let png = pixmap.encode_png().context("failed to encode icon png")?;
    fs::write(path, &png).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(png)
}

fn write_icns(path: &Path, chunks: &[(&str, Vec<u8>)]) -> anyhow::Result<()> {
    let mut total_len = 8u32;
    for (chunk_type, data) in chunks {
        if chunk_type.len() != 4 {
            bail!("invalid icns chunk type {}", chunk_type);
        }
        let chunk_len = data.len().checked_add(8).context("icns chunk is too large")?;
        total_len = total_len
            .checked_add(u32::try_from(chunk_len).context("icns chunk is too large")?)
            .context("icns file is too large")?;
    }

    let mut file =
        fs::File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(b"icns")?;
    file.write_all(&total_len.to_be_bytes())?;
    for (chunk_type, data) in chunks {
        file.write_all(chunk_type.as_bytes())?;
        let chunk_len = u32::try_from(data.len() + 8).context("icns chunk is too large")?;
        file.write_all(&chunk_len.to_be_bytes())?;
        file.write_all(data)?;
    }
    Ok(())
}
