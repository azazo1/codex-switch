//! 自动更新: 检查 GitHub Release, 下载校验并安装新版本.
//!
//! 入口 [`UpdateRuntime`] 持有共享状态机, UI 通过它发起手动检查, 下载安装与重启;
//! 应用启动时 [`UpdateRuntime::start`] 派发一次静默后台检查, 发现新版本走系统通知;
//! 启动延迟窗口内用户已手动发起检查时, 自动检查让位, 不再重复触发.

mod download;
mod install;
mod release;

use crate::app::{http, AppEvents, AppState, data_dir};
use crate::storage::Store;
use anyhow::Context;
use release::ReleaseInfo;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const AUTO_CHECK_DELAY: Duration = Duration::from_secs(5);
const AUTO_CHECK_SETTING: &str = "update.auto_check";
const SKIPPED_VERSION_SETTING: &str = "update.skipped_version";

/// 更新流程的共享状态, UI 每帧读取快照进行渲染.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Downloading { received: u64, total: Option<u64> },
    ReadyToRestart,
    DmgOpened,
    Failed(String),
}

#[derive(Clone)]
pub(crate) struct UpdateRuntime {
    state: Arc<Mutex<UpdateState>>,
    events: AppEvents,
    store: Store,
    auto_check: Arc<AtomicBool>,
    /// 启动自动检查延迟 5 秒触发, 期间用户可能已手动发起检查, 置位后自动检查让位.
    manual_checked: Arc<AtomicBool>,
    /// UI 线程没有 tokio 上下文, 后台任务必须经此 handle 派发.
    runtime: tokio::runtime::Handle,
}

