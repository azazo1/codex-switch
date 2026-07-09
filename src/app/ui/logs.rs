use super::{
    CodexSwitchApp, F64RangeFilter, I64RangeFilter, LogDateTimeFilter, LogRetentionChoice,
    LogStatusFilter, tokens,
};
use crate::core::models::{RequestLog, Upstream};
use crate::storage::RequestLogRetention;
use chrono::{Duration, Local, Utc};
use eframe::egui;
use std::collections::BTreeSet;

const LOG_RANGE_LABEL_WIDTH: f32 = 220.0;
const LOG_PAGE_BUTTON_WIDTH: f32 = 32.0;
const LOG_PAGE_SLOT_COUNT: usize = 7;

impl CodexSwitchApp {
    pub(super) fn logs_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("最近请求");
            self.log_filter_button(ui);
            self.log_cleanup_button(ui);
        });
        self.log_filter_window(ui.ctx());
        self.log_cleanup_window(ui.ctx());
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

    fn log_cleanup_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("清理日志")
            .on_hover_text("打开日志清理选项")
            .clicked()
        {
            self.log_cleanup_open = true;
        }
    }

    fn log_filter_button(&mut self, ui: &mut egui::Ui) {
        let active_count = self.log_filter_applied.active_count();
        let label = if active_count > 0 {
            format!("筛选日志 ({active_count})")
        } else {
            "筛选日志".to_string()
        };
        if ui.button(label).on_hover_text("打开日志筛选选项").clicked() {
            self.log_filter_editor = self.log_filter_applied.clone();
            self.log_filter_open = true;
        }
        if ui
            .add_enabled(self.log_filter_applied.is_active(), egui::Button::new("清空筛选"))
            .on_hover_text("清空当前日志筛选条件")
            .clicked()
        {
            self.clear_log_filter();
        }
    }

    fn log_filter_window(&mut self, ctx: &egui::Context) {
        if !self.log_filter_open {
            return;
        }
        let mut open = self.log_filter_open;
        let mut apply_requested = false;
        let mut clear_requested = false;
        let mut cancel_requested = false;
        let model_options = log_model_options(&self.logs);
        let upstream_options = log_upstream_options(&self.logs, &self.upstreams);
        let reasoning_effort_options = log_reasoning_effort_options(&self.logs);
        let endpoint_options = log_endpoint_options(&self.logs);
        egui::Window::new("筛选日志")
            .collapsible(false)
            .resizable(true)
            .default_width(720.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("log_filter_grid")
                    .num_columns(3)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        option_filter_row(
                            ui,
                            "模型",
                            &mut self.log_filter_editor.model,
                            &model_options,
                            "全部模型",
                        );
                        option_filter_row(
                            ui,
                            "上游",
                            &mut self.log_filter_editor.upstream,
                            &upstream_options,
                            "全部上游",
                        );
                        option_filter_row(
                            ui,
                            "推理强度",
                            &mut self.log_filter_editor.reasoning_effort,
                            &reasoning_effort_options,
                            "全部强度",
                        );
                        option_filter_row(
                            ui,
                            "endpoint",
                            &mut self.log_filter_editor.endpoint,
                            &endpoint_options,
                            "全部 endpoint",
                        );
                        status_filter_row(ui, &mut self.log_filter_editor.status, &mut self.log_filter_editor.status_custom);
                        date_time_filter_row(
                            ui,
                            "开始时间",
                            &mut self.log_filter_editor.started_at,
                        );
                        date_time_filter_row(
                            ui,
                            "结束时间",
                            &mut self.log_filter_editor.ended_at,
                        );
                        f64_range_filter_row(ui, "价格 USD", &mut self.log_filter_editor.price_usd);
                        i64_range_filter_row(
                            ui,
                            "耗时 ms",
                            &mut self.log_filter_editor.duration_ms,
                        );
                        i64_range_filter_row(
                            ui,
                            "首 token ms",
                            &mut self.log_filter_editor.first_token_ms,
                        );
                        i64_range_filter_row(
                            ui,
                            "输入 tokens",
                            &mut self.log_filter_editor.input_tokens,
                        );
                        i64_range_filter_row(
                            ui,
                            "输出 tokens",
                            &mut self.log_filter_editor.output_tokens,
                        );
                        i64_range_filter_row(
                            ui,
                            "缓存输入 tokens",
                            &mut self.log_filter_editor.cache_read_tokens,
                        );
                        i64_range_filter_row(
                            ui,
                            "写入缓存 tokens",
                            &mut self.log_filter_editor.cache_creation_tokens,
                        );
                        i64_range_filter_row(
                            ui,
                            "总 tokens",
                            &mut self.log_filter_editor.total_tokens,
                        );
                    });
                ui.horizontal(|ui| {
                    if ui.button("应用").clicked() {
                        apply_requested = true;
                    }
                    if ui.button("清空").clicked() {
                        clear_requested = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel_requested = true;
                    }
                });
            });
        if apply_requested {
            self.apply_log_filter();
        } else if clear_requested {
            self.clear_log_filter();
        } else if cancel_requested {
            self.log_filter_open = false;
        } else {
            self.log_filter_open = open;
        }
    }

    fn apply_log_filter(&mut self) {
        match self.log_filter_editor.to_runtime_filter() {
            Ok(filter) => {
                self.log_runtime_filter = filter;
                self.log_filter_applied = self.log_filter_editor.clone();
                self.log_filter_open = false;
                self.log_page = 0;
                self.status = if self.log_filter_applied.is_active() {
                    format!("日志筛选已应用: {} 项", self.log_filter_applied.active_count())
                } else {
                    "日志筛选已清空".to_string()
                };
                self.refresh_all();
            }
            Err(err) => {
                self.status = format!("日志筛选无效: {err}");
            }
        }
    }

    fn clear_log_filter(&mut self) {
        self.log_filter_editor = Default::default();
        self.log_filter_applied = Default::default();
        self.log_runtime_filter = Default::default();
        self.log_filter_open = false;
        self.log_page = 0;
        self.status = "日志筛选已清空".to_string();
        self.refresh_all();
    }

    fn log_cleanup_window(&mut self, ctx: &egui::Context) {
        if !self.log_cleanup_open {
            return;
        }
        let mut open = self.log_cleanup_open;
        let mut cleanup_requested = false;
        let mut cancel_requested = false;
        egui::Window::new("清理日志")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("当前共有 {} 条日志", self.log_total_count));
                ui.horizontal(|ui| {
                    ui.label("保留范围");
                    retention_choice_ui(ui, &mut self.log_retention_choice);
                });
                if self.log_retention_choice == LogRetentionChoice::Count {
                    ui.add(
                        egui::DragValue::new(&mut self.log_retention_count)
                            .range(0..=1_000_000)
                            .prefix("保留 ")
                            .suffix(" 条"),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.button("清理").clicked() {
                        cleanup_requested = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel_requested = true;
                    }
                });
            });
        if cleanup_requested {
            self.log_cleanup_open = false;
            self.cleanup_logs();
        } else if cancel_requested {
            self.log_cleanup_open = false;
        } else {
            self.log_cleanup_open = open;
        }
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
            ui.add_sized(
                [LOG_RANGE_LABEL_WIDTH, ui.spacing().interact_size.y],
                egui::Label::new(format!(
                    "显示 {range_start} 至 {range_end} 共 {} 条结果",
                    self.log_total_count
                )),
            );
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
            if log_page_button(ui, self.log_page > 0, false, "<").clicked() {
                target_page = Some(self.log_page - 1);
            }
            for item in log_page_items(self.log_page, total_pages) {
                match item {
                    LogPageItem::Page(page) => {
                        let selected = page == self.log_page;
                        if log_page_button(ui, true, selected, (page + 1).to_string()).clicked() {
                            target_page = Some(page);
                        }
                    }
                    LogPageItem::Ellipsis => log_page_slot(ui, "..."),
                    LogPageItem::Empty => log_page_slot(ui, ""),
                }
            }
            if log_page_button(ui, self.log_page + 1 < total_pages, false, ">").clicked() {
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

    fn cleanup_logs(&mut self) {
        let retention = match self.log_retention_choice {
            LogRetentionChoice::OneDay => {
                RequestLogRetention::Since(Utc::now() - Duration::days(1))
            }
            LogRetentionChoice::OneWeek => {
                RequestLogRetention::Since(Utc::now() - Duration::weeks(1))
            }
            LogRetentionChoice::OneMonth => {
                RequestLogRetention::Since(Utc::now() - Duration::days(30))
            }
            LogRetentionChoice::OneYear => {
                RequestLogRetention::Since(Utc::now() - Duration::days(365))
            }
            LogRetentionChoice::Count => RequestLogRetention::Newest(self.log_retention_count),
            LogRetentionChoice::Failed => RequestLogRetention::Failed,
        };
        match self
            .runtime
            .block_on(self.state.store.cleanup_request_logs(retention))
        {
            Ok(deleted) => {
                self.log_page = 0;
                self.status = format!("日志已清理: 删除 {deleted} 条");
                self.state.events.bump_request_logs();
                self.refresh_all();
            }
            Err(err) => {
                self.status = format!("日志清理失败: {err}");
            }
        }
    }
}

