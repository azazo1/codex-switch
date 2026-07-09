use crate::app::AppState;
use crate::core::models::{
    ScheduleGroup, ScheduleMode, ScheduleRouteRule, ScheduleRouteTargetKind, Upstream,
    UpstreamKind,
};
use crate::scheduler::{
    DirectSchedulerPlan, ScheduleRouteTraceStep, SchedulerPlan, glob_captures,
    rewrite_model_template,
};
use anyhow::Context;

pub(super) async fn selection_plan(
    state: &AppState,
    body: &[u8],
    endpoint: &str,
    model: Option<&str>,
    responses_api: bool,
    compact: bool,
) -> anyhow::Result<SchedulerPlan> {
    let group = state.store.current_schedule_group().await?;
    let max_hops = state.store.scheduler_route_max_hops().await?;
    let resolved = resolve_schedule_route(state, group, model, max_hops).await?;
    let effective_model = resolved
        .target_model
        .clone()
        .or_else(|| model.map(str::to_string));
    if let Some(upstream) = resolved.direct_upstream {
        let upstream = if upstream_available(&upstream, responses_api, compact) {
            upstream
        } else {
            anyhow::bail!("routed upstream is not available for this request");
        };
        return state
            .scheduler
            .plan_direct(
                DirectSchedulerPlan {
                    group: resolved.group,
                    upstream,
                    target_model: resolved.target_model,
                    route_path: resolved.route_path,
                },
                body,
                endpoint,
                effective_model.as_deref(),
            )
            .await;
    }
    let upstreams = state
        .store
        .schedule_group_upstreams_nested(&resolved.group, max_hops)
        .await?;
    let upstreams = upstreams
        .into_iter()
        .filter(|upstream| upstream_available(upstream, responses_api, compact))
        .collect::<Vec<_>>();
    let mut plan = state
        .scheduler
        .plan(
            resolved.group,
            upstreams,
            body,
            endpoint,
            effective_model.as_deref(),
        )
        .await?;
    plan.target_model = resolved.target_model;
    plan.route_path = resolved.route_path;
    Ok(plan)
}

struct ResolvedScheduleRoute {
    group: ScheduleGroup,
    direct_upstream: Option<Upstream>,
    target_model: Option<String>,
    route_path: Vec<ScheduleRouteTraceStep>,
}

async fn resolve_schedule_route(
    state: &AppState,
    group: ScheduleGroup,
    model: Option<&str>,
    max_hops: i64,
) -> anyhow::Result<ResolvedScheduleRoute> {
    if group.mode != ScheduleMode::ModelMapping && !fixed_targets_group(&group) {
        return Ok(ResolvedScheduleRoute {
            group,
            direct_upstream: None,
            target_model: None,
            route_path: Vec::new(),
        });
    }
    let original_model = model.unwrap_or_default();
    let mut group = group;
    let mut target_model = None;
    let mut route_path = Vec::new();
    loop {
        if fixed_targets_group(&group) {
            if route_path.len() as i64 >= max_hops.max(1) {
                anyhow::bail!("调度组嵌套超过最大跳转次数");
            }
            let target_id = group
                .fixed_group_id
                .as_deref()
                .and_then(trimmed_value)
                .context("fixed schedule target group is missing")?;
            route_path.push(ScheduleRouteTraceStep {
                group_id: group.id.clone(),
                rule_id: "fixed".to_string(),
                target_kind: ScheduleRouteTargetKind::Group,
                target_id: target_id.clone(),
            });
            group = state
                .store
                .get_schedule_group(&target_id)
                .await?
                .with_context(|| format!("fixed target group not found: {target_id}"))?;
            continue;
        }
        if group.mode != ScheduleMode::ModelMapping {
            return Ok(ResolvedScheduleRoute {
                group,
                direct_upstream: None,
                target_model,
                route_path,
            });
        }
        let matching_model = target_model.as_deref().unwrap_or(original_model);
        let rules = state.store.list_schedule_route_rules(&group.id).await?;
        let Some((rule, captures)) = rules
            .into_iter()
            .filter(|rule| rule.enabled)
            .find_map(|rule| {
                glob_captures(&rule.pattern, matching_model).map(|captures| (rule, captures))
            })
        else {
            anyhow::bail!("模型映射调度组没有匹配的路由规则");
        };
        if route_path.len() as i64 >= max_hops.max(1) {
            tracing::warn!(
                model = original_model,
                effective_model = matching_model,
                max_hops,
                route_path = %format_route_path(&route_path),
                "schedule route exceeded max hops"
            );
            anyhow::bail!("模型路由超过最大跳转次数");
        }
        if let Some(model) = rule.target_model.as_deref().and_then(trimmed_value) {
            target_model = Some(rewrite_model_template(&model, &captures));
        }
        let target_id = route_target_id(&rule)?;
        route_path.push(ScheduleRouteTraceStep {
            group_id: group.id.clone(),
            rule_id: rule.id.clone(),
            target_kind: rule.target_kind,
            target_id: target_id.clone(),
        });
        match rule.target_kind {
            ScheduleRouteTargetKind::Group => {
                tracing::debug!(
                    group_id = %group.id,
                    rule_id = %rule.id,
                    target_group_id = %target_id,
                    model = original_model,
                    effective_model = target_model.as_deref().unwrap_or(original_model),
                    "schedule route matched group target"
                );
                group = state
                    .store
                    .get_schedule_group(&target_id)
                    .await?
                    .with_context(|| format!("route target group not found: {target_id}"))?;
            }
            ScheduleRouteTargetKind::Upstream => {
                tracing::debug!(
                    group_id = %group.id,
                    rule_id = %rule.id,
                    target_upstream_id = %target_id,
                    model = original_model,
                    effective_model = target_model.as_deref().unwrap_or(original_model),
                    "schedule route matched upstream target"
                );
                let upstream = state
                    .store
                    .get_upstream(&target_id)
                    .await?
                    .with_context(|| format!("route target upstream not found: {target_id}"))?;
                return Ok(ResolvedScheduleRoute {
                    group,
                    direct_upstream: Some(upstream),
                    target_model,
                    route_path,
                });
            }
        }
    }
}

fn route_target_id(rule: &ScheduleRouteRule) -> anyhow::Result<String> {
    match rule.target_kind {
        ScheduleRouteTargetKind::Group => rule
            .target_group_id
            .as_deref()
            .and_then(trimmed_value)
            .context("route target group is missing"),
        ScheduleRouteTargetKind::Upstream => rule
            .target_upstream_id
            .as_deref()
            .and_then(trimmed_value)
            .context("route target upstream is missing"),
    }
}

fn trimmed_value(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn upstream_available(upstream: &Upstream, responses_api: bool, compact: bool) -> bool {
    upstream.enabled
        && (!compact || upstream.supports_compact)
        && (!responses_api || upstream.kind == UpstreamKind::CodexOauth || !upstream.base_url.is_empty())
}

fn fixed_targets_group(group: &ScheduleGroup) -> bool {
    group.mode == ScheduleMode::Fixed && group.fixed_target_kind == ScheduleRouteTargetKind::Group
}

fn format_route_path(route_path: &[ScheduleRouteTraceStep]) -> String {
    route_path
        .iter()
        .map(|step| {
            format!(
                "{}:{}->{}:{}",
                step.group_id,
                step.rule_id,
                step.target_kind.as_str(),
                step.target_id
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}
