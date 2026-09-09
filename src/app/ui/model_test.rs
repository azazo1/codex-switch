use super::CodexSwitchApp;
use super::UiTaskEvent;
use super::tokens;
use crate::core::models::{BalanceProvider, Upstream, UpstreamKind, WireApi};
use crate::proxy::forward::model_test as test_bench;
use crate::proxy::forward::model_test::ModelTestOutcome;
use crate::proxy::forward::model_test::ModelTestRawTrace;
use crate::proxy::forward::model_test::ModelTestStreamPart;
use chrono::{DateTime, Local};
use eframe::egui;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_PROMPT: &str = "请只回复: pong";
const DEFAULT_MAX_TOKENS: &str = "64";
const DEFAULT_TIMEOUT_SECS: &str = "120";
const DEFAULT_REASONING_EFFORT: &str = "medium";
const CHAT_HISTORY_HEIGHT: f32 = 260.0;
const RESULT_TEXT_HEIGHT: f32 = 160.0;
/// 单次测试与对话测试的回复内容不占满页面, 统一限制为可用宽度的比例.
const TEST_CONTENT_WIDTH_RATIO: f32 = 0.75;

/// 临时上游在测试记录中显示的名称.
const CUSTOM_UPSTREAM_NAME: &str = "临时上游";

/// 测试种类, 用于把异步结果路由回对应的 UI 状态.
#[derive(Debug, Clone)]
pub(super) enum ModelTestKind {
    Single,
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetMode {
    Direct,
    Scheduler,
    Custom,
}

impl TargetMode {
    fn label(self) -> &'static str {
        match self {
            Self::Direct => "上游",
            Self::Scheduler => "调度组",
            Self::Custom => "临时上游",
        }
    }

    const ALL: [Self; 3] = [Self::Direct, Self::Scheduler, Self::Custom];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Single,
    Chat,
}

#[derive(Debug, Clone)]
struct SingleResult {
    started: Option<Instant>,
    outcome: Option<ModelTestOutcome>,
    live_text: String,
    live_reasoning: String,
}

