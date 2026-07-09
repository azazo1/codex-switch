use super::CodexSwitchApp;
use crate::core::models::{
    ScheduleGroup, ScheduleGroupChild, ScheduleGroupMember, ScheduleMode, ScheduleRouteRule,
    ScheduleRouteTargetKind, Upstream,
};
use eframe::egui;
use std::collections::BTreeSet;

#[derive(Clone)]
pub(super) struct ScheduleGroupEditor {
    pub group: ScheduleGroup,
    pub member_ids: BTreeSet<String>,
    pub child_group_ids: BTreeSet<String>,
    pub route_rules: Vec<ScheduleRouteRule>,
}

impl ScheduleGroupEditor {
    pub fn new_empty() -> Self {
        Self {
            group: ScheduleGroup::new(String::new()),
            member_ids: BTreeSet::new(),
            child_group_ids: BTreeSet::new(),
            route_rules: Vec::new(),
        }
    }

    fn new(
        group: ScheduleGroup,
        members: &[ScheduleGroupMember],
        children: &[ScheduleGroupChild],
        route_rules: &[ScheduleRouteRule],
    ) -> Self {
        let member_ids = members
            .iter()
            .filter(|member| member.enabled)
            .map(|member| member.upstream_id.clone())
            .collect();
        let child_group_ids = children
            .iter()
            .filter(|child| child.enabled)
            .map(|child| child.target_group_id.clone())
            .collect();
        Self {
            group,
            member_ids,
            child_group_ids,
            route_rules: route_rules.to_vec(),
        }
    }
}

impl CodexSwitchApp {
    pub(super) fn scheduler_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("路由设置");
        ui.horizontal(|ui| {
            ui.label("最大跳转次数");
            ui.add(egui::DragValue::new(&mut self.scheduler_route_max_hops).speed(1));
            if ui.button("保存").clicked() {
                self.save_scheduler_route_max_hops();
            }
        });
        ui.separator();

