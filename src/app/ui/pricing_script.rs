use super::CodexSwitchApp;
use crate::core::models::TokenUsage;
use crate::pricing;
use eframe::egui::{self, TextBuffer};

/// 与 docs/pricing-guide.md 同源, 编译进二进制后可离线查阅.
const PRICING_GUIDE: &str = include_str!("../../../docs/pricing-guide.md");

#[derive(Debug, Clone)]
pub(super) struct PricingScriptUi {
    pub open: bool,
    pub docs_open: bool,
    pub enabled: bool,
    pub source: String,
    pub compile_message: String,
    pub compile_ok: bool,
    pub preview_model: String,
    pub preview_input: String,
    pub preview_output: String,
    pub preview_cache_read: String,
    pub preview_cache_write: String,
    pub preview_upstream_id: Option<String>,
    pub preview_script: String,
    pub preview_builtin: String,
    pub preview_logs: String,
}

impl Default for PricingScriptUi {
    fn default() -> Self {
        Self {
            open: false,
            docs_open: false,
            enabled: false,
            source: String::new(),
            compile_message: String::new(),
            compile_ok: true,
            preview_model: "gpt-5-codex".to_string(),
            preview_input: "1000".to_string(),
            preview_output: "200".to_string(),
            preview_cache_read: "0".to_string(),
            preview_cache_write: "0".to_string(),
            preview_upstream_id: None,
            preview_script: String::new(),
            preview_builtin: String::new(),
            preview_logs: String::new(),
        }
    }
}

impl CodexSwitchApp {
    pub(super) fn open_pricing_script_window(&mut self) {
        self.pricing_ui.enabled = self.state.pricing.enabled();
        self.pricing_ui.source = self.state.pricing.source();
        self.refresh_pricing_compile_message();
        self.pricing_ui.preview_script.clear();
        self.pricing_ui.preview_builtin.clear();
        self.pricing_ui.preview_logs.clear();
        self.pricing_ui.open = true;
    }

