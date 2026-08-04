use super::{CodexSwitchApp, DeleteAction, token_amount};
use crate::core::models::TemporaryAccessKey;
use chrono::{Local, TimeZone, Utc};
use eframe::egui;
use std::time::{Duration, Instant};

const COPY_FEEDBACK_DURATION: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct TempKeysUiState {
    new_name: String,
    new_key_value: String,
    limit_requests: bool,
    request_limit_input: String,
    limit_tokens: bool,
    token_limit_input: String,
    limit_time: bool,
    duration_input: String,
    copied_key_id: Option<String>,
    copied_at: Option<Instant>,
    editor: Option<TempKeyEditorState>,
}

struct TempKeyEditorState {
    id: String,
    key_value: String,
    name: String,
    limit_requests: bool,
    request_limit_input: String,
    limit_tokens: bool,
    token_limit_input: String,
    limit_time: bool,
    duration_input: String,
}

impl CodexSwitchApp {
    pub(super) fn temp_keys_ui(&mut self, ui: &mut egui::Ui) {
        self.clear_expired_copy_feedback();
        let mut create_requested = false;
        let mut toggle_id = None;
        let mut delete_id = None;
        let mut edit_id = None;
        egui::ScrollArea::vertical()
            .id_salt("temp_keys_page")
            .max_height(ui.available_height())
            .show(ui, |ui| {
                ui.heading("临时 Key");
                ui.heading("新建临时 Key");
                ui.horizontal(|ui| {
                    ui.label("备注");
                    ui.text_edit_singleline(&mut self.temp_keys_ui.new_name);
                });
                ui.horizontal(|ui| {
                    ui.label("Key");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.temp_keys_ui.new_key_value)
                            .desired_width(220.0)
                            .hint_text("留空自动生成"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.temp_keys_ui.limit_requests, "限制次数");
                    if self.temp_keys_ui.limit_requests {
                        ui.label("上限");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.temp_keys_ui.request_limit_input)
                                .desired_width(100.0),
                        )
                            .on_hover_text("成功请求上限");
                    }
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.temp_keys_ui.limit_tokens, "限制用量");
                    if self.temp_keys_ui.limit_tokens {
                        ui.label("上限");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.temp_keys_ui.token_limit_input)
                                .desired_width(100.0),
                        )
                            .on_hover_text("总 token 上限");
                    }
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.temp_keys_ui.limit_time, "限制时间");
                    if self.temp_keys_ui.limit_time {
                        ui.label("时长");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.temp_keys_ui.duration_input)
                                .desired_width(180.0)
                                .hint_text("如 1d2h30m10s"),
                        )
                            .on_hover_text("支持 d/h/m/s, 如 1d2h30m 或 1天2小时30分钟");
                    }
                });
                if ui.button("创建临时 Key").clicked() {
                    create_requested = true;
                }

                ui.separator();
                ui.heading(format!(
                    "已有临时 Key ({})",
                    self.temporary_access_keys.len()
                ));
                if self.temporary_access_keys.is_empty() {
                    ui.label("暂无临时 key");
                } else {
                    egui::Grid::new("temp_keys_grid")
                        .striped(true)
                        .num_columns(7)
                        .spacing([14.0, 8.0])
                        .show(ui, |ui| {
                            ui.strong("备注");
                            ui.strong("Key");
                            ui.strong("状态");
                            ui.strong("次数");
                            ui.strong("Token");
                            ui.strong("过期时间");
                            ui.strong("操作");
                            ui.end_row();

                            for key in &self.temporary_access_keys {
                                ui.label(if key.name.is_empty() {
                                    "-"
                                } else {
                                    &key.name
                                });
                                ui.horizontal(|ui| {
                                    if ui
                                        .button(&key.key_value)
                                        .on_hover_text("点击复制")
                                        .clicked()
                                    {
                                        ui.ctx().copy_text(key.key_value.clone());
                                        self.temp_keys_ui.copied_key_id =
                                            Some(key.id.clone());
                                        self.temp_keys_ui.copied_at = Some(Instant::now());
                                    }
                                    if self.temp_keys_ui.copied_key_id.as_deref()
                                        == Some(key.id.as_str())
                                    {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(34, 197, 94),
                                            "已复制",
                                        );
                                    }
                                });
                                ui.label(temp_key_status(key));
                                ui.label(request_limit_text(key));
                                ui.label(token_limit_text(key));
                                ui.label(format_expires_at(key.expires_at));
                                ui.horizontal(|ui| {
                                    if ui
                                        .button(if key.enabled { "停用" } else { "启用" })
                                        .clicked()
                                    {
                                        toggle_id = Some(key.id.clone());
                                    }
                                    if ui.button("编辑").clicked() {
                                        edit_id = Some(key.id.clone());
                                    }
                                    if ui.button("删除").clicked() {
                                        delete_id = Some(key.id.clone());
                                    }
                                });
                                ui.end_row();
                            }
                        });
                }
            });
        self.temp_keys_editor_window(ui.ctx());
        if create_requested {
            self.create_temp_key();
        }
        if let Some(id) = toggle_id {
            self.toggle_temp_key(&id);
        }
        if let Some(id) = edit_id {
            self.open_temp_key_editor(&id);
        }
        if let Some(id) = delete_id {
            self.request_delete(
                DeleteAction::TemporaryAccessKey(id.clone()),
                "删除临时 Key",
                "确认删除这个临时 key? 删除后该 key 立即失效",
            );
        }
    }

    fn create_temp_key(&mut self) {
        let name = self.temp_keys_ui.new_name.trim().to_string();
        let request_limit = match self.parse_optional_limit(
            "成功请求次数",
            self.temp_keys_ui.limit_requests,
            &self.temp_keys_ui.request_limit_input,
        ) {
            Ok(value) => value,
            Err(message) => {
                self.status = message;
                return;
            }
        };
        let token_limit = if self.temp_keys_ui.limit_tokens {
            match token_amount::parse_token_amount(&self.temp_keys_ui.token_limit_input) {
                Ok(value) if value > 0 => Some(value),
                Ok(_) => {
                    self.status = "总 token 上限必须大于 0".to_string();
                    return;
                }
                Err(err) => {
                    self.status = format!("总 token 上限 {err}");
                    return;
                }
            }
        } else {
            None
        };
        let expires_at = match self.parse_expires_at() {
            Ok(value) => value,
            Err(message) => {
                self.status = message;
                return;
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        let key_value = self.temp_keys_ui.new_key_value.trim().to_string();
        let key_value = if key_value.is_empty() {
            format!("cs-tmp-{}", uuid::Uuid::new_v4())
        } else {
            key_value
        };
        let key = TemporaryAccessKey::new(
            id,
            name,
            key_value,
            request_limit,
            token_limit,
            expires_at,
        );
        match self
            .runtime
            .block_on(self.state.store.create_temporary_access_key(&key))
        {
            Ok(()) => {
                self.status = "临时 Key 已创建".to_string();
                self.temp_keys_ui.new_name.clear();
                self.temp_keys_ui.new_key_value.clear();
                self.temp_keys_ui.request_limit_input.clear();
                self.temp_keys_ui.token_limit_input.clear();
                self.temp_keys_ui.duration_input.clear();
                self.temp_keys_ui.limit_requests = false;
                self.temp_keys_ui.limit_tokens = false;
                self.temp_keys_ui.limit_time = false;
                self.refresh_temporary_access_keys();
            }
            Err(err) => {
                self.status = format!("创建临时 Key 失败: {err}");
            }
        }
    }

    fn parse_optional_limit(
        &self,
        label: &str,
        enabled: bool,
        input: &str,
    ) -> Result<Option<i64>, String> {
        if !enabled {
            return Ok(None);
        }
        let value = input.trim().parse::<i64>().map_err(|_| {
            format!("{label} 需要填写正整数")
        })?;
        if value <= 0 {
            return Err(format!("{label} 必须大于 0"));
        }
        Ok(Some(value))
    }

    fn parse_expires_at(&self) -> Result<Option<i64>, String> {
        if !self.temp_keys_ui.limit_time {
            return Ok(None);
        }
        let seconds = parse_duration_seconds(&self.temp_keys_ui.duration_input)?;
        Ok(Some(Utc::now().timestamp() + seconds))
    }

    pub(super) fn refresh_temporary_access_keys(&mut self) {
        self.temporary_access_keys = self
            .runtime
            .block_on(self.state.store.list_temporary_access_keys())
            .unwrap_or_default();
    }

    fn toggle_temp_key(&mut self, id: &str) {
        let enabled = self
            .temporary_access_keys
            .iter()
            .find(|key| key.id == id)
            .map(|key| !key.enabled)
            .unwrap_or(true);
        match self
            .runtime
            .block_on(self.state.store.set_temporary_access_key_enabled(id, enabled))
        {
            Ok(()) => {
                self.status = if enabled {
                    "临时 Key 已启用".to_string()
                } else {
                    "临时 Key 已停用".to_string()
                };
                self.refresh_temporary_access_keys();
            }
            Err(err) => {
                self.status = format!("更新临时 Key 失败: {err}");
            }
        }
    }

    pub(super) fn delete_temporary_access_key(&mut self, id: &str) {
        match self
            .runtime
            .block_on(self.state.store.delete_temporary_access_key(id))
        {
            Ok(()) => {
                self.status = "临时 Key 已删除".to_string();
                self.refresh_temporary_access_keys();
            }
            Err(err) => {
                self.status = format!("删除临时 Key 失败: {err}");
            }
        }
    }

    fn clear_expired_copy_feedback(&mut self) {
        let Some(copied_at) = self.temp_keys_ui.copied_at else {
            return;
        };
        if copied_at.elapsed() >= COPY_FEEDBACK_DURATION {
            self.temp_keys_ui.copied_key_id = None;
            self.temp_keys_ui.copied_at = None;
        }
    }

    fn open_temp_key_editor(&mut self, id: &str) {
        let Some(key) = self
            .temporary_access_keys
            .iter()
            .find(|key| key.id == id)
            .cloned()
        else {
            return;
        };
        let remaining = key.expires_at.map(|expires_at| (expires_at - Utc::now().timestamp()).max(0));
        self.temp_keys_ui.editor = Some(TempKeyEditorState {
            id: key.id,
            key_value: key.key_value,
            name: key.name,
            limit_requests: key.request_limit.is_some(),
            request_limit_input: key
                .request_limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
            limit_tokens: key.token_limit.is_some(),
            token_limit_input: key
                .token_limit
                .map(token_amount::format_token_input)
                .unwrap_or_default(),
            limit_time: key.expires_at.is_some(),
            duration_input: remaining
                .map(format_duration_seconds)
                .unwrap_or_default(),
        });
    }

    fn temp_keys_editor_window(&mut self, ctx: &egui::Context) {
        let Some(editor) = self.temp_keys_ui.editor.as_mut() else {
            return;
        };
        let mut save_requested = false;
        let mut cancel_requested = false;
        egui::Window::new("编辑临时 Key")
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Key");
                    ui.add(
                        egui::TextEdit::singleline(&mut editor.key_value)
                            .desired_width(220.0)
                            .hint_text("留空自动生成"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("备注");
                    ui.text_edit_singleline(&mut editor.name);
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut editor.limit_requests, "限制次数");
                    if editor.limit_requests {
                        ui.label("上限");
                        ui.add(
                            egui::TextEdit::singleline(&mut editor.request_limit_input)
                                .desired_width(100.0)
                                .hint_text("如 10"),
                        );
                    }
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut editor.limit_tokens, "限制用量");
                    if editor.limit_tokens {
                        ui.label("上限");
                        ui.add(
                            egui::TextEdit::singleline(&mut editor.token_limit_input)
                                .desired_width(100.0)
                                .hint_text("如 1M"),
                        );
                    }
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut editor.limit_time, "限制时间");
                    if editor.limit_time {
                        ui.label("时长");
                        ui.add(
                            egui::TextEdit::singleline(&mut editor.duration_input)
                                .desired_width(180.0)
                                .hint_text("如 1d2h30m10s"),
                        );
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        save_requested = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel_requested = true;
                    }
                });
            });
        if save_requested {
            self.save_temp_key_editor();
        } else if cancel_requested {
            self.temp_keys_ui.editor = None;
        }
    }

    fn save_temp_key_editor(&mut self) {
        let Some(editor) = self.temp_keys_ui.editor.as_ref() else {
            return;
        };
        let id = editor.id.clone();
        let name = editor.name.trim().to_string();
        let key_value = editor.key_value.trim().to_string();
        let key_value = if key_value.is_empty() {
            format!("cs-tmp-{}", uuid::Uuid::new_v4())
        } else {
            key_value
        };
        let request_limit = match self.parse_optional_limit(
            "成功请求次数",
            editor.limit_requests,
            &editor.request_limit_input,
        ) {
            Ok(value) => value,
            Err(message) => {
                self.status = message;
                return;
            }
        };
        let token_limit = if editor.limit_tokens {
            match token_amount::parse_token_amount(&editor.token_limit_input) {
                Ok(value) if value > 0 => Some(value),
                Ok(_) => {
                    self.status = "总 token 上限必须大于 0".to_string();
                    return;
                }
                Err(err) => {
                    self.status = format!("总 token 上限 {err}");
                    return;
                }
            }
        } else {
            None
        };
        let expires_at = if editor.limit_time {
            match parse_duration_seconds(&editor.duration_input) {
                Ok(seconds) => Some(Utc::now().timestamp() + seconds),
                Err(message) => {
                    self.status = message;
                    return;
                }
            }
        } else {
            None
        };
        match self
            .runtime
            .block_on(self.state.store.update_temporary_access_key(
                &id,
                &name,
                &key_value,
                request_limit,
                token_limit,
                expires_at,
            ))
        {
            Ok(()) => {
                self.status = "临时 Key 已更新".to_string();
                self.temp_keys_ui.editor = None;
                self.refresh_temporary_access_keys();
            }
            Err(err) => {
                self.status = format!("更新临时 Key 失败: {err}");
            }
        }
    }
}

