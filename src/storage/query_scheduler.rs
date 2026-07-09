use crate::core::models::{
    ScheduleGroup, ScheduleGroupChild, ScheduleGroupMember, ScheduleMode, ScheduleRouteRule,
    ScheduleRouteTargetKind, Upstream,
};
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
                id, name, mode, use_all_upstreams, fixed_target_kind, fixed_upstream_id, fixed_group_id,
                failure_threshold, failover_on_balance, failover_on_network, failover_on_5xx,
                affinity_ttl_seconds, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                mode = excluded.mode,
                use_all_upstreams = excluded.use_all_upstreams,
                fixed_target_kind = excluded.fixed_target_kind,
                fixed_upstream_id = excluded.fixed_upstream_id,
                fixed_group_id = excluded.fixed_group_id,
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
        .bind(group.fixed_target_kind.as_str())
        .bind(&group.fixed_upstream_id)
        .bind(&group.fixed_group_id)
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
        sqlx::query(
            "DELETE FROM schedule_route_rules
             WHERE group_id = ?1 OR target_group_id = ?1",
        )
        .bind(id)
        .execute(self.pool())
        .await?;
        sqlx::query(
            "DELETE FROM schedule_group_child_groups
             WHERE group_id = ?1 OR target_group_id = ?1",
        )
        .bind(id)
        .execute(self.pool())
        .await?;
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

    pub async fn list_schedule_group_children(
        &self,
        group_id: &str,
    ) -> anyhow::Result<Vec<ScheduleGroupChild>> {
        let rows = sqlx::query(
            "SELECT * FROM schedule_group_child_groups WHERE group_id = ?1 ORDER BY priority DESC",
        )
        .bind(group_id)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(row_to_group_child).collect()
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

    pub async fn save_schedule_group_child(
        &self,
        child: &ScheduleGroupChild,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO schedule_group_child_groups (
                group_id, target_group_id, enabled, priority, weight, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(group_id, target_group_id) DO UPDATE SET
                enabled = excluded.enabled,
                priority = excluded.priority,
                weight = excluded.weight,
                updated_at = excluded.updated_at",
        )
        .bind(&child.group_id)
        .bind(&child.target_group_id)
        .bind(i64::from(child.enabled))
        .bind(child.priority)
        .bind(child.weight.max(1))
        .bind(child.created_at.to_rfc3339())
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

    pub async fn schedule_group_upstreams_nested(
        &self,
        group: &ScheduleGroup,
        max_hops: i64,
    ) -> anyhow::Result<Vec<Upstream>> {
        let mut upstreams = Vec::new();
        let mut queue = std::collections::VecDeque::from([(group.clone(), 0_i64)]);
        let mut seen_groups = std::collections::BTreeSet::new();
        let mut seen_upstreams = std::collections::BTreeSet::new();
        while let Some((group, hops)) = queue.pop_front() {
            if !seen_groups.insert(group.id.clone()) {
                continue;
            }
            for upstream in self.schedule_group_upstreams(&group).await? {
                if seen_upstreams.insert(upstream.id.clone()) {
                    upstreams.push(upstream);
                }
            }
            if group.use_all_upstreams {
                continue;
            }
            if hops >= max_hops.max(1) {
                anyhow::bail!("调度组嵌套超过最大跳转次数");
            }
            for child in self.list_schedule_group_children(&group.id).await? {
                if !child.enabled {
                    continue;
                }
                if let Some(child_group) = self.get_schedule_group(&child.target_group_id).await? {
                    queue.push_back((child_group, hops + 1));
                }
            }
        }
        Ok(upstreams)
    }

    pub async fn list_schedule_route_rules(
        &self,
        group_id: &str,
    ) -> anyhow::Result<Vec<ScheduleRouteRule>> {
        let rows = sqlx::query(
            "SELECT * FROM schedule_route_rules
             WHERE group_id = ?1
             ORDER BY priority DESC, length(pattern) DESC, created_at ASC",
        )
        .bind(group_id)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(row_to_route_rule).collect()
    }

    pub async fn save_schedule_route_rule(
        &self,
        rule: &ScheduleRouteRule,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO schedule_route_rules (
                id, group_id, name, enabled, pattern, target_kind,
                target_group_id, target_upstream_id, target_model, priority,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                group_id = excluded.group_id,
                name = excluded.name,
                enabled = excluded.enabled,
                pattern = excluded.pattern,
                target_kind = excluded.target_kind,
                target_group_id = excluded.target_group_id,
                target_upstream_id = excluded.target_upstream_id,
                target_model = excluded.target_model,
                priority = excluded.priority,
                updated_at = excluded.updated_at",
        )
        .bind(&rule.id)
        .bind(&rule.group_id)
        .bind(&rule.name)
        .bind(i64::from(rule.enabled))
        .bind(&rule.pattern)
        .bind(rule.target_kind.as_str())
        .bind(&rule.target_group_id)
        .bind(&rule.target_upstream_id)
        .bind(&rule.target_model)
        .bind(rule.priority)
        .bind(rule.created_at.to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn delete_schedule_route_rule(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM schedule_route_rules WHERE id = ?1")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn scheduler_route_max_hops(&self) -> anyhow::Result<i64> {
        Ok(self
            .get_setting("scheduler_route_max_hops")
            .await?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(8)
            .max(1))
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
        fixed_target_kind: ScheduleRouteTargetKind::from_str(&row.get::<String, _>("fixed_target_kind")),
        fixed_upstream_id: row.get("fixed_upstream_id"),
        fixed_group_id: row.get("fixed_group_id"),
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

fn row_to_group_child(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<ScheduleGroupChild> {
    let created_at: String = row.get("created_at");
    let updated_at: String = row.get("updated_at");
    Ok(ScheduleGroupChild {
        group_id: row.get("group_id"),
        target_group_id: row.get("target_group_id"),
        enabled: row.get::<i64, _>("enabled") != 0,
        priority: row.get("priority"),
        weight: row.get("weight"),
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .context("invalid schedule child created_at")?
            .with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(&updated_at)
            .context("invalid schedule child updated_at")?
            .with_timezone(&Utc),
    })
}

fn row_to_route_rule(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<ScheduleRouteRule> {
    let created_at: String = row.get("created_at");
    let updated_at: String = row.get("updated_at");
    Ok(ScheduleRouteRule {
        id: row.get("id"),
        group_id: row.get("group_id"),
        name: row.get("name"),
        enabled: row.get::<i64, _>("enabled") != 0,
        pattern: row.get("pattern"),
        target_kind: ScheduleRouteTargetKind::from_str(&row.get::<String, _>("target_kind")),
        target_group_id: row.get("target_group_id"),
        target_upstream_id: row.get("target_upstream_id"),
        target_model: row.get("target_model"),
        priority: row.get("priority"),
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .context("invalid schedule route created_at")?
            .with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(&updated_at)
            .context("invalid schedule route updated_at")?
            .with_timezone(&Utc),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{BalanceProvider, WireApi};

    #[tokio::test]
    async fn route_rules_are_saved_and_removed_with_target_group() {
        let path = std::env::temp_dir()
            .join(format!("codex-switch-route-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(&path).await.unwrap();
        let source = ScheduleGroup::new("source".to_string());
        let target = ScheduleGroup::new("target".to_string());
        store.save_schedule_group(&source).await.unwrap();
        store.save_schedule_group(&target).await.unwrap();

        let mut rule = ScheduleRouteRule::new(source.id.clone());
        rule.name = "glm".to_string();
        rule.pattern = "glm-*".to_string();
        rule.target_kind = ScheduleRouteTargetKind::Group;
        rule.target_group_id = Some(target.id.clone());
        rule.target_model = Some("glm-4.5".to_string());
        rule.priority = 9;
        store.save_schedule_route_rule(&rule).await.unwrap();

        let rules = store.list_schedule_route_rules(&source.id).await.unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "glm-*");
        assert_eq!(rules[0].target_model.as_deref(), Some("glm-4.5"));

        store.delete_schedule_group(&target.id).await.unwrap();
        let rules = store.list_schedule_route_rules(&source.id).await.unwrap();
        assert!(rules.is_empty());
    }

    #[tokio::test]
    async fn deleting_upstream_removes_direct_route_rules() {
        let path = std::env::temp_dir()
            .join(format!("codex-switch-upstream-route-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(&path).await.unwrap();
        let group = ScheduleGroup::new("source".to_string());
        store.save_schedule_group(&group).await.unwrap();
        let upstream = Upstream::new_relay(
            "image".to_string(),
            "http://127.0.0.1".to_string(),
            WireApi::Responses,
            true,
            BalanceProvider::Unsupported,
        );
        store.save_upstream(&upstream).await.unwrap();

        let mut rule = ScheduleRouteRule::new(group.id.clone());
        rule.name = "image".to_string();
        rule.pattern = "gpt-image-*".to_string();
        rule.target_kind = ScheduleRouteTargetKind::Upstream;
        rule.target_upstream_id = Some(upstream.id.clone());
        store.save_schedule_route_rule(&rule).await.unwrap();
        store.delete_upstream(&upstream.id).await.unwrap();

        let rules = store.list_schedule_route_rules(&group.id).await.unwrap();
        assert!(rules.is_empty());
    }
}
