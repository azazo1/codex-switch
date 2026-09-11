//! 上游活跃度追踪.
//!
//! 记录每个上游在途的模型调用数量, 以及最近一次调用的结束时间.
//! 后台任务 (如余额刷新) 据此区分上游正处于调用中和空闲两种状态.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
pub struct UpstreamActivity {
    inner: Arc<Mutex<HashMap<String, UpstreamUsage>>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct UpstreamUsage {
    inflight: usize,
    last_active_at: Option<Instant>,
}

impl UpstreamActivity {
    /// 登记一次模型调用开始; 返回的 guard 被释放时登记调用结束.
    pub fn begin(&self, upstream_id: &str) -> ActivityGuard {
        if let Ok(mut inner) = self.inner.lock() {
            let usage = inner.entry(upstream_id.to_string()).or_default();
            usage.inflight += 1;
        }
        ActivityGuard {
            activity: self.clone(),
            upstream_id: upstream_id.to_string(),
        }
    }

    /// 上游是否正在调用, 或最近一次调用结束时间在 window 之内.
    pub fn is_active(&self, upstream_id: &str, window: Duration) -> bool {
        let Ok(inner) = self.inner.lock() else {
            return false;
        };
        let Some(usage) = inner.get(upstream_id) else {
            return false;
        };
        if usage.inflight > 0 {
            return true;
        }
        usage
            .last_active_at
            .is_some_and(|last| last.elapsed() <= window)
    }

    fn finish(&self, upstream_id: &str) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let Some(usage) = inner.get_mut(upstream_id) else {
            return;
        };
        usage.inflight = usage.inflight.saturating_sub(1);
        usage.last_active_at = Some(Instant::now());
    }
}

/// 一次在途模型调用的活跃标记.
pub struct ActivityGuard {
    activity: UpstreamActivity,
    upstream_id: String,
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.activity.finish(&self.upstream_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflight_call_marks_upstream_active() {
        let activity = UpstreamActivity::default();
        let guard = activity.begin("upstream-a");

        assert!(activity.is_active("upstream-a", Duration::ZERO));
        assert!(!activity.is_active("upstream-b", Duration::from_secs(60)));

        drop(guard);
    }

    #[test]
    fn finished_call_keeps_upstream_active_within_window() {
        let activity = UpstreamActivity::default();
        drop(activity.begin("upstream-a"));

        assert!(activity.is_active("upstream-a", Duration::from_secs(60)));
        assert!(!activity.is_active("upstream-a", Duration::ZERO));
    }

    #[test]
    fn concurrent_calls_keep_upstream_active_until_all_finish() {
        let activity = UpstreamActivity::default();
        let first = activity.begin("upstream-a");
        let second = activity.begin("upstream-a");

        drop(first);
        assert!(activity.is_active("upstream-a", Duration::ZERO));

        drop(second);
        assert!(!activity.is_active("upstream-a", Duration::ZERO));
    }
}
