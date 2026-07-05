use super::CodexSwitchApp;
use crate::core::models::WireApi;
use eframe::egui;

impl CodexSwitchApp {
    pub(super) fn upstreams_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("添加 API Key 上游");
        ui.horizontal(|ui| {
            ui.label("名称");
            ui.text_edit_singleline(&mut self.relay_name);
        });
        ui.horizontal(|ui| {
            ui.label("Base URL");
            ui.text_edit_singleline(&mut self.relay_base_url);
        });
        ui.horizontal(|ui| {
            ui.label("API Key");
            ui.add(egui::TextEdit::singleline(&mut self.relay_api_key).password(true));
        });
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.relay_wire_api, WireApi::Responses, "Responses");
            ui.radio_value(
                &mut self.relay_wire_api,
                WireApi::ChatCompletions,
                "Chat Completions",
            );
            ui.checkbox(&mut self.relay_supports_compact, "支持 compact");
            if ui.button("添加").clicked() {
                self.add_relay();
            }
        });
        ui.separator();
        ui.heading("Codex OAuth");
        ui.horizontal(|ui| {
            if ui.button("开始登录").clicked() {
                self.start_oauth();
            }
            if ui.button("轮询授权").clicked() {
                self.poll_oauth();
            }
        });
        if let Some(device) = &self.oauth_device {
            ui.label(format!("访问: {}", device.verification_uri));
            ui.label(format!("用户码: {}", device.user_code));
            ui.label(format!(
                "轮询间隔: {} 秒, 有效期: {} 秒",
                device.interval, device.expires_in
            ));
        }
        ui.separator();
        ui.heading("上游列表");
        let mut changed = Vec::new();
        let mut deleted = Vec::new();
        let mut edit = None;
        for upstream in &self.upstreams {
            ui.horizontal(|ui| {
                let mut enabled = upstream.enabled;
                if ui.checkbox(&mut enabled, "").changed() {
                    changed.push((upstream.id.clone(), enabled));
                }
                ui.label(format!("{} [{}]", upstream.name, upstream.kind.as_str()));
                ui.label(&upstream.base_url);
                if ui.button("编辑").clicked() {
                    edit = Some(upstream.clone());
                }
                if ui.button("删除").clicked() {
                    deleted.push(upstream.id.clone());
                }
            });
        }
        if let Some(upstream) = edit {
            self.open_upstream_editor(upstream);
        }
        let should_refresh = !deleted.is_empty() || !changed.is_empty();
        for (id, enabled) in changed {
            if let Err(err) = self
                .runtime
                .block_on(self.state.store.set_upstream_enabled(&id, enabled))
            {
                self.status = format!("更新启用状态失败: {err}");
            }
        }
        for id in deleted {
            if let Err(err) = self.runtime.block_on(self.state.store.delete_upstream(&id)) {
                self.status = format!("删除失败: {err}");
            }
        }
        if should_refresh {
            self.refresh_all();
        }
        self.show_upstream_editor(ui.ctx());
    }
}
