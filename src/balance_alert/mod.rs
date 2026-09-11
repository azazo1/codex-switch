use crate::app::AppState;
use crate::core::models::{BalanceSnapshot, UpstreamBalanceAlertSettings, UpstreamKind};
use std::time::Duration;

const SCAN_INTERVAL: Duration = Duration::from_secs(5);

pub fn start(state: AppState) {
    tokio::spawn(async move {
        run(state).await;
    });
}

async fn run(state: AppState) {
    let mut ticker = tokio::time::interval(SCAN_INTERVAL);
    loop {
        ticker.tick().await;
        if let Err(err) = scan_once(&state).await {
            tracing::warn!(error = %err, "balance refresh scan failed");
        }
    }
}

async fn scan_once(state: &AppState) -> anyhow::Result<()> {
    let settings = state.store.list_enabled_balance_alert_settings().await?;
    let now = chrono::Utc::now().timestamp();
    for setting in settings {
        let active = is_upstream_active(state, &setting);
        if !is_due(&setting, now, active) {
            continue;
        }
        let Some(upstream) = state.store.get_upstream(&setting.upstream_id).await? else {
            continue;
        };
        if !upstream.enabled || upstream.kind != UpstreamKind::RelayApiKey {
            continue;
        }
        tracing::info!(
            upstream_id = %upstream.id,
            upstream_name = %upstream.name,
            active,
            interval_seconds = refresh_interval_seconds(&setting, active),
            alert_enabled = setting.alert_enabled,
            "refreshing upstream balance"
        );
        match crate::balance::query_and_store(state, &upstream.id).await {
            Ok(snapshot) => {
                let alert_active = if setting.alert_enabled {
                    evaluate_alert(&upstream.id, &upstream.name, &setting, &snapshot).await
                } else {
                    false
                };
                state
                    .store
                    .mark_balance_alert_checked(&upstream.id, now, alert_active)
                    .await?;
                state.events.bump_balance_snapshots();
            }
            Err(err) => {
                tracing::warn!(
                    upstream_id = %upstream.id,
                    upstream_name = %upstream.name,
                    error = %err,
                    "failed to query upstream balance"
                );
                state
                    .store
                    .mark_balance_alert_checked(
                        &upstream.id,
                        now,
                        setting.alert_enabled && setting.alert_active,
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

async fn evaluate_alert(
    upstream_id: &str,
    upstream_name: &str,
    setting: &UpstreamBalanceAlertSettings,
    snapshot: &BalanceSnapshot,
) -> bool {
    let low = is_balance_low(snapshot, setting.threshold);
    if low && !setting.alert_active {
        let amount = format_balance(snapshot);
        let body = format!(
            "上游 {} 当前余额为 {}, 已低于提醒阈值 {:.4}",
            upstream_name, amount, setting.threshold
        );
        let notified = match crate::notification::send("上游余额不足".to_string(), body).await
        {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(
                    upstream_id = %upstream_id,
                    error = %err,
                    "failed to send balance alert notification"
                );
                false
            }
        };
        tracing::warn!(
            upstream_id = %upstream_id,
            upstream_name = %upstream_name,
            remaining = snapshot.remaining,
            threshold = setting.threshold,
            "upstream balance is below alert threshold"
        );
        notified
    } else if !low && setting.alert_active {
        tracing::info!(
            upstream_id = %upstream_id,
            upstream_name = %upstream_name,
            remaining = snapshot.remaining,
            "upstream balance alert recovered"
        );
        false
    } else {
        low
    }
}

fn is_due(settings: &UpstreamBalanceAlertSettings, now: i64, active: bool) -> bool {
    let interval = refresh_interval_seconds(settings, active);
    settings
        .last_checked_at
        .is_none_or(|last| now.saturating_sub(last) >= interval)
}

/// 上游正在调用或刚调用过时返回 true, 此时使用活跃刷新间隔.
fn is_upstream_active(state: &AppState, settings: &UpstreamBalanceAlertSettings) -> bool {
    let window = Duration::from_secs(settings.active_window_seconds.max(1) as u64);
    state
        .activity
        .is_active(&settings.upstream_id, window)
}

fn refresh_interval_seconds(settings: &UpstreamBalanceAlertSettings, active: bool) -> i64 {
    let interval = if active {
        settings.active_interval_seconds
    } else {
        settings.interval_seconds
    };
    interval.max(1)
}

fn is_balance_low(snapshot: &BalanceSnapshot, threshold: f64) -> bool {
    snapshot
        .remaining
        .is_some_and(|remaining| remaining <= threshold.max(0.0))
}

fn format_balance(snapshot: &BalanceSnapshot) -> String {
    let amount = snapshot
        .remaining
        .map(|value| format!("{value:.4}"))
        .unwrap_or_else(|| "未知".to_string());
    match snapshot.unit.as_deref() {
        Some(unit) if !unit.is_empty() => format!("{amount} {unit}"),
        _ => amount,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_balance_uses_remaining_even_when_provider_marks_snapshot_invalid() {
        let snapshot = BalanceSnapshot {
            remaining: Some(0.0),
            is_valid: false,
            ..BalanceSnapshot::default()
        };
        assert!(is_balance_low(&snapshot, 5.0));
    }

    #[test]
    fn alert_interval_controls_due_checks() {
        let mut settings = UpstreamBalanceAlertSettings::new("upstream".to_string());
        settings.interval_seconds = 600;
        settings.last_checked_at = Some(1000);
        assert!(!is_due(&settings, 1599, false));
        assert!(is_due(&settings, 1600, false));
    }

    #[test]
    fn active_upstream_uses_the_active_interval() {
        let mut settings = UpstreamBalanceAlertSettings::new("upstream".to_string());
        settings.interval_seconds = 1800;
        settings.active_interval_seconds = 120;
        settings.last_checked_at = Some(1000);

        assert!(!is_due(&settings, 1119, true));
        assert!(is_due(&settings, 1120, true));
        assert!(!is_due(&settings, 1120, false));
        assert!(is_due(&settings, 2800, false));
    }

    #[test]
    fn refresh_interval_defaults_to_the_upstream_setting() {
        let mut settings = UpstreamBalanceAlertSettings::new("upstream".to_string());
        settings.interval_seconds = 900;
        settings.active_interval_seconds = 5;

        assert_eq!(refresh_interval_seconds(&settings, false), 900);
        assert_eq!(refresh_interval_seconds(&settings, true), 5);
    }

    #[test]
    fn non_positive_refresh_interval_is_clamped_to_one_second() {
        let mut settings = UpstreamBalanceAlertSettings::new("upstream".to_string());
        settings.interval_seconds = 0;
        settings.active_interval_seconds = -10;

        assert_eq!(refresh_interval_seconds(&settings, false), 1);
        assert_eq!(refresh_interval_seconds(&settings, true), 1);
    }
}