    pub(super) fn pricing_script_window(&mut self, ctx: &egui::Context) {
        if !self.pricing_ui.open {
            self.pricing_script_docs_window(ctx);
            return;
        }
        let mut open = self.pricing_ui.open;
        let mut save_requested = false;
        let mut cancel_requested = false;
        let mut template_requested = false;
        let mut preview_requested = false;
        let mut docs_requested = false;
        let mut clear_live_logs_requested = false;
        let mut source_changed = false;
        egui::Window::new("计价脚本")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .default_height(520.0)
            .show(ctx, |ui| {
                ui.label(
                    "保存后立即覆盖费用估算, 无需重启. 返回值必须是 USD. 未启用, 为空, 编译失败或返回 () 时回退内置公式.",
                );
                ui.checkbox(&mut self.pricing_ui.enabled, "启用脚本");
                egui::ScrollArea::both()
                    .id_salt("pricing_script_source_scroll")
                    .max_height(240.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut layouter =
                            |ui: &egui::Ui, buf: &dyn TextBuffer, _wrap_width: f32| {
                                let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                                let mut job = egui::text::LayoutJob::simple(
                                    buf.as_str().to_owned(),
                                    font_id,
                                    ui.visuals().text_color(),
                                    f32::INFINITY,
                                );
                                job.wrap.max_width = f32::INFINITY;
                                job.keep_trailing_whitespace = true;
                                ui.fonts_mut(|fonts| fonts.layout_job(job))
                            };
                        let editor_id = ui.id().with("pricing_script_source");
                        replace_tab_key_with_spaces(ui, editor_id);
                        let response = ui.add(
                            egui::TextEdit::multiline(&mut self.pricing_ui.source)
                                .id(editor_id)
                                .code_editor()
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY)
                                .desired_rows(14)
                                .layouter(&mut layouter),
                        );
                        source_changed = response.changed();
                    });
                let message_color = if self.pricing_ui.compile_ok {
                    ui.visuals().weak_text_color()
                } else {
                    ui.visuals().error_fg_color
                };
                ui.colored_label(message_color, &self.pricing_ui.compile_message);
                if let Some(error) = self.state.pricing.compile_error() {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("已保存脚本: {error}"));
                }
                if let Some(error) = self.state.pricing.runtime_error() {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("最近运行: {error}"));
                }
                ui.separator();
                ui.label("试算");
                ui.horizontal(|ui| {
                    ui.label("模型");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pricing_ui.preview_model)
                            .desired_width(180.0),
                    );
                    ui.label("上游");
                    let selected = self
                        .pricing_ui
                        .preview_upstream_id
                        .as_deref()
                        .and_then(|id| {
                            self.upstreams
                                .iter()
                                .find(|upstream| upstream.id == id)
                                .map(|upstream| upstream.name.as_str())
                        })
                        .unwrap_or("无");
                    egui::ComboBox::from_id_salt("pricing_preview_upstream")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.pricing_ui.preview_upstream_id,
                                None,
                                "无",
                            );
                            for upstream in &self.upstreams {
                                ui.selectable_value(
                                    &mut self.pricing_ui.preview_upstream_id,
                                    Some(upstream.id.clone()),
                                    &upstream.name,
                                );
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label("输入");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pricing_ui.preview_input)
                            .desired_width(72.0),
                    );
                    ui.label("输出");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pricing_ui.preview_output)
                            .desired_width(72.0),
                    );
                    ui.label("缓存读");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pricing_ui.preview_cache_read)
                            .desired_width(72.0),
                    );
                    ui.label("缓存写");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pricing_ui.preview_cache_write)
                            .desired_width(72.0),
                    );
                });
                if !self.pricing_ui.preview_script.is_empty()
                    || !self.pricing_ui.preview_builtin.is_empty()
                {
                    ui.label(format!(
                        "脚本: {}    内置: {}",
                        self.pricing_ui.preview_script, self.pricing_ui.preview_builtin
                    ));
                }
                let live_logs = self.state.pricing.last_logs();
                if !self.pricing_ui.preview_logs.is_empty() {
                    ui.label("试算日志");
                    egui::ScrollArea::vertical()
                        .id_salt("pricing_preview_logs_scroll")
                        .max_height(80.0)
                        .show(ui, |ui| {
                            ui.monospace(&self.pricing_ui.preview_logs);
                        });
                }
                if !live_logs.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label("最近运行日志");
                        if ui
                            .small_button("清理")
                            .on_hover_text("清空窗口中的最近运行日志, 不影响应用主日志")
                            .clicked()
                        {
                            clear_live_logs_requested = true;
                        }
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("pricing_live_logs_scroll")
                        .max_height(80.0)
                        .show(ui, |ui| {
                            ui.monospace(live_logs.join("\n"));
                        });
                }
                ui.horizontal(|ui| {
                    if ui
                        .button("文档")
                        .on_hover_text("打开内置约定, 字段和示例")
                        .clicked()
                    {
                        docs_requested = true;
                    }
                    if ui.button("填入模板").clicked() {
                        template_requested = true;
                    }
                    if ui.button("试算").clicked() {
                        preview_requested = true;
                    }
                    if ui.button("保存").clicked() {
                        save_requested = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel_requested = true;
                    }
                });
            });
        if source_changed {
            self.refresh_pricing_compile_message();
        }
        if docs_requested {
            self.pricing_ui.docs_open = true;
        }
        if template_requested {
            self.pricing_ui.source = pricing::DEFAULT_SCRIPT.to_string();
            self.refresh_pricing_compile_message();
        }
        if preview_requested {
            self.preview_pricing_script();
        }
        if clear_live_logs_requested {
            self.state.pricing.clear_last_logs();
        }
        if save_requested {
            self.save_pricing_script();
        } else if cancel_requested {
            self.pricing_ui.open = false;
        } else {
            self.pricing_ui.open = open;
        }
        self.pricing_script_docs_window(ctx);
    }

    fn pricing_script_docs_window(&mut self, ctx: &egui::Context) {
        if !self.pricing_ui.docs_open {
            return;
        }
        let mut open = self.pricing_ui.docs_open;
        egui::Window::new("计价脚本文档")
            .id(egui::Id::new("pricing_script_docs_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([480.0, 520.0])
            .min_size([360.0, 240.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("pricing_script_docs_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width());
                        show_pricing_guide(ui, PRICING_GUIDE);
                    });
            });
        self.pricing_ui.docs_open = open;
    }

    fn refresh_pricing_compile_message(&mut self) {
        match pricing::compile_check(&self.pricing_ui.source) {
            Ok(()) => {
                self.pricing_ui.compile_ok = true;
                self.pricing_ui.compile_message = if self.pricing_ui.source.trim().is_empty() {
                    "空脚本将使用内置公式".to_string()
                } else {
                    "编译通过".to_string()
                };
            }
            Err(error) => {
                self.pricing_ui.compile_ok = false;
                self.pricing_ui.compile_message = error;
            }
        }
    }

    fn preview_pricing_script(&mut self) {
        let mut usage = TokenUsage {
            input_tokens: parse_i64_or_zero(&self.pricing_ui.preview_input),
            output_tokens: parse_i64_or_zero(&self.pricing_ui.preview_output),
            cache_read_tokens: parse_i64_or_zero(&self.pricing_ui.preview_cache_read),
            cache_creation_tokens: parse_i64_or_zero(&self.pricing_ui.preview_cache_write),
            total_tokens: 0,
        };
        usage.finish();
        let owned_upstream = self
            .pricing_ui
            .preview_upstream_id
            .as_deref()
            .and_then(|id| self.upstreams.iter().find(|upstream| upstream.id == id))
            .map(pricing::CostUpstream::from_upstream);
        let source = self.pricing_ui.source.clone();
        let model = self.pricing_ui.preview_model.trim().to_string();
        let env = self
            .runtime
            .block_on(pricing::load_cost_estimate_env(&self.state.store))
            .unwrap_or_else(|err| {
                tracing::warn!(error = %err, "failed to load pricing env for preview");
                pricing::CostEstimateEnv::now(self.usd_cny_rate)
            });
        let preview = self.runtime.block_on(pricing::preview_estimate(
            &self.state.store,
            &source,
            &env,
            pricing::CostEstimateInput {
                model: Some(model.as_str()).filter(|value| !value.is_empty()),
                target_model: None,
                usage: &usage,
                upstream: owned_upstream.as_ref(),
            },
        ));
        if let Some(error) = preview.compile_error.or(preview.runtime_error) {
            self.pricing_ui.preview_script = format!("失败: {error}");
        } else {
            self.pricing_ui.preview_script = format_optional_cost(preview.script_cost);
        }
        self.pricing_ui.preview_builtin = format_optional_cost(preview.builtin_cost);
        self.pricing_ui.preview_logs = preview.logs.join("\n");
        let fx_text = match env.fx {
            Some(fx) => format!("1 USD = {:.4} CNY", fx.rate),
            None => "无汇率缓存".to_string(),
        };
        self.status = format!(
            "试算时间 {} (本地 {} 时), {}",
            env.now.to_rfc3339(),
            env.now.with_timezone(&chrono::Local).format("%H:%M"),
            fx_text
        );
    }

    fn save_pricing_script(&mut self) {
        self.state
            .pricing
            .apply(self.pricing_ui.enabled, self.pricing_ui.source.clone());
        match self
            .runtime
            .block_on(self.state.pricing.persist(&self.state.store))
        {
            Ok(()) => {
                self.pricing_ui.open = false;
                self.status = if self.state.pricing.is_ready() {
                    "计价脚本已保存并启用".to_string()
                } else if self.state.pricing.enabled() {
                    "计价脚本已保存, 但编译失败, 已回退内置公式".to_string()
                } else {
                    "计价脚本已保存".to_string()
                };
                self.refresh_all_if_visible();
            }
            Err(err) => {
                self.status = format!("保存计价脚本失败: {err}");
            }
        }
    }
}

