use super::{CodexSwitchApp, UiTaskEvent};
use crate::oauth as oauth_api;
use eframe::egui;
use std::time::{Duration, Instant};

mod state;

pub(super) use state::{OAuthPollTaskResult, OAuthUiState};
use state::{
    OAuthImportBatchUi, OAuthLoginTask, OAuthLoginTaskState, import_source_label,
    oauth_task_status, oauth_task_title,
};

impl CodexSwitchApp {
    pub(super) fn oauth_accounts_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Codex OAuth");
        ui.horizontal(|ui| {
            if ui.button("新增登录任务").clicked() {
                self.start_oauth_task();
            }
            let import_pending = self
                .oauth_ui
                .import_batch
                .as_ref()
                .is_some_and(|batch| batch.result.is_none());
            if ui
                .add_enabled(!import_pending, egui::Button::new("导入 auth.json"))
                .clicked()
            {
                self.select_oauth_auth_files();
            }
            let has_finished = self
                .oauth_ui
                .tasks
                .iter()
                .any(|task| task.state.is_terminal())
                || self
                    .oauth_ui
                    .import_batch
                    .as_ref()
                    .is_some_and(|batch| batch.result.is_some());
            if ui
                .add_enabled(has_finished, egui::Button::new("清理已结束"))
                .clicked()
            {
                self.oauth_ui.tasks.retain(|task| !task.state.is_terminal());
                if self
                    .oauth_ui
                    .import_batch
                    .as_ref()
                    .is_some_and(|batch| batch.result.is_some())
                {
                    self.oauth_ui.import_batch = None;
                }
            }
        });

