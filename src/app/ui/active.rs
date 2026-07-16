use super::CodexSwitchApp;
use crate::live::{
    LiveOutputSettings, LiveRequestSnapshot, LiveResponseState, MAX_COMPLETED_HOLD_SECONDS,
    MAX_SCROLL_CHARS_PER_SECOND, MIN_SCROLL_CHARS_PER_SECOND,
};
use chrono::{Local, Utc};
use eframe::egui;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

mod scroll;

pub(super) use scroll::LiveTailScrollState;

const LIVE_TAIL_MIN_WIDTH: f32 = 200.0;
const LIVE_TAIL_MAX_WIDTH: f32 = 640.0;
const LIVE_TAIL_RESERVED_WIDTH: f32 = 700.0;
const LIVE_GRID_COLUMN_SPACING: f32 = 18.0;
const LIVE_OUTPUT_SPEED_WIDTH: f32 = 104.0;
const APPROX_CHAR_WIDTH: f32 = 9.0;
const LIVE_HOVER_MIN_WIDTH: f32 = 480.0;
const LIVE_HOVER_MAX_HEIGHT: f32 = 320.0;
const LIVE_RATE_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const LIVE_ANIMATION_INTERVAL: Duration = Duration::from_millis(16);
const LIVE_BACKGROUND_INTERVAL: Duration = Duration::from_millis(250);

impl CodexSwitchApp {
    pub(super) fn update_live_connections(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let live_version = self.state.events.live_stream_version();
        let version_changed = live_version != self.last_seen_live_stream_version;
        let rate_refresh_due = !self.live_connections.is_empty()
            && now.saturating_duration_since(self.last_live_output_rate_refresh_at)
                >= LIVE_RATE_REFRESH_INTERVAL;

        if version_changed || rate_refresh_due {
            self.last_seen_live_stream_version = live_version;
            self.last_live_output_rate_refresh_at = now;
            let snapshots = self.state.live_requests.snapshots();
            let ids = snapshots
                .iter()
                .map(|item| item.id.clone())
                .collect::<BTreeSet<_>>();
            for item in &snapshots {
                self.live_tail_scroll_states
                    .entry(item.id.clone())
                    .or_insert_with(|| {
                        LiveTailScrollState::new(item, self.live_output_settings, now)
                    })
                    .sync(item, self.live_output_settings, now);
            }
            self.live_tail_scroll_states
                .retain(|request_id, _| ids.contains(request_id));
            self.live_connections = snapshots;
        } else {
            for item in &self.live_connections {
                if let Some(scroll) = self.live_tail_scroll_states.get_mut(&item.id) {
                    scroll.advance(self.live_output_settings, now);
                    scroll.update_reached_end(item.finished_at.is_some(), now);
                }
            }
        }

        let completed_hold_seconds = self.live_output_settings.completed_hold_seconds;
        let remove_ids = self
            .live_connections
            .iter()
            .filter(|item| item.finished_at.is_some())
            .filter_map(|item| {
                self.live_tail_scroll_states
                    .get(&item.id)
                    .filter(|scroll| scroll.hold_finished(now, completed_hold_seconds))
                    .map(|_| item.id.clone())
            })
            .collect::<Vec<_>>();
        for request_id in &remove_ids {
            self.state.live_requests.remove_finished(request_id);
            self.live_tail_scroll_states.remove(request_id);
        }
        if !remove_ids.is_empty() {
            self.live_connections
                .retain(|item| !remove_ids.contains(&item.id));
        }

        let needs_scroll = self
            .live_tail_scroll_states
            .values()
            .any(LiveTailScrollState::needs_scroll);
        let needs_hold = self
            .live_connections
            .iter()
            .any(|item| item.finished_at.is_some());
        let needs_rate_refresh = self.live_connections.iter().any(|item| {
            item.finished_at.is_none()
                && item.response_state == LiveResponseState::Streaming
                && item.output_rate.is_some()
        });
        if needs_scroll || needs_hold || needs_rate_refresh {
            let interval = if self.tab == super::Tab::ActiveConnections
                && !self.window_hidden_to_tray
                && needs_scroll
            {
                LIVE_ANIMATION_INTERVAL
            } else {
                LIVE_BACKGROUND_INTERVAL
            };
            ctx.request_repaint_after(interval);
        }
    }

    pub(super) fn active_connections_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("活跃连接");
        self.live_output_settings_ui(ui);
        ui.separator();
        if self.live_connections.is_empty() {
            ui.label("当前没有活跃请求");
            return;
        }

