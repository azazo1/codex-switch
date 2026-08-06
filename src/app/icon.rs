use anyhow::Context;
use eframe::egui;
use resvg::{tiny_skia, usvg};

const APP_ICON_SIZE: u32 = 256;
const TRAY_ICON_SIZE: u32 = 32;
const BADGE_CENTER_X: f32 = 22.0;
const BADGE_CENTER_Y: f32 = 22.0;
const BADGE_RADIUS: f32 = 10.0;

const FONT_6X8: &[(char, &[u8])] = &[
    ('0', &[0b011100, 0b100010, 0b100110, 0b101010, 0b110010, 0b100010, 0b100010, 0b011100]),
    ('1', &[0b001000, 0b011000, 0b001000, 0b001000, 0b001000, 0b001000, 0b001000, 0b011100]),
    ('2', &[0b111100, 0b000010, 0b000010, 0b000100, 0b001000, 0b010000, 0b100000, 0b111110]),
    ('3', &[0b111110, 0b000010, 0b000010, 0b001110, 0b000010, 0b000010, 0b000010, 0b111100]),
    ('4', &[0b000100, 0b001100, 0b010100, 0b100100, 0b111110, 0b000100, 0b000100, 0b000100]),
    ('5', &[0b111110, 0b100000, 0b100000, 0b111100, 0b000010, 0b000010, 0b100010, 0b011100]),
    ('6', &[0b011100, 0b100000, 0b100000, 0b111100, 0b100010, 0b100010, 0b100010, 0b011100]),
    ('7', &[0b111110, 0b000010, 0b000100, 0b001000, 0b010000, 0b010000, 0b010000, 0b010000]),
    ('8', &[0b011100, 0b100010, 0b100010, 0b011100, 0b100010, 0b100010, 0b100010, 0b011100]),
    ('9', &[0b011100, 0b100010, 0b100010, 0b011110, 0b000010, 0b000010, 0b000010, 0b011100]),
    ('K', &[0b100010, 0b100100, 0b101000, 0b110000, 0b101000, 0b100100, 0b100010, 0b100010]),
    ('M', &[0b100010, 0b110110, 0b101010, 0b101010, 0b100010, 0b100010, 0b100010, 0b100010]),
    ('B', &[0b111100, 0b100010, 0b100010, 0b111100, 0b100010, 0b100010, 0b100010, 0b111100]),
    ('+', &[0b000000, 0b001000, 0b001000, 0b111110, 0b001000, 0b001000, 0b000000, 0b000000]),
    ('.', &[0b000000, 0b000000, 0b000000, 0b000000, 0b000000, 0b001100, 0b001100, 0b000000]),
];

const FONT_5X7: &[(char, &[u8])] = &[
    ('0', &[0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110]),
    ('1', &[0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('2', &[0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111]),
    ('3', &[0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110]),
    ('4', &[0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010]),
    ('5', &[0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110]),
    ('6', &[0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110]),
    ('7', &[0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000]),
    ('8', &[0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110]),
    ('9', &[0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110]),
    ('K', &[0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001]),
    ('M', &[0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001]),
    ('B', &[0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110]),
    ('+', &[0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000]),
    ('.', &[0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00110]),
];

const FONT_4X6: &[(char, &[u8])] = &[
    ('0', &[0b1110, 0b1001, 0b1001, 0b1001, 0b1001, 0b1110]),
    ('1', &[0b0100, 0b1100, 0b0100, 0b0100, 0b0100, 0b1110]),
    ('2', &[0b1110, 0b0001, 0b0001, 0b1110, 0b1000, 0b1110]),
    ('3', &[0b1110, 0b0001, 0b0001, 0b0110, 0b0001, 0b1110]),
    ('4', &[0b1001, 0b1001, 0b1001, 0b1111, 0b0001, 0b0001]),
    ('5', &[0b1110, 0b1000, 0b1110, 0b0001, 0b0001, 0b1110]),
    ('6', &[0b1110, 0b1000, 0b1110, 0b1001, 0b1001, 0b1110]),
    ('7', &[0b1110, 0b0001, 0b0010, 0b0100, 0b0100, 0b0100]),
    ('8', &[0b1110, 0b1001, 0b1110, 0b1001, 0b1001, 0b1110]),
    ('9', &[0b1110, 0b1001, 0b1001, 0b1110, 0b0001, 0b1110]),
    ('K', &[0b1001, 0b1010, 0b1100, 0b1000, 0b1010, 0b1001]),
    ('M', &[0b1001, 0b1101, 0b1111, 0b1011, 0b1001, 0b1001]),
    ('B', &[0b1110, 0b1001, 0b1110, 0b1001, 0b1001, 0b1110]),
    ('+', &[0b0000, 0b0100, 0b0100, 0b1110, 0b0100, 0b0100]),
    ('.', &[0b0000, 0b0000, 0b0000, 0b0000, 0b0100, 0b0100]),
];

struct BadgeFont {
    width: usize,
    height: usize,
    spacing: usize,
    glyphs: &'static [(char, &'static [u8])],
}

fn badge_font_for(text: &str) -> BadgeFont {
    match text.chars().count() {
        1..=2 => BadgeFont {
            width: 6,
            height: 8,
            spacing: 1,
            glyphs: FONT_6X8,
        },
        3 => BadgeFont {
            width: 5,
            height: 7,
            spacing: 1,
            glyphs: FONT_5X7,
        },
        _ => BadgeFont {
            width: 4,
            height: 6,
            spacing: 0,
            glyphs: FONT_4X6,
        },
    }
}

