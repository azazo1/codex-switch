//! 上游并发限制.
//!
//! 按上游维护并发信号量, 限制同时转发的请求数. 达到上限后按上游配置的
//! 溢出策略处理: `Reject` 直接向客户端返回 429, `Hold` 挂起等待空位.
//! 并发计数的生命周期从向上游发起请求开始, 到响应完全结束 (含流式输出) 为止.

use crate::core::models::{ConcurrencyOverflowPolicy, Upstream};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

/// 单个上游的并发限制条目.
struct Entry {
    limit: u32,
    semaphore: Arc<Semaphore>,
}

#[derive(Clone, Default)]
pub struct UpstreamConcurrency {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

/// 一次上游请求占用的并发许可; Drop 时自动释放.
pub enum UpstreamPermit {
    /// 上游未配置并发限制, 无需占用.
    Unlimited,
    /// 占用的并发许可.
    Held(OwnedSemaphorePermit),
}

/// reject 策略下并发已满的错误, 转发层据此向客户端返回 429.
#[derive(Debug, thiserror::Error)]
#[error("upstream concurrency limit reached: {upstream_name}")]
pub struct ConcurrencyRejected {
    pub upstream_name: String,
}

impl UpstreamConcurrency {
    /// 获取一次上游请求的并发许可.
    ///
    /// `limit <= 0` 视为不限制. reject 策略下并发已满立即返回
    /// [`ConcurrencyRejected`]; hold 策略下挂起等待, 期间请求被用户终止则返回错误.
    pub async fn acquire(
        &self,
        upstream: &Upstream,
        terminate_rx: &mut watch::Receiver<bool>,
    ) -> anyhow::Result<UpstreamPermit> {
        let limit = upstream.concurrency_limit;
        if limit <= 0 {
            return Ok(UpstreamPermit::Unlimited);
        }
        let Ok(limit) = u32::try_from(limit) else {
            return Ok(UpstreamPermit::Unlimited);
        };
        let semaphore = self.semaphore_for(&upstream.id, limit);
        match upstream.concurrency_overflow {
            ConcurrencyOverflowPolicy::Reject => semaphore
                .try_acquire_owned()
                .map(UpstreamPermit::Held)
                .map_err(|_| ConcurrencyRejected {
                    upstream_name: upstream.name.clone(),
                })
                .map_err(anyhow::Error::new),
            ConcurrencyOverflowPolicy::Hold => {
                let mut terminated = false;
                loop {
                    tokio::select! {
                        permit = Semaphore::acquire_owned(semaphore.clone()) => {
                            return permit
                                .map(UpstreamPermit::Held)
                                .map_err(anyhow::Error::new);
                        }
                        changed = terminate_rx.changed(), if !terminated => {
                            match changed {
                                Ok(()) if *terminate_rx.borrow() => {
                                    anyhow::bail!("terminated by user");
                                }
                                Ok(()) => continue,
                                Err(_) => {
                                    terminated = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// 取出上游当前配置对应的信号量; 上游不存在或 limit 变化时重建.
    ///
    /// 重建后已在旧信号量上等待的请求仍持有旧 Arc, 会按旧 limit 等到
    /// 在途请求释放的许可后放行, 属于配置变更后的短暂过渡.
    fn semaphore_for(&self, upstream_id: &str, limit: u32) -> Arc<Semaphore> {
        let Ok(mut inner) = self.inner.lock() else {
            return Arc::new(Semaphore::new(limit as usize));
        };
        match inner.get(upstream_id) {
            Some(entry) if entry.limit == limit => entry.semaphore.clone(),
            _ => {
                let semaphore = Arc::new(Semaphore::new(limit as usize));
                inner.insert(
                    upstream_id.to_string(),
                    Entry {
                        limit,
                        semaphore: semaphore.clone(),
                    },
                );
                semaphore
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{ConcurrencyOverflowPolicy, Upstream};

    fn relay_upstream(limit: i64, overflow: ConcurrencyOverflowPolicy) -> Upstream {
        let mut upstream = Upstream::new_relay(
            "relay".to_string(),
            "https://example.com/v1".to_string(),
            crate::core::models::WireApi::Responses,
            false,
            crate::core::models::BalanceProvider::Unsupported,
        );
        upstream.concurrency_limit = limit;
        upstream.concurrency_overflow = overflow;
        upstream
    }

    fn terminate_channel() -> watch::Receiver<bool> {
        watch::channel(false).1
    }

    #[tokio::test]
    async fn zero_limit_is_unlimited() {
        let concurrency = UpstreamConcurrency::default();
        let upstream = relay_upstream(0, ConcurrencyOverflowPolicy::Reject);
        let mut terminate_rx = terminate_channel();

        let permit = concurrency
            .acquire(&upstream, &mut terminate_rx)
            .await
            .unwrap();
        assert!(matches!(permit, UpstreamPermit::Unlimited));
    }

    #[tokio::test]
    async fn reject_fails_when_slots_exhausted() {
        let concurrency = UpstreamConcurrency::default();
        let upstream = relay_upstream(1, ConcurrencyOverflowPolicy::Reject);
        let mut terminate_rx = terminate_channel();

        let permit = concurrency
            .acquire(&upstream, &mut terminate_rx)
            .await
            .unwrap();
        assert!(matches!(permit, UpstreamPermit::Held(_)));
        let result = concurrency.acquire(&upstream, &mut terminate_rx).await;
        assert!(
            result
                .err()
                .is_some_and(|err| err.downcast_ref::<ConcurrencyRejected>().is_some())
        );
    }

    #[tokio::test]
    async fn hold_waits_until_permit_released() {
        let concurrency = UpstreamConcurrency::default();
        let upstream = relay_upstream(1, ConcurrencyOverflowPolicy::Hold);
        let mut terminate_rx = terminate_channel();

        let permit = concurrency
            .acquire(&upstream, &mut terminate_rx)
            .await
            .unwrap();
        let wait_task = tokio::spawn({
            let concurrency = concurrency.clone();
            let upstream = upstream.clone();
            async move {
                let mut terminate_rx = terminate_channel();
                concurrency
                    .acquire(&upstream, &mut terminate_rx)
                    .await
                    .unwrap()
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(!wait_task.is_finished());
        drop(permit);
        let _second = wait_task.await.unwrap();
    }

    #[tokio::test]
    async fn hold_aborts_on_termination() {
        let concurrency = UpstreamConcurrency::default();
        let upstream = relay_upstream(1, ConcurrencyOverflowPolicy::Hold);

        let permit = {
            let concurrency = concurrency.clone();
            let upstream = upstream.clone();
            let mut terminate_rx = terminate_channel();
            concurrency
                .acquire(&upstream, &mut terminate_rx)
                .await
                .unwrap()
        };
        let wait_task = tokio::spawn(async move {
            let (terminate_tx, mut terminate_rx) = watch::channel(false);
            let _ = terminate_tx.send(true);
            let result = concurrency.acquire(&upstream, &mut terminate_rx).await;
            assert!(result.is_err());
        });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        wait_task.await.unwrap();
        drop(permit);
    }

    #[tokio::test]
    async fn limit_change_takes_effect_for_new_requests() {
        let concurrency = UpstreamConcurrency::default();
        let mut upstream = relay_upstream(2, ConcurrencyOverflowPolicy::Reject);
        let mut terminate_rx = terminate_channel();

        let first = concurrency
            .acquire(&upstream, &mut terminate_rx)
            .await
            .unwrap();
        assert!(matches!(first, UpstreamPermit::Held(_)));
        let second = concurrency
            .acquire(&upstream, &mut terminate_rx)
            .await
            .unwrap();
        assert!(matches!(second, UpstreamPermit::Held(_)));
        // 旧信号量已满; 调小 limit 后新请求改用重建的信号量, 并被新 limit 约束.
        // 已在途的许可随其请求结束在旧信号量上释放, 存在短暂的过渡窗口.
        upstream.concurrency_limit = 1;
        let third = concurrency
            .acquire(&upstream, &mut terminate_rx)
            .await
            .unwrap();
        assert!(matches!(third, UpstreamPermit::Held(_)));
        let result = concurrency.acquire(&upstream, &mut terminate_rx).await;
        assert!(
            result
                .err()
                .is_some_and(|err| err.downcast_ref::<ConcurrencyRejected>().is_some())
        );
    }
}