fn log_retention_label(choice: LogRetentionChoice) -> &'static str {
    match choice {
        LogRetentionChoice::OneDay => "保留一天",
        LogRetentionChoice::OneWeek => "保留一周",
        LogRetentionChoice::OneMonth => "保留一个月",
        LogRetentionChoice::OneYear => "保留一年",
        LogRetentionChoice::Count => "保留指定条数",
        LogRetentionChoice::Failed => "只清理失败请求",
    }
}

fn retention_choice_ui(ui: &mut egui::Ui, choice: &mut LogRetentionChoice) {
    egui::ComboBox::from_id_salt("log_retention_choice")
        .selected_text(log_retention_label(*choice))
        .show_ui(ui, |ui| {
            for value in [
                LogRetentionChoice::OneDay,
                LogRetentionChoice::OneWeek,
                LogRetentionChoice::OneMonth,
                LogRetentionChoice::OneYear,
                LogRetentionChoice::Count,
                LogRetentionChoice::Failed,
            ] {
                ui.selectable_value(choice, value, log_retention_label(value));
            }
        });
}

fn option_filter_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<String>,
    options: &[String],
    all_label: &str,
) {
    ui.label(label);
    let selected = value.as_deref().unwrap_or(all_label);
    egui::ComboBox::from_id_salt(format!("log_filter_{label}"))
        .selected_text(selected)
        .width(240.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, all_label);
            for option in options {
                ui.selectable_value(value, Some(option.clone()), option);
            }
        });
    ui.label("");
    ui.end_row();
}

