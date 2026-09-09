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

pub(super) fn usage_tokens(ui: &mut egui::Ui, mode: &mut TokenDisplayMode, usage: &TokenUsage) {
    token_value(ui, mode, "输入", usage.input_tokens);
    token_value(ui, mode, "缓存输入", usage.cache_read_tokens);
    if usage.cache_creation_tokens > 0 {
        token_value(ui, mode, "写入缓存", usage.cache_creation_tokens);
    }
    token_value(ui, mode, "输出", usage.output_tokens);
    token_value(ui, mode, "总计", usage.total_tokens);
}

pub(super) fn token_value(ui: &mut egui::Ui, mode: &mut TokenDisplayMode, label: &str, value: i64) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CurrencyDisplayMode {
    Usd,
    Cny,
}

impl CurrencyDisplayMode {
    fn toggle(&mut self) {
        *self = match self {
            Self::Usd => Self::Cny,
            Self::Cny => Self::Usd,
        };
    }
}

pub(super) fn estimated_cost(
    ui: &mut egui::Ui,
    mode: &mut CurrencyDisplayMode,
    rate: Option<crate::pricing::fx::UsdCnyRate>,
    label: &str,
    value: Option<f64>,
) {
    match value {
        Some(value) => {
            let text = format!("{label}: {}", format_cost(value, *mode, rate));
            let response = ui
                .add(egui::Label::new(text).sense(egui::Sense::click()))
                .on_hover_text(cost_hover_text(*mode, rate));
            if response.clicked() {
                mode.toggle();
            }
        }
        None => {
            ui.label(format!("{label}: 无价格缓存"));
        }
    }
}

pub(super) fn cost_value(
    ui: &mut egui::Ui,
    mode: &mut CurrencyDisplayMode,
    rate: Option<crate::pricing::fx::UsdCnyRate>,
    value: f64,
) {
    let response = ui
        .add(egui::Label::new(format_cost(value, *mode, rate)).sense(egui::Sense::click()))
        .on_hover_text(cost_hover_text(*mode, rate));
    if response.clicked() {
        mode.toggle();
    }
}

fn cost_hover_text(
    mode: CurrencyDisplayMode,
    rate: Option<crate::pricing::fx::UsdCnyRate>,
) -> String {
    match (mode, rate) {
        (CurrencyDisplayMode::Usd, Some(rate)) => {
            format!("汇率 1 USD = {:.4} CNY, 点击切换为人民币显示", rate.rate)
        }
        (CurrencyDisplayMode::Usd, None) => {
            "尚未获取汇率, 可通过\"获取模型信息\"按钮获取, 点击切换显示".to_string()
        }
        (CurrencyDisplayMode::Cny, Some(rate)) => {
            format!("汇率 1 USD = {:.4} CNY, 点击切换为美元显示", rate.rate)
        }
        (CurrencyDisplayMode::Cny, None) => "尚未获取汇率, 暂时显示美元, 点击切换显示".to_string(),
    }
}

pub(super) fn format_cost(
    value: f64,
    mode: CurrencyDisplayMode,
    rate: Option<crate::pricing::fx::UsdCnyRate>,
) -> String {
    match mode {
        CurrencyDisplayMode::Usd => format_usd(value),
        CurrencyDisplayMode::Cny => match rate {
            Some(rate) => format_amount(value * rate.rate, "¥"),
            None => format_usd(value),
        },
    }
}

pub(super) fn format_tokens(mode: TokenDisplayMode, value: i64) -> String {
    match mode {
        TokenDisplayMode::Raw => value.to_string(),
        TokenDisplayMode::Human => human_tokens(value),
    }
}

pub(super) fn format_usd(value: f64) -> String {
    format_amount(value, "$")
}

fn format_amount(value: f64, prefix: &str) -> String {
    if value == 0.0 {
        format!("{prefix}0")
    } else if value.abs() < 0.0001 {
        format!("{prefix}{value:.6}")
    } else if value.abs() < 1.0 {
        format!("{prefix}{value:.4}")
    } else {
        format!("{prefix}{value:.2}")
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
    use super::{CurrencyDisplayMode, TokenDisplayMode, format_cost, format_tokens, format_usd};
    use crate::pricing::fx::UsdCnyRate;

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

    #[test]
    fn formats_cost_by_currency_mode() {
        let rate = Some(UsdCnyRate {
            rate: 7.2,
            fetched_at: 0,
        });
        assert_eq!(format_cost(1.5, CurrencyDisplayMode::Usd, rate), "$1.50");
        assert_eq!(format_cost(1.5, CurrencyDisplayMode::Cny, rate), "¥10.80");
        assert_eq!(format_cost(1.5, CurrencyDisplayMode::Cny, None), "$1.50");
        assert_eq!(
            format_cost(0.0000123, CurrencyDisplayMode::Cny, rate),
            "¥0.000089"
        );
    }
}