        ui.heading("添加调度组");
        let upstreams = self.upstreams.clone();
        let schedule_groups = self.schedule_groups.clone();
        let mut add = false;
        ui.horizontal(|ui| {
            ui.label("名称");
            ui.text_edit_singleline(&mut self.new_schedule_group.group.name);
            if ui.button("添加").clicked() {
                add = true;
            }
        });
        schedule_group_options_form(
            ui,
            &mut self.new_schedule_group.group,
            &upstreams,
            &schedule_groups,
        );
        schedule_members_form(
            ui,
            &mut self.new_schedule_group,
            &upstreams,
            &schedule_groups,
        );
        schedule_route_section_form(
            ui,
            &mut self.new_schedule_group,
            &schedule_groups,
            &upstreams,
        );
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
                    schedule_group_summary(
                        group,
                        &upstreams,
                        self.schedule_route_rules
                            .get(&group.id)
                            .map(Vec::as_slice)
                            .unwrap_or(&[])
                    )
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
        let child_group_ids = prepared.child_group_ids;
        let route_rules = prepared.route_rules;
        let upstream_ids = self
            .upstreams
            .iter()
            .map(|upstream| upstream.id.clone())
            .collect::<Vec<_>>();
        let group_ids = self
            .schedule_groups
            .iter()
            .map(|group| group.id.clone())
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
                for group_id in group_ids {
                    if group_id == group.id {
                        continue;
                    }
                    let mut child = ScheduleGroupChild::new(group.id.clone(), group_id.clone());
                    child.enabled = child_group_ids.contains(&group_id);
                    self.state.store.save_schedule_group_child(&child).await?;
                }
            }
            for rule in route_rules {
                self.state.store.save_schedule_route_rule(&rule).await?;
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
        let route_rules = self
            .schedule_route_rules
            .get(&group.id)
            .cloned()
            .unwrap_or_default();
        let children = self
            .schedule_children
            .get(&group.id)
            .cloned()
            .unwrap_or_default();
        self.schedule_group_editor = Some(ScheduleGroupEditor::new(
            group,
            &members,
            &children,
            &route_rules,
        ));
    }

    fn show_schedule_group_editor(&mut self, ctx: &egui::Context) {
        let Some(editor) = self.schedule_group_editor.as_mut() else {
            return;
        };
        let upstreams = self.upstreams.clone();
        let schedule_groups = self.schedule_groups.clone();
        let mut open = true;
        let mut action = EditorAction::None;
        egui::Window::new("编辑调度组")
            .collapsible(false)
            .resizable(true)
            .open(&mut open)
            .show(ctx, |ui| {
                schedule_group_form(ui, &mut editor.group, &upstreams, &schedule_groups);
                schedule_members_form(ui, editor, &upstreams, &schedule_groups);
                schedule_route_section_form(ui, editor, &schedule_groups, &upstreams);
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
        let child_group_ids = prepared.child_group_ids;
        let route_rules = prepared.route_rules;
        let route_rule_ids = route_rules
            .iter()
            .map(|rule| rule.id.clone())
            .collect::<BTreeSet<_>>();
        let existing_route_rules = self
            .schedule_route_rules
            .get(&group.id)
            .cloned()
            .unwrap_or_default();

        let upstream_ids = self
            .upstreams
            .iter()
            .map(|upstream| upstream.id.clone())
            .collect::<Vec<_>>();
        let group_ids = self
            .schedule_groups
            .iter()
            .map(|group| group.id.clone())
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
                for group_id in group_ids {
                    if group_id == group.id {
                        continue;
                    }
                    let mut child = ScheduleGroupChild::new(group.id.clone(), group_id.clone());
                    child.enabled = child_group_ids.contains(&group_id);
                    self.state.store.save_schedule_group_child(&child).await?;
                }
            }
            for existing in existing_route_rules {
                if !route_rule_ids.contains(&existing.id) {
                    self.state.store.delete_schedule_route_rule(&existing.id).await?;
                }
            }
            for rule in route_rules {
                self.state.store.save_schedule_route_rule(&rule).await?;
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

    fn save_scheduler_route_max_hops(&mut self) {
        self.scheduler_route_max_hops = self.scheduler_route_max_hops.max(1);
        match self.runtime.block_on(
            self.state
                .store
                .set_setting("scheduler_route_max_hops", &self.scheduler_route_max_hops.to_string()),
        ) {
            Ok(()) => self.status = "路由设置已保存".to_string(),
            Err(err) => self.status = format!("保存路由设置失败: {err}"),
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
        match editor.group.fixed_target_kind {
            ScheduleRouteTargetKind::Upstream => {
                let Some(upstream_id) = &editor.group.fixed_upstream_id else {
                    return Err("固定模式需要选择上游".to_string());
                };
                editor.member_ids.insert(upstream_id.clone());
                editor.group.fixed_group_id = None;
            }
            ScheduleRouteTargetKind::Group => {
                let Some(group_id) = &editor.group.fixed_group_id else {
                    return Err("固定模式需要选择调度组".to_string());
                };
                editor.child_group_ids.insert(group_id.clone());
                editor.group.fixed_upstream_id = None;
            }
        }
    }
    if editor.group.mode != ScheduleMode::ModelMapping
        && !editor.group.use_all_upstreams
        && editor.member_ids.is_empty()
        && editor.child_group_ids.is_empty()
    {
        return Err("至少选择一个组内目标".to_string());
    }
    if editor.group.mode != ScheduleMode::ModelMapping {
        return Ok(editor);
    }
    if editor.route_rules.is_empty() {
        return Err("模型映射模式至少需要一条路由规则".to_string());
    }
    for rule in &mut editor.route_rules {
        rule.group_id = editor.group.id.clone();
        rule.name = rule.name.trim().to_string();
        rule.pattern = rule.pattern.trim().to_string();
        if rule.pattern.is_empty() {
            return Err("模型路由 pattern 不能为空".to_string());
        }
        if rule.name.is_empty() {
            rule.name = rule.pattern.clone();
        }
        rule.target_model = rule
            .target_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        match rule.target_kind {
            ScheduleRouteTargetKind::Group => {
                rule.target_group_id = rule
                    .target_group_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                rule.target_upstream_id = None;
                if rule.target_group_id.is_none() {
                    return Err("模型路由需要选择目标调度组".to_string());
                }
            }
            ScheduleRouteTargetKind::Upstream => {
                rule.target_upstream_id = rule
                    .target_upstream_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                rule.target_group_id = None;
                if rule.target_upstream_id.is_none() {
                    return Err("模型路由需要选择目标上游".to_string());
                }
            }
        }
    }
    Ok(editor)
}

fn schedule_group_form(
    ui: &mut egui::Ui,
    group: &mut ScheduleGroup,
    upstreams: &[Upstream],
    groups: &[ScheduleGroup],
) {
    ui.horizontal(|ui| {
        ui.label("名称");
        ui.text_edit_singleline(&mut group.name);
    });
    schedule_group_options_form(ui, group, upstreams, groups);
}

fn schedule_group_options_form(
    ui: &mut egui::Ui,
    group: &mut ScheduleGroup,
    upstreams: &[Upstream],
    groups: &[ScheduleGroup],
) {
    egui::ComboBox::from_label("模式")
        .selected_text(mode_label(group.mode))
        .show_ui(ui, |ui| {
            for mode in ScheduleMode::ALL {
                ui.selectable_value(&mut group.mode, mode, mode_label(mode));
            }
        });
    ui.horizontal(|ui| {
        if group.mode != ScheduleMode::ModelMapping {
            ui.checkbox(&mut group.use_all_upstreams, "使用全部目标");
        }
        ui.label("亲和 TTL 秒");
        ui.add(egui::DragValue::new(&mut group.affinity_ttl_seconds).speed(60));
    });
    match group.mode {
        ScheduleMode::Failover => failover_options(ui, group),
        ScheduleMode::Fixed => fixed_options(ui, group, upstreams, groups),
        ScheduleMode::Random | ScheduleMode::RoundRobin | ScheduleMode::ModelMapping => {}
    }
}

fn schedule_members_form(
    ui: &mut egui::Ui,
    editor: &mut ScheduleGroupEditor,
    upstreams: &[Upstream],
    groups: &[ScheduleGroup],
) {
    if editor.group.mode == ScheduleMode::ModelMapping {
        return;
    }
    if editor.group.use_all_upstreams {
        ui.label("当前调度组使用全部启用上游");
        return;
    }
    ui.separator();
    ui.label("组内目标");
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
    for group in groups.iter().filter(|group| group.id != editor.group.id) {
        let mut enabled = editor.child_group_ids.contains(&group.id);
        let label = format!("调度组: {}", group.name);
        if ui.checkbox(&mut enabled, label).changed() {
            if enabled {
                editor.child_group_ids.insert(group.id.clone());
            } else {
                editor.child_group_ids.remove(&group.id);
            }
        }
    }
}

fn schedule_route_section_form(
    ui: &mut egui::Ui,
    editor: &mut ScheduleGroupEditor,
    groups: &[ScheduleGroup],
    upstreams: &[Upstream],
) {
    if editor.group.mode != ScheduleMode::ModelMapping {
        return;
    }
    schedule_route_rules_form(ui, editor, groups, upstreams);
}

fn schedule_route_rules_form(
    ui: &mut egui::Ui,
    editor: &mut ScheduleGroupEditor,
    groups: &[ScheduleGroup],
    upstreams: &[Upstream],
) {
    ui.separator();
    ui.horizontal(|ui| {
        ui.label("模型路由规则");
        if ui.button("添加规则").clicked() {
            editor
                .route_rules
                .push(ScheduleRouteRule::new(editor.group.id.clone()));
        }
        if ui.button("添加默认规则").clicked() {
            let mut rule = ScheduleRouteRule::new(editor.group.id.clone());
            rule.name = "Default".to_string();
            rule.pattern = "*".to_string();
            editor.route_rules.push(rule);
        }
    });
    if editor.route_rules.is_empty() {
        ui.label("当前调度组没有模型路由规则");
        return;
    }
    let mut deleted = BTreeSet::new();
    egui::Grid::new(format!("route_rules_{}", editor.group.id))
        .striped(true)
        .num_columns(8)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.strong("启用");
            ui.strong("名称");
            ui.strong("Pattern");
            ui.strong("目标类型");
            ui.strong("目标");
            ui.strong("目标模型");
            ui.strong("优先级");
            ui.strong("操作");
            ui.end_row();

            for rule in &mut editor.route_rules {
                ui.checkbox(&mut rule.enabled, "");
                ui.text_edit_singleline(&mut rule.name);
                ui.text_edit_singleline(&mut rule.pattern);
                route_target_kind_combo(ui, rule);
                match rule.target_kind {
                    ScheduleRouteTargetKind::Group => route_target_group_combo(ui, rule, groups),
                    ScheduleRouteTargetKind::Upstream => {
                        route_target_upstream_combo(ui, rule, upstreams)
                    }
                }
                let target_model = rule.target_model.get_or_insert_with(String::new);
                ui.text_edit_singleline(target_model);
                ui.add(egui::DragValue::new(&mut rule.priority).speed(1));
                if ui.button("删除").clicked() {
                    deleted.insert(rule.id.clone());
                }
                ui.end_row();
            }
        });
    editor
        .route_rules
        .retain(|rule| !deleted.contains(&rule.id));
}

fn route_target_kind_combo(ui: &mut egui::Ui, rule: &mut ScheduleRouteRule) {
    let selected = match rule.target_kind {
        ScheduleRouteTargetKind::Group => "调度组",
        ScheduleRouteTargetKind::Upstream => "上游",
    };
    egui::ComboBox::from_id_salt(format!("target_kind_{}", rule.id))
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut rule.target_kind,
                ScheduleRouteTargetKind::Group,
                "调度组",
            );
            ui.selectable_value(
                &mut rule.target_kind,
                ScheduleRouteTargetKind::Upstream,
                "上游",
            );
        });
}