        let mut poll_task = None;
        let mut retry_task = None;
        let mut remove_task = None;
        for task in &self.oauth_ui.tasks {
            ui.separator();
            ui.horizontal(|ui| {
                ui.strong(oauth_task_title(task));
                ui.label(oauth_task_status(&task.state));
            });
            if let Some(flow) = &task.flow {
                ui.horizontal(|ui| {
                    ui.hyperlink_to("打开验证页", &flow.verification_uri);
                    ui.label(format!("用户码: {}", flow.user_code));
                    ui.label(format!("间隔: {} 秒", flow.interval));
                });
            }
            if let Some(warning) = &task.warning {
                ui.colored_label(egui::Color32::YELLOW, warning);
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        matches!(task.state, OAuthLoginTaskState::Waiting),
                        egui::Button::new("立即检查"),
                    )
                    .clicked()
                {
                    poll_task = Some(task.id.clone());
                }
                if ui
                    .add_enabled(
                        matches!(
                            task.state,
                            OAuthLoginTaskState::Failed(_) | OAuthLoginTaskState::Expired
                        ),
                        egui::Button::new("重试"),
                    )
                    .clicked()
                {
                    retry_task = Some(task.id.clone());
                }
                if ui
                    .add_enabled(
                        !matches!(task.state, OAuthLoginTaskState::Polling),
                        egui::Button::new("移除"),
                    )
                    .clicked()
                {
                    remove_task = Some(task.id.clone());
                }
            });
        }
        if let Some(id) = poll_task {
            self.poll_oauth_task(&id);
        }
        if let Some(id) = retry_task {
            self.restart_oauth_task(&id);
        }
        if let Some(id) = remove_task {
            self.oauth_ui.tasks.retain(|task| task.id != id);
        }

        if let Some(batch) = &self.oauth_ui.import_batch {
            ui.separator();
            ui.strong("凭据导入");
            let progress = if batch.total == 0 {
                0.0
            } else {
                batch.processed as f32 / batch.total as f32
            };
            ui.add(
                egui::ProgressBar::new(progress)
                    .text(format!("{}/{}", batch.processed, batch.total)),
            );
            if let Some(result) = &batch.result {
                ui.label(format!(
                    "新增 {}, 更新 {}, 失败 {}",
                    result.created, result.updated, result.failed
                ));
            }
            for item in &batch.items {
                ui.horizontal(|ui| {
                    ui.label(import_source_label(&item.source))
                        .on_hover_text(item.source.display().to_string());
                    match &item.outcome {
                        oauth_api::OAuthFileImportOutcome::Created {
                            upstream_id,
                            name,
                            refreshable,
                        } => {
                            ui.label(import_result_label("已新增", name, *refreshable))
                                .on_hover_text(format!("upstream: {upstream_id}"));
                        }
                        oauth_api::OAuthFileImportOutcome::Updated {
                            upstream_id,
                            name,
                            refreshable,
                        } => {
                            ui.label(import_result_label("已更新", name, *refreshable))
                                .on_hover_text(format!("upstream: {upstream_id}"));
                        }
                        oauth_api::OAuthFileImportOutcome::Failed { message } => {
                            ui.colored_label(egui::Color32::RED, format!("失败: {message}"));
                        }
                    }
                });
            }
        }
        self.oauth_quota_ui(ui);
    }

    pub(super) fn drive_oauth_tasks(&mut self) {
        let now = Instant::now();
        for task in &mut self.oauth_ui.tasks {
            if matches!(task.state, OAuthLoginTaskState::Waiting)
                && task.expires_at.is_some_and(|expires_at| now >= expires_at)
            {
                task.state = OAuthLoginTaskState::Expired;
                task.next_poll_at = None;
            }
        }
        let due = self
            .oauth_ui
            .tasks
            .iter()
            .filter(|task| {
                matches!(task.state, OAuthLoginTaskState::Waiting)
                    && task.next_poll_at.is_some_and(|next_poll_at| now >= next_poll_at)
            })
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        for id in due {
            self.poll_oauth_task(&id);
        }
    }

    pub(super) fn handle_oauth_started(
        &mut self,
        task_id: String,
        result: anyhow::Result<oauth_api::DeviceFlow>,
    ) {
        let Some(task) = self.oauth_ui.tasks.iter_mut().find(|task| task.id == task_id) else {
            return;
        };
        match result {
            Ok(flow) => {
                let now = Instant::now();
                task.expires_at = Some(now + Duration::from_secs(flow.expires_in));
                task.next_poll_at = Some(now + Duration::from_secs(flow.interval.max(1)));
                task.flow = Some(flow);
                task.warning = None;
                task.state = OAuthLoginTaskState::Waiting;
                self.status = "OAuth 登录任务已创建".to_string();
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to start codex oauth login task");
                task.state = OAuthLoginTaskState::Failed(err.to_string());
                self.status = format!("OAuth 启动失败: {err}");
            }
        }
    }

    pub(super) fn handle_oauth_polled(
        &mut self,
        task_id: String,
        result: anyhow::Result<OAuthPollTaskResult>,
    ) {
        let Some(task) = self.oauth_ui.tasks.iter_mut().find(|task| task.id == task_id) else {
            return;
        };
        let interval = task.flow.as_ref().map_or(5, |flow| flow.interval.max(1));
        match result {
            Ok(OAuthPollTaskResult::Pending) => {
                task.state = OAuthLoginTaskState::Waiting;
                task.warning = None;
                task.next_poll_at = Some(Instant::now() + Duration::from_secs(interval));
                self.status = "等待 OAuth 授权".to_string();
            }
            Ok(OAuthPollTaskResult::RetryableError(message)) => {
                tracing::debug!(error = %message, "codex oauth login task will retry");
                task.state = OAuthLoginTaskState::Waiting;
                task.warning = Some(message);
                task.next_poll_at = Some(Instant::now() + Duration::from_secs(interval));
                self.status = "OAuth 轮询暂时失败, 将自动重试".to_string();
            }
            Ok(OAuthPollTaskResult::Expired) => {
                tracing::info!("codex oauth login task expired");
                task.state = OAuthLoginTaskState::Expired;
                task.next_poll_at = None;
                self.status = "OAuth 用户码已过期".to_string();
            }
            Ok(OAuthPollTaskResult::Stored(saved)) => {
                let action = match saved.outcome {
                    oauth_api::OAuthAccountStoreOutcome::Created => "已新增",
                    oauth_api::OAuthAccountStoreOutcome::Updated => "已更新",
                };
                self.status = format!("OAuth 账号{action}: {}", saved.upstream.name);
                task.state = OAuthLoginTaskState::Succeeded {
                    outcome: saved.outcome,
                    upstream_name: saved.upstream.name,
                    refreshable: saved.refreshable,
                };
                task.warning = None;
                task.next_poll_at = None;
                self.refresh_all_if_visible();
            }
            Err(err) => {
                tracing::warn!(error = %err, "codex oauth login task failed");
                task.state = OAuthLoginTaskState::Failed(err.to_string());
                task.next_poll_at = None;
                self.status = format!("OAuth 轮询失败: {err}");
            }
        }
    }

    pub(super) fn handle_oauth_import_progress(
        &mut self,
        batch_id: String,
        progress: oauth_api::OAuthImportProgress,
    ) {
        let Some(batch) = self
            .oauth_ui
            .import_batch
            .as_mut()
            .filter(|batch| batch.id == batch_id)
        else {
            return;
        };
        batch.processed = progress.processed;
        batch.total = progress.total;
        batch.items.push(progress.item);
        self.status = format!("正在导入 OAuth 账号: {}/{}", batch.processed, batch.total);
    }

    pub(super) fn handle_oauth_import_finished(
        &mut self,
        batch_id: String,
        result: oauth_api::OAuthImportBatchResult,
    ) {
        let Some(batch) = self
            .oauth_ui
            .import_batch
            .as_mut()
            .filter(|batch| batch.id == batch_id)
        else {
            return;
        };
        self.status = format!(
            "OAuth 导入完成: 新增 {}, 更新 {}, 失败 {}",
            result.created, result.updated, result.failed
        );
        let changed = result.created + result.updated > 0;
        batch.result = Some(result);
        if changed {
            self.refresh_all_if_visible();
        }
    }

    fn start_oauth_task(&mut self) {
        let task_id = uuid::Uuid::new_v4().to_string();
        self.oauth_ui.tasks.push(OAuthLoginTask {
            id: task_id.clone(),
            state: OAuthLoginTaskState::Starting,
            flow: None,
            expires_at: None,
            next_poll_at: None,
            warning: None,
        });
        tracing::info!(
            active_task_count = self.oauth_ui.tasks.len(),
            "created codex oauth login task"
        );
        self.spawn_oauth_start(task_id);
    }

    fn restart_oauth_task(&mut self, task_id: &str) {
        let Some(task) = self.oauth_ui.tasks.iter_mut().find(|task| task.id == task_id) else {
            return;
        };
        task.state = OAuthLoginTaskState::Starting;
        task.flow = None;
        task.expires_at = None;
        task.next_poll_at = None;
        task.warning = None;
        self.spawn_oauth_start(task_id.to_string());
    }

    fn spawn_oauth_start(&self, task_id: String) {
        let http = self.state.http.clone();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let result = oauth_api::start_device_flow(&http).await;
            let _ = tx.send(UiTaskEvent::OAuthStarted { task_id, result });
        });
    }

    fn poll_oauth_task(&mut self, task_id: &str) {
        let Some(task) = self.oauth_ui.tasks.iter_mut().find(|task| task.id == task_id) else {
            return;
        };
        if !matches!(task.state, OAuthLoginTaskState::Waiting) {
            return;
        }
        let Some(flow) = task.flow.clone() else {
            task.state = OAuthLoginTaskState::Failed("missing device flow".to_string());
            return;
        };
        task.state = OAuthLoginTaskState::Polling;
        task.next_poll_at = None;
        let http = self.state.http.clone();
        let accounts = self.state.oauth_accounts.clone();
        let tx = self.task_tx.clone();
        let task_id = task_id.to_string();
        self.runtime.spawn(async move {
            let result = match oauth_api::poll_device_flow(&http, &flow).await {
                Ok(oauth_api::DevicePollOutcome::Pending) => Ok(OAuthPollTaskResult::Pending),
                Ok(oauth_api::DevicePollOutcome::RetryableError(message)) => {
                    Ok(OAuthPollTaskResult::RetryableError(message))
                }
                Ok(oauth_api::DevicePollOutcome::Expired) => Ok(OAuthPollTaskResult::Expired),
                Ok(oauth_api::DevicePollOutcome::Authorized(tokens)) => {
                    match oauth_api::OAuthAccountInput::from_token_response(tokens) {
                        Ok(input) => accounts
                            .store_tokens(input)
                            .await
                            .map(|saved| OAuthPollTaskResult::Stored(Box::new(saved))),
                        Err(err) => Err(err),
                    }
                }
                Err(err) => Err(err),
            };
            let _ = tx.send(UiTaskEvent::OAuthPolled { task_id, result });
        });
    }

    fn select_oauth_auth_files(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .set_title("选择 Codex auth.json")
            .add_filter("JSON", &["json"])
            .pick_files()
        else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        let batch_id = uuid::Uuid::new_v4().to_string();
        self.oauth_ui.import_batch = Some(OAuthImportBatchUi {
            id: batch_id.clone(),
            processed: 0,
            total: paths.len(),
            items: Vec::new(),
            result: None,
        });
        self.status = format!("准备导入 {} 个 OAuth 凭据文件", paths.len());
        let accounts = self.state.oauth_accounts.clone();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let progress_tx = tx.clone();
            let progress_batch_id = batch_id.clone();
            let result = oauth_api::import_auth_files(&accounts, paths, move |progress| {
                let _ = progress_tx.send(UiTaskEvent::OAuthImportProgress {
                    batch_id: progress_batch_id.clone(),
                    progress,
                });
            })
            .await;
            let _ = tx.send(UiTaskEvent::OAuthImportFinished { batch_id, result });
        });
    }
}

fn import_result_label(action: &str, name: &str, refreshable: bool) -> String {
    if refreshable {
        format!("{action}: {name}")
    } else {
        format!("{action}: {name}, 仅 access token")
    }
}