fn status_filter_row(
    ui: &mut egui::Ui,
    value: &mut LogStatusFilter,
    custom_range: &mut I64RangeFilter,
) {
    ui.label("状态码");
    egui::ComboBox::from_id_salt("log_filter_status")
        .selected_text(status_filter_label(*value))
        .width(160.0)
        .show_ui(ui, |ui| {
            for option in [
                LogStatusFilter::All,
                LogStatusFilter::Success,
                LogStatusFilter::Error,
                LogStatusFilter::ClientError,
                LogStatusFilter::ServerError,
                LogStatusFilter::Custom,
            ] {
                ui.selectable_value(value, option, status_filter_label(option));
            }
        });
    if *value == LogStatusFilter::Custom {
        custom_range.enabled = true;
        i64_range_values_ui(ui, custom_range);
    } else {
        ui.label("");
    }
    ui.end_row();
}

fn i64_range_filter_row(ui: &mut egui::Ui, label: &str, value: &mut I64RangeFilter) {
    ui.checkbox(&mut value.enabled, label);
    i64_range_values_ui(ui, value);
    ui.label("");
    ui.end_row();
}

fn i64_range_values_ui(ui: &mut egui::Ui, value: &mut I64RangeFilter) {
    ui.add_enabled_ui(value.enabled, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut value.min).range(0..=i64::MAX).speed(1));
            ui.label("至");
            ui.add(egui::DragValue::new(&mut value.max).range(0..=i64::MAX).speed(1));
        });
    });
}

fn f64_range_filter_row(ui: &mut egui::Ui, label: &str, value: &mut F64RangeFilter) {
    ui.checkbox(&mut value.enabled, label);
    ui.add_enabled_ui(value.enabled, |ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut value.min)
                    .range(0.0..=f64::MAX)
                    .speed(0.0001)
                    .prefix("$"),
            );
            ui.label("至");
            ui.add(
                egui::DragValue::new(&mut value.max)
                    .range(0.0..=f64::MAX)
                    .speed(0.0001)
                    .prefix("$"),
            );
        });
    });
    ui.label("");
    ui.end_row();
}

fn date_time_filter_row(ui: &mut egui::Ui, label: &str, value: &mut LogDateTimeFilter) {
    ui.checkbox(&mut value.enabled, label);
    ui.add_enabled_ui(value.enabled, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut value.value.year).range(1970..=9999));
            ui.label("-");
            ui.add(egui::DragValue::new(&mut value.value.month).range(1..=12));
            ui.label("-");
            ui.add(egui::DragValue::new(&mut value.value.day).range(1..=31));
            ui.separator();
            ui.add(egui::DragValue::new(&mut value.value.hour).range(0..=23));
            ui.label(":");
            ui.add(egui::DragValue::new(&mut value.value.minute).range(0..=59));
            ui.label(":");
            ui.add(egui::DragValue::new(&mut value.value.second).range(0..=59));
        });
    });
    ui.label("");
    ui.end_row();
}