impl SingleResult {
    fn running() -> Self {
        Self {
            started: Some(Instant::now()),
            outcome: None,
            live_text: String::new(),
            live_reasoning: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatRole {
    User,
    Assistant,
}

/// 测试台内存记录: 一次测试的完整信息, 只保存在 UI 中, 不写入数据库,
/// 携带原始请求/响应报文, 因此"导出全部"可以导出完整的 HAR.
#[derive(Debug, Clone)]
struct ModelTestRecord {
    finished_at: DateTime<Local>,
    upstream_name: Option<String>,
    endpoint: String,
    model: String,
    stream: bool,
    status: i64,
    error: Option<String>,
    duration_ms: i64,
    first_token_ms: Option<i64>,
    total_tokens: i64,
    estimated_cost_usd: Option<f64>,
    raw: Option<Arc<ModelTestRawTrace>>,
}

impl ModelTestRecord {
    fn from_outcome(outcome: &ModelTestOutcome) -> Self {
        Self {
            finished_at: Local::now(),
            upstream_name: outcome.upstream_name.clone(),
            endpoint: outcome.endpoint.clone(),
            model: outcome.model.clone(),
            stream: outcome.stream,
            status: outcome.status,
            error: outcome.error.clone(),
            duration_ms: outcome.duration_ms,
            first_token_ms: outcome.first_token_ms,
            total_tokens: outcome.usage.total_tokens,
            estimated_cost_usd: outcome.estimated_cost_usd,
            raw: outcome.raw.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct ChatEntry {
    role: ChatRole,
    text: String,
    reasoning: String,
    error: bool,
    /// 已完成回复对应的完整测试记录, 用于展示耗时与导出 HAR;
    /// 用户消息与派发前就失败的条目为 None.
    record: Option<Arc<ModelTestRecord>>,
}

#[derive(Debug)]
pub(super) struct ModelTestUiState {
    target_mode: TargetMode,
    selected_upstream_id: Option<String>,
    selected_group_id: Option<String>,
    custom_base_url: String,
    custom_api_key: String,
    custom_wire_api: WireApi,
    model_input: String,
    fetched_models: Vec<String>,
    fetched_for_upstream: Option<String>,
    models_fetching: bool,
    stream: bool,
    max_tokens_input: String,
    reasoning_enabled: bool,
    reasoning_effort_input: String,
    timeout_input: String,
    prompt: String,
    section: Section,
    single_result: SingleResult,
    chat_messages: Vec<ChatEntry>,
    chat_input: String,
    chat_running: bool,
    chat_started: Option<Instant>,
    chat_live_text: String,
    chat_live_reasoning: String,
    history: Vec<ModelTestRecord>,
}

impl Default for ModelTestUiState {
    fn default() -> Self {
        Self {
            target_mode: TargetMode::Direct,
            selected_upstream_id: None,
            selected_group_id: None,
            custom_base_url: String::new(),
            custom_api_key: String::new(),
            custom_wire_api: WireApi::ChatCompletions,
            model_input: String::new(),
            fetched_models: Vec::new(),
            fetched_for_upstream: None,
            models_fetching: false,
            stream: true,
            max_tokens_input: DEFAULT_MAX_TOKENS.to_string(),
            reasoning_enabled: false,
            reasoning_effort_input: DEFAULT_REASONING_EFFORT.to_string(),
            timeout_input: DEFAULT_TIMEOUT_SECS.to_string(),
            prompt: DEFAULT_PROMPT.to_string(),
            section: Section::Single,
            single_result: SingleResult {
                started: None,
                outcome: None,
                live_text: String::new(),
                live_reasoning: String::new(),
            },
            chat_messages: Vec::new(),
            chat_input: String::new(),
            chat_running: false,
            chat_started: None,
            chat_live_text: String::new(),
            chat_live_reasoning: String::new(),
            history: Vec::new(),
        }
    }
}

impl ModelTestUiState {
    fn busy(&self) -> bool {
        self.single_result.started.is_some() || self.chat_running
    }
}

impl CodexSwitchApp {
    pub(super) fn model_test_ui(&mut self, ui: &mut egui::Ui) {
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
                    for (section, label) in
                        [(Section::Single, "单次测试"), (Section::Chat, "对话测试")]
                    {
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
                .on_hover_text("直连上游精确测试单个上游, 经调度组走本地代理完整链路, 临时上游使用临时的地址与密钥");
            match self.model_test_ui.target_mode {
                TargetMode::Direct => {
                    ui.separator();
                    ui.label("上游");
                    let selected_name = enabled_upstreams
                        .iter()
                        .find(|upstream| {
                            self.model_test_ui.selected_upstream_id.as_deref()
                                == Some(&upstream.id)
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
                                    self.model_test_ui.selected_upstream_id =
                                        Some(upstream.id.clone());
                                }
                            }
                        });
                    let selected_is_oauth = enabled_upstreams
                        .iter()
                        .find(|upstream| {
                            self.model_test_ui.selected_upstream_id.as_deref()
                                == Some(&upstream.id)
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
                TargetMode::Scheduler => {
                    ui.separator();
                    ui.label("调度组");
                    let selected_label = self
                        .model_test_selected_group_name()
                        .unwrap_or_else(|| "当前调度组".to_string());
                    egui::ComboBox::from_id_salt("model_test_group")
                        .selected_text(selected_label)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(
                                    self.model_test_ui.selected_group_id.is_none(),
                                    "当前调度组",
                                )
                                .clicked()
                            {
                                self.model_test_ui.selected_group_id = None;
                            }
                            for group in &self.schedule_groups {
                                let selected = self
                                    .model_test_ui
                                    .selected_group_id
                                    .as_deref()
                                    == Some(&group.id);
                                if ui.selectable_label(selected, &group.name).clicked() {
                                    self.model_test_ui.selected_group_id =
                                        Some(group.id.clone());
                                }
                            }
                        })
                        .response
                        .on_hover_text("选择测试请求经哪个调度组路由, 默认使用当前调度组");
                }
                TargetMode::Custom => {
                    ui.separator();
                    ui.label("Base URL");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.model_test_ui.custom_base_url)
                            .desired_width(280.0)
                            .hint_text("https://relay.example.com"),
                    );
                    ui.label("API Key");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.model_test_ui.custom_api_key)
                            .desired_width(160.0)
                            .password(true)
                            .hint_text("留空则不带认证"),
                    );
                }
            }
        });
        if self.model_test_ui.target_mode == TargetMode::Custom {
            ui.horizontal(|ui| {
                ui.label("请求模式");
                let wire_label = wire_api_label(self.model_test_ui.custom_wire_api);
                egui::ComboBox::from_id_salt("model_test_custom_wire_api")
                    .selected_text(wire_label)
                    .show_ui(ui, |ui| {
                        for wire in [
                            WireApi::ChatCompletions,
                            WireApi::Responses,
                            WireApi::AnthropicMessages,
                        ] {
                            if ui
                                .selectable_label(
                                    self.model_test_ui.custom_wire_api == wire,
                                    wire_api_label(wire),
                                )
                                .clicked()
                            {
                                self.model_test_ui.custom_wire_api = wire;
                            }
                        }
                    })
                    .response
                    .on_hover_text("临时上游使用的 API 协议");
            });
        }
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
            if !self.model_test_ui.fetched_models.is_empty()
                && model_list_for_target
                && self.model_test_ui.target_mode == TargetMode::Direct
            {
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
                    .on_hover_text("该上游的模型列表");
            }
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
            .on_disabled_hover_text("Anthropic 协议的直连测试不支持推理力度");
            if self.model_test_ui.reasoning_enabled && reasoning_supported {
                ui.add(
                    egui::TextEdit::singleline(&mut self.model_test_ui.reasoning_effort_input)
                        .desired_width(72.0)
                        .hint_text("low / medium / high"),
                )
                .on_hover_text("推理力度原样写入请求, 例如 low, medium, high, xhigh 或 minimal");
            }
        });
    }

    fn model_test_selected_group_name(&self) -> Option<String> {
        let id = self.model_test_ui.selected_group_id.as_deref()?;
        self.schedule_groups
            .iter()
            .find(|group| group.id == id)
            .map(|group| group.name.clone())
    }

    fn model_test_reasoning_supported(&self) -> bool {
        if self.model_test_ui.target_mode == TargetMode::Scheduler {
            return true;
        }
        if self.model_test_ui.target_mode == TargetMode::Custom {
            return self.model_test_ui.custom_wire_api != WireApi::AnthropicMessages;
        }
        self.model_test_target_upstream()
            .is_none_or(|upstream| upstream.wire_api != WireApi::AnthropicMessages)
    }

    fn model_test_single_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("测试 prompt:");
            ui.add(
                egui::TextEdit::singleline(&mut self.model_test_ui.prompt)
                    .desired_width(380.0)
                    .hint_text("单次测试使用的提示词"),
            );
            let running = self.model_test_ui.single_result.started.is_some();
            if ui.add_enabled(!running, egui::Button::new("发送")).clicked() {
                self.send_model_test(ModelTestKind::Single);
            }
            if running {
                ui.spinner();
                if let Some(started) = self.model_test_ui.single_result.started {
                    ui.label(format!("进行中, 已等待 {:.1}s", started.elapsed().as_secs_f32()));
                }
            }
        });
        ui.scope(|ui| {
            ui.set_max_width(test_content_width(ui));
            if self.model_test_ui.single_result.started.is_some() {
                // live 块与完成后的结果卡使用同一思维链 id, 展开状态跨阶段保留.
                model_test_stream_block(
                    ui,
                    single_reasoning_base_id(),
                    &self.model_test_ui.single_result.live_reasoning,
                    &self.model_test_ui.single_result.live_text,
                    true,
                );
                return;
            }
            let Some(outcome) = self.model_test_ui.single_result.outcome.clone() else {
                return;
            };
            self.model_test_outcome_card(ui, &outcome);
        });
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
        // 循环借用 chat_messages 期间不能调用 &mut self 的导出方法, 先记录意图再执行.
        let mut har_export: Option<(Arc<ModelTestRawTrace>, i64)> = None;
        // 对话内容不占满页面宽度, 与单次测试共用同一比例.
        let chat_width = test_content_width(ui);
        let chat_history = nested_scroll_area(
            ui,
            egui::Id::new("model_test_chat_history"),
            CHAT_HISTORY_HEIGHT,
            chat_width,
            true,
            |ui| {
                if self.model_test_ui.chat_messages.is_empty() {
                    ui.label("暂无消息, 在下方输入内容开始对话");
                }
                for (message_index, entry) in
                    self.model_test_ui.chat_messages.iter().enumerate()
                {
                    let (role_label, color) = match entry.role {
                        ChatRole::User => ("[用户]", egui::Color32::from_rgb(96, 165, 250)),
                        ChatRole::Assistant => ("[模型]", egui::Color32::from_rgb(34, 197, 94)),
                    };
                    ui.colored_label(color, role_label);
                    if !entry.reasoning.is_empty() {
                        model_test_reasoning_block(
                            ui,
                            chat_reasoning_base_id(message_index).with("reasoning"),
                            &entry.reasoning,
                        );
                    }
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
                    if let Some(record) = &entry.record {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "耗时 {} ms, 首 token {}, 总 tokens {}",
                                    record.duration_ms,
                                    record
                                        .first_token_ms
                                        .map(|ms| format!("{ms} ms"))
                                        .unwrap_or_else(|| "未返回".to_string()),
                                    tokens::format_tokens(
                                        self.token_display_mode,
                                        record.total_tokens,
                                    ),
                                ))
                                .weak()
                                .small(),
                            );
                            if let Some(raw) = &record.raw
                                && ui.small_button("导出 HAR").clicked()
                            {
                                har_export = Some((raw.clone(), record.duration_ms));
                            }
                        });
                    }
                }
                if self.model_test_ui.chat_running {
                    // 流式期间用户消息已入列, len 即完成后 assistant 条目的下标,
                    // 用它派生思维链 id, 生成结束后折叠区的展开状态得以延续.
                    let live_entry_index = self.model_test_ui.chat_messages.len();
                    if !self.model_test_ui.chat_live_text.is_empty()
                        || !self.model_test_ui.chat_live_reasoning.is_empty()
                    {
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(egui::Color32::from_rgb(34, 197, 94), "[模型]");
                            ui.label(egui::RichText::new("生成中...").weak());
                        });
                        model_test_stream_block(
                            ui,
                            chat_reasoning_base_id(live_entry_index),
                            &self.model_test_ui.chat_live_reasoning,
                            &self.model_test_ui.chat_live_text,
                            false,
                        );
                    }
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
            },
        );
        stop_scroll_chaining(ui, chat_history.inner_rect);
        if let Some((raw, duration_ms)) = har_export {
            self.export_model_test_har(raw, duration_ms, None);
        }
        ui.add_space(4.0);
        // 右到左布局: 输入框占满剩余宽度, 发送按钮贴在最右侧.
        // 同样用显式 max_rect, 避免依赖页面滚动后可能失效的剩余视口高度.
        let mut send_clicked = false;
        let mut enter_pressed = false;
        let input_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(chat_width, ui.spacing().interact_size.y),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(input_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.label("对话 prompt:");
                let input_response = ui.add(
                    egui::TextEdit::singleline(&mut self.model_test_ui.chat_input)
                        .desired_width(380.0)
                        .hint_text("输入消息, Enter 发送"),
                );
                send_clicked = ui
                    .add_enabled(!self.model_test_ui.chat_running, egui::Button::new("发送"))
                    .clicked();
                enter_pressed = input_response.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
            },
        );
        if (send_clicked || enter_pressed) && !self.model_test_ui.chat_running {
            self.send_model_test_chat();
        }
    }

    fn model_test_history_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(format!("测试记录 ({})", self.model_test_ui.history.len()));
            if ui
                .add_enabled(
                    !self.model_test_ui.history.is_empty(),
                    egui::Button::new("导出全部"),
                )
                .on_hover_text("把全部测试记录导出为一个 HAR 文件, 记录保存在内存中, 包含完整请求与响应报文, 敏感头已脱敏")
                .on_disabled_hover_text("暂无测试记录")
                .clicked()
            {
                self.export_model_test_history_har();
            }
            if ui
                .add_enabled(
                    !self.model_test_ui.history.is_empty(),
                    egui::Button::new("清除记录"),
                )
                .on_hover_text("清空内存中的测试记录, 不影响日志页中的请求日志")
                .clicked()
            {
                let count = self.model_test_ui.history.len();
                self.model_test_ui.history.clear();
                self.status = format!("已清除 {count} 条测试记录");
            }
        });
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
                for record in &self.model_test_ui.history {
                    ui.label(record.finished_at.format("%m-%d %H:%M:%S").to_string());
                    ui.label(record.upstream_name.as_deref().unwrap_or("调度组"));
                    ui.label(&record.model);
                    let (label, color) = match &record.error {
                        Some(_) => ("失败", error_color()),
                        None if (200..300).contains(&record.status) => ("成功", success_color()),
                        None => ("失败", error_color()),
                    };
                    let hover = record
                        .error
                        .clone()
                        .unwrap_or_else(|| format!("测试台请求 {}", record.endpoint));
                    ui.colored_label(color, label).on_hover_text(hover);
                    ui.label(format!("{} ms", record.duration_ms));
                    ui.label(match record.first_token_ms {
                        Some(ms) => format!("{ms} ms"),
                        None if record.stream => "未返回".to_string(),
                        None => "非流式".to_string(),
                    });
                    tokens::token_number(
                        ui,
                        &mut token_display_mode,
                        record.total_tokens,
                    );
                    match record.estimated_cost_usd {
                        Some(cost) => {
                            tokens::cost_value(ui, &mut currency_display_mode, rate, cost);
                        }
                        None => {
                            ui.label("-").on_hover_text("无价格缓存");
                        }
                    }
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
        if !outcome.reasoning_text.is_empty() {
            // 与流式阶段的思维链折叠区同 id, 输出结束后保持展开状态.
            model_test_reasoning_block(
                ui,
                single_reasoning_base_id().with("reasoning"),
                &outcome.reasoning_text,
            );
        }
        if !outcome.output_text.is_empty() {
            nested_scroll_area(
                ui,
                egui::Id::new("model_test_single_output"),
                RESULT_TEXT_HEIGHT,
                ui.available_width(),
                false,
                |ui| {
                    ui.add(
                        egui::Label::new(&outcome.output_text)
                            .wrap()
                            .selectable(true),
                    );
                },
            );
        }
        if !outcome.output_text.is_empty() || outcome.raw.is_some() {
            ui.horizontal(|ui| {
                if !outcome.output_text.is_empty() && ui.button("复制").clicked() {
                    ui.ctx().copy_text(outcome.output_text.clone());
                    self.status = "回复已复制".to_string();
                }
                if let (Some(raw), duration_ms) = (&outcome.raw, outcome.duration_ms)
                    && ui.button("导出 HAR").clicked()
                {
                    let error = outcome.error.as_deref();
                    self.export_model_test_har(raw.clone(), duration_ms, error);
                }
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
                    .reasoning_effort_input
                    .trim()
                    .to_string()
            }).filter(|value| !value.is_empty()),
            timeout: Duration::from_secs(timeout),
        })
    }

    /// 构造临时上游对象, 供自定义直连测试使用.
    fn model_test_custom_upstream(&self) -> Result<Upstream, String> {
        let base_url = self.model_test_ui.custom_base_url.trim().to_string();
        if base_url.is_empty() {
            return Err("请填写临时上游的 Base URL".to_string());
        }
        Ok(Upstream::new_relay(
            CUSTOM_UPSTREAM_NAME.to_string(),
            base_url,
            self.model_test_ui.custom_wire_api,
            true,
            BalanceProvider::Unsupported,
        ))
    }

    /// 按当前发送方式派发一次测试请求, 返回是否成功派发.
    fn dispatch_model_test(
        &mut self,
        kind: ModelTestKind,
        params: test_bench::ModelTestParams,
    ) -> bool {
        match self.model_test_ui.target_mode {
            TargetMode::Scheduler => {
                if self.server.is_none() {
                    self.status = "本地代理未启动, 请先在仪表盘启动服务".to_string();
                    return false;
                }
                let state = self.state.clone();
                let tx = self.task_tx.clone();
                let bind_addr = self.bind_addr.clone();
                let local_key = self.local_key.clone();
                let group_id = self.model_test_ui.selected_group_id.clone();
                let sink = self.make_model_test_delta_sink(kind.clone());
                self.runtime.spawn(async move {
                    let result = test_bench::run_scheduler_test(
                        &state,
                        &bind_addr,
                        &local_key,
                        group_id.as_deref(),
                        params,
                        Some(sink),
                    )
                    .await;
                    let _ = tx.send(UiTaskEvent::ModelTestFinished { kind, result });
                });
                true
            }
            TargetMode::Direct => {
                let Some(upstream) = self.model_test_target_upstream() else {
                    self.status = "请选择要测试的上游".to_string();
                    return false;
                };
                let state = self.state.clone();
                let tx = self.task_tx.clone();
                let sink = self.make_model_test_delta_sink(kind.clone());
                self.runtime.spawn(async move {
                    let result =
                        test_bench::run_direct_test(&state, &upstream, None, params, Some(sink))
                            .await;
                    let _ = tx.send(UiTaskEvent::ModelTestFinished { kind, result });
                });
                true
            }
            TargetMode::Custom => {
                let Ok(upstream) = self.model_test_custom_upstream() else {
                    self.status = "请填写临时上游的 Base URL".to_string();
                    return false;
                };
                let api_key = self.model_test_ui.custom_api_key.trim().to_string();
                let state = self.state.clone();
                let tx = self.task_tx.clone();
                let sink = self.make_model_test_delta_sink(kind.clone());
                self.runtime.spawn(async move {
                    let result = test_bench::run_direct_test(
                        &state,
                        &upstream,
                        Some(&api_key),
                        params,
                        Some(sink),
                    )
                    .await;
                    let _ = tx.send(UiTaskEvent::ModelTestFinished { kind, result });
                });
                true
            }
        }
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
        if self.dispatch_model_test(kind, params) {
            self.model_test_ui.single_result = SingleResult::running();
            self.status = "测试请求已发送".to_string();
        }
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
            reasoning: String::new(),
            error: false,
            record: None,
        });
        self.model_test_ui.chat_input.clear();
        if self.dispatch_model_test(ModelTestKind::Chat, params) {
            self.model_test_ui.chat_running = true;
            self.model_test_ui.chat_started = Some(Instant::now());
        } else {
            self.model_test_ui.chat_messages.push(ChatEntry {
                role: ChatRole::Assistant,
                text: self.status.clone(),
                reasoning: String::new(),
                error: true,
                record: None,
            });
        }
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

    pub(super) fn handle_model_test_delta(
        &mut self,
        kind: ModelTestKind,
        part: ModelTestStreamPart,
    ) {
        match (&kind, part) {
            (ModelTestKind::Single, ModelTestStreamPart::Text(text)) => {
                self.model_test_ui.single_result.live_text.push_str(&text);
            }
            (ModelTestKind::Single, ModelTestStreamPart::Reasoning(text)) => {
                self.model_test_ui
                    .single_result
                    .live_reasoning
                    .push_str(&text);
            }
            (ModelTestKind::Chat, ModelTestStreamPart::Text(text)) => {
                self.model_test_ui.chat_live_text.push_str(&text);
            }
            (ModelTestKind::Chat, ModelTestStreamPart::Reasoning(text)) => {
                self.model_test_ui.chat_live_reasoning.push_str(&text);
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
        let record = Arc::new(ModelTestRecord::from_outcome(&outcome));
        self.model_test_ui.history.push((*record).clone());
        match kind {
            ModelTestKind::Single => {
                self.model_test_ui.single_result = SingleResult {
                    started: None,
                    outcome: Some(outcome),
                    live_text: String::new(),
                    live_reasoning: String::new(),
                };
            }
            ModelTestKind::Chat => {
                self.model_test_ui.chat_running = false;
                self.model_test_ui.chat_started = None;
                self.model_test_ui.chat_live_text.clear();
                self.model_test_ui.chat_live_reasoning.clear();
                let entry = if success {
                    ChatEntry {
                        role: ChatRole::Assistant,
                        text: outcome.output_text,
                        reasoning: outcome.reasoning_text,
                        error: false,
                        record: Some(record),
                    }
                } else {
                    ChatEntry {
                        role: ChatRole::Assistant,
                        text: error_text
                            .clone()
                            .unwrap_or_else(|| format!("请求失败 ({})", outcome.status)),
                        reasoning: String::new(),
                        error: true,
                        record: Some(record),
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

    /// 构造把流式增量转发回 UI 事件的回调.
    fn make_model_test_delta_sink(&self, kind: ModelTestKind) -> test_bench::ModelTestDeltaSink {
        let tx = self.task_tx.clone();
        Box::new(move |part| {
            let _ = tx.send(UiTaskEvent::ModelTestDelta { kind: kind.clone(), part });
        })
    }

    /// 把一次测试的原始请求与响应导出为 HAR 文件.
    fn export_model_test_har(
        &mut self,
        trace: Arc<ModelTestRawTrace>,
        duration_ms: i64,
        error: Option<&str>,
    ) {
        let entry = trace.to_har_entry(duration_ms, error);
        self.save_har_document("model-test.har", har_document(vec![entry]));
    }

    /// 把测试记录表格中的全部记录导出为一个 HAR 文件.
    /// 记录在内存中保存了完整报文, 导出内容与单条导出一致.
    fn export_model_test_history_har(&mut self) {
        let entries: Vec<serde_json::Value> = self
            .model_test_ui
            .history
            .iter()
            .map(model_test_record_har_entry)
            .collect();
        self.save_har_document("model-test-history.har", har_document(entries));
    }

    /// 弹出保存对话框并写入 HAR 文件.
    fn save_har_document(&mut self, default_file_name: &str, har: serde_json::Value) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(default_file_name)
            .add_filter("HAR", &["har"])
            .save_file()
        else {
            return;
        };
        match serde_json::to_vec_pretty(&har)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| std::fs::write(&path, bytes).map_err(anyhow::Error::from))
        {
            Ok(()) => {
                self.status = format!("已导出 HAR: {}", path.display());
            }
            Err(err) => {
                self.status = format!("导出 HAR 失败: {err}");
            }
        }
    }
}

/// 单次测试思维链区域的基准 id, 流式 live 块与结果卡都必须由它派生,
/// 输出结束后思维链才不会因 id 变化而自动收起.
fn single_reasoning_base_id() -> egui::Id {
    egui::Id::new("model_test_single_reasoning")
}

/// 第 i 条对话消息思维链区域的基准 id, message_index 是 assistant 条目
/// 在消息列表中的最终下标, live 与完成后共用.
fn chat_reasoning_base_id(message_index: usize) -> egui::Id {
    egui::Id::new("model_test_chat_reasoning").with(message_index)
}

fn outcome_status_label(outcome: &ModelTestOutcome) -> (&'static str, egui::Color32) {
    if outcome.is_success() {
        ("成功", success_color())
    } else {
        ("失败", error_color())
    }
}

/// 组装 HAR 1.2 文档.
fn har_document(entries: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "log": {
            "version": "1.2",
            "creator": {
                "name": "codex-switch",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "entries": entries,
        }
    })
}

/// 把一条内存记录转成 HAR entry: 有原始报文时导出完整报文,
/// 无报文 (网络层失败未拿到响应) 时退化为仅含元数据的 entry.
fn model_test_record_har_entry(record: &ModelTestRecord) -> serde_json::Value {
    let mut entry = match &record.raw {
        Some(raw) => raw.to_har_entry(record.duration_ms, None),
        None => {
            let status_text = reqwest::StatusCode::from_u16(
                record.status.clamp(0, u16::MAX as i64) as u16,
            )
            .ok()
            .and_then(|status| status.canonical_reason())
            .unwrap_or_default();
            serde_json::json!({
                "startedDateTime": record.finished_at.to_rfc3339(),
                "time": record.duration_ms,
                "request": {
                    "method": "POST",
                    "url": record.endpoint,
                    "httpVersion": "HTTP/1.1",
                    "headers": [],
                    "queryString": [],
                    "cookies": [],
                    "headersSize": -1,
                    "bodySize": -1,
                },
                "response": {
                    "status": record.status,
                    "statusText": status_text,
                    "httpVersion": "HTTP/1.1",
                    "headers": [],
                    "content": {
                        "size": -1,
                        "mimeType": "",
                    },
                    "redirectURL": "",
                    "headersSize": -1,
                    "bodySize": -1,
                },
                "cache": {},
                "timings": {
                    "send": 0,
                    "wait": record.duration_ms,
                    "receive": 0,
                },
            })
        }
    };
    if let Some(upstream) = &record.upstream_name {
        entry["_upstream"] = serde_json::json!(upstream);
    }
    entry["_model"] = serde_json::json!(record.model);
    if let Some(ms) = record.first_token_ms {
        entry["_first_token_ms"] = serde_json::json!(ms);
    }
    entry["_total_tokens"] = serde_json::json!(record.total_tokens);
    if let Some(cost) = record.estimated_cost_usd {
        entry["_estimated_cost_usd"] = serde_json::json!(cost);
    }
    if let Some(error) = &record.error
        && !error.is_empty()
    {
        entry["_error"] = serde_json::json!(error);
    }
    entry
}

/// 单次测试与对话测试的回复内容宽度, 不占满页面.
fn test_content_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() * TEST_CONTENT_WIDTH_RATIO).round()
}

/// 渲染流式/已完成的回复块: 思维链折叠区 + 正文区, live 为 true 时标注生成中.
/// 思维链折叠头的 id 是 id_salt.with("reasoning"), 调用方在流式阶段与完成阶段
/// 必须派生自同一基准 id, 否则折叠展开状态会重置, 输出结束时思维链看似被自动收起.
fn model_test_stream_block(
    ui: &mut egui::Ui,
    id_salt: egui::Id,
    reasoning: &str,
    text: &str,
    live: bool,
) {
    if !reasoning.is_empty() {
        model_test_reasoning_block(ui, id_salt.with("reasoning"), reasoning);
    }
    if live {
        ui.label(egui::RichText::new("生成中...").weak());
    }
    let inner_rect = nested_scroll_area(
        ui,
        id_salt.with("text"),
        RESULT_TEXT_HEIGHT,
        ui.available_width(),
        live,
        |ui| {
            let display = if text.is_empty() && live {
                "(等待输出...)"
            } else {
                text
            };
            ui.add(egui::Label::new(display).wrap().selectable(true));
        },
    )
    .inner_rect;
    stop_scroll_chaining(ui, inner_rect);
}

/// 渲染折叠的思维链区块, 流式中展开会自动跟随最新内容.
fn model_test_reasoning_block(ui: &mut egui::Ui, id_salt: egui::Id, reasoning: &str) {
    egui::CollapsingHeader::new("思维链")
        .id_salt(id_salt)
        .default_open(false)
        .show(ui, |ui| {
            let inner_rect = nested_scroll_area(
                ui,
                id_salt.with("text"),
                RESULT_TEXT_HEIGHT,
                ui.available_width(),
                true,
                |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(reasoning).weak())
                            .wrap()
                            .selectable(true),
                    );
                },
            )
            .inner_rect;
            stop_scroll_chaining(ui, inner_rect);
        });
}