fn route_target_group_combo(
    ui: &mut egui::Ui,
    rule: &mut ScheduleRouteRule,
    groups: &[ScheduleGroup],
) {
    let selected = rule
        .target_group_id
        .as_ref()
        .and_then(|id| groups.iter().find(|group| group.id == *id))
        .map(|group| group.name.clone())
        .unwrap_or_else(|| "未选择".to_string());
    egui::ComboBox::from_id_salt(format!("target_group_{}", rule.id))
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for group in groups {
                ui.selectable_value(
                    &mut rule.target_group_id,
                    Some(group.id.clone()),
                    &group.name,
                );
            }
        });
}

fn route_target_upstream_combo(
    ui: &mut egui::Ui,
    rule: &mut ScheduleRouteRule,
    upstreams: &[Upstream],
) {
    let selected = rule
        .target_upstream_id
        .as_ref()
        .and_then(|id| upstreams.iter().find(|upstream| upstream.id == *id))
        .map(|upstream| upstream.name.clone())
        .unwrap_or_else(|| "未选择".to_string());
    egui::ComboBox::from_id_salt(format!("target_upstream_{}", rule.id))
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for upstream in upstreams {
                ui.selectable_value(
                    &mut rule.target_upstream_id,
                    Some(upstream.id.clone()),
                    &upstream.name,
                );
            }
        });
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

