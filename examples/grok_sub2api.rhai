// @label Grok 中转价

// Grok 中转计价脚本. 中转把官方美元单价原样记成人民币, 再乘上游价格倍率.
// 单价来自 ctx.builtin / models.dev 官方缓存, 不按模型名写死, 4.3 / 4.5 / 4.6 / 4.20 / grok-build 等都走同一套.
// 价格倍率填 `0.2` 时, 估算的人民币费用对齐账单 Billed Cost.
// models.dev 只有短上下文单价. prompt (未缓存输入 + 缓存读取 + 缓存写入) >= 200k 时,
// 官方会把该请求全部 token 按 2 倍计, 脚本在 builtin 上补这一档.
// 查价用客户端模型名. 映射后的 target_model 不参与, 缓存未命中时返回 () 交给下一层.
// 不含工具调用费: 宿主 usage 没有 tool call 次数.

// 计费规则适配上游: https://oinkie.top 的 SuperGrok 分组.

fn estimate(ctx) {
    if ctx.builtin == () {
        return ();
    }
    let prompt = ctx.usage.uncached_input_tokens
        + ctx.usage.cache_creation_tokens
        + ctx.usage.cache_read_tokens;
    let scale = if prompt >= 200000 { 2.0 } else { 1.0 };
    let fx = if ctx.fx != () { ctx.fx.usd_cny } else { 7.2 };
    let cny = ctx.builtin * scale;
    let billed = cny * ctx.multiplier;
    // estimate 必须返回 USD. 这条中转的余额数字是人民币, 即使 unit 标成 USD 也按人民币扣.
    if ctx.balance != () {
        charge(billed);
    }
    billed / fx
}