fn parse_i64_or_zero(value: &str) -> i64 {
    value.trim().parse().unwrap_or(0).max(0)
}

/// egui code_editor 默认把 Tab 写成 `\t`. 聚焦脚本框时改成两个空格.
fn replace_tab_key_with_spaces(ui: &mut egui::Ui, editor_id: egui::Id) {
    if !ui.memory(|mem| mem.has_focus(editor_id)) {
        return;
    }
    ui.ctx().input_mut(|input| {
        for event in &mut input.events {
            if let egui::Event::Key {
                key: egui::Key::Tab,
                pressed: true,
                modifiers,
                ..
            } = event
                && !modifiers.any()
            {
                *event = egui::Event::Text("  ".to_owned());
            }
        }
    });
}

fn format_optional_cost(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.6} USD"),
        None => "无".to_string(),
    }
}

fn show_pricing_guide(ui: &mut egui::Ui, source: &str) {
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with("```") {
            let mut code = String::new();
            for inner in lines.by_ref() {
                if inner.starts_with("```") {
                    break;
                }
                if !code.is_empty() {
                    code.push('\n');
                }
                code.push_str(inner);
            }
            let width = ui.available_width();
            egui::Frame::group(ui.style())
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ui.set_max_width(width);
                    ui.add(
                        egui::Label::new(egui::RichText::new(&code).monospace())
                            .wrap_mode(egui::TextWrapMode::Wrap)
                            .selectable(true),
                    );
                });
            continue;
        }
        if line.starts_with('|') {
            let mut rows = vec![parse_md_row(line)];
            while lines.peek().is_some_and(|next| next.starts_with('|')) {
                if let Some(row) = lines.next() {
                    rows.push(parse_md_row(row));
                }
            }
            rows.retain(|row| !is_md_table_separator(row));
            show_guide_table(ui, &rows);
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(egui::RichText::new(strip_md_inline(rest)).heading())
                    .wrap_mode(egui::TextWrapMode::Wrap)
                    .selectable(true),
            );
            continue;
        }
        if let Some(rest) = line.strip_prefix("## ") {
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(strip_md_inline(rest)).strong().size(16.0),
                )
                .wrap_mode(egui::TextWrapMode::Wrap)
                .selectable(true),
            );
            continue;
        }
        if let Some(rest) = line.strip_prefix("- ") {
            ui.horizontal_top(|ui| {
                ui.label("•");
                ui.add(
                    egui::Label::new(strip_md_inline(rest))
                        .wrap_mode(egui::TextWrapMode::Wrap)
                        .selectable(true),
                );
            });
            continue;
        }
        if line.is_empty() {
            ui.add_space(6.0);
            continue;
        }
        ui.add(
            egui::Label::new(strip_md_inline(line))
                .wrap_mode(egui::TextWrapMode::Wrap)
                .selectable(true),
        );
    }
}

