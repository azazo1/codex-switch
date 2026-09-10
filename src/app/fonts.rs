use eframe::egui::{Context, FontData, FontDefinitions, FontFamily};
use std::{fs, path::Path, sync::Arc};

const CJK_FONT_NAME: &str = "codex-switch-cjk";
const MONO_FONT_NAME: &str = "codex-switch-mono";
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

const MONO_FONT_PATHS: &[&str] = &[
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Monaco.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
    "C:\\Windows\\Fonts\\consola.ttf",
    "C:\\Windows\\Fonts\\cour.ttf",
];

pub fn install_fonts(ctx: &Context) {
    scale_text_styles(ctx);

    let Some((cjk_path, cjk_bytes)) = load_font(CJK_FONT_PATHS) else {
        tracing::warn!("no CJK UI font found, Chinese text may render as missing glyph boxes");
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        CJK_FONT_NAME.to_string(),
        Arc::new(FontData::from_owned(cjk_bytes)),
    );
    // 比例字体把 CJK 放最前, 保证界面中文优先用系统字体.
    prepend_named_font(&mut fonts, FontFamily::Proportional, CJK_FONT_NAME);
    // 等宽栈必须先走真正的等宽字体, 否则 code_editor 的 ASCII 会被 PingFang 拉成比例宽.
    if let Some((mono_path, mono_bytes)) = load_font(MONO_FONT_PATHS) {
        fonts.font_data.insert(
            MONO_FONT_NAME.to_string(),
            Arc::new(FontData::from_owned(mono_bytes)),
        );
        prepend_named_font(&mut fonts, FontFamily::Monospace, MONO_FONT_NAME);
        tracing::info!(path = %mono_path.display(), "installed monospace UI font");
    }
    append_named_font(&mut fonts, FontFamily::Monospace, CJK_FONT_NAME);
    ctx.set_fonts(fonts);
    tracing::info!(path = %cjk_path.display(), "installed CJK UI font");
}

fn scale_text_styles(ctx: &Context) {
    ctx.all_styles_mut(|style| {
        for font_id in style.text_styles.values_mut() {
            font_id.size *= UI_FONT_SCALE;
        }
    });
}

fn load_font(paths: &'static [&'static str]) -> Option<(&'static Path, Vec<u8>)> {
    paths
        .iter()
        .map(Path::new)
        .find_map(|path| fs::read(path).ok().map(|bytes| (path, bytes)))
}

fn prepend_named_font(fonts: &mut FontDefinitions, family: FontFamily, name: &str) {
    fonts
        .families
        .entry(family)
        .or_default()
        .insert(0, name.to_string());
}

fn append_named_font(fonts: &mut FontDefinitions, family: FontFamily, name: &str) {
    fonts
        .families
        .entry(family)
        .or_default()
        .push(name.to_string());
}
