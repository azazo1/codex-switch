use super::CodexSwitchApp;
use crate::core::models::{ScheduleGroup, ScheduleGroupMember, ScheduleMode, Upstream};
use eframe::egui;
use std::collections::BTreeSet;

#[derive(Clone)]
pub(super) struct ScheduleGroupEditor {
    pub group: ScheduleGroup,
    pub member_ids: BTreeSet<String>,
}

impl ScheduleGroupEditor {
    pub fn new_empty() -> Self {
        Self {
            group: ScheduleGroup::new(String::new()),
            member_ids: BTreeSet::new(),
        }
    }

    fn new(group: ScheduleGroup, members: &[ScheduleGroupMember]) -> Self {
        let member_ids = members
            .iter()
            .filter(|member| member.enabled)
            .map(|member| member.upstream_id.clone())
            .collect();
        Self { group, member_ids }
    }
}

impl CodexSwitchApp {
    pub(super) fn scheduler_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("添加调度组");
        let upstreams = self.upstreams.clone();
        let mut add = false;
        ui.horizontal(|ui| {
            ui.label("名称");
            ui.text_edit_singleline(&mut self.new_schedule_group.group.name);
            if ui.button("添加").clicked() {
                add = true;
            }
        });
        schedule_group_options_form(ui, &mut self.new_schedule_group.group, &upstreams);
        schedule_members_form(ui, &mut self.new_schedule_group, &upstreams);
        if add {
            self.add_schedule_group();
        }

        ui.separator();
        ui.heading("调度组列表");
        let mut current = None;
        let mut edit = None;
        let mut deleted = Vec::new();
        for group in &self.schedule_groups {
            ui.horizontal(|ui| {
                let selected = self.current_schedule_group_id.as_deref() == Some(&group.id);
                if ui.radio(selected, "").clicked() && !selected {
                    current = Some(group.id.clone());
                }
                ui.label(format!(
                    "{} [{}]",
                    group.name,
                    schedule_group_summary(group, &upstreams)
                ));
                if ui.button("编辑").clicked() {
                    edit = Some(group.clone());
                }
                if ui
                    .add_enabled(group.id != "default", egui::Button::new("删除"))
                    .clicked()
                {
                    deleted.push(group.id.clone());
                }
            });
        }