impl BadgeFont {
    fn glyph(&self, ch: char) -> &'static [u8] {
        self.glyphs
            .iter()
            .find(|(candidate, _)| *candidate == ch)
            .map(|(_, glyph)| *glyph)
            .unwrap_or(&[])
    }
}

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

pub fn tray_icon_with_badge(
    dark: bool,
    badge: Option<&str>,
    template: bool,
) -> anyhow::Result<tray_icon::Icon> {
    let rgba = tray_icon_rgba(dark, badge, template)?;
    tray_icon::Icon::from_rgba(rgba, TRAY_ICON_SIZE, TRAY_ICON_SIZE).map_err(Into::into)
}

pub fn tray_icon_rgba(dark: bool, badge: Option<&str>, template: bool) -> anyhow::Result<Vec<u8>> {
    let svg = if dark { TRAY_ICON_LIGHT_SVG } else { TRAY_ICON_SVG };
    let mut pixmap = render_svg_pixmap(svg, TRAY_ICON_SIZE)?;
    if let Some(text) = badge.filter(|text| !text.is_empty()) {
        draw_badge(&mut pixmap, text, template);
    }
    Ok(demultiply_rgba(&pixmap))
}

fn draw_badge(pixmap: &mut tiny_skia::Pixmap, text: &str, template: bool) {
    if let Some(path) =
        tiny_skia::PathBuilder::from_circle(BADGE_CENTER_X, BADGE_CENTER_Y, BADGE_RADIUS)
    {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(0xe5, 0x48, 0x4d, 0xff);
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            tiny_skia::Transform::identity(),
            None,
        );
    }
    let font = badge_font_for(text);
    let glyphs = text.chars().map(|ch| font.glyph(ch)).collect::<Vec<_>>();
    let width = glyphs.len() * font.width + (glyphs.len().saturating_sub(1)) * font.spacing;
    let start_x = BADGE_CENTER_X as i32 - (width / 2) as i32;
    let start_y = BADGE_CENTER_Y as i32 - (font.height / 2) as i32;
    for (glyph_index, glyph) in glyphs.iter().enumerate() {
        for row_index in 0..font.height {
            let bits = glyph.get(row_index).copied().unwrap_or(0);
            for column in 0..font.width {
                if bits & (1 << (font.width - 1 - column)) == 0 {
                    continue;
                }
                let x = start_x
                    + (glyph_index * (font.width + font.spacing)) as i32
                    + column as i32;
                let y = start_y + row_index as i32;
                set_badge_pixel(pixmap, x, y, template);
            }
        }
    }
}

fn set_badge_pixel(pixmap: &mut tiny_skia::Pixmap, x: i32, y: i32, template: bool) {
    if x < 0
        || y < 0
        || x >= TRAY_ICON_SIZE as i32
        || y >= TRAY_ICON_SIZE as i32
    {
        return;
    }
    let offset = ((y * TRAY_ICON_SIZE as i32 + x) * 4) as usize;
    let data = pixmap.data_mut();
    if template {
        data[offset..offset + 4].copy_from_slice(&[0, 0, 0, 0]);
    } else {
        data[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
    }
}

fn render_svg(svg: &str, size: u32) -> anyhow::Result<Vec<u8>> {
    Ok(demultiply_rgba(&render_svg_pixmap(svg, size)?))
}

fn render_svg_pixmap(svg: &str, size: u32) -> anyhow::Result<tiny_skia::Pixmap> {
    let tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default())
        .context("failed to parse icon svg")?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).context("failed to create icon pixmap")?;
    let mut pixmap_mut = pixmap.as_mut();
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixmap_mut);
    Ok(pixmap)
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
        assert_eq!(light_tray.len(), (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize);
        assert!(light_tray.chunks_exact(4).any(|pixel| pixel[3] > 0));
    }

    #[test]
    fn tray_icon_badge_overlay_changes_pixels() {
        let plain = tray_icon_rgba(false, None, false).unwrap();
        let badge = tray_icon_rgba(false, Some("3"), false).unwrap();
        let template_badge = tray_icon_rgba(false, Some("3"), true).unwrap();

        assert_eq!(plain.len(), badge.len());
        assert_ne!(plain, badge);
        assert_ne!(badge, template_badge);
        assert!(badge.chunks_exact(4).any(|pixel| pixel[3] > 0));
    }

    #[test]
    fn badge_pixels_fill_bottom_right_corner() {
        let badge = tray_icon_rgba(false, Some("3"), false).unwrap();
        let offset = (BADGE_CENTER_Y as usize - 7) * TRAY_ICON_SIZE as usize
            + BADGE_CENTER_X as usize;
        let pixel = &badge[offset * 4..offset * 4 + 4];

        assert_eq!(pixel[3], 255);
        assert!(pixel[0] > 200);
        assert!(pixel[1] < 120);
    }

    #[test]
    fn badge_text_renders_white_pixels() {
        let badge = tray_icon_rgba(false, Some("3"), false).unwrap();
        let white = badge
            .chunks_exact(4)
            .filter(|pixel| pixel[0] == 255 && pixel[1] == 255 && pixel[2] == 255 && pixel[3] == 255)
            .count();
        assert!(white > 0, "no white text pixels rendered");
    }
}
