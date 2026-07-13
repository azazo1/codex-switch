use crate::oauth as oauth_api;
use std::path::Path;
use std::time::Instant;

#[derive(Default)]
pub(crate) struct OAuthUiState {
    pub(super) tasks: Vec<OAuthLoginTask>,
    pub(super) import_batch: Option<OAuthImportBatchUi>,
}

pub(super) struct OAuthLoginTask {
    pub(super) id: String,
    pub(super) state: OAuthLoginTaskState,
    pub(super) flow: Option<oauth_api::DeviceFlow>,
    pub(super) expires_at: Option<Instant>,
    pub(super) next_poll_at: Option<Instant>,
    pub(super) warning: Option<String>,
}

pub(super) enum OAuthLoginTaskState {
    Starting,
    Waiting,
    Polling,
    Succeeded {
        outcome: oauth_api::OAuthAccountStoreOutcome,
        upstream_name: String,
    },
    Failed(String),
    Expired,
}

impl OAuthLoginTaskState {
    pub(super) fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded { .. } | Self::Failed(_) | Self::Expired)
    }
}

pub(super) struct OAuthImportBatchUi {
    pub(super) id: String,
    pub(super) processed: usize,
    pub(super) total: usize,
    pub(super) items: Vec<oauth_api::OAuthFileImportItem>,
    pub(super) result: Option<oauth_api::OAuthImportBatchResult>,
}

pub(crate) enum OAuthPollTaskResult {
    Pending,
    RetryableError(String),
    Expired,
    Stored(Box<oauth_api::OAuthAccountStoreResult>),
}

pub(super) fn oauth_task_title(task: &OAuthLoginTask) -> String {
    task.flow
        .as_ref()
        .map(|flow| format!("登录任务 {}", flow.user_code))
        .unwrap_or_else(|| format!("登录任务 {}", &task.id[..8]))
}

pub(super) fn oauth_task_status(state: &OAuthLoginTaskState) -> String {
    match state {
        OAuthLoginTaskState::Starting => "正在创建".to_string(),
        OAuthLoginTaskState::Waiting => "等待授权".to_string(),
        OAuthLoginTaskState::Polling => "正在检查".to_string(),
        OAuthLoginTaskState::Succeeded {
            outcome,
            upstream_name,
        } => {
            let action = match outcome {
                oauth_api::OAuthAccountStoreOutcome::Created => "已新增",
                oauth_api::OAuthAccountStoreOutcome::Updated => "已更新",
            };
            format!("{action}: {upstream_name}")
        }
        OAuthLoginTaskState::Failed(message) => format!("失败: {message}"),
        OAuthLoginTaskState::Expired => "已过期".to_string(),
    }
}

pub(super) fn import_source_label(path: &Path) -> String {
    let file = path
        .file_name()
        .map(|value| value.to_string_lossy())
        .unwrap_or_default();
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .map(|value| value.to_string_lossy());
    match parent {
        Some(parent) => format!("{parent}/{file}"),
        None => file.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn terminal_states_are_clearable() {
        assert!(OAuthLoginTaskState::Expired.is_terminal());
        assert!(OAuthLoginTaskState::Failed("failed".to_string()).is_terminal());
        assert!(!OAuthLoginTaskState::Waiting.is_terminal());
        assert!(!OAuthLoginTaskState::Polling.is_terminal());
    }

    #[test]
    fn independent_tasks_keep_independent_poll_deadlines() {
        let now = Instant::now();
        let first = OAuthLoginTask {
            id: "first-task".to_string(),
            state: OAuthLoginTaskState::Waiting,
            flow: None,
            expires_at: Some(now + Duration::from_secs(60)),
            next_poll_at: Some(now),
            warning: None,
        };
        let second = OAuthLoginTask {
            id: "second-task".to_string(),
            state: OAuthLoginTaskState::Waiting,
            flow: None,
            expires_at: Some(now + Duration::from_secs(60)),
            next_poll_at: Some(now + Duration::from_secs(10)),
            warning: None,
        };
        assert!(first.next_poll_at.is_some_and(|deadline| deadline <= now));
        assert!(second.next_poll_at.is_some_and(|deadline| deadline > now));
    }
}
