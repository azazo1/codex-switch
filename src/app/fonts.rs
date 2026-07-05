use eframe::egui::{Context, FontData, FontDefinitions, FontFamily};
use std::{fs, path::Path, sync::Arc};

const CJK_FONT_NAME: &str = "codex-switch-cjk";
const UI_FONT_SCALE: f32 = 1.06;

const CJK_FONT_PATHS: &[&str] = &[
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/System/Library/Fonts/Supplemental/Songti.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "C:\\Windows\\Fonts\\msyh.ttc",
    "C:\\Windows\\Fonts\\simhei.ttf",
];

pub fn install_fonts(ctx: &Context) {
    scale_text_styles(ctx);

    let Some((path, bytes)) = load_cjk_font() else {
        tracing::warn!("no CJK UI font found, Chinese text may render as missing glyph boxes");
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        CJK_FONT_NAME.to_string(),
        Arc::new(FontData::from_owned(bytes)),
    );
    prepend_font(&mut fonts, FontFamily::Proportional);
    prepend_font(&mut fonts, FontFamily::Monospace);
    ctx.set_fonts(fonts);
    tracing::info!(path = %path.display(), "installed CJK UI font");
}

fn scale_text_styles(ctx: &Context) {
    ctx.style_mut(|style| {
        for font_id in style.text_styles.values_mut() {
            font_id.size *= UI_FONT_SCALE;
        }
    });
}

fn load_cjk_font() -> Option<(&'static Path, Vec<u8>)> {
    CJK_FONT_PATHS
        .iter()
        .map(Path::new)
        .find_map(|path| fs::read(path).ok().map(|bytes| (path, bytes)))
}

fn prepend_font(fonts: &mut FontDefinitions, family: FontFamily) {
    fonts
        .families
        .entry(family)
        .or_default()
        .insert(0, CJK_FONT_NAME.to_string());
}
