use super::{
    CodexSwitchApp, tokens,
    upstreams::{balance_snapshot_for, balance_snapshot_label, cache_keepalive_label},
};
use crate::app::platform;
use crate::core::models::UpstreamKind;
use eframe::egui::{self, Color32};
use std::time::{Duration, Instant};

const COPY_FEEDBACK_DURATION: Duration = Duration::from_secs(2);

impl CodexSwitchApp {
    pub(super) fn dashboard_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("dashboard")
            .max_height(ui.available_height())
            .show(ui, |ui| self.dashboard_content_ui(ui));
    }

    fn dashboard_content_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Codex Switch");
        ui.horizontal(|ui| {
            ui.label("监听地址");
            ui.text_edit_singleline(&mut self.bind_addr);
            if self.server.is_none() {
                if ui.button("启动").clicked() {
                    self.start_server();
                }
            } else if ui.button("停止").clicked() {
                self.stop_server();
            }
        });
        ui.label(format!("Base URL: http://{}/v1", self.bind_addr));
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("节点指纹: {}", self.node_fingerprint));
            if self.peer_listen_enabled {
                ui.label(format!("节点口: {}", self.peer_bind_addr));
            } else {
                ui.label("节点口: 关闭");
            }
        });
        ui.horizontal(|ui| {
            ui.label("本地访问 key");
            if ui
                .button(&self.local_key)
                .on_hover_text("点击复制")
                .clicked()
            {
                ui.ctx().copy_text(self.local_key.clone());
                self.local_key_copied_at = Some(Instant::now());
            }
            if ui
                .button("刷新 key")
                .on_hover_text("打开 key 刷新确认")
                .clicked()
            {
                self.open_local_key_refresh_window();
            }
            if let Some(copied_at) = self.local_key_copied_at {
                let elapsed = copied_at.elapsed();
                if elapsed < COPY_FEEDBACK_DURATION {
                    draw_copy_success_icon(ui);
                    ui.colored_label(copy_success_color(), "已复制");
                    ui.ctx()
                        .request_repaint_after(COPY_FEEDBACK_DURATION - elapsed);
                } else {
                    self.local_key_copied_at = None;
                }
            }
        });
        self.local_key_refresh_window(ui.ctx());
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("总请求: {}", self.stats.total_requests));
            tokens::usage_tokens(ui, &mut self.token_display_mode, &self.stats.total_usage);
            tokens::estimated_cost(ui, "总估算", self.total_estimated_cost_usd);
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("今日请求: {}", self.stats.today_requests));
            tokens::usage_tokens(ui, &mut self.token_display_mode, &self.stats.today_usage);
            tokens::estimated_cost(ui, "今日估算", self.today_estimated_cost_usd);
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("模型信息缓存: {} 条", self.price_cache_count));
            ui.label(price_cache_age_text(self.price_cache_age_seconds));
            let label = if self.price_fetch_pending {
                "获取中"
            } else {
                "获取模型信息"
            };
            if ui
                .add_enabled(!self.price_fetch_pending, egui::Button::new(label))
                .clicked()
            {
                self.fetch_price_cache();
            }
        });
        ui.separator();
        ui.heading("按上游统计");
        let mut token_display_mode = self.token_display_mode;
        let mut query_balance = None;
        egui::Grid::new("provider_stats_grid")
            .striped(true)
            .num_columns(10)
            .spacing([18.0, 8.0])
            .show(ui, |ui| {
                ui.strong("上游");
                ui.strong("请求");
                ui.strong("输入");
                ui.strong("缓存输入");
                ui.strong("输出");
                ui.strong("总计");
                ui.strong("估算");
                ui.strong("缓存保持");
                ui.strong("余额");
                ui.strong("操作");
                ui.end_row();

                for item in &self.provider_stats {
                    ui.label(&item.upstream_name)
                        .on_hover_text(format!("id: {}", item.upstream_id));
                    ui.label(item.requests.to_string());
                    tokens::token_number(ui, &mut token_display_mode, item.usage.input_tokens);
                    tokens::token_number(ui, &mut token_display_mode, item.usage.cache_read_tokens);
                    tokens::token_number(ui, &mut token_display_mode, item.usage.output_tokens);
                    tokens::token_number(ui, &mut token_display_mode, item.usage.total_tokens);
                    let cost = self
                        .provider_estimated_cost_usd
                        .get(&item.upstream_id)
                        .copied()
                        .flatten()
                        .map(tokens::format_usd)
                        .unwrap_or_else(|| "无价格缓存".to_string());
                    ui.label(cost);
                    cache_keepalive_label(ui, self.cache_keepalive_settings.get(&item.upstream_id));
                    balance_snapshot_label(
                        ui,
                        balance_snapshot_for(&self.balance_snapshots, &item.upstream_id),
                    );
                    let upstream = self
                        .upstreams
                        .iter()
                        .find(|upstream| upstream.id == item.upstream_id);
                    if upstream
                        .map(|upstream| upstream.kind == UpstreamKind::RelayApiKey)
                        .unwrap_or(false)
                    {
                        let pending = self.balance_query_pending_ids.contains(&item.upstream_id);
                        let label = if pending { "查询中" } else { "刷新余额" };
                        if ui.add_enabled(!pending, egui::Button::new(label)).clicked() {
                            query_balance = Some(item.upstream_id.clone());
                        }
                    } else {
                        ui.label("-");
                    }
                    ui.end_row();
                }
            });
        if let Some(id) = query_balance {
            self.query_selected_balance(&id);
        }
        self.token_display_mode = token_display_mode;
        ui.separator();
        ui.heading("SQLite 数据库");
        ui.horizontal_wrapped(|ui| {
            ui.label("路径");
            ui.monospace(&self.database_info.path);
            if ui
                .add_enabled(
                    !self.database_info.path.is_empty(),
                    egui::Button::new("打开位置"),
                )
                .clicked()
            {
                match platform::open_file_location(&self.database_info.path) {
                    Ok(()) => {
                        self.status = "已打开数据库位置".to_string();
                    }
                    Err(err) => {
                        self.status = format!("打开数据库位置失败: {err}");
                    }
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "文件大小: {}",
                format_bytes(database_total_bytes(&self.database_info))
            ));
            ui.label(format!(
                "主文件: {}",
                format_bytes(self.database_info.main_file_bytes)
            ));
            ui.label(format!(
                "WAL: {}",
                format_bytes(self.database_info.wal_file_bytes)
            ));
            ui.label(format!(
                "SHM: {}",
                format_bytes(self.database_info.shm_file_bytes)
            ));
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("页面: {}", self.database_info.page_count));
            ui.label(format!(
                "页面大小: {}",
                format_bytes(self.database_info.page_size as u64)
            ));
            ui.label(format!("空闲页: {}", self.database_info.freelist_count));
            ui.label(format!(
                "日志条数: {}",
                self.database_info.request_log_count
            ));
        });
        ui.separator();
        ui.heading("调试日志");
        ui.horizontal_wrapped(|ui| {
            if ui
                .checkbox(&mut self.debug_log_enabled, "启用完整调试日志")
                .on_hover_text("开启后持续写入 tracing 和完整代理 body 到日志文件")
                .changed()
            {
                self.apply_debug_log_enabled();
            }
            if crate::logging::env_override_active() {
                ui.colored_label(Color32::YELLOW, "当前由环境变量控制");
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("单文件大小");
            ui.add(
                egui::DragValue::new(&mut self.log_rotation_size_mb)
                    .range(1..=10240)
                    .suffix(" MB"),
            );
            ui.label("轮转文件数");
            ui.add(
                egui::DragValue::new(&mut self.log_max_files)
                    .range(1..=1000),
            );
            if ui.button("应用轮转设置").clicked() {
                self.apply_log_rotation_settings();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("日志路径");
            ui.monospace(&self.debug_log_path);
            if ui
                .add_enabled(
                    !self.debug_log_path.is_empty(),
                    egui::Button::new("打开日志位置"),
                )
                .clicked()
            {
                match platform::open_file_location(&self.debug_log_path) {
                    Ok(()) => {
                        self.status = "已打开日志位置".to_string();
                    }
                    Err(err) => {
                        self.status = format!("打开日志位置失败: {err}");
                    }
                }
            }
        });
        ui.label("启用后会记录完整入站 body, 转换后的上游 body, 上游响应和流式块, 可能包含 prompt 和模型输出, 不要公开日志文件.");
    }

    fn apply_debug_log_enabled(&mut self) {
        let enabled = self.debug_log_enabled;
        let value = if enabled { "true" } else { "false" };
        if let Err(err) = self
            .runtime
            .block_on(self.state.store.set_setting("debug_log_enabled", value))
        {
            self.status = format!("保存调试日志设置失败: {err}");
            return;
        }
        match crate::logging::set_debug_log_enabled(enabled) {
            Ok(()) => {
                self.status = if enabled {
                    "调试日志已开启, 将持续写入日志文件".to_string()
                } else {
                    "调试日志已关闭".to_string()
                };
            }
            Err(err) => {
                self.status = format!("切换调试日志失败: {err}");
            }
        }
    }

    fn apply_log_rotation_settings(&mut self) {
        self.log_rotation_size_mb = self.log_rotation_size_mb.max(1);
        self.log_max_files = self.log_max_files.max(1);
        let size_mb = self.log_rotation_size_mb.to_string();
        let max_files = self.log_max_files.to_string();
        if let Err(err) = self.runtime.block_on(
            self.state
                .store
                .set_setting("log_rotation_size_mb", &size_mb),
        ) {
            self.status = format!("保存日志大小设置失败: {err}");
            return;
        }
        if let Err(err) = self
            .runtime
            .block_on(self.state.store.set_setting("log_max_files", &max_files))
        {
            self.status = format!("保存日志文件数设置失败: {err}");
            return;
        }
        match crate::logging::set_rotation_config(self.log_rotation_size_mb, self.log_max_files) {
            Ok(()) => {
                self.status = "日志轮转设置已应用".to_string();
            }
            Err(err) => {
                self.status = format!("应用日志轮转设置失败: {err}");
            }
        }
    }

    fn open_local_key_refresh_window(&mut self) {
        self.local_key_refresh_value = generate_local_key();
        self.local_key_refresh_open = true;
    }

    fn local_key_refresh_window(&mut self, ctx: &egui::Context) {
        if !self.local_key_refresh_open {
            return;
        }
        let mut open = self.local_key_refresh_open;
        let mut confirm_requested = false;
        let mut cancel_requested = false;
        egui::Window::new("刷新本地访问 key")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("刷新后 Codex 需要使用新 key");
                ui.horizontal(|ui| {
                    ui.label("当前 key");
                    ui.monospace(&self.local_key);
                });
                ui.horizontal(|ui| {
                    ui.label("新 key");
                    ui.text_edit_singleline(&mut self.local_key_refresh_value);
                    if ui.button("生成随机 key").clicked() {
                        self.local_key_refresh_value = generate_local_key();
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("确认刷新").clicked() {
                        confirm_requested = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel_requested = true;
                    }
                });
            });
        if confirm_requested {
            let key = self.local_key_refresh_value.trim().to_string();
            if key.is_empty() {
                self.status = "本地访问 key 不能为空".to_string();
                self.local_key_refresh_open = true;
            } else {
                self.local_key_refresh_open = false;
                self.refresh_local_key(key);
            }
        } else if cancel_requested {
            self.local_key_refresh_open = false;
        } else {
            self.local_key_refresh_open = open;
        }
    }
}