        let tail_width = live_tail_width(ui.available_width());
        let settings = self.live_output_settings;
        let live_connections = &self.live_connections;
        let scroll_states = &mut self.live_tail_scroll_states;
        let mut terminate_request_id = None;
        egui::ScrollArea::vertical()
            .id_salt("active_connections")
            .max_height(ui.available_height())
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("active_connections_grid")
                    .striped(true)
                    .num_columns(9)
                    .spacing([LIVE_GRID_COLUMN_SPACING, 10.0])
                    .show(ui, |ui| {
                        ui.strong("上游");
                        ui.strong("模型");
                        ui.strong("推理强度");
                        ui.strong("endpoint");
                        ui.strong("实时输出窗口");
                        ui.strong("实时输出速度");
                        ui.strong("已持续");
                        ui.strong("开始时间");
                        ui.strong("操作");
                        ui.end_row();

                        for item in live_connections {
                            let finished = item.finished_at.is_some();
                            row_label(ui, item.upstream_name.as_deref().unwrap_or("-"), finished)
                                .on_hover_text(format!("id: {}", item.id));
                            row_label(ui, item.model.as_deref().unwrap_or("-"), finished);
                            row_label(
                                ui,
                                item.reasoning_effort.as_deref().unwrap_or("-"),
                                finished,
                            );
                            row_label(ui, &item.endpoint, finished);
                            if let Some(scroll) = scroll_states.get_mut(&item.id) {
                                live_tail_label(ui, item, scroll, settings, tail_width, finished);
                            } else {
                                row_label(ui, "-", finished);
                            }
                            output_speed_label(ui, item, finished);
                            row_label(ui, format_elapsed(item), finished);
                            row_label(ui, format_started_at(item), finished);
                            if ui
                                .add_enabled(
                                    !item.terminating && !finished,
                                    egui::Button::new(terminate_text(item)),
                                )
                                .on_hover_text(terminate_hover_text(item))
                                .clicked()
                            {
                                terminate_request_id = Some(item.id.clone());
                            }
                            ui.end_row();
                        }
                    });
            });
        if let Some(request_id) = terminate_request_id {
            self.terminate_live_connection(&request_id);
        }
    }

    fn live_output_settings_ui(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            changed |= ui
                .checkbox(
                    &mut self.live_output_settings.scroll_limit_enabled,
                    "限制最快滚动速度",
                )
                .changed();
            ui.add_enabled_ui(
                self.live_output_settings.scroll_limit_enabled,
                |ui| {
                    changed |= ui
                        .add(
                            egui::DragValue::new(
                                &mut self.live_output_settings.max_scroll_chars_per_second,
                            )
                            .range(MIN_SCROLL_CHARS_PER_SECOND..=MAX_SCROLL_CHARS_PER_SECOND)
                            .speed(1)
                            .suffix(" 字符/秒"),
                        )
                        .changed();
                },
            );
            ui.label("末尾保留时间");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut self.live_output_settings.completed_hold_seconds)
                        .range(0..=MAX_COMPLETED_HOLD_SECONDS)
                        .speed(1)
                        .suffix(" 秒"),
                )
                .changed();
        });
        if changed {
            self.live_output_settings = self.live_output_settings.normalized();
            ui.ctx().request_repaint();
        }
    }

    fn terminate_live_connection(&mut self, request_id: &str) {
        if self.state.live_requests.terminate(request_id) {
            self.status = "正在终止活跃请求".to_string();
            self.live_connections = self.state.live_requests.snapshots();
            self.state.events.bump_live_streams();
        } else {
            self.status = "活跃请求已经结束".to_string();
        }
    }
}

pub(super) fn active_connection_count(items: &[LiveRequestSnapshot]) -> usize {
    items
        .iter()
        .filter(|item| item.finished_at.is_none())
        .count()
}

fn live_tail_width(available_width: f32) -> f32 {
    if !available_width.is_finite() {
        return LIVE_TAIL_MIN_WIDTH;
    }
    (available_width - LIVE_TAIL_RESERVED_WIDTH).clamp(LIVE_TAIL_MIN_WIDTH, LIVE_TAIL_MAX_WIDTH)
}