fn show_guide_table(ui: &mut egui::Ui, rows: &[Vec<String>]) {
    let cols = rows.iter().map(|row| row.len()).max().unwrap_or(0);
    if cols == 0 {
        return;
    }
    for (index, row) in rows.iter().enumerate() {
        let fill = if index == 0 {
            ui.visuals().widgets.inactive.weak_bg_fill
        } else if index % 2 == 1 {
            ui.visuals().faint_bg_color
        } else {
            egui::Color32::TRANSPARENT
        };
        egui::Frame::new()
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(8, 5))
            .show(ui, |ui| {
                ui.columns(cols, |columns| {
                    for (col, column) in columns.iter_mut().enumerate() {
                        let text = row
                            .get(col)
                            .map(|cell| strip_md_inline(cell))
                            .unwrap_or_default();
                        let rich = if index == 0 {
                            egui::RichText::new(text).strong()
                        } else {
                            egui::RichText::new(text)
                        };
                        column.add(
                            egui::Label::new(rich)
                                .wrap_mode(egui::TextWrapMode::Wrap)
                                .selectable(true),
                        );
                    }
                });
            });
    }
}

fn parse_md_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn is_md_table_separator(row: &[String]) -> bool {
    !row.is_empty()
        && row.iter().all(|cell| {
            !cell.is_empty() && cell.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
        })
}

fn strip_md_inline(text: &str) -> String {
    text.replace('`', "")
}
