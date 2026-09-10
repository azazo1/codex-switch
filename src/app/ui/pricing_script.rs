use super::CodexSwitchApp;
use crate::core::models::TokenUsage;
use crate::pricing;
use eframe::egui::{self, TextBuffer};

#[derive(Debug, Clone)]
pub(super) struct PricingScriptUi {
    pub open: bool,
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
            return;
        }
        let mut open = self.pricing_ui.open;
        let mut save_requested = false;
        let mut cancel_requested = false;
        let mut template_requested = false;
        let mut preview_requested = false;
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
                        let response = ui.add(
                            egui::TextEdit::multiline(&mut self.pricing_ui.source)
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
                    ui.label("最近运行日志");
                    egui::ScrollArea::vertical()
                        .id_salt("pricing_live_logs_scroll")
                        .max_height(80.0)
                        .show(ui, |ui| {
                            ui.monospace(live_logs.join("\n"));
                        });
                }
                ui.horizontal(|ui| {
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
        if template_requested {
            self.pricing_ui.source = pricing::DEFAULT_SCRIPT.to_string();
            self.refresh_pricing_compile_message();
        }
        if preview_requested {
            self.preview_pricing_script();
        }
        if save_requested {
            self.save_pricing_script();
        } else if cancel_requested {
            self.pricing_ui.open = false;
        } else {
            self.pricing_ui.open = open;
        }
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

fn format_optional_cost(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.6} USD"),
        None => "无".to_string(),
    }
}
