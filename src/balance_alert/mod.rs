use crate::app::AppState;
use crate::core::models::{BalanceSnapshot, UpstreamBalanceAlertSettings, UpstreamKind};
use std::time::Duration;

const SCAN_INTERVAL: Duration = Duration::from_secs(30);

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
            tracing::warn!(error = %err, "balance alert scan failed");
        }
    }
}

async fn scan_once(state: &AppState) -> anyhow::Result<()> {
    let settings = state.store.list_enabled_balance_alert_settings().await?;
    let now = chrono::Utc::now().timestamp();
    for setting in settings {
        if !is_due(&setting, now) {
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
            threshold = setting.threshold,
            "checking upstream balance alert"
        );
        match crate::balance::query_and_store(state, &upstream.id).await {
            Ok(snapshot) => {
                let low = is_balance_low(&snapshot, setting.threshold);
                let alert_active = if low && !setting.alert_active {
                    let amount = format_balance(&snapshot);
                    let body = format!(
                        "上游 {} 当前余额为 {}, 已低于提醒阈值 {:.4}",
                        upstream.name, amount, setting.threshold
                    );
                    let notified = match crate::notification::send(
                        "上游余额不足".to_string(),
                        body,
                    )
                    .await
                    {
                        Ok(()) => true,
                        Err(err) => {
                            tracing::warn!(
                                upstream_id = %upstream.id,
                                error = %err,
                                "failed to send balance alert notification"
                            );
                            false
                        }
                    };
                    tracing::warn!(
                        upstream_id = %upstream.id,
                        upstream_name = %upstream.name,
                        remaining = snapshot.remaining,
                        threshold = setting.threshold,
                        "upstream balance is below alert threshold"
                    );
                    notified
                } else if !low && setting.alert_active {
                    tracing::info!(
                        upstream_id = %upstream.id,
                        upstream_name = %upstream.name,
                        remaining = snapshot.remaining,
                        "upstream balance alert recovered"
                    );
                    false
                } else {
                    low
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
                    "failed to query upstream balance for alert"
                );
                state
                    .store
                    .mark_balance_alert_checked(&upstream.id, now, setting.alert_active)
                    .await?;
            }
        }
    }
    Ok(())
}

fn is_due(settings: &UpstreamBalanceAlertSettings, now: i64) -> bool {
    settings
        .last_checked_at
        .is_none_or(|last| now.saturating_sub(last) >= settings.interval_seconds.max(60))
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
        assert!(!is_due(&settings, 1599));
        assert!(is_due(&settings, 1600));
    }
}
