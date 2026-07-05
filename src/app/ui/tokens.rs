use crate::core::models::TokenUsage;
use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenDisplayMode {
    Human,
    Raw,
}

impl TokenDisplayMode {
    fn toggle(&mut self) {
        *self = match self {
            Self::Human => Self::Raw,
            Self::Raw => Self::Human,
        };
    }
}

pub(super) fn usage_tokens(
    ui: &mut egui::Ui,
    mode: &mut TokenDisplayMode,
    usage: &TokenUsage,
) {
    token_value(ui, mode, "输入", usage.input_tokens);
    token_value(ui, mode, "缓存输入", usage.cache_read_tokens);
    if usage.cache_creation_tokens > 0 {
        token_value(ui, mode, "写入缓存", usage.cache_creation_tokens);
    }
    token_value(ui, mode, "输出", usage.output_tokens);
    token_value(ui, mode, "总计", usage.total_tokens);
}

pub(super) fn token_value(
    ui: &mut egui::Ui,
    mode: &mut TokenDisplayMode,
    label: &str,
    value: i64,
) {
    let text = format!("{label}: {}", format_tokens(*mode, value));
    let response = ui
        .add(egui::Label::new(text).sense(egui::Sense::click()))
        .on_hover_text("点击切换令牌显示格式");
    if response.clicked() {
        mode.toggle();
    }
}

pub(super) fn token_number(ui: &mut egui::Ui, mode: &mut TokenDisplayMode, value: i64) {
    let response = ui
        .add(egui::Label::new(format_tokens(*mode, value)).sense(egui::Sense::click()))
        .on_hover_text("点击切换令牌显示格式");
    if response.clicked() {
        mode.toggle();
    }
}

pub(super) fn estimated_cost(ui: &mut egui::Ui, label: &str, value: Option<f64>) {
    match value {
        Some(value) => {
            ui.label(format!("{label}: {}", format_usd(value)));
        }
        None => {
            ui.label(format!("{label}: 无价格缓存"));
        }
    }
}

pub(super) fn format_tokens(mode: TokenDisplayMode, value: i64) -> String {
    match mode {
        TokenDisplayMode::Raw => value.to_string(),
        TokenDisplayMode::Human => human_tokens(value),
    }
}

pub(super) fn format_usd(value: f64) -> String {
    if value == 0.0 {
        "$0".to_string()
    } else if value.abs() < 0.0001 {
        format!("${value:.6}")
    } else if value.abs() < 1.0 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

fn human_tokens(value: i64) -> String {
    let abs = value.abs();
    if abs < 1_000 {
        return value.to_string();
    }
    let (unit, scale) = if abs < 1_000_000 {
        ("K", 1_000.0)
    } else if abs < 1_000_000_000 {
        ("M", 1_000_000.0)
    } else {
        ("B", 1_000_000_000.0)
    };
    let number = value as f64 / scale;
    let digits = if number.abs() < 10.0 { 1 } else { 0 };
    format!("{number:.digits$}{unit}")
}

#[cfg(test)]
mod tests {
    use super::{TokenDisplayMode, format_tokens, format_usd};

    #[test]
    fn formats_tokens_for_human_and_raw_modes() {
        assert_eq!(format_tokens(TokenDisplayMode::Raw, 12345), "12345");
        assert_eq!(format_tokens(TokenDisplayMode::Human, 12345), "12K");
        assert_eq!(format_tokens(TokenDisplayMode::Human, 1234), "1.2K");
    }

    #[test]
    fn formats_small_costs() {
        assert_eq!(format_usd(0.0), "$0");
        assert_eq!(format_usd(0.0000123), "$0.000012");
        assert_eq!(format_usd(0.12345), "$0.1235");
    }
}