fn status_filter_label(value: LogStatusFilter) -> &'static str {
    match value {
        LogStatusFilter::All => "全部状态",
        LogStatusFilter::Success => "成功",
        LogStatusFilter::Error => "错误",
        LogStatusFilter::ClientError => "4xx",
        LogStatusFilter::ServerError => "5xx",
        LogStatusFilter::Custom => "自定义",
    }
}

fn log_model_options(logs: &[RequestLog]) -> Vec<String> {
    logs.iter()
        .filter_map(|log| log.model.as_deref())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn log_upstream_options(logs: &[RequestLog], upstreams: &[Upstream]) -> Vec<String> {
    let mut values = BTreeSet::new();
    for upstream in upstreams {
        if !upstream.name.is_empty() {
            values.insert(upstream.name.as_str());
        }
    }
    for log in logs {
        if let Some(value) = log.upstream_name.as_deref()
            && !value.is_empty()
        {
            values.insert(value);
        }
    }
    values.into_iter().map(str::to_string).collect()
}

fn log_reasoning_effort_options(logs: &[RequestLog]) -> Vec<String> {
    logs.iter()
        .filter_map(|log| log.reasoning_effort.as_deref())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn log_endpoint_options(logs: &[RequestLog]) -> Vec<String> {
    logs.iter()
        .map(|log| log.endpoint.as_str())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogPageItem {
    Page(usize),
    Ellipsis,
    Empty,
}

fn log_page_button(
    ui: &mut egui::Ui,
    enabled: bool,
    selected: bool,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    let response = ui
        .add_enabled_ui(enabled, |ui| {
            ui.add_sized(
                [LOG_PAGE_BUTTON_WIDTH, ui.spacing().interact_size.y],
                egui::Button::new(text).selected(selected),
            )
        })
        .inner;
    if enabled {
        response
    } else {
        response.on_disabled_hover_text("不可翻页")
    }
}

fn log_page_slot(ui: &mut egui::Ui, text: &str) {
    ui.add_sized(
        [LOG_PAGE_BUTTON_WIDTH, ui.spacing().interact_size.y],
        egui::Label::new(text),
    );
}

fn log_result_range(page: usize, page_size: usize, total_count: i64) -> (i64, i64) {
    if total_count <= 0 || page_size == 0 {
        return (0, 0);
    }
    let start = (page * page_size) as i64 + 1;
    let end = ((page + 1) * page_size) as i64;
    (start.min(total_count), end.min(total_count))
}

fn log_page_items(current_page: usize, total_pages: usize) -> Vec<LogPageItem> {
    if total_pages == 0 {
        return vec![LogPageItem::Empty; LOG_PAGE_SLOT_COUNT];
    }
    if total_pages <= LOG_PAGE_SLOT_COUNT {
        return (0..LOG_PAGE_SLOT_COUNT)
            .map(|index| {
                if index < total_pages {
                    LogPageItem::Page(index)
                } else {
                    LogPageItem::Empty
                }
            })
            .collect();
    }
    if current_page <= 2 {
        return vec![
            LogPageItem::Page(0),
            LogPageItem::Page(1),
            LogPageItem::Page(2),
            LogPageItem::Page(3),
            LogPageItem::Page(4),
            LogPageItem::Ellipsis,
            LogPageItem::Page(total_pages - 1),
        ];
    }
    if current_page + 3 >= total_pages {
        return vec![
            LogPageItem::Page(0),
            LogPageItem::Ellipsis,
            LogPageItem::Page(total_pages - 5),
            LogPageItem::Page(total_pages - 4),
            LogPageItem::Page(total_pages - 3),
            LogPageItem::Page(total_pages - 2),
            LogPageItem::Page(total_pages - 1),
        ];
    }
    vec![
        LogPageItem::Page(0),
        LogPageItem::Ellipsis,
        LogPageItem::Page(current_page - 1),
        LogPageItem::Page(current_page),
        LogPageItem::Page(current_page + 1),
        LogPageItem::Ellipsis,
        LogPageItem::Page(total_pages - 1),
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