fn temp_key_status(key: &TemporaryAccessKey) -> String {
    let now = Utc::now().timestamp();
    if !key.enabled {
        "已停用".to_string()
    } else if key.expires_at.is_some_and(|expires_at| expires_at <= now) {
        "已过期".to_string()
    } else if key
        .request_limit
        .is_some_and(|limit| key.requests_used >= limit)
    {
        "次数已用尽".to_string()
    } else if key.token_limit.is_some_and(|limit| key.tokens_used >= limit) {
        "用量已用尽".to_string()
    } else {
        "正常".to_string()
    }
}

fn request_limit_text(key: &TemporaryAccessKey) -> String {
    match key.request_limit {
        Some(limit) => format!("{}/{}", key.requests_used, limit),
        None => key.requests_used.to_string(),
    }
}

fn token_limit_text(key: &TemporaryAccessKey) -> String {
    let used = token_amount::format_token_input(key.tokens_used);
    match key.token_limit {
        Some(limit) => format!("{used}/{}", token_amount::format_token_input(limit)),
        None => used,
    }
}

fn format_expires_at(expires_at: Option<i64>) -> String {
    match expires_at {
        Some(value) => Local
            .timestamp_opt(value, 0)
            .single()
            .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "-".to_string()),
        None => "-".to_string(),
    }
}

