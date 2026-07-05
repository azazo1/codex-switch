use crate::core::models::{ScheduleGroup, ScheduleGroupMember, ScheduleMode, Upstream};
use crate::storage::Store;
use crate::storage::query_upstreams::row_to_upstream;
use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::Row;

impl Store {
    pub async fn list_schedule_groups(&self) -> anyhow::Result<Vec<ScheduleGroup>> {
        let rows = sqlx::query("SELECT * FROM schedule_groups ORDER BY created_at ASC")
            .fetch_all(self.pool())
            .await?;
        rows.into_iter().map(row_to_schedule_group).collect()
    }

    pub async fn get_schedule_group(&self, id: &str) -> anyhow::Result<Option<ScheduleGroup>> {
        let row = sqlx::query("SELECT * FROM schedule_groups WHERE id = ?1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.map(row_to_schedule_group).transpose()
    }

    pub async fn current_schedule_group(&self) -> anyhow::Result<ScheduleGroup> {
        let current = self
            .get_setting("current_schedule_group_id")
            .await?
            .unwrap_or_else(|| "default".to_string());
        if let Some(group) = self.get_schedule_group(&current).await? {
            return Ok(group);
        }
        self.get_schedule_group("default")
            .await?
            .context("default schedule group is missing")
    }

    pub async fn set_current_schedule_group(&self, id: &str) -> anyhow::Result<()> {
        let exists = self.get_schedule_group(id).await?.is_some();
        if !exists {
            anyhow::bail!("schedule group does not exist");
        }
        self.set_setting("current_schedule_group_id", id).await
    }

    pub async fn save_schedule_group(&self, group: &ScheduleGroup) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO schedule_groups (
                id, name, mode, use_all_upstreams, fixed_upstream_id,
                failure_threshold, failover_on_balance, failover_on_network, failover_on_5xx,
                affinity_ttl_seconds, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                mode = excluded.mode,
                use_all_upstreams = excluded.use_all_upstreams,
                fixed_upstream_id = excluded.fixed_upstream_id,
                failure_threshold = excluded.failure_threshold,
                failover_on_balance = excluded.failover_on_balance,
                failover_on_network = excluded.failover_on_network,
                failover_on_5xx = excluded.failover_on_5xx,
                affinity_ttl_seconds = excluded.affinity_ttl_seconds,
                updated_at = excluded.updated_at",
        )
        .bind(&group.id)
        .bind(&group.name)
        .bind(group.mode.as_str())
        .bind(i64::from(group.use_all_upstreams))
        .bind(&group.fixed_upstream_id)
        .bind(group.failure_threshold.max(1))
        .bind(i64::from(group.failover_on_balance))
        .bind(i64::from(group.failover_on_network))
        .bind(i64::from(group.failover_on_5xx))
        .bind(group.affinity_ttl_seconds.max(60))
        .bind(group.created_at.to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn delete_schedule_group(&self, id: &str) -> anyhow::Result<()> {
        if id == "default" {
            anyhow::bail!("default schedule group cannot be deleted");
        }
        sqlx::query("DELETE FROM schedule_group_members WHERE group_id = ?1")
            .bind(id)
            .execute(self.pool())
            .await?;
        sqlx::query("DELETE FROM schedule_groups WHERE id = ?1")
            .bind(id)
            .execute(self.pool())
            .await?;
        if self
            .get_setting("current_schedule_group_id")
            .await?
            .as_deref()
            == Some(id)
        {
            self.set_setting("current_schedule_group_id", "default").await?;
        }
        Ok(())
    }

    pub async fn list_schedule_group_members(
        &self,
        group_id: &str,
    ) -> anyhow::Result<Vec<ScheduleGroupMember>> {
        let rows = sqlx::query(
            "SELECT * FROM schedule_group_members WHERE group_id = ?1 ORDER BY priority DESC",
        )
        .bind(group_id)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(row_to_group_member).collect()
    }

    pub async fn save_schedule_group_member(
        &self,
        member: &ScheduleGroupMember,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO schedule_group_members (
                group_id, upstream_id, enabled, priority, weight, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(group_id, upstream_id) DO UPDATE SET
                enabled = excluded.enabled,
                priority = excluded.priority,
                weight = excluded.weight,
                updated_at = excluded.updated_at",
        )
        .bind(&member.group_id)
        .bind(&member.upstream_id)
        .bind(i64::from(member.enabled))
        .bind(member.priority)
        .bind(member.weight.max(1))
        .bind(member.created_at.to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn schedule_group_upstreams(
        &self,
        group: &ScheduleGroup,
    ) -> anyhow::Result<Vec<Upstream>> {
        if group.use_all_upstreams {
            return self.enabled_upstreams().await;
        }
        let rows = sqlx::query(
            "SELECT upstreams.*
             FROM schedule_group_members
             JOIN upstreams ON upstreams.id = schedule_group_members.upstream_id
             WHERE schedule_group_members.group_id = ?1
                AND schedule_group_members.enabled = 1
                AND upstreams.enabled = 1
             ORDER BY schedule_group_members.priority DESC,
                upstreams.priority DESC,
                upstreams.created_at ASC",
        )
        .bind(&group.id)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(row_to_upstream).collect()
    }
}

fn row_to_schedule_group(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<ScheduleGroup> {
    let created_at: String = row.get("created_at");
    let updated_at: String = row.get("updated_at");
    Ok(ScheduleGroup {
        id: row.get("id"),
        name: row.get("name"),
        mode: ScheduleMode::from_str(&row.get::<String, _>("mode")),
        use_all_upstreams: row.get::<i64, _>("use_all_upstreams") != 0,
        fixed_upstream_id: row.get("fixed_upstream_id"),
        failure_threshold: row.get("failure_threshold"),
        failover_on_balance: row.get::<i64, _>("failover_on_balance") != 0,
        failover_on_network: row.get::<i64, _>("failover_on_network") != 0,
        failover_on_5xx: row.get::<i64, _>("failover_on_5xx") != 0,
        affinity_ttl_seconds: row.get("affinity_ttl_seconds"),
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .context("invalid schedule group created_at")?
            .with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(&updated_at)
            .context("invalid schedule group updated_at")?
            .with_timezone(&Utc),
    })
}

fn row_to_group_member(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<ScheduleGroupMember> {
    let created_at: String = row.get("created_at");
    let updated_at: String = row.get("updated_at");
    Ok(ScheduleGroupMember {
        group_id: row.get("group_id"),
        upstream_id: row.get("upstream_id"),
        enabled: row.get::<i64, _>("enabled") != 0,
        priority: row.get("priority"),
        weight: row.get("weight"),
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .context("invalid schedule member created_at")?
            .with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(&updated_at)
            .context("invalid schedule member updated_at")?
            .with_timezone(&Utc),
    })
}