fn fixed_options(
    ui: &mut egui::Ui,
    group: &mut ScheduleGroup,
    upstreams: &[Upstream],
    groups: &[ScheduleGroup],
) {
    egui::ComboBox::from_label("固定目标类型")
        .selected_text(target_kind_label(group.fixed_target_kind))
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut group.fixed_target_kind,
                ScheduleRouteTargetKind::Upstream,
                "上游",
            );
            ui.selectable_value(
                &mut group.fixed_target_kind,
                ScheduleRouteTargetKind::Group,
                "调度组",
            );
        });
    match group.fixed_target_kind {
        ScheduleRouteTargetKind::Upstream => fixed_upstream_options(ui, group, upstreams),
        ScheduleRouteTargetKind::Group => fixed_group_options(ui, group, groups),
    }
}

fn fixed_upstream_options(ui: &mut egui::Ui, group: &mut ScheduleGroup, upstreams: &[Upstream]) {
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

fn fixed_group_options(ui: &mut egui::Ui, group: &mut ScheduleGroup, groups: &[ScheduleGroup]) {
    let selected = group
        .fixed_group_id
        .as_ref()
        .and_then(|id| groups.iter().find(|candidate| candidate.id == *id))
        .map(|candidate| candidate.name.clone())
        .unwrap_or_else(|| "未选择".to_string());
    egui::ComboBox::from_label("固定调度组")
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for candidate in groups.iter().filter(|candidate| candidate.id != group.id) {
                ui.selectable_value(
                    &mut group.fixed_group_id,
                    Some(candidate.id.clone()),
                    &candidate.name,
                );
            }
        });
}

fn schedule_group_summary(
    group: &ScheduleGroup,
    upstreams: &[Upstream],
    route_rules: &[ScheduleRouteRule],
) -> String {
    let mode = if group.mode != ScheduleMode::Fixed {
        mode_label(group.mode).to_string()
    } else {
        match group.fixed_target_kind {
            ScheduleRouteTargetKind::Upstream => {
                match group
                    .fixed_upstream_id
                    .as_ref()
                    .and_then(|id| upstreams.iter().find(|upstream| upstream.id == *id))
                {
                    Some(upstream) => format!("固定上游: {}", upstream.name),
                    None if group.fixed_upstream_id.is_some() => "固定上游: 不存在".to_string(),
                    None => "固定上游: 未选择".to_string(),
                }
            }
            ScheduleRouteTargetKind::Group => "固定调度组".to_string(),
        }
    };
    if group.mode != ScheduleMode::ModelMapping || route_rules.is_empty() {
        mode
    } else {
        format!("{mode}, 路由 {} 条", route_rules.len())
    }
}

fn mode_label(mode: ScheduleMode) -> &'static str {
    match mode {
        ScheduleMode::Random => "随机",
        ScheduleMode::RoundRobin => "轮询",
        ScheduleMode::Failover => "失败切换",
        ScheduleMode::Fixed => "固定",
        ScheduleMode::ModelMapping => "模型映射",
    }
}

fn target_kind_label(kind: ScheduleRouteTargetKind) -> &'static str {
    match kind {
        ScheduleRouteTargetKind::Group => "调度组",
        ScheduleRouteTargetKind::Upstream => "上游",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorAction {
    None,
    Save,
    Cancel,
}
