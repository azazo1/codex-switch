use super::CodexSwitchApp;
use crate::live::LiveRequestSnapshot;
use chrono::{Local, Utc};
use eframe::egui;

const LIVE_TAIL_MIN_WIDTH: f32 = 220.0;
const LIVE_TAIL_MAX_WIDTH: f32 = 720.0;
const LIVE_TAIL_RESERVED_WIDTH: f32 = 520.0;
const APPROX_CHAR_WIDTH: f32 = 9.0;

impl CodexSwitchApp {
    pub(super) fn active_connections_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("活跃连接");
        if self.live_connections.is_empty() {
            ui.label("当前没有活跃流式请求");
            return;
        }
        let tail_width = live_tail_width(ui.available_width());
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("active_connections_grid")
                    .striped(true)
                    .num_columns(7)
                    .spacing([24.0, 10.0])
                    .show(ui, |ui| {
                        ui.strong("上游");
                        ui.strong("模型");
                        ui.strong("推理强度");
                        ui.strong("endpoint");
                        ui.strong("实时输出窗口");
                        ui.strong("已持续");
                        ui.strong("开始时间");
                        ui.end_row();

                        for item in &self.live_connections {
                            ui.label(item.upstream_name.as_deref().unwrap_or("-"))
                                .on_hover_text(format!("id: {}", item.id));
                            ui.label(item.model.as_deref().unwrap_or("-"));
                            ui.label(item.reasoning_effort.as_deref().unwrap_or("-"));
                            ui.label(&item.endpoint);
                            live_tail_label(ui, item, tail_width);
                            ui.label(format_elapsed(item));
                            ui.label(format_started_at(item));
                            ui.end_row();
                        }
                    });
            });
    }
}

fn live_tail_width(available_width: f32) -> f32 {
    if !available_width.is_finite() {
        return LIVE_TAIL_MIN_WIDTH;
    }
    (available_width - LIVE_TAIL_RESERVED_WIDTH).clamp(LIVE_TAIL_MIN_WIDTH, LIVE_TAIL_MAX_WIDTH)
}

fn live_tail_label(ui: &mut egui::Ui, item: &LiveRequestSnapshot, width: f32) {
    let text = if item.tail.is_empty() {
        "等待输出"
    } else {
        item.tail.as_str()
    };
    let max_chars = (width / APPROX_CHAR_WIDTH).floor().max(8.0) as usize;
    let visible = tail_window(text, max_chars);
    ui.add_sized(
        [width, ui.spacing().interact_size.y],
        egui::Label::new(visible).truncate(),
    )
    .on_hover_text(text);
}

fn tail_window(text: &str, max_chars: usize) -> &str {
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text;
    }
    let skip_chars = total_chars - max_chars;
    text.char_indices()
        .nth(skip_chars)
        .map(|(index, _)| &text[index..])
        .unwrap_or(text)
}

fn format_elapsed(item: &LiveRequestSnapshot) -> String {
    let seconds = (Utc::now() - item.started_at).num_seconds().max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

fn format_started_at(item: &LiveRequestSnapshot) -> String {
    item.started_at
        .with_timezone(&Local)
        .format("%H:%M:%S")
        .to_string()
}
