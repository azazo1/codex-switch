use super::{CodexSwitchApp, tokens};
use crate::core::models::RequestLog;
use chrono::Local;
use eframe::egui;

impl CodexSwitchApp {
    pub(super) fn logs_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("最近请求");
        self.log_pagination_ui(ui);
        let mut token_display_mode = self.token_display_mode;
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("recent_logs_grid")
                    .striped(true)
                    .num_columns(8)
                    .spacing([28.0, 10.0])
                    .show(ui, |ui| {
                        ui.strong("上游");
                        ui.strong("模型");
                        ui.strong("推理强度");
                        ui.strong("TOKEN");
                        ui.strong("费用");
                        ui.strong("首 TOKEN");
                        ui.strong("耗时");
                        ui.strong("时间");
                        ui.end_row();

                        for (index, log) in self.logs.iter().enumerate() {
                            ui.label(upstream_text(log))
                                .on_hover_text(log_hover_text(log));
                            let model_response = ui.label(model_text(log));
                            model_response.on_hover_text(log_hover_text(log));
                            ui.label(log.reasoning_effort.as_deref().unwrap_or("-"));
                            log_token_cell(ui, &mut token_display_mode, log);
                            log_cost_cell(
                                ui,
                                self.log_estimated_cost_usd.get(index).copied().flatten(),
                            );
                            ui.label(format_optional_duration(log.first_token_ms));
                            ui.label(format_duration(log.duration_ms));
                            ui.label(format_log_time(log));
                            ui.end_row();
                        }
                    });
            });
        self.token_display_mode = token_display_mode;
    }

    fn log_pagination_ui(&mut self, ui: &mut egui::Ui) {
        let total_pages = log_total_pages(self.log_total_count, self.log_page_size);
        let (range_start, range_end) = log_result_range(
            self.log_page,
            self.log_page_size,
            self.log_total_count,
        );
        let mut target_page = None;
        let mut target_page_size = None;
        ui.horizontal(|ui| {
            ui.label(format!(
                "显示 {range_start} 至 {range_end} 共 {} 条结果",
                self.log_total_count
            ));
            ui.label("每页:");
            let mut page_size = self.log_page_size;
            egui::ComboBox::from_id_salt("log_page_size")
                .selected_text(page_size.to_string())
                .show_ui(ui, |ui| {
                    for value in [10, 20, 50, 100] {
                        if ui
                            .selectable_value(&mut page_size, value, value.to_string())
                            .changed()
                        {
                            target_page_size = Some(value);
                        }
                    }
                });
            if ui
                .add_enabled(self.log_page > 0, egui::Button::new("<"))
                .clicked()
            {
                target_page = Some(self.log_page - 1);
            }
            for item in log_page_items(self.log_page, total_pages) {
                match item {
                    Some(page) => {
                        let selected = page == self.log_page;
                        if ui
                            .selectable_label(selected, (page + 1).to_string())
                            .clicked()
                        {
                            target_page = Some(page);
                        }
                    }
                    None => {
                        ui.label("...");
                    }
                }
            }
            if ui
                .add_enabled(self.log_page + 1 < total_pages, egui::Button::new(">"))
                .clicked()
            {
                target_page = Some(self.log_page + 1);
            }
        });
        if let Some(page_size) = target_page_size {
            self.log_page_size = page_size;
            self.log_page = 0;
            self.refresh_all();
        } else if let Some(page) = target_page {
            self.log_page = page;
            self.refresh_all();
        }
    }
}

fn log_result_range(page: usize, page_size: usize, total_count: i64) -> (i64, i64) {
    if total_count <= 0 || page_size == 0 {
        return (0, 0);
    }
    let start = (page * page_size) as i64 + 1;
    let end = ((page + 1) * page_size) as i64;
    (start.min(total_count), end.min(total_count))
}

fn log_page_items(current_page: usize, total_pages: usize) -> Vec<Option<usize>> {
    if total_pages == 0 {
        return Vec::new();
    }
    if total_pages <= 6 {
        return (0..total_pages).map(Some).collect();
    }
    if current_page <= 2 {
        return vec![Some(0), Some(1), Some(2), None, Some(total_pages - 1)];
    }
    if current_page + 3 >= total_pages {
        return vec![
            Some(0),
            None,
            Some(total_pages - 3),
            Some(total_pages - 2),
            Some(total_pages - 1),
        ];
    }
    vec![
        Some(0),
        None,
        Some(current_page - 1),
        Some(current_page),
        Some(current_page + 1),
        None,
        Some(total_pages - 1),
    ]
}

fn log_total_pages(total_count: i64, page_size: usize) -> usize {
    if total_count <= 0 || page_size == 0 {
        return 0;
    }
    (total_count as usize).div_ceil(page_size)
}

fn log_token_cell(ui: &mut egui::Ui, mode: &mut tokens::TokenDisplayMode, log: &RequestLog) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            tokens::token_value(ui, mode, "输入", log.usage.input_tokens);
            tokens::token_value(ui, mode, "输出", log.usage.output_tokens);
        });
        ui.horizontal(|ui| {
            tokens::token_value(ui, mode, "缓存输入", log.usage.cache_read_tokens);
            if log.usage.cache_creation_tokens > 0 {
                tokens::token_value(ui, mode, "写入缓存", log.usage.cache_creation_tokens);
            }
        });
    });
}

fn log_cost_cell(ui: &mut egui::Ui, cost: Option<f64>) {
    match cost {
        Some(value) => {
            ui.label(tokens::format_usd(value));
        }
        None => {
            ui.label("-").on_hover_text("无价格缓存");
        }
    }
}

fn model_text(log: &RequestLog) -> String {
    let mut text = log.model.clone().unwrap_or_else(|| "-".to_string());
    if log.status >= 400 {
        text.push_str(" / 错误");
    }
    text
}

fn upstream_text(log: &RequestLog) -> String {
    log.upstream_name.clone().unwrap_or_else(|| "-".to_string())
}

fn log_hover_text(log: &RequestLog) -> String {
    let mut lines = Vec::new();
    if let Some(upstream) = &log.upstream_name {
        lines.push(format!("上游: {upstream}"));
    }
    lines.push(format!("endpoint: {}", log.endpoint));
    lines.push(format!("status: {}", log.status));
    if let Some(error) = &log.error
        && !error.is_empty()
    {
        lines.push(format!("error: {error}"));
    }
    lines.join("\n")
}

fn format_optional_duration(value: Option<i64>) -> String {
    value.map(format_duration).unwrap_or_else(|| "-".to_string())
}

fn format_duration(value: i64) -> String {
    if value < 1000 {
        format!("{value}ms")
    } else {
        format!("{:.2}s", value as f64 / 1000.0)
    }
}

fn format_log_time(log: &RequestLog) -> String {
    log.ts
        .map(|ts| {
            ts.with_timezone(&Local)
                .format("%Y/%m/%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "-".to_string())
}