fn generate_local_key() -> String {
    format!("cs-{}", uuid::Uuid::new_v4())
}

fn database_total_bytes(info: &crate::core::models::DatabaseInfo) -> u64 {
    info.main_file_bytes + info.wal_file_bytes + info.shm_file_bytes
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = UNITS[0];
    for next_unit in UNITS.iter().skip(1) {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next_unit;
    }
    if unit == "B" {
        format!("{bytes} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

fn price_cache_age_text(age_seconds: Option<i64>) -> String {
    match age_seconds {
        Some(age) if age < 60 => "刚刚更新".to_string(),
        Some(age) if age < 3600 => format!("{} 分钟前更新", age / 60),
        Some(age) if age < 86_400 => format!("{} 小时前更新", age / 3600),
        Some(age) => format!("{} 天前更新", age / 86_400),
        None => "尚未缓存模型信息".to_string(),
    }
}

fn draw_copy_success_icon(ui: &mut egui::Ui) {
    let size = egui::vec2(14.0, 14.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let stroke = egui::Stroke::new(2.0, copy_success_color());
    let p1 = egui::pos2(rect.left() + 2.0, rect.center().y + 0.5);
    let p2 = egui::pos2(rect.left() + 5.5, rect.bottom() - 3.0);
    let p3 = egui::pos2(rect.right() - 2.0, rect.top() + 3.0);
    ui.painter().line_segment([p1, p2], stroke);
    ui.painter().line_segment([p2, p3], stroke);
}

fn copy_success_color() -> Color32 {
    Color32::from_rgb(34, 197, 94)
}
