use anyhow::Context;
use eframe::egui;
use resvg::{tiny_skia, usvg};

const APP_ICON_SIZE: u32 = 256;
const TRAY_ICON_SIZE: u32 = 32;

const APP_ICON_SVG: &str = include_str!("../../assets/app-icon.svg");
const TRAY_ICON_SVG: &str = include_str!("../../assets/tray-icon.svg");

pub fn app_icon() -> egui::IconData {
    let rgba = render_svg(APP_ICON_SVG, APP_ICON_SIZE).unwrap_or_else(|err| {
        tracing::warn!(error = %err, "failed to render app icon svg");
        vec![0; (APP_ICON_SIZE * APP_ICON_SIZE * 4) as usize]
    });
    egui::IconData {
        rgba,
        width: APP_ICON_SIZE,
        height: APP_ICON_SIZE,
    }
}

pub fn tray_icon() -> anyhow::Result<tray_icon::Icon> {
    let rgba = render_svg(TRAY_ICON_SVG, TRAY_ICON_SIZE)?;
    tray_icon::Icon::from_rgba(rgba, TRAY_ICON_SIZE, TRAY_ICON_SIZE).map_err(Into::into)
}

fn render_svg(svg: &str, size: u32) -> anyhow::Result<Vec<u8>> {
    let tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default())
        .context("failed to parse icon svg")?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).context("failed to create icon pixmap")?;
    let mut pixmap_mut = pixmap.as_mut();
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixmap_mut);
    Ok(demultiply_rgba(&pixmap))
}

fn demultiply_rgba(pixmap: &tiny_skia::Pixmap) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(pixmap.data().len());
    for pixel in pixmap.pixels() {
        let color = pixel.demultiply();
        rgba.push(color.red());
        rgba.push(color.green());
        rgba.push(color.blue());
        rgba.push(color.alpha());
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_svg_icons_to_non_empty_rgba() {
        let app = render_svg(APP_ICON_SVG, APP_ICON_SIZE).unwrap();
        assert_eq!(app.len(), (APP_ICON_SIZE * APP_ICON_SIZE * 4) as usize);
        assert!(app.chunks_exact(4).any(|pixel| pixel[3] > 0));

        let tray = render_svg(TRAY_ICON_SVG, TRAY_ICON_SIZE).unwrap();
        assert_eq!(tray.len(), (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize);
        assert!(tray.chunks_exact(4).any(|pixel| pixel[3] > 0));
    }
}
