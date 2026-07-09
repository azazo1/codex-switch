use super::CodexSwitchApp;
use crate::cache_keepalive::CacheKeepaliveSessionSnapshot;
use eframe::egui;

impl CodexSwitchApp {
    pub(super) fn cache_keepalive_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("缓存保持会话");
        if self.cache_keepalive_sessions.is_empty() {
            ui.label("当前没有缓存保持会话");
            return;
        }
        self.sync_selected_cache_keepalive_session();
        egui::ScrollArea::horizontal()
            .id_salt("cache_keepalive_session_tabs")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for session in &self.cache_keepalive_sessions {
                        let selected = self
                            .selected_cache_keepalive_key
                            .as_deref()
                            .is_some_and(|key| key == session.key);
                        if ui
                            .selectable_label(selected, session_tab_title(session))
                            .on_hover_text(&session.key)
                            .clicked()
                        {
                            self.selected_cache_keepalive_key = Some(session.key.clone());
                        }
                    }
                });
            });
        let selected_session = self.selected_cache_keepalive_session().cloned();
        let mut remove_key = None;
        ui.horizontal(|ui| {
            if let Some(session) = &selected_session {
                if ui.button("删除会话").clicked() {
                    remove_key = Some(session.key.clone());
                }
            } else {
                ui.add_enabled(false, egui::Button::new("删除会话"));
            }
        });
        if let Some(key) = remove_key {
            self.runtime
                .block_on(self.state.cache_keepalive.remove_session(&key));
            self.selected_cache_keepalive_key = None;
            self.refresh_cache_keepalive_sessions();
            ui.separator();
            if self.cache_keepalive_sessions.is_empty() {
                ui.label("当前没有缓存保持会话");
            } else {
                ui.label("已删除缓存保持会话");
            }
            return;
        }
        ui.separator();
        let Some(session) = selected_session.as_ref() else {
            ui.label("请选择缓存保持会话");
            return;
        };
        cache_keepalive_detail_ui(ui, session);
    }

    fn sync_selected_cache_keepalive_session(&mut self) {
        if self
            .selected_cache_keepalive_key
            .as_ref()
            .is_some_and(|key| {
                self.cache_keepalive_sessions
                    .iter()
                    .any(|session| &session.key == key)
            })
        {
            return;
        }
        self.selected_cache_keepalive_key = self
            .cache_keepalive_sessions
            .first()
            .map(|session| session.key.clone());
    }

    fn selected_cache_keepalive_session(&self) -> Option<&CacheKeepaliveSessionSnapshot> {
        self.selected_cache_keepalive_key
            .as_deref()
            .and_then(|key| {
                self.cache_keepalive_sessions
                    .iter()
                    .find(|session| session.key == key)
            })
            .or_else(|| self.cache_keepalive_sessions.first())
    }
}

fn cache_keepalive_detail_ui(ui: &mut egui::Ui, session: &CacheKeepaliveSessionSnapshot) {
    egui::Grid::new("cache_keepalive_detail_grid")
        .num_columns(2)
        .spacing([18.0, 8.0])
        .show(ui, |ui| {
            detail_row(ui, "状态", status_text(session));
            detail_row(
                ui,
                "停用原因",
                session.disabled_reason.as_deref().unwrap_or("-"),
            );
            detail_row(ui, "上游", &session.upstream_name);
            detail_row(ui, "上游 ID", &session.upstream_id);
            detail_row(ui, "模型", &session.model);
            detail_row(ui, "endpoint", &session.endpoint);
            detail_row(ui, "wire api", session.wire_api.as_str());
            detail_row(ui, "缓存 tokens", &session.cached_tokens.to_string());
            detail_row(ui, "保活次数", &session.keepalive_count.to_string());
            detail_row(
                ui,
                "最近真实请求",
                &format_seconds(session.last_user_request_elapsed_seconds),
            );
            detail_row(
                ui,
                "最近活动",
                &format_seconds(session.last_activity_elapsed_seconds),
            );
            detail_row(ui, "下次保活", &format_next_keepalive(session));
            detail_row(ui, "请求体大小", &format_body_bytes(session.body_bytes));
            ui.strong("session key");
            ui.monospace(short_key(&session.key))
                .on_hover_text(&session.key);
            ui.end_row();
        });
}

fn detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.strong(label);
    ui.label(value);
    ui.end_row();
}

fn session_tab_title(session: &CacheKeepaliveSessionSnapshot) -> String {
    format!(
        "{} / {} / {}",
        compact_text(&session.upstream_name, 18),
        compact_text(&session.model, 20),
        status_text(session)
    )
}

fn status_text(session: &CacheKeepaliveSessionSnapshot) -> &'static str {
    if session.disabled_reason.is_some() {
        "已停用"
    } else if session.next_keepalive_seconds == 0 {
        "活跃"
    } else {
        "等待保活"
    }
}

fn format_next_keepalive(session: &CacheKeepaliveSessionSnapshot) -> String {
    if session.disabled_reason.is_some() {
        "已停用".to_string()
    } else if session.next_keepalive_seconds <= 0 {
        "即将执行".to_string()
    } else {
        format_seconds(session.next_keepalive_seconds)
    }
}

fn format_seconds(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3600 {
        return format!("{}m {}s", seconds / 60, seconds % 60);
    }
    format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
}

fn format_body_bytes(bytes: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * 1024;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    }
}

fn short_key(key: &str) -> String {
    if key.len() <= 16 {
        return key.to_string();
    }
    format!("{}...{}", &key[..8], &key[key.len() - 8..])
}

fn compact_text(value: &str, max_chars: usize) -> String {
    let total = value.chars().count();
    if total <= max_chars {
        return value.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let mut result = value.chars().take(keep).collect::<String>();
    result.push('.');
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::WireApi;

    #[test]
    fn formats_seconds_for_common_ranges() {
        assert_eq!(format_seconds(12), "12s");
        assert_eq!(format_seconds(125), "2m 5s");
        assert_eq!(format_seconds(7320), "2h 2m");
    }

    #[test]
    fn session_tab_title_compacts_long_fields() {
        let session = snapshot("very-long-upstream-name", "very-long-model-name-plus");

        let title = session_tab_title(&session);

        assert!(title.contains("very-long-upstrea."));
        assert!(title.contains("very-long-model-nam."));
        assert!(title.contains("等待保活"));
    }

    #[test]
    fn disabled_session_title_shows_stopped_state() {
        let mut session = snapshot("upstream", "model");
        session.disabled_reason = Some("cache miss".to_string());

        assert!(session_tab_title(&session).contains("已停用"));
        assert_eq!(format_next_keepalive(&session), "已停用");
    }

    #[test]
    fn due_session_title_shows_active_state() {
        let mut session = snapshot("upstream", "model");
        session.next_keepalive_seconds = 0;

        assert!(session_tab_title(&session).contains("活跃"));
    }

    fn snapshot(upstream_name: &str, model: &str) -> CacheKeepaliveSessionSnapshot {
        CacheKeepaliveSessionSnapshot {
            key: "0123456789abcdef0123456789abcdef".to_string(),
            upstream_id: "upstream-id".to_string(),
            upstream_name: upstream_name.to_string(),
            endpoint: "/responses".to_string(),
            model: model.to_string(),
            wire_api: WireApi::Responses,
            cached_tokens: 2048,
            keepalive_count: 2,
            last_user_request_elapsed_seconds: 30,
            last_activity_elapsed_seconds: 20,
            next_keepalive_seconds: 60,
            disabled_reason: None,
            body_bytes: 4096,
        }
    }
}
