use super::CodexSwitchApp;
use super::UiTaskEvent;
use super::tokens;
use crate::core::models::{RequestLog, RequestLogSource, Upstream, UpstreamKind, WireApi};
use crate::proxy::forward::model_test as test_bench;
use crate::proxy::forward::model_test::ModelTestOutcome;
use crate::storage::RequestLogFilter;
use chrono::Local;
use eframe::egui;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const DEFAULT_PROMPT: &str = "请只回复: pong";
const DEFAULT_MAX_TOKENS: &str = "64";
const DEFAULT_TIMEOUT_SECS: &str = "120";
const HISTORY_LIMIT: i64 = 50;
const CHAT_HISTORY_HEIGHT: f32 = 260.0;
const RESULT_TEXT_HEIGHT: f32 = 160.0;

/// 测试种类, 用于把异步结果路由回对应的 UI 状态.
#[derive(Debug, Clone)]
pub(super) enum ModelTestKind {
    Single,
    Batch(String),
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetMode {
    Direct,
    Scheduler,
}

impl TargetMode {
    fn label(self) -> &'static str {
        match self {
            Self::Direct => "直连上游",
            Self::Scheduler => "经调度组",
        }
    }

    const ALL: [Self; 2] = [Self::Direct, Self::Scheduler];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Single,
    Batch,
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReasoningEffort {
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    fn label(self) -> &'static str {
        match self {
            Self::Low => "低",
            Self::Medium => "中",
            Self::High => "高",
        }
    }

    fn as_request_value(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone)]
struct SingleResult {
    started: Option<Instant>,
    outcome: Option<ModelTestOutcome>,
}

#[derive(Debug, Clone)]
struct BatchRow {
    upstream_id: String,
    upstream_name: String,
    started: Option<Instant>,
    outcome: Option<ModelTestOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy)]
struct ChatMeta {
    duration_ms: i64,
    first_token_ms: Option<i64>,
    total_tokens: i64,
}

#[derive(Debug, Clone)]
struct ChatEntry {
    role: ChatRole,
    text: String,
    error: bool,
    meta: Option<ChatMeta>,
}

#[derive(Debug)]
pub(super) struct ModelTestUiState {
    target_mode: TargetMode,
    selected_upstream_id: Option<String>,
    model_input: String,
    fetched_models: Vec<String>,
    fetched_for_upstream: Option<String>,
    models_fetching: bool,
    stream: bool,
    max_tokens_input: String,
    reasoning_enabled: bool,
    reasoning_effort: ReasoningEffort,
    timeout_input: String,
    prompt: String,
    section: Section,
    single_result: SingleResult,
    batch_selected: BTreeSet<String>,
    batch_rows: Vec<BatchRow>,
    chat_messages: Vec<ChatEntry>,
    chat_input: String,
    chat_running: bool,
    chat_started: Option<Instant>,
    history: Vec<RequestLog>,
    history_version_seen: u64,
}

impl Default for ModelTestUiState {
    fn default() -> Self {
        Self {
            target_mode: TargetMode::Direct,
            selected_upstream_id: None,
            model_input: String::new(),
            fetched_models: Vec::new(),
            fetched_for_upstream: None,
            models_fetching: false,
            stream: true,
            max_tokens_input: DEFAULT_MAX_TOKENS.to_string(),
            reasoning_enabled: false,
            reasoning_effort: ReasoningEffort::Medium,
            timeout_input: DEFAULT_TIMEOUT_SECS.to_string(),
            prompt: DEFAULT_PROMPT.to_string(),
            section: Section::Single,
            single_result: SingleResult {
                started: None,
                outcome: None,
            },
            batch_selected: BTreeSet::new(),
            batch_rows: Vec::new(),
            chat_messages: Vec::new(),
            chat_input: String::new(),
            chat_running: false,
            chat_started: None,
            history: Vec::new(),
            history_version_seen: 0,
        }
    }
}

impl ModelTestUiState {
    fn busy(&self) -> bool {
        self.single_result.started.is_some()
            || self.chat_running
            || self.batch_rows.iter().any(|row| row.started.is_some())
    }
}

