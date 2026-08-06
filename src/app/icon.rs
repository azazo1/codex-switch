use anyhow::Context;
use eframe::egui;
use resvg::{tiny_skia, usvg};

const APP_ICON_SIZE: u32 = 256;
const TRAY_ICON_SIZE: u32 = 64;

const APP_ICON_SVG: &str = include_str!("../../assets/app-icon.svg");
const TRAY_ICON_SVG: &str = include_str!("../../assets/tray-icon.svg");
const TRAY_ICON_LIGHT_SVG: &str = include_str!("../../assets/tray-icon-light.svg");

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

pub fn tray_icon_for_theme(dark: bool) -> anyhow::Result<tray_icon::Icon> {
    let svg = if dark { TRAY_ICON_LIGHT_SVG } else { TRAY_ICON_SVG };
    let rgba = render_svg(svg, TRAY_ICON_SIZE)?;
    tray_icon::Icon::from_rgba(rgba, TRAY_ICON_SIZE, TRAY_ICON_SIZE).map_err(Into::into)
}

fn render_svg(svg: &str, size: u32) -> anyhow::Result<Vec<u8>> {
    let tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default())
        .context("failed to parse icon svg")?;
    let svg_size = tree.size();
    let scale = size as f32 / svg_size.width();
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    let mut pixmap = tiny_skia::Pixmap::new(size, size).context("failed to create icon pixmap")?;
    let mut pixmap_mut = pixmap.as_mut();
    resvg::render(&tree, transform, &mut pixmap_mut);
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

        let light_tray = render_svg(TRAY_ICON_LIGHT_SVG, TRAY_ICON_SIZE).unwrap();
        assert_eq!(
            light_tray.len(),
            (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize
        );
        assert!(light_tray.chunks_exact(4).any(|pixel| pixel[3] > 0));
    }

    #[test]
    fn scaled_tray_icon_fills_canvas() {
        let tray = render_svg(TRAY_ICON_SVG, TRAY_ICON_SIZE).unwrap();
        let reaches_scaled_region = tray
            .chunks_exact(4)
            .enumerate()
            .any(|(index, pixel)| {
                let x = index % TRAY_ICON_SIZE as usize;
                let y = index / TRAY_ICON_SIZE as usize;
                pixel[3] > 0 && x >= 48 && y >= 48
            });
        assert!(reaches_scaled_region, "svg should be scaled beyond its original 32px bounds");
    }
}