fn parse_duration_seconds(input: &str) -> Result<i64, String> {
    let mut chars = input.chars().peekable();
    let mut total = 0i64;
    let mut has_component = false;
    while let Some(ch) = chars.peek().copied() {
        if ch.is_whitespace() || ch == '_' {
            chars.next();
            continue;
        }
        let mut number = String::new();
        while let Some(ch) = chars.peek().copied() {
            if ch.is_ascii_digit() {
                number.push(ch);
                chars.next();
            } else {
                break;
            }
        }
        if number.is_empty() {
            return Err("限制时间需要填写数字和单位, 例如 1d2h30m10s".to_string());
        }
        while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
            chars.next();
        }
        let value = number
            .parse::<i64>()
            .map_err(|_| "限制时间需要填写有效数字".to_string())?;
        let multiplier = match chars.peek().copied() {
            Some('d' | 'D') => {
                chars.next();
                86_400
            }
            Some('h' | 'H') => {
                chars.next();
                3600
            }
            Some('m' | 'M') => {
                chars.next();
                60
            }
            Some('s' | 'S') => {
                chars.next();
                1
            }
            Some('天') => {
                chars.next();
                86_400
            }
            Some('小') => {
                let mut next = chars.clone();
                next.next();
                if next.next() == Some('时') {
                    chars.next();
                    chars.next();
                    3600
                } else {
                    return Err("无法识别的时间单位".to_string());
                }
            }
            Some('分') => {
                let mut next = chars.clone();
                next.next();
                if next.next() == Some('钟') {
                    chars.next();
                    chars.next();
                    60
                } else {
                    return Err("无法识别的时间单位".to_string());
                }
            }
            Some('秒') => {
                chars.next();
                1
            }
            _ => return Err("无法识别的时间单位".to_string()),
        };
        total = total
            .checked_add(
                value
                    .checked_mul(multiplier)
                    .ok_or_else(|| "限制时间数值过大".to_string())?,
            )
            .ok_or_else(|| "限制时间数值过大".to_string())?;
        has_component = true;
    }
    if !has_component || total <= 0 {
        return Err("限制时间需要填写大于 0 的时长".to_string());
    }
    Ok(total)
}

fn format_duration_seconds(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds == 0 {
        return "0s".to_string();
    }
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if secs > 0 {
        parts.push(format!("{secs}s"));
    }
    parts.join("")
}

#[cfg(test)]
mod tests {
    use super::{format_duration_seconds, parse_duration_seconds};

    #[test]
    fn parses_combined_duration_suffixes() {
        assert_eq!(parse_duration_seconds("1d2h30m10s").unwrap(), 95_410);
        assert_eq!(parse_duration_seconds("2D 1H 5M 3S").unwrap(), 176_703);
        assert_eq!(parse_duration_seconds("1天2小时30分钟10秒").unwrap(), 95_410);
    }

    #[test]
    fn rejects_invalid_duration_input() {
        assert!(parse_duration_seconds("").is_err());
        assert!(parse_duration_seconds("10").is_err());
        assert!(parse_duration_seconds("1x2h").is_err());
        assert!(parse_duration_seconds("0s").is_err());
    }

    #[test]
    fn formats_combined_duration_suffixes() {
        assert_eq!(format_duration_seconds(93_010), "1d1h50m10s");
        assert_eq!(format_duration_seconds(0), "0s");
    }
}
