use crate::core::models::{BalanceSnapshot, QuotaSnapshot};
use crate::storage::Store;
use sqlx::Row;

impl Store {
    pub async fn save_quota_snapshot(&self, snapshot: &QuotaSnapshot) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO quota_snapshots (
                upstream_id, used_5h_percent, reset_5h_seconds, window_5h_minutes,
                used_7d_percent, reset_7d_seconds, window_7d_minutes, fetched_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(upstream_id) DO UPDATE SET
                used_5h_percent = excluded.used_5h_percent,
                reset_5h_seconds = excluded.reset_5h_seconds,
                window_5h_minutes = excluded.window_5h_minutes,
                used_7d_percent = excluded.used_7d_percent,
                reset_7d_seconds = excluded.reset_7d_seconds,
                window_7d_minutes = excluded.window_7d_minutes,
                fetched_at = excluded.fetched_at",
        )
        .bind(&snapshot.upstream_id)
        .bind(snapshot.used_5h_percent)
        .bind(snapshot.reset_5h_seconds)
        .bind(snapshot.window_5h_minutes)
        .bind(snapshot.used_7d_percent)
        .bind(snapshot.reset_7d_seconds)
        .bind(snapshot.window_7d_minutes)
        .bind(snapshot.fetched_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn get_quota_snapshot(
        &self,
        upstream_id: &str,
    ) -> anyhow::Result<Option<QuotaSnapshot>> {
        let row = sqlx::query("SELECT * FROM quota_snapshots WHERE upstream_id = ?1")
            .bind(upstream_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.map(|row| QuotaSnapshot {
            upstream_id: row.get("upstream_id"),
            used_5h_percent: row.get("used_5h_percent"),
            reset_5h_seconds: row.get("reset_5h_seconds"),
            window_5h_minutes: row.get("window_5h_minutes"),
            used_7d_percent: row.get("used_7d_percent"),
            reset_7d_seconds: row.get("reset_7d_seconds"),
            window_7d_minutes: row.get("window_7d_minutes"),
            fetched_at: row.get("fetched_at"),
        }))
    }

    pub async fn save_balance_snapshot(&self, snapshot: &BalanceSnapshot) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO balance_snapshots (
                upstream_id, provider, remaining, total, used, unit, is_valid, message, fetched_at,
                remaining_adjusted
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(upstream_id) DO UPDATE SET
                provider = excluded.provider,
                remaining = excluded.remaining,
                total = excluded.total,
                used = excluded.used,
                unit = excluded.unit,
                is_valid = excluded.is_valid,
                message = excluded.message,
                fetched_at = excluded.fetched_at,
                remaining_adjusted = excluded.remaining_adjusted",
        )
        .bind(&snapshot.upstream_id)
        .bind(&snapshot.provider)
        .bind(snapshot.remaining)
        .bind(snapshot.total)
        .bind(snapshot.used)
        .bind(&snapshot.unit)
        .bind(i64::from(snapshot.is_valid))
        .bind(&snapshot.message)
        .bind(snapshot.fetched_at)
        .bind(i64::from(snapshot.remaining_adjusted))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// 按计价脚本 charge 的 delta 扣减 remaining. 无有效快照或 remaining 缺失时不改.
    pub async fn apply_balance_debit(
        &self,
        upstream_id: &str,
        debit: f64,
    ) -> anyhow::Result<bool> {
        if !debit.is_finite() || debit == 0.0 {
            return Ok(false);
        }
        let Some(snapshot) = self.get_balance_snapshot(upstream_id).await? else {
            return Ok(false);
        };
        if !snapshot.is_valid {
            return Ok(false);
        }
        let Some(remaining) = snapshot.remaining else {
            return Ok(false);
        };
        let used = snapshot.used.map(|used| used + debit);
        sqlx::query(
            "UPDATE balance_snapshots
             SET remaining = ?1, used = ?2, remaining_adjusted = 1
             WHERE upstream_id = ?3",
        )
        .bind(remaining - debit)
        .bind(used)
        .bind(upstream_id)
        .execute(self.pool())
        .await?;
        Ok(true)
    }

    pub async fn get_balance_snapshot(
        &self,
        upstream_id: &str,
    ) -> anyhow::Result<Option<BalanceSnapshot>> {
        let row = sqlx::query("SELECT * FROM balance_snapshots WHERE upstream_id = ?1")
            .bind(upstream_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.map(|row| BalanceSnapshot {
            upstream_id: row.get("upstream_id"),
            provider: row.get("provider"),
            remaining: row.get("remaining"),
            total: row.get("total"),
            used: row.get("used"),
            unit: row.get("unit"),
            is_valid: row.get::<i64, _>("is_valid") != 0,
            message: row.get("message"),
            fetched_at: row.get("fetched_at"),
            remaining_adjusted: row.get::<i64, _>("remaining_adjusted") != 0,
        }))
    }
}
