use super::CodexSwitchApp;
use crate::core::models::PeerDiscoverySource;
use eframe::egui;

impl CodexSwitchApp {
    pub(super) fn peers_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("peers")
            .max_height(ui.available_height())
            .show(ui, |ui| self.peers_content_ui(ui));
        self.lnd_settings_window(ui.ctx());
    }

    fn peers_content_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("本机节点");
        ui.horizontal(|ui| {
            ui.label("节点 ID");
            if ui.button(&self.node_id).on_hover_text("点击复制").clicked() {
                ui.ctx().copy_text(self.node_id.clone());
                self.status = "节点 ID 已复制".to_string();
            }
        });
        ui.horizontal(|ui| {
            ui.label("指纹");
            if ui
                .button(&self.node_fingerprint)
                .on_hover_text("点击复制")
                .clicked()
            {
                ui.ctx().copy_text(self.node_fingerprint.clone());
                self.status = "节点指纹已复制".to_string();
            }
        });
        ui.horizontal(|ui| {
            ui.label("显示名");
            if ui.text_edit_singleline(&mut self.node_display_name).lost_focus() {
                self.save_node_display_name();
            }
        });
        ui.horizontal(|ui| {
            ui.label("监听节点请求");
            ui.text_edit_singleline(&mut self.peer_bind_addr);
            if self.peer_server.is_none() {
                if ui.button("启动").clicked() {
                    self.start_peer_server();
                }
            } else if ui.button("停止").clicked() {
                self.stop_peer_server();
            }
        });
        ui.label("节点口只接受已配对证书的 mTLS, 不要把它和本地明文代理口混用.");
        ui.separator();
        ui.heading("发现");
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut self.mdns_discovery_enabled, "启用 mDNS")
                .changed()
            {
                self.save_peer_setting(
                    "mdns_discovery_enabled",
                    bool_setting(self.mdns_discovery_enabled),
                );
            }
            if ui
                .checkbox(&mut self.lnd_discovery_enabled, "启用 LND")
                .changed()
            {
                self.save_peer_setting(
                    "lnd_discovery_enabled",
                    bool_setting(self.lnd_discovery_enabled),
                );
            }
            if ui.button("LND 设置").clicked() {
                self.open_lnd_settings_window();
            }
        });
        ui.label("发现只负责找地址, 不会自动配对.");
        ui.separator();
        ui.heading("直连配对");
        ui.horizontal(|ui| {
            ui.label("地址");
            ui.add(
                egui::TextEdit::singleline(&mut self.peer_direct_address)
                    .hint_text("192.168.1.8:15722"),
            );
            ui.label("指纹");
            ui.add(
                egui::TextEdit::singleline(&mut self.peer_direct_fingerprint)
                    .hint_text("可选 CS-XXXX-XXXX"),
            );
            if ui
                .add_enabled(!self.pairing_pending, egui::Button::new("配对"))
                .clicked()
            {
                self.pair_direct_peer();
            }
        });
        ui.separator();
        ui.heading("待确认配对");
        if self.pairing_requests.is_empty() {
            ui.label("没有待确认请求");
        } else {
            let requests = self.pairing_requests.clone();
            for request in requests {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{}  {}  {}",
                        request.display_name, request.fingerprint, request.node_id
                    ));
                    if ui.button("接受").clicked() {
                        self.accept_pairing_request(&request.id);
                    }
                    if ui.button("拒绝").clicked() {
                        self.reject_pairing_request(&request.id);
                    }
                });
            }
        }
        ui.separator();
        ui.heading("发现到的节点");
        if self.discovered_peers.is_empty() {
            ui.label("当前没有发现结果");
        } else {
            let discovered = self.discovered_peers.clone();
            for item in discovered {
                let (is_paired, can_update) = match self
                    .node_peers
                    .iter()
                    .find(|peer| peer.node_id == item.node_id)
                {
                    Some(peer) => (
                        true,
                        peer.fingerprint == item.fingerprint
                            && peer_addresses_differ(peer, &item),
                    ),
                    None => (false, false),
                };
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{}  {}  {}  {}",
                        item.display_name,
                        item.fingerprint,
                        source_label(item.source),
                        item.addresses.first().cloned().unwrap_or_default()
                    ));
                    if is_paired {
                        ui.label("已配对");
                        if can_update && ui.button("更新").clicked() {
                            self.update_discovered_peer_address(item);
                        }
                    } else if ui
                        .add_enabled(!self.pairing_pending, egui::Button::new("配对"))
                        .clicked()
                    {
                        self.pair_discovered_peer(item);
                    }
                });
            }
        }
        ui.separator();
        ui.heading("已配对节点");
        if self.node_peers.is_empty() {
            ui.label("还没有配对节点");
        } else {
            for peer in &self.node_peers {
                ui.label(format!(
                    "{}  {}  {}  {}",
                    peer.display_name,
                    peer.fingerprint,
                    source_label(peer.discovery_source),
                    peer.addresses.first().cloned().unwrap_or_default()
                ));
            }
        }
    }

    pub(super) fn refresh_peer_views(&mut self) {
        self.reload_peer_settings();
        self.refresh_peer_lists();
    }

    fn reload_peer_settings(&mut self) {
        let identity = self.state.peers.identity();
        self.node_display_name = identity.display_name.clone();
        self.peer_bind_addr = self
            .runtime
            .block_on(self.state.store.peer_bind_addr())
            .unwrap_or_else(|_| crate::peer::protocol::DEFAULT_PEER_BIND_ADDR.to_string());
        self.mdns_discovery_enabled = self
            .runtime
            .block_on(self.state.store.mdns_discovery_enabled())
            .unwrap_or(false);
        self.lnd_discovery_enabled = self
            .runtime
            .block_on(self.state.store.lnd_discovery_enabled())
            .unwrap_or(false);
        self.lnd_server_url = self
            .runtime
            .block_on(self.state.store.lnd_server_url())
            .ok()
            .flatten()
            .unwrap_or_default();
        self.lnd_bearer_token = self
            .runtime
            .block_on(self.state.store.lnd_bearer_token())
            .ok()
            .flatten()
            .unwrap_or_default();
        self.lnd_discovery_domain = self
            .runtime
            .block_on(self.state.store.lnd_discovery_domain())
            .ok()
            .flatten()
            .unwrap_or_default();
    }

    pub(super) fn refresh_peer_lists(&mut self) {
        let identity = self.state.peers.identity();
        self.node_id = identity.node_id.clone();
        self.node_fingerprint = identity.fingerprint();
        self.node_peers = self
            .runtime
            .block_on(self.state.store.list_node_peers())
            .unwrap_or_default();
        self.pairing_requests = self
            .runtime
            .block_on(self.state.store.list_peer_pairing_requests())
            .unwrap_or_default();
        self.discovered_peers = self.state.peers.discovered_peers();
        self.last_seen_peer_version = self.state.events.peer_version();
    }

    fn save_node_display_name(&mut self) {
        let name = self.node_display_name.trim().to_string();
        if name.is_empty() {
            self.status = "节点显示名不能为空".to_string();
            return;
        }
        if let Err(err) = self
            .runtime
            .block_on(self.state.store.save_node_display_name(&name))
        {
            self.status = format!("保存节点显示名失败: {err}");
            return;
        }
        self.status = "节点显示名已保存, 重启节点监听后生效".to_string();
    }

    fn save_peer_setting(&mut self, key: &str, value: String) {
        if let Err(err) = self
            .runtime
            .block_on(self.state.store.set_setting(key, &value))
        {
            self.status = format!("保存节点设置失败: {err}");
            return;
        }
        self.status = "节点设置已保存, 重启节点监听后生效".to_string();
    }

    fn open_lnd_settings_window(&mut self) {
        self.lnd_settings_url = self.lnd_server_url.clone();
        self.lnd_settings_token = self.lnd_bearer_token.clone();
        self.lnd_settings_domain = self.lnd_discovery_domain.clone();
        self.lnd_settings_open = true;
    }

    fn lnd_settings_window(&mut self, ctx: &egui::Context) {
        if !self.lnd_settings_open {
            return;
        }
        let mut open = self.lnd_settings_open;
        let mut save_requested = false;
        let mut cancel_requested = false;
        egui::Window::new("LND 设置")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("填写已有 lnd-server, Codex Switch 只做 client.");
                ui.horizontal(|ui| {
                    ui.label("lnd server");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.lnd_settings_url)
                            .hint_text("http://192.168.1.2:8765"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("lnd token");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.lnd_settings_token)
                            .password(true)
                            .hint_text("bearer token"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("lnd domain");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.lnd_settings_domain)
                            .hint_text("可选 discovery domain"),
                    );
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
            self.save_lnd_settings();
        } else if cancel_requested || !open {
            self.lnd_settings_open = false;
        } else {
            self.lnd_settings_open = open;
        }
    }

    fn save_lnd_settings(&mut self) {
        let url = self.lnd_settings_url.trim().to_string();
        let token = self.lnd_settings_token.trim().to_string();
        let domain = self.lnd_settings_domain.trim().to_string();
        let result = self.runtime.block_on(async {
            self.state.store.set_setting("lnd_server_url", &url).await?;
            self.state
                .store
                .set_setting("lnd_bearer_token", &token)
                .await?;
            self.state
                .store
                .set_setting("lnd_discovery_domain", &domain)
                .await?;
            anyhow::Ok(())
        });
        match result {
            Ok(()) => {
                self.lnd_server_url = url;
                self.lnd_bearer_token = token;
                self.lnd_discovery_domain = domain;
                self.lnd_settings_open = false;
                self.status = "LND 设置已保存, 重启节点监听后生效".to_string();
            }
            Err(err) => {
                self.status = format!("保存 LND 设置失败: {err}");
                self.lnd_settings_open = true;
            }
        }
    }

    fn pair_direct_peer(&mut self) {
        if self.pairing_pending {
            return;
        }
        let address = self.peer_direct_address.trim().to_string();
        if address.is_empty() {
            self.status = "请填写节点地址".to_string();
            return;
        }
        let fingerprint = self.peer_direct_fingerprint.trim().to_string();
        self.pairing_pending = true;
        self.status = "正在配对节点".to_string();
        let state = self.state.clone();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let result = state
                .peers
                .pair_direct(
                    &state.store,
                    &address,
                    (!fingerprint.is_empty()).then_some(fingerprint.as_str()),
                )
                .await
                .map(|peer| format!("已配对 {}", peer.display_name));
            let _ = tx.send(super::UiTaskEvent::PeerPaired(result));
        });
    }

    fn update_discovered_peer_address(
        &mut self,
        discovered: crate::peer::discovery::DiscoveredPeer,
    ) {
        match self.runtime.block_on(self.state.store.update_paired_peer_addresses(
            &discovered.node_id,
            &discovered.fingerprint,
            &discovered.addresses,
            discovered.source,
        )) {
            Ok(()) => {
                self.status = format!("已更新 {} 的地址", discovered.display_name);
                self.state.events.bump_peers();
                self.refresh_all();
            }
            Err(err) => self.status = format!("更新节点地址失败: {err}"),
        }
    }

    fn pair_discovered_peer(&mut self, discovered: crate::peer::discovery::DiscoveredPeer) {
        if self.pairing_pending {
            return;
        }
        self.pairing_pending = true;
        self.status = "正在配对发现到的节点".to_string();
        let state = self.state.clone();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let result = state
                .peers
                .pair_discovered(&state.store, &discovered)
                .await
                .map(|peer| format!("已配对 {}", peer.display_name));
            let _ = tx.send(super::UiTaskEvent::PeerPaired(result));
        });
    }

    fn accept_pairing_request(&mut self, request_id: &str) {
        match self.runtime.block_on(
            self.state
                .peers
                .accept_pairing_request(&self.state.store, request_id),
        ) {
            Ok(peer) => {
                self.status = format!("已接受配对 {}", peer.display_name);
                self.state.events.bump_peers();
                self.refresh_all();
            }
            Err(err) => self.status = format!("接受配对失败: {err}"),
        }
    }

    fn reject_pairing_request(&mut self, request_id: &str) {
        match self.runtime.block_on(
            self.state
                .peers
                .reject_pairing_request(&self.state.store, request_id),
        ) {
            Ok(()) => {
                self.status = "已拒绝配对请求".to_string();
                self.state.events.bump_peers();
                self.refresh_peer_views();
            }
            Err(err) => self.status = format!("拒绝配对失败: {err}"),
        }
    }
}

fn bool_setting(value: bool) -> String {
    if value { "true" } else { "false" }.to_string()
}

fn peer_addresses_differ(
    peer: &crate::core::models::NodePeer,
    discovered: &crate::peer::discovery::DiscoveredPeer,
) -> bool {
    canonical_peer_addresses(&peer.addresses) != canonical_peer_addresses(&discovered.addresses)
}

fn canonical_peer_addresses(addresses: &[String]) -> Vec<String> {
    addresses
        .iter()
        .filter_map(|address| crate::peer::protocol::parse_peer_address(address).ok())
        .collect()
}

fn source_label(source: PeerDiscoverySource) -> &'static str {
    match source {
        PeerDiscoverySource::Direct => "直连",
        PeerDiscoverySource::Mdns => "mDNS",
        PeerDiscoverySource::Lnd => "lnd",
    }
}