        if let Some(group_id) = current {
            self.set_current_schedule_group(&group_id);
        }
        if let Some(group) = edit {
            self.open_schedule_group_editor(group);
        }
        for group_id in deleted {
            self.delete_schedule_group(&group_id);
        }
        self.show_schedule_group_editor(ui.ctx());
    }

    fn add_schedule_group(&mut self) {
        let mut editor = self.new_schedule_group.clone();
        editor.group.id = uuid::Uuid::new_v4().to_string();
        editor.group.created_at = chrono::Utc::now();
        editor.group.updated_at = editor.group.created_at;
        if editor.group.name.trim().is_empty() {
            editor.group.name = "New Group".to_string();
        }
        let prepared = match prepare_schedule_group_editor(editor) {
            Ok(prepared) => prepared,
            Err(err) => {
                self.status = err;
                return;
            }
        };
        let group = prepared.group;
        let member_ids = prepared.member_ids;
        let upstream_ids = self
            .upstreams
            .iter()
            .map(|upstream| upstream.id.clone())
            .collect::<Vec<_>>();
        let result = self.runtime.block_on(async {
            self.state.store.save_schedule_group(&group).await?;
            if !group.use_all_upstreams {
                for upstream_id in upstream_ids {
                    let mut member =
                        ScheduleGroupMember::new(group.id.clone(), upstream_id.clone());
                    member.enabled = member_ids.contains(&upstream_id);
                    self.state.store.save_schedule_group_member(&member).await?;
                }
            }
            self.state.store.set_current_schedule_group(&group.id).await
        });
        match result {
            Ok(()) => {
                self.new_schedule_group = ScheduleGroupEditor::new_empty();
                self.schedule_group_editor = None;
                self.status = "已添加调度组".to_string();
                self.refresh_all();
            }
            Err(err) => self.status = format!("添加调度组失败: {err}"),
        }
    }

    fn open_schedule_group_editor(&mut self, group: ScheduleGroup) {
        let members = self
            .schedule_members
            .get(&group.id)
            .cloned()
            .unwrap_or_default();
        self.schedule_group_editor = Some(ScheduleGroupEditor::new(group, &members));
    }

    fn show_schedule_group_editor(&mut self, ctx: &egui::Context) {
        let Some(editor) = self.schedule_group_editor.as_mut() else {
            return;
        };
        let upstreams = self.upstreams.clone();
        let mut open = true;
        let mut action = EditorAction::None;
        egui::Window::new("编辑调度组")
            .collapsible(false)
            .resizable(true)
            .open(&mut open)
            .show(ctx, |ui| {
                schedule_group_form(ui, &mut editor.group, &upstreams);
                schedule_members_form(ui, editor, &upstreams);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        action = EditorAction::Save;
                    }
                    if ui.button("取消").clicked() {
                        action = EditorAction::Cancel;
                    }
                });
            });
        if !open {
            action = EditorAction::Cancel;
        }
        match action {
            EditorAction::None => {}
            EditorAction::Cancel => {
                self.schedule_group_editor = None;
            }
            EditorAction::Save => {
                if let Some(editor) = self.schedule_group_editor.clone() {
                    self.save_schedule_group_editor(editor);
                }
            }
        }
    }

    fn save_schedule_group_editor(&mut self, editor: ScheduleGroupEditor) {
        let prepared = match prepare_schedule_group_editor(editor) {
            Ok(prepared) => prepared,
            Err(err) => {
                self.status = err;
                return;
            }
        };
        let group = prepared.group;
        let member_ids = prepared.member_ids;

        let upstream_ids = self
            .upstreams
            .iter()
            .map(|upstream| upstream.id.clone())
            .collect::<Vec<_>>();
        let result = self.runtime.block_on(async {
            self.state.store.save_schedule_group(&group).await?;
            if !group.use_all_upstreams {
                for upstream_id in upstream_ids {
                    let mut member =
                        ScheduleGroupMember::new(group.id.clone(), upstream_id.clone());
                    member.enabled = member_ids.contains(&upstream_id);
                    self.state.store.save_schedule_group_member(&member).await?;
                }
            }
            anyhow::Ok(())
        });
        match result {
            Ok(()) => {
                self.schedule_group_editor = None;
                self.status = "调度组已保存".to_string();
                self.refresh_all();
            }
            Err(err) => self.status = format!("保存调度组失败: {err}"),
        }
    }

    fn delete_schedule_group(&mut self, group_id: &str) {
        match self
            .runtime
            .block_on(self.state.store.delete_schedule_group(group_id))
        {
            Ok(()) => {
                self.schedule_group_editor = None;
                self.status = "调度组已删除".to_string();
                self.refresh_all();
            }
            Err(err) => self.status = format!("删除调度组失败: {err}"),
        }
    }

    fn set_current_schedule_group(&mut self, group_id: &str) {
        match self
            .runtime
            .block_on(self.state.store.set_current_schedule_group(group_id))
        {
            Ok(()) => {
                self.current_schedule_group_id = Some(group_id.to_string());
                self.schedule_group_editor = None;
                self.status = "当前调度组已切换".to_string();
                self.refresh_all();
            }
            Err(err) => self.status = format!("切换调度组失败: {err}"),
        }
    }
}

fn prepare_schedule_group_editor(
    mut editor: ScheduleGroupEditor,
) -> Result<ScheduleGroupEditor, String> {
    editor.group.name = editor.group.name.trim().to_string();
    editor.group.failure_threshold = editor.group.failure_threshold.max(1);
    editor.group.affinity_ttl_seconds = editor.group.affinity_ttl_seconds.max(60);
    if editor.group.name.is_empty() {
        return Err("调度组名称不能为空".to_string());
    }
    if editor.group.mode == ScheduleMode::Fixed {
        let Some(upstream_id) = &editor.group.fixed_upstream_id else {
            return Err("固定模式需要选择上游".to_string());
        };
        editor.member_ids.insert(upstream_id.clone());
    }
    if !editor.group.use_all_upstreams && editor.member_ids.is_empty() {
        return Err("至少选择一个组内上游".to_string());
    }
    Ok(editor)
}

