use eframe::egui;

pub(super) fn measure_text_width(ui: &egui::Ui, font_id: &egui::FontId, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font_id.clone(), egui::Color32::WHITE)
        .size()
        .x
}

/// 过长时保留末尾并补前导省略号, 不需要截断时返回 None.
pub(super) fn elide_to_tail(ui: &egui::Ui, text: &str, max_width: f32) -> Option<String> {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let measure = |value: &str| measure_text_width(ui, &font_id, value);
    if measure(text) <= max_width {
        return None;
    }
    const ELLIPSIS: &str = "…";
    let budget = max_width - measure(ELLIPSIS);
    let mut keep_from = text.len();
    for (ch_idx, _) in text.char_indices() {
        if measure(&text[ch_idx..]) <= budget {
            keep_from = ch_idx;
            break;
        }
    }
    Some(format!("{ELLIPSIS}{}", &text[keep_from..]))
}

/// 过长时保留开头并补末尾省略号, 不需要截断时返回 None.
pub(super) fn elide_to_head(ui: &egui::Ui, text: &str, max_width: f32) -> Option<String> {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let measure = |value: &str| measure_text_width(ui, &font_id, value);
    if measure(text) <= max_width {
        return None;
    }
    const ELLIPSIS: &str = "…";
    let budget = max_width - measure(ELLIPSIS);
    let mut keep_to = 0;
    for (ch_idx, ch) in text.char_indices() {
        let next = ch_idx + ch.len_utf8();
        if measure(&text[..next]) > budget {
            break;
        }
        keep_to = next;
    }
    Some(format!("{}{ELLIPSIS}", &text[..keep_to]))
}

pub(super) fn truncated_head_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let display = elide_to_head(ui, text, ui.available_width()).unwrap_or_else(|| text.to_string());
    ui.add(egui::Label::new(display))
}
