// @label DeepSeek 官方价
// @prefer https://api.deepseek.ai/v1

// DeepSeek 上游计价脚本, 按 [官方文档](https://api-docs.deepseek.com/zh-cn/quick_start/pricing)
// 的 CNY / 百万 tokens 计算, 再除以汇率得到 USD.
// 挂到 DeepSeek 上游上使用, 该上游上的请求一律按官方价估算.

// 高峰为 UTC 周一至周五 `01:00-04:00` 与 `06:00-10:00` (北京时间 `9:00-12:00`, `14:00-18:00`).
// 旧名 `deepseek-v4-flash` / `deepseek-v4-flash-vision-exp` 按 Flash 计费.
// 北京时间 2026-09-14 12:00 起 `deepseek-v4-pro` 也按 Flash 计费.
// 价格来自 https://api-docs.deepseek.com/zh-cn/quick_start/pricing (2026-09-10).

fn estimate(ctx) {
    let model = if ctx.target_model != () { ctx.target_model } else { ctx.model };
    let peak = is_peak(ctx.utc);
    let pro = is_v4_pro(model) && !v4_pro_retired(ctx.utc);
    let hit = cny_hit(pro, peak);
    let miss = cny_miss(pro, peak);
    let out = cny_out(pro, peak);
    let fx = if ctx.fx != () { ctx.fx.usd_cny } else { 7.2 };
    let miss_tokens = ctx.usage.uncached_input_tokens + ctx.usage.cache_creation_tokens;
    let cny = usd_for_tokens(miss_tokens, miss)
        + usd_for_tokens(ctx.usage.cache_read_tokens, hit)
        + usd_for_tokens(ctx.usage.output_tokens, out);
    let usd = cny / fx * ctx.multiplier;
    // 请求落库时按余额单位扣费, 试算和仪表盘重算为空操作.
    if ctx.balance != () {
        if ctx.balance.unit == "CNY" {
            charge(cny * ctx.multiplier);
        } else {
            charge(usd);
        }
    }
    usd
}

fn is_v4_pro(model) {
    let name = model.to_lower();
    name.contains("v4-pro") || name.contains("v4_pro")
}

fn is_peak(utc) {
    if utc.weekday > 5 {
        return false;
    }
    let hour = utc.hour;
    (hour >= 1 && hour < 4) || (hour >= 6 && hour < 10)
}

fn v4_pro_retired(utc) {
    if utc.year != 2026 {
        return utc.year > 2026;
    }
    if utc.month != 9 {
        return utc.month > 9;
    }
    if utc.day != 14 {
        return utc.day > 14;
    }
    utc.hour >= 4
}

fn cny_hit(pro, peak) {
    if pro {
        if peak { 0.30 } else { 0.15 }
    } else {
        if peak { 0.04 } else { 0.02 }
    }
}

fn cny_miss(pro, peak) {
    if pro {
        if peak { 9.0 } else { 4.5 }
    } else {
        if peak { 2.0 } else { 1.0 }
    }
}

fn cny_out(pro, peak) {
    if pro {
        if peak { 27.0 } else { 13.5 }
    } else {
        if peak { 8.0 } else { 4.0 }
    }
}