/// 在显式定宽定高的子区域内渲染嵌套滚动区, 高度按内容收缩, 上限 max_height.
/// 嵌套 ScrollArea 不能直接依赖外层剩余视口高度: 页面下滚后它可能塌缩甚至
/// 得到负的可用矩形, 导致布局错乱, 必须用显式 max_rect 定尺寸.
fn nested_scroll_area<R>(
    ui: &mut egui::Ui,
    id_salt: egui::Id,
    max_height: f32,
    width: f32,
    stick_to_bottom: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::scroll_area::ScrollAreaOutput<R> {
    let rect =
        egui::Rect::from_min_size(ui.cursor().min, egui::vec2(width, max_height));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        egui::ScrollArea::vertical()
            .id_salt(id_salt)
            .max_height(max_height)
            .stick_to_bottom(stick_to_bottom)
            .show(ui, add_contents)
    })
    .inner
}

/// 指针悬停在嵌套滚动区上时清掉越过边界的剩余滚动量,
/// 防止内层滚到头后滚动链带动外层滚动区.
/// 内层自身需要的滚动量已在 show 内部被消费, 不受影响.
fn stop_scroll_chaining(ui: &mut egui::Ui, inner_rect: egui::Rect) {
    if ui.rect_contains_pointer(inner_rect) {
        ui.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
    }
}

/// 请求模式下拉框的显示文案.
fn wire_api_label(wire_api: WireApi) -> &'static str {
    match wire_api {
        WireApi::ChatCompletions => "Chat Completions",
        WireApi::Responses => "Responses",
        WireApi::AnthropicMessages => "Anthropic Messages",
    }
}

fn error_color() -> egui::Color32 {
    egui::Color32::from_rgb(239, 68, 68)
}

fn success_color() -> egui::Color32 {
    egui::Color32::from_rgb(34, 197, 94)
}