impl UpdateRuntime {
    pub(crate) async fn new(store: Store, events: AppEvents) -> Self {
        install::cleanup_stale_backup();
        let auto_check_enabled = match store.get_setting(AUTO_CHECK_SETTING).await {
            Ok(value) => value.as_deref() != Some("false"),
            Err(err) => {
                tracing::warn!(error = %err, "failed to load update auto check setting");
                true
            }
        };
        Self {
            state: Arc::new(Mutex::new(UpdateState::Idle)),
            events,
            store,
            auto_check: Arc::new(AtomicBool::new(auto_check_enabled)),
            manual_checked: Arc::new(AtomicBool::new(false)),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// 仅测试使用; 生产路径统一走 async new.
    #[cfg(test)]
    pub(crate) fn new_for_tests(store: Store, events: AppEvents) -> Self {
        Self {
            state: Arc::new(Mutex::new(UpdateState::Idle)),
            events,
            store,
            auto_check: Arc::new(AtomicBool::new(true)),
            manual_checked: Arc::new(AtomicBool::new(false)),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// 派发启动后的静默检查; 网络失败只记日志, 不打扰用户.
    /// 延迟窗口内用户已手动发起检查时直接跳过, 避免两路检查争抢状态机.
    pub(crate) fn start(self) {
        self.runtime.clone().spawn(async move {
            tokio::time::sleep(AUTO_CHECK_DELAY).await;
            if !self.auto_check_value() {
                return;
            }
            if self.manual_checked.load(Ordering::Relaxed) {
                tracing::debug!("skip startup update check: manual check already triggered");
                return;
            }
            match self.check().await {
                Ok(Some(tag)) => {
                    if self.skipped_version().await.as_deref() != Some(tag.as_str())
                        && let Err(err) = crate::notification::send(
                            "发现新版本".to_string(),
                            format!("Codex Switch {tag} 已发布, 请在主界面查看详情."),
                        )
                        .await
                    {
                        tracing::debug!(error = %err, "failed to send update notification");
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::debug!(error = %err, "background update check failed");
                }
            }
        });
    }

    pub(crate) fn state(&self) -> UpdateState {
        self.state.lock().map(|state| state.clone()).unwrap_or(UpdateState::Idle)
    }

    pub(crate) fn auto_check_value(&self) -> bool {
        self.auto_check.load(Ordering::Relaxed)
    }

    /// 发起一次手动检查, 结果写入状态机; 同步置位标记, 使尚未触发的启动自动检查让位.
    pub(crate) fn check_now(&self) {
        self.manual_checked.store(true, Ordering::Relaxed);
        let this = self.clone();
        self.runtime.spawn(async move {
            if let Err(err) = this.check().await {
                tracing::warn!(error = %err, "manual update check failed");
            }
        });
    }

    /// 下载并安装 Available 状态对应的版本.
    pub(crate) fn install_now(&self) {
        let UpdateState::Available(info) = self.state() else {
            return;
        };
        let this = self.clone();
        self.runtime.spawn(async move {
            this.set_state(UpdateState::Downloading {
                received: 0,
                total: None,
            });
            match this.download_and_apply(&info).await {
                Ok(install::InstallOutcome::Replaced) => {
                    this.set_state(UpdateState::ReadyToRestart);
                }
                Ok(install::InstallOutcome::DmgOpened) => {
                    this.set_state(UpdateState::DmgOpened);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "update install failed");
                    this.set_state(UpdateState::Failed(format!("{err:#}")));
                }
            }
        });
    }

    /// 启动已替换的新二进制; 退出当前进程由调用方完成.
    pub(crate) fn restart(&self) -> anyhow::Result<()> {
        install::spawn_restart()
    }

    pub(crate) async fn set_auto_check(&self, enabled: bool) {
        self.auto_check.store(enabled, Ordering::Relaxed);
        let value = if enabled { "true" } else { "false" };
        if let Err(err) = self.store.set_setting(AUTO_CHECK_SETTING, value).await {
            tracing::warn!(error = %err, "failed to persist update auto check setting");
        }
    }

    pub(crate) async fn skip_version(&self, tag: &str) {
        if let Err(err) = self.store.set_setting(SKIPPED_VERSION_SETTING, tag).await {
            tracing::warn!(error = %err, "failed to persist skipped version");
        }
    }

    async fn skipped_version(&self) -> Option<String> {
        self.store.get_setting(SKIPPED_VERSION_SETTING).await.ok().flatten()
    }

    /// 执行一次检查, 返回 `Some(tag)` 表示有比当前构建更新的稳定版本.
    async fn check(&self) -> anyhow::Result<Option<String>> {
        self.set_state(UpdateState::Checking);
        let client = http::build_client(None)?;
        let info = match release::fetch_latest(&client).await {
            Ok(info) => info,
            Err(err) => {
                self.set_state(UpdateState::Failed(format!("{err:#}")));
                return Err(err);
            }
        };
        if release::is_newer_version(&info.tag).is_none() {
            self.set_state(UpdateState::UpToDate);
            return Ok(None);
        }
        self.set_state(UpdateState::Available(info.clone()));
        Ok(Some(info.tag))
    }

    async fn download_and_apply(&self, info: &ReleaseInfo) -> anyhow::Result<install::InstallOutcome> {
        let dest_dir = update_dir()?;
        let client = http::build_client(None)?;
        let expected = download::fetch_checksum_for(
            &client,
            &info.checksums,
            &info.archive.name,
        )
        .await?;
        let runtime = self.clone();
        let archive = download::download_archive(
            &client,
            &info.archive,
            &dest_dir,
            &expected,
            move |received, total| {
                runtime.set_state(UpdateState::Downloading { received, total });
            },
        )
        .await?;
        let outcome = tokio::task::spawn_blocking(move || install::apply(&archive))
            .await
            .context("install task failed")??;
        Ok(outcome)
    }

    fn set_state(&self, state: UpdateState) {
        if let Ok(mut guard) = self.state.lock() {
            *guard = state;
        }
        self.events.bump_update();
    }
}

fn update_dir() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("update"))
}

/// 对齐 balance_alert 的启动模式, 在 AppState 就绪后派发自动检查.
pub(crate) fn start(state: &AppState) {
    state.update.clone().start();
}