impl CodexSwitchApp {
    pub(super) fn model_test_ui(&mut self, ui: &mut egui::Ui) {
        self.refresh_model_test_history_if_needed();
        if self.model_test_ui.busy() {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        let mut switch_to = None;
        egui::ScrollArea::vertical()
            .id_salt("model_test_page")
            .max_height(ui.available_height())
            .show(ui, |ui| {
                ui.heading("模型测试台");
                ui.add_space(4.0);
                self.model_test_target_section(ui);
                ui.separator();
                ui.horizontal(|ui| {
                    for (section, label) in [
                        (Section::Single, "单次测试"),
                        (Section::Batch, "批量对比"),
                        (Section::Chat, "对话测试"),
                    ] {
                        if ui
                            .selectable_label(self.model_test_ui.section == section, label)
                            .clicked()
                        {
                            switch_to = Some(section);
                        }
                    }
                });
                ui.add_space(2.0);
                match self.model_test_ui.section {
                    Section::Single => self.model_test_single_section(ui),
                    Section::Batch => self.model_test_batch_section(ui),
                    Section::Chat => self.model_test_chat_section(ui),
                }
                ui.separator();
                self.model_test_history_section(ui);
            });
        if let Some(section) = switch_to {
            self.model_test_ui.section = section;
        }
    }

    fn model_test_target_section(&mut self, ui: &mut egui::Ui) {
        let enabled_upstreams: Vec<Upstream> = self
            .upstreams
            .iter()
            .filter(|upstream| upstream.enabled)
            .cloned()
            .collect();
        if let Some(selected) = self.model_test_ui.selected_upstream_id.clone()
            && !enabled_upstreams.iter().any(|upstream| upstream.id == selected)
        {
            self.model_test_ui.selected_upstream_id = None;
        }
        if self.model_test_ui.selected_upstream_id.is_none()
            && let Some(first) = enabled_upstreams.first()
        {
            self.model_test_ui.selected_upstream_id = Some(first.id.clone());
        }

        ui.horizontal(|ui| {
            ui.label("发送方式");
            egui::ComboBox::from_id_salt("model_test_target_mode")
                .selected_text(self.model_test_ui.target_mode.label())
                .show_ui(ui, |ui| {
                    for mode in TargetMode::ALL {
                        if ui
                            .selectable_label(
                                self.model_test_ui.target_mode == mode,
                                mode.label(),
                            )
                            .clicked()
                        {
                            self.model_test_ui.target_mode = mode;
                        }
                    }
                })
                .response
                .on_hover_text("直连上游绕过调度组精确测试单个上游, 经调度组走本地代理完整链路");
            if self.model_test_ui.target_mode == TargetMode::Direct {
                ui.separator();
                ui.label("上游");
                let selected_name = enabled_upstreams
                    .iter()
                    .find(|upstream| {
                        self.model_test_ui.selected_upstream_id.as_deref() == Some(&upstream.id)
                    })
                    .map(|upstream| upstream.name.clone())
                    .unwrap_or_else(|| "选择上游".to_string());
                egui::ComboBox::from_id_salt("model_test_upstream")
                    .selected_text(selected_name)
                    .show_ui(ui, |ui| {
                        for upstream in &enabled_upstreams {
                            let selected = self.model_test_ui.selected_upstream_id.as_deref()
                                == Some(&upstream.id);
                            if ui.selectable_label(selected, &upstream.name).clicked() {
                                if !selected {
                                    self.model_test_ui.fetched_models.clear();
                                    self.model_test_ui.fetched_for_upstream = None;
                                }
                                self.model_test_ui.selected_upstream_id = Some(upstream.id.clone());
                            }
                        }
                    });
                let selected_is_oauth = enabled_upstreams
                    .iter()
                    .find(|upstream| {
                        self.model_test_ui.selected_upstream_id.as_deref() == Some(&upstream.id)
                    })
                    .is_some_and(|upstream| upstream.kind == UpstreamKind::CodexOauth);
                let fetch_label = if self.model_test_ui.models_fetching {
                    "拉取中..."
                } else {
                    "拉取模型列表"
                };
                if ui
                    .add_enabled(
                        !self.model_test_ui.models_fetching && !selected_is_oauth,
                        egui::Button::new(fetch_label),
                    )
                    .clicked()
                {
                    self.fetch_model_test_models();
                }
            }
        });
        if self.model_test_ui.target_mode == TargetMode::Direct
            && let Some(upstream) = self.model_test_target_upstream()
            && upstream.kind == UpstreamKind::CodexOauth
        {
            ui.label(
                egui::RichText::new("OAuth 上游无法拉取模型列表, 且直连请求可能缺少必要的 Codex 请求字段, 建议使用\"经调度组\"方式测试")
                    .weak(),
            );
        }
        ui.horizontal(|ui| {
            ui.label("模型");
            ui.add(
                egui::TextEdit::singleline(&mut self.model_test_ui.model_input)
                    .desired_width(260.0)
                    .hint_text("模型名, 如 gpt-5-codex"),
            );
            let model_list_for_target = self.model_test_ui.fetched_for_upstream.as_deref()
                == self.model_test_ui.selected_upstream_id.as_deref();
            if !self.model_test_ui.fetched_models.is_empty() && model_list_for_target {
                egui::ComboBox::from_id_salt("model_test_model_list")
                    .selected_text("从列表选择")
                    .show_ui(ui, |ui| {
                        for model in &self.model_test_ui.fetched_models {
                            if ui.selectable_label(false, model).clicked() {
                                self.model_test_ui.model_input = model.clone();
                            }
                        }
                    })
                    .response
                    .on_hover_text("该上游的模型列表");            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.model_test_ui.stream, "流式")
                .on_hover_text("流式请求可测量首 token 延迟");
            ui.separator();
            ui.label("max tokens");
            ui.add(
                egui::TextEdit::singleline(&mut self.model_test_ui.max_tokens_input)
                    .desired_width(56.0),
            );
            ui.separator();
            ui.label("超时(秒)");
            ui.add(
                egui::TextEdit::singleline(&mut self.model_test_ui.timeout_input)
                    .desired_width(56.0),
            );
            ui.separator();
            let reasoning_supported = self.model_test_reasoning_supported();
            ui.add_enabled(
                reasoning_supported,
                egui::Checkbox::new(
                    &mut self.model_test_ui.reasoning_enabled,
                    "推理力度",
                ),
            )
            .on_disabled_hover_text("Anthropic 上游的直连测试不支持推理力度");
            if self.model_test_ui.reasoning_enabled && reasoning_supported {
                egui::ComboBox::from_id_salt("model_test_reasoning")
                    .selected_text(self.model_test_ui.reasoning_effort.label())
                    .show_ui(ui, |ui| {
                        for effort in ReasoningEffort::ALL {
                            if ui
                                .selectable_label(
                                    self.model_test_ui.reasoning_effort == effort,
                                    effort.label(),
                                )
                                .clicked()
                            {
                                self.model_test_ui.reasoning_effort = effort;
                            }
                        }
                    });
            }
        });
        ui.horizontal(|ui| {
            ui.label("测试 prompt");
            ui.add(
                egui::TextEdit::multiline(&mut self.model_test_ui.prompt)
                    .desired_width(520.0)
                    .desired_rows(2)
                    .hint_text("单次测试与批量对比使用的提示词"),
            );
        });
    }

    fn model_test_reasoning_supported(&self) -> bool {
        if self.model_test_ui.target_mode == TargetMode::Scheduler {
            return true;
        }
        self.model_test_target_upstream()
            .is_none_or(|upstream| upstream.wire_api != WireApi::AnthropicMessages)
    }

    fn model_test_single_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let running = self.model_test_ui.single_result.started.is_some();
            if ui.add_enabled(!running, egui::Button::new("发送测试")).clicked() {
                self.send_model_test(ModelTestKind::Single);
            }
            if running {
                ui.spinner();
                if let Some(started) = self.model_test_ui.single_result.started {
                    ui.label(format!("进行中, 已等待 {:.1}s", started.elapsed().as_secs_f32()));
                }
            }
        });
        let Some(outcome) = self.model_test_ui.single_result.outcome.clone() else {
            return;
        };
        self.model_test_outcome_card(ui, &outcome);
    }

    fn model_test_batch_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("批量对比");
        let enabled_upstreams: Vec<Upstream> = self
            .upstreams
            .iter()
            .filter(|upstream| upstream.enabled)
            .cloned()
            .collect();
        let batch_running = self
            .model_test_ui
            .batch_rows
            .iter()
            .any(|row| row.started.is_some());
        ui.horizontal(|ui| {
            if ui.button("全选").clicked() {
                self.model_test_ui.batch_selected = enabled_upstreams
                    .iter()
                    .map(|upstream| upstream.id.clone())
                    .collect();
            }
            if ui.button("清空选择").clicked() {
                self.model_test_ui.batch_selected.clear();
            }
            if ui
                .add_enabled(
                    !batch_running && !self.model_test_ui.batch_selected.is_empty(),
                    egui::Button::new(format!(
                        "开始批量测试 ({})",
                        self.model_test_ui.batch_selected.len()
                    )),
                )
                .on_hover_text("对勾选的上游以当前模型和 prompt 并发直连测试")
                .clicked()
            {
                self.send_model_test_batch();
            }
        });
        ui.horizontal_wrapped(|ui| {
            for upstream in &enabled_upstreams {
                let mut checked = self.model_test_ui.batch_selected.contains(&upstream.id);
                if ui.checkbox(&mut checked, &upstream.name).changed() {
                    if checked {
                        self.model_test_ui
                            .batch_selected
                            .insert(upstream.id.clone());
                    } else {
                        self.model_test_ui.batch_selected.remove(&upstream.id);
                    }
                }
            }
        });
        if self.model_test_ui.batch_rows.is_empty() {
            return;
        }
        ui.add_space(4.0);
        let mut rows = self.model_test_ui.batch_rows.clone();
        rows.sort_by_key(|row| match (&row.started, &row.outcome) {
            (Some(_), _) => (0, 0),
            (None, Some(outcome)) if outcome.is_success() => (1, outcome.duration_ms),
            (None, Some(outcome)) => (2, outcome.duration_ms),
            (None, None) => (3, 0),
        });
        let mut token_display_mode = self.token_display_mode;
        egui::Grid::new("model_test_batch_grid")
            .striped(true)
            .num_columns(7)
            .spacing([14.0, 8.0])
            .show(ui, |ui| {
                ui.strong("上游");
                ui.strong("状态");
                ui.strong("总耗时");
                ui.strong("首 token");
                ui.strong("输入");
                ui.strong("输出");
                ui.strong("详情");
                ui.end_row();
                for row in &rows {
                    ui.label(&row.upstream_name);
                    match (&row.started, &row.outcome) {
                        (Some(started), _) => {
                            ui.spinner();
                            ui.label(format!(
                                "{:.1}s",
                                started.elapsed().as_secs_f32()
                            ));
                            ui.end_row();
                        }
                        (None, Some(outcome)) => {
                            let (label, color) = outcome_status_label(outcome);
                            ui.colored_label(color, label);
                            ui.label(format!("{} ms", outcome.duration_ms));
                            ui.label(match outcome.first_token_ms {
                                Some(ms) => format!("{ms} ms"),
                                None if self.model_test_ui.stream => "未返回".to_string(),
                                None => "非流式".to_string(),
                            });
                            tokens::token_value(
                                ui,
                                &mut token_display_mode,
                                "输入",
                                outcome.usage.input_tokens,
                            );
                            tokens::token_value(
                                ui,
                                &mut token_display_mode,
                                "输出",
                                outcome.usage.output_tokens,
                            );
                            ui.horizontal(|ui| {
                                if let Some(error) = &outcome.error {
                                    ui.label(
                                        egui::RichText::new(truncate_text(error, 60)).weak(),
                                    )
                                    .on_hover_text(error);
                                } else if !outcome.output_text.is_empty() {
                                    ui.label(egui::RichText::new(truncate_text(
                                        &outcome.output_text,
                                        60,
                                    ))
                                    .weak())
                                    .on_hover_text(&outcome.output_text);
                                }
                            });
                            ui.end_row();
                        }
                        (None, None) => {
                            ui.label("-");
                            ui.label("-");
                            ui.label("-");
                            ui.label("-");
                            ui.end_row();
                        }
                    }
                }
            });
        self.token_display_mode = token_display_mode;
    }

    fn model_test_chat_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("对话测试");
        ui.horizontal(|ui| {
            ui.label(format!("消息数: {}", self.model_test_ui.chat_messages.len()));
            if ui
                .add_enabled(
                    !self.model_test_ui.chat_messages.is_empty() && !self.model_test_ui.chat_running,
                    egui::Button::new("清空对话"),
                )
                .clicked()
            {
                self.model_test_ui.chat_messages.clear();
                self.model_test_ui.chat_input.clear();
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("model_test_chat_history")
            .max_height(CHAT_HISTORY_HEIGHT)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.model_test_ui.chat_messages.is_empty() {
                    ui.label("暂无消息, 在下方输入内容开始对话");
                }
                for entry in &self.model_test_ui.chat_messages {
                    ui.horizontal_wrapped(|ui| {
                        let (role_label, color) = match entry.role {
                            ChatRole::User => ("[用户]", egui::Color32::from_rgb(96, 165, 250)),
                            ChatRole::Assistant => ("[模型]", egui::Color32::from_rgb(34, 197, 94)),
                        };
                        ui.colored_label(color, role_label);
                        let text_color = if entry.error {
                            egui::Color32::from_rgb(239, 68, 68)
                        } else {
                            ui.visuals().text_color()
                        };
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&entry.text).color(text_color),
                            )
                            .wrap(),
                        );
                    });
                    if let Some(meta) = entry.meta {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "耗时 {} ms, 首 token {}, 总 tokens {}",
                                    meta.duration_ms,
                                    meta.first_token_ms
                                        .map(|ms| format!("{ms} ms"))
                                        .unwrap_or_else(|| "未返回".to_string()),
                                    tokens::format_tokens(self.token_display_mode, meta.total_tokens),
                                ))
                                .weak()
                                .small(),
                            );
                        });
                    }
                }
                if self.model_test_ui.chat_running {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        if let Some(started) = self.model_test_ui.chat_started {
                            ui.label(format!(
                                "模型回复中, 已等待 {:.1}s",
                                started.elapsed().as_secs_f32()
                            ));
                        }
                    });
                }
            });
        ui.add_space(4.0);
        let send_clicked = ui
            .add_enabled(!self.model_test_ui.chat_running, egui::Button::new("发送"))
            .clicked();
        let input_response = ui.add(
            egui::TextEdit::singleline(&mut self.model_test_ui.chat_input)
                .desired_width(f32::INFINITY)
                .hint_text("输入消息, Enter 发送"),
        );
        let enter_pressed =
            input_response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if (send_clicked || enter_pressed) && !self.model_test_ui.chat_running {
            self.send_model_test_chat();
        }
    }

    fn model_test_history_section(&mut self, ui: &mut egui::Ui) {
        ui.heading(format!("测试记录 ({})", self.model_test_ui.history.len()));
        if self.model_test_ui.history.is_empty() {
            ui.label("暂无测试记录");
            return;
        }
        let mut token_display_mode = self.token_display_mode;
        let mut currency_display_mode = self.currency_display_mode;
        let rate = self.usd_cny_rate;
        egui::Grid::new("model_test_history_grid")
            .striped(true)
            .num_columns(8)
            .spacing([14.0, 8.0])
            .show(ui, |ui| {
                ui.strong("时间");
                ui.strong("上游");
                ui.strong("模型");
                ui.strong("状态");
                ui.strong("耗时");
                ui.strong("首 token");
                ui.strong("Tokens");
                ui.strong("费用");
                ui.end_row();
                for log in &self.model_test_ui.history {
                    ui.label(log
                        .ts
                        .map(|ts| ts.with_timezone(&Local).format("%m-%d %H:%M:%S").to_string())
                        .unwrap_or_else(|| "-".to_string()));
                    ui.label(
                        log.upstream_name
                            .as_deref()
                            .unwrap_or("未选择"),
                    );
                    ui.label(log.model.as_deref().unwrap_or("-"));
                    let (label, color) = match &log.error {
                        Some(_) => ("失败", error_color()),
                        None if (200..300).contains(&log.status) => ("成功", success_color()),
                        None => ("失败", error_color()),
                    };
                    let hover = log
                        .error
                        .clone()
                        .unwrap_or_else(|| "测试台请求".to_string());
                    ui.colored_label(color, label).on_hover_text(hover);
                    ui.label(format!("{} ms", log.duration_ms));
                    ui.label(match log.first_token_ms {
                        Some(ms) => format!("{ms} ms"),
                        None if self.model_test_ui.stream => "未返回".to_string(),
                        None => "非流式".to_string(),
                    });
                    tokens::token_number(
                        ui,
                        &mut token_display_mode,
                        log.usage.total_tokens,
                    );
                    tokens::estimated_cost(
                        ui,
                        &mut currency_display_mode,
                        rate,
                        "费用",
                        log.estimated_cost_usd,
                    );
                    ui.end_row();
                }
            });
        self.token_display_mode = token_display_mode;
        self.currency_display_mode = currency_display_mode;
    }

    fn model_test_outcome_card(&mut self, ui: &mut egui::Ui, outcome: &ModelTestOutcome) {
        ui.add_space(4.0);
        let (status_label, status_color) = outcome_status_label(outcome);
        ui.colored_label(status_color, format!("状态: {status_label}"));
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("HTTP 状态码: {}", outcome.status));
            ui.separator();
            ui.label(format!("总耗时: {} ms", outcome.duration_ms));
            ui.separator();
            ui.label(format!(
                "首 token: {}",
                outcome
                    .first_token_ms
                    .map(|ms| format!("{ms} ms"))
                    .unwrap_or_else(|| "未返回".to_string())
            ));
        });
        let mut token_display_mode = self.token_display_mode;
        let mut currency_display_mode = self.currency_display_mode;
        let rate = self.usd_cny_rate;
        ui.horizontal_wrapped(|ui| {
            tokens::usage_tokens(ui, &mut token_display_mode, &outcome.usage);
            ui.separator();
            tokens::estimated_cost(
                ui,
                &mut currency_display_mode,
                rate,
                "估算费用",
                outcome.estimated_cost_usd,
            );
        });
        self.token_display_mode = token_display_mode;
        self.currency_display_mode = currency_display_mode;
        if let Some(error) = &outcome.error {
            ui.colored_label(error_color(), format!("错误: {error}"));
        }
        if !outcome.output_text.is_empty() {
            ui.horizontal(|ui| {
                ui.label("回复");
                if ui.button("复制").clicked() {
                    ui.ctx().copy_text(outcome.output_text.clone());
                    self.status = "回复已复制".to_string();
                }
            });
            egui::ScrollArea::vertical()
                .id_salt("model_test_single_output")
                .max_height(RESULT_TEXT_HEIGHT)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(&outcome.output_text)
                            .wrap()
                            .selectable(true),
                    );
                });
        }
    }

    fn model_test_target_upstream(&self) -> Option<Upstream> {
        let id = self.model_test_ui.selected_upstream_id.as_deref()?;
        self.upstreams
            .iter()
            .find(|upstream| upstream.id == id && upstream.enabled)
            .cloned()
    }

    fn build_model_test_params(
        &self,
        messages: Vec<test_bench::ModelTestMessage>,
    ) -> Result<test_bench::ModelTestParams, String> {
        let model = self.model_test_ui.model_input.trim().to_string();
        if model.is_empty() {
            return Err("请填写模型名".to_string());
        }
        let max_tokens: i64 = self
            .model_test_ui
            .max_tokens_input
            .trim()
            .parse()
            .map_err(|_| "max tokens 需要为正整数".to_string())?;
        if max_tokens <= 0 {
            return Err("max tokens 需要为正整数".to_string());
        }
        let timeout: u64 = self
            .model_test_ui
            .timeout_input
            .trim()
            .parse()
            .map_err(|_| "超时需要为 1-600 的整数秒".to_string())?;
        if !(1..=600).contains(&timeout) {
            return Err("超时需要为 1-600 的整数秒".to_string());
        }
        Ok(test_bench::ModelTestParams {
            model,
            messages,
            stream: self.model_test_ui.stream,
            max_tokens,
            reasoning_effort: self.model_test_ui.reasoning_enabled.then(|| {
                self.model_test_ui
                    .reasoning_effort
                    .as_request_value()
                    .to_string()
            }),
            timeout: Duration::from_secs(timeout),
        })
    }

    fn send_model_test(&mut self, kind: ModelTestKind) {
        if self.model_test_ui.single_result.started.is_some() {
            return;
        }
        let prompt = self.model_test_ui.prompt.trim().to_string();
        if prompt.is_empty() {
            self.status = "请填写测试 prompt".to_string();
            return;
        }
        let params = match self.build_model_test_params(vec![test_bench::ModelTestMessage::user(prompt)]) {
            Ok(params) => params,
            Err(message) => {
                self.status = message;
                return;
            }
        };
        if self.model_test_ui.target_mode == TargetMode::Scheduler {
            if self.server.is_none() {
                self.status = "本地代理未启动, 请先在仪表盘启动服务".to_string();
                return;
            }
            let state = self.state.clone();
            let tx = self.task_tx.clone();
            let bind_addr = self.bind_addr.clone();
            let local_key = self.local_key.clone();
            self.runtime.spawn(async move {
                let result =
                    test_bench::run_scheduler_test(&state, &bind_addr, &local_key, params).await;
                let _ = tx.send(UiTaskEvent::ModelTestFinished { kind, result });
            });
        } else {
            let Some(upstream) = self.model_test_target_upstream() else {
                self.status = "请选择要测试的上游".to_string();
                return;
            };
            let state = self.state.clone();
            let tx = self.task_tx.clone();
            self.runtime.spawn(async move {
                let result = test_bench::run_direct_test(&state, &upstream, params).await;
                let _ = tx.send(UiTaskEvent::ModelTestFinished { kind, result });
            });
        }
        self.model_test_ui.single_result = SingleResult {
            started: Some(Instant::now()),
            outcome: None,
        };
        self.status = "测试请求已发送".to_string();
    }

    fn send_model_test_batch(&mut self) {
        if self
            .model_test_ui
            .batch_rows
            .iter()
            .any(|row| row.started.is_some())
        {
            return;
        }
        let prompt = self.model_test_ui.prompt.trim().to_string();
        if prompt.is_empty() {
            self.status = "请填写测试 prompt".to_string();
            return;
        }
        let params = match self.build_model_test_params(vec![test_bench::ModelTestMessage::user(prompt)]) {
            Ok(params) => params,
            Err(message) => {
                self.status = message;
                return;
            }
        };
        let targets: Vec<Upstream> = self
            .upstreams
            .iter()
            .filter(|upstream| {
                upstream.enabled && self.model_test_ui.batch_selected.contains(&upstream.id)
            })
            .cloned()
            .collect();
        if targets.is_empty() {
            self.status = "请先勾选要测试的上游".to_string();
            return;
        }
        self.model_test_ui.batch_rows = targets
            .iter()
            .map(|upstream| BatchRow {
                upstream_id: upstream.id.clone(),
                upstream_name: upstream.name.clone(),
                started: Some(Instant::now()),
                outcome: None,
            })
            .collect();
        for upstream in targets {
            let state = self.state.clone();
            let tx = self.task_tx.clone();
            let kind = ModelTestKind::Batch(upstream.id.clone());
            let params = params.clone();
            self.runtime.spawn(async move {
                let result = test_bench::run_direct_test(&state, &upstream, params).await;
                let _ = tx.send(UiTaskEvent::ModelTestFinished { kind, result });
            });
        }
        self.status = "批量测试已启动".to_string();
    }

    fn send_model_test_chat(&mut self) {
        let content = self.model_test_ui.chat_input.trim().to_string();
        if content.is_empty() {
            return;
        }
        let params = match self.build_model_test_params(
            self.model_test_ui
                .chat_messages
                .iter()
                .map(|entry| match entry.role {
                    ChatRole::User => test_bench::ModelTestMessage::user(entry.text.clone()),
                    ChatRole::Assistant => {
                        test_bench::ModelTestMessage::assistant(entry.text.clone())
                    }
                })
                .chain(std::iter::once(test_bench::ModelTestMessage::user(
                    content.clone(),
                )))
                .collect(),
        ) {
            Ok(params) => params,
            Err(message) => {
                self.status = message;
                return;
            }
        };
        self.model_test_ui.chat_messages.push(ChatEntry {
            role: ChatRole::User,
            text: content,
            error: false,
            meta: None,
        });
        self.model_test_ui.chat_input.clear();
        if self.model_test_ui.target_mode == TargetMode::Scheduler {
            if self.server.is_none() {
                self.model_test_ui.chat_messages.push(ChatEntry {
                    role: ChatRole::Assistant,
                    text: "本地代理未启动, 请先在仪表盘启动服务".to_string(),
                    error: true,
                    meta: None,
                });
                self.status = "本地代理未启动, 请先在仪表盘启动服务".to_string();
                return;
            }
            let state = self.state.clone();
            let tx = self.task_tx.clone();
            let bind_addr = self.bind_addr.clone();
            let local_key = self.local_key.clone();
            self.runtime.spawn(async move {
                let result =
                    test_bench::run_scheduler_test(&state, &bind_addr, &local_key, params).await;
                let _ = tx.send(UiTaskEvent::ModelTestFinished {
                    kind: ModelTestKind::Chat,
                    result,
                });
            });
        } else {
            let Some(upstream) = self.model_test_target_upstream() else {
                self.model_test_ui.chat_messages.push(ChatEntry {
                    role: ChatRole::Assistant,
                    text: "请先选择要测试的上游".to_string(),
                    error: true,
                    meta: None,
                });
                self.status = "请选择要测试的上游".to_string();
                return;
            };
            let state = self.state.clone();
            let tx = self.task_tx.clone();
            self.runtime.spawn(async move {
                let result = test_bench::run_direct_test(&state, &upstream, params).await;
                let _ = tx.send(UiTaskEvent::ModelTestFinished {
                    kind: ModelTestKind::Chat,
                    result,
                });
            });
        }
        self.model_test_ui.chat_running = true;
        self.model_test_ui.chat_started = Some(Instant::now());
    }

    fn fetch_model_test_models(&mut self) {
        if self.model_test_ui.models_fetching {
            return;
        }
        let Some(upstream) = self.model_test_target_upstream() else {
            self.status = "请选择要测试的上游".to_string();
            return;
        };
        self.model_test_ui.models_fetching = true;
        self.status = format!("正在拉取上游 {} 的模型列表", upstream.name);
        let state = self.state.clone();
        let tx = self.task_tx.clone();
        let upstream_id = upstream.id.clone();
        self.runtime.spawn(async move {
            let result = test_bench::fetch_upstream_model_ids(&state, &upstream).await;
            let _ = tx.send(UiTaskEvent::ModelTestModelsFetched { upstream_id, result });
        });
    }

    pub(super) fn handle_model_test_models_fetched(
        &mut self,
        upstream_id: String,
        result: anyhow::Result<Vec<String>>,
    ) {
        self.model_test_ui.models_fetching = false;
        match result {
            Ok(models) => {
                self.status = format!("已获取 {} 个模型", models.len());
                self.model_test_ui.fetched_models = models;
                self.model_test_ui.fetched_for_upstream = Some(upstream_id);
            }
            Err(err) => {
                self.status = format!("拉取模型列表失败: {err}");
            }
        }
    }

    pub(super) fn handle_model_test_finished(
        &mut self,
        kind: ModelTestKind,
        outcome: ModelTestOutcome,
    ) {
        let success = outcome.is_success();
        let duration_ms = outcome.duration_ms;
        let first_token_ms = outcome.first_token_ms;
        let error_text = outcome.error.clone();
        match kind {
            ModelTestKind::Single => {
                self.model_test_ui.single_result = SingleResult {
                    started: None,
                    outcome: Some(outcome),
                };
            }
            ModelTestKind::Batch(upstream_id) => {
                if let Some(row) = self
                    .model_test_ui
                    .batch_rows
                    .iter_mut()
                    .find(|row| row.upstream_id == upstream_id)
                {
                    row.started = None;
                    row.outcome = Some(outcome);
                }
            }
            ModelTestKind::Chat => {
                self.model_test_ui.chat_running = false;
                self.model_test_ui.chat_started = None;
                let entry = if success {
                    ChatEntry {
                        role: ChatRole::Assistant,
                        text: outcome.output_text,
                        error: false,
                        meta: Some(ChatMeta {
                            duration_ms,
                            first_token_ms,
                            total_tokens: outcome.usage.total_tokens,
                        }),
                    }
                } else {
                    ChatEntry {
                        role: ChatRole::Assistant,
                        text: error_text
                            .clone()
                            .unwrap_or_else(|| format!("请求失败 ({})", outcome.status)),
                        error: true,
                        meta: None,
                    }
                };
                self.model_test_ui.chat_messages.push(entry);
            }
        }
        if success {
            self.status = format!(
                "测试完成: {duration_ms} ms, 首 token {}",
                first_token_ms
                    .map(|ms| format!("{ms} ms"))
                    .unwrap_or_else(|| "未返回".to_string())
            );
        } else {
            self.status = format!(
                "测试失败: {}",
                error_text.as_deref().unwrap_or("未知错误")
            );
        }
    }

    fn refresh_model_test_history_if_needed(&mut self) {
        let version = self.state.events.request_log_version();
        if version == self.model_test_ui.history_version_seen {
            return;
        }
        self.model_test_ui.history_version_seen = version;
        let filter = RequestLogFilter {
            source: Some(RequestLogSource::TestBench),
            ..Default::default()
        };
        self.model_test_ui.history = self
            .runtime
            .block_on(
                self.state
                    .store
                    .recent_logs_page_filtered(HISTORY_LIMIT, 0, &filter),
            )
            .unwrap_or_default();
    }
}

fn outcome_status_label(outcome: &ModelTestOutcome) -> (&'static str, egui::Color32) {
    if outcome.is_success() {
        ("成功", success_color())
    } else {
        ("失败", error_color())
    }
}

fn error_color() -> egui::Color32 {
    egui::Color32::from_rgb(239, 68, 68)
}

fn success_color() -> egui::Color32 {
    egui::Color32::from_rgb(34, 197, 94)
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}...")
}