fn live_tail_label(
    ui: &mut egui::Ui,
    item: &LiveRequestSnapshot,
    scroll: &mut LiveTailScrollState,
    settings: LiveOutputSettings,
    width: f32,
    finished: bool,
) {
    let max_chars = (width / APPROX_CHAR_WIDTH).floor().max(8.0) as usize;
    scroll.set_window_chars(item, max_chars, settings, Instant::now());
    let text = match item.response_state {
        LiveResponseState::AwaitingHeaders => "请求中",
        LiveResponseState::NonStreaming => "非流式",
        LiveResponseState::Streaming if item.tail.is_empty() => "等待输出",
        LiveResponseState::Streaming => visible_tail_window(item, scroll, max_chars),
    };
    let response = ui.add_sized(
        [width, ui.spacing().interact_size.y],
        egui::Label::new(row_text(ui, text, finished)).truncate(),
    );
    let hover_text = if item.hover_output.is_empty() {
        text
    } else {
        item.hover_output.as_str()
    };
    response.on_hover_ui(|ui| {
        ui.set_min_width(LIVE_HOVER_MIN_WIDTH);
        ui.style_mut().interaction.selectable_labels = true;
        egui::ScrollArea::vertical()
            .id_salt(("live_output_hover", item.id.as_str()))
            .max_height(LIVE_HOVER_MAX_HEIGHT)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.add(egui::Label::new(row_text(ui, hover_text, finished)).wrap());
            });
    });
}

fn output_speed_label(ui: &mut egui::Ui, item: &LiveRequestSnapshot, finished: bool) {
    let Some(rate) = item.output_rate else {
        ui.add_sized(
            [LIVE_OUTPUT_SPEED_WIDTH, ui.spacing().interact_size.y],
            egui::Label::new(row_text(ui, "-", finished)),
        );
        return;
    };
    ui.add_sized(
        [LIVE_OUTPUT_SPEED_WIDTH, ui.spacing().interact_size.y],
        egui::Label::new(row_text(
            ui,
            format!("~{:.1} tps", rate.estimated_tokens_per_second),
            finished,
        )),
    )
    .on_hover_text(format!("字符速度: {:.1} 字符/秒", rate.chars_per_second));
}

fn visible_tail_window<'a>(
    item: &'a LiveRequestSnapshot,
    scroll: &LiveTailScrollState,
    max_chars: usize,
) -> &'a str {
    let end = (scroll.visible_end_char_index().floor() as usize)
        .clamp(item.tail_start_char_index, item.tail_end_char_index);
    let start = end
        .saturating_sub(max_chars)
        .max(item.tail_start_char_index);
    let relative_start = start.saturating_sub(item.tail_start_char_index);
    let relative_end = end.saturating_sub(item.tail_start_char_index);
    char_slice(&item.tail, relative_start, relative_end)
}

fn char_slice(text: &str, start: usize, end: usize) -> &str {
    let start_byte = char_byte_index(text, start);
    let end_byte = char_byte_index(text, end);
    &text[start_byte..end_byte]
}

fn char_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

fn row_label(
    ui: &mut egui::Ui,
    text: impl Into<String>,
    finished: bool,
) -> egui::Response {
    ui.label(row_text(ui, text, finished))
}

fn row_text(ui: &egui::Ui, text: impl Into<String>, finished: bool) -> egui::RichText {
    let text = egui::RichText::new(text);
    if finished {
        text.color(ui.visuals().weak_text_color())
    } else {
        text
    }
}

fn terminate_text(item: &LiveRequestSnapshot) -> &'static str {
    if item.finished_at.is_some() {
        "已关闭"
    } else if item.terminating {
        "终止中"
    } else {
        "终止"
    }
}

fn terminate_hover_text(item: &LiveRequestSnapshot) -> &'static str {
    if item.finished_at.is_some() {
        "连接已经关闭"
    } else {
        "直接终止该活跃请求"
    }
}

fn format_elapsed(item: &LiveRequestSnapshot) -> String {
    let ended_at = item.finished_at.unwrap_or_else(Utc::now);
    let seconds = (ended_at - item.started_at).num_seconds().max(0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::LiveOutputRate;

    fn snapshot(tail: &str, finished: bool) -> LiveRequestSnapshot {
        LiveRequestSnapshot {
            id: "request-a".to_string(),
            upstream_name: None,
            endpoint: "/responses".to_string(),
            model: Some("gpt-5".to_string()),
            reasoning_effort: None,
            response_state: LiveResponseState::Streaming,
            tail: tail.to_string(),
            tail_start_char_index: 0,
            tail_end_char_index: tail.chars().count(),
            hover_output: tail.to_string(),
            output_rate: Some(LiveOutputRate {
                estimated_tokens_per_second: 10.0,
                chars_per_second: 40.0,
            }),
            started_at: Utc::now(),
            finished_at: finished.then(Utc::now),
            terminating: false,
        }
    }

    #[test]
    fn active_count_excludes_closed_rows() {
        let items = vec![snapshot("active", false), snapshot("closed", true)];

        assert_eq!(active_connection_count(&items), 1);
    }
}