fn schedule_group_form(ui: &mut egui::Ui, group: &mut ScheduleGroup, upstreams: &[Upstream]) {
    ui.horizontal(|ui| {
        ui.label("名称");
        ui.text_edit_singleline(&mut group.name);
    });
    schedule_group_options_form(ui, group, upstreams);
}

fn schedule_group_options_form(
    ui: &mut egui::Ui,
    group: &mut ScheduleGroup,
    upstreams: &[Upstream],
) {
    egui::ComboBox::from_label("模式")
        .selected_text(mode_label(group.mode))
        .show_ui(ui, |ui| {
            for mode in ScheduleMode::ALL {
                ui.selectable_value(&mut group.mode, mode, mode_label(mode));
            }
        });
    ui.horizontal(|ui| {
        ui.checkbox(&mut group.use_all_upstreams, "使用全部上游");
        ui.label("亲和 TTL 秒");
        ui.add(egui::DragValue::new(&mut group.affinity_ttl_seconds).speed(60));
    });
    match group.mode {
        ScheduleMode::Failover => failover_options(ui, group),
        ScheduleMode::Fixed => fixed_options(ui, group, upstreams),
        ScheduleMode::Random | ScheduleMode::RoundRobin => {}
    }
}

fn schedule_members_form(
    ui: &mut egui::Ui,
    editor: &mut ScheduleGroupEditor,
    upstreams: &[Upstream],
) {
    if editor.group.use_all_upstreams {
        ui.label("当前调度组使用全部启用上游");
        return;
    }
    ui.separator();
    ui.label("组内上游");
    for upstream in upstreams {
        let mut enabled = editor.member_ids.contains(&upstream.id);
        let label = if upstream.enabled {
            upstream.name.clone()
        } else {
            format!("{} (disabled)", upstream.name)
        };
        if ui.checkbox(&mut enabled, label).changed() {
            if enabled {
                editor.member_ids.insert(upstream.id.clone());
            } else {
                editor.member_ids.remove(&upstream.id);
            }
        }
    }
}

fn failover_options(ui: &mut egui::Ui, group: &mut ScheduleGroup) {
    ui.horizontal(|ui| {
        ui.label("失败阈值");
        ui.add(egui::DragValue::new(&mut group.failure_threshold).speed(1));
        ui.checkbox(&mut group.failover_on_balance, "余额不足切换");
        ui.checkbox(&mut group.failover_on_network, "网络失败切换");
        ui.checkbox(&mut group.failover_on_5xx, "5xx 切换");
    });
}

fn fixed_options(ui: &mut egui::Ui, group: &mut ScheduleGroup, upstreams: &[Upstream]) {
    let selected = group
        .fixed_upstream_id
        .as_ref()
        .and_then(|id| upstreams.iter().find(|upstream| upstream.id == *id))
        .map(|upstream| upstream.name.clone())
        .unwrap_or_else(|| "未选择".to_string());
    egui::ComboBox::from_label("固定上游")
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for upstream in upstreams {
                ui.selectable_value(
                    &mut group.fixed_upstream_id,
                    Some(upstream.id.clone()),
                    &upstream.name,
                );
            }
        });
}

fn schedule_group_summary(group: &ScheduleGroup, upstreams: &[Upstream]) -> String {
    if group.mode != ScheduleMode::Fixed {
        return mode_label(group.mode).to_string();
    }
    match group
        .fixed_upstream_id
        .as_ref()
        .and_then(|id| upstreams.iter().find(|upstream| upstream.id == *id))
    {
        Some(upstream) => format!("固定: {}", upstream.name),
        None if group.fixed_upstream_id.is_some() => "固定: 上游不存在".to_string(),
        None => "固定: 未选择".to_string(),
    }
}

fn mode_label(mode: ScheduleMode) -> &'static str {
    match mode {
        ScheduleMode::Random => "随机",
        ScheduleMode::RoundRobin => "轮询",
        ScheduleMode::Failover => "失败切换",
        ScheduleMode::Fixed => "固定",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorAction {
    None,
    Save,
    Cancel,
}
