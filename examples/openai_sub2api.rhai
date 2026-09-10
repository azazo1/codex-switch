// @label OpenAI 中转价

// OpenAI 中转计价脚本. 中转把官方美元单价原样记成人民币, 再乘上游价格倍率.
// 单价来自 ctx.price / models.dev 官方缓存, 不按模型名写死.
// luna / terra / sol / gpt-5.4 / gpt-5.6 等只要价格缓存命中都走同一套.
// 倍率乘 ctx.multiplier, 按 key 拆上游各自填写: medium `0.125`, high `0.18`, max `0.25`.
// models.dev 只有短上下文单价. prompt (未缓存输入 + 缓存读取 + 缓存写入) > 272k 时,
// 官方整单输入侧 2 倍, 输出 1.5 倍. 脚本按 ctx.price 拆开补这一档, 不能 builtin * 2.
// 缓存写入若价格缓存有 cache_write 就用, 否则回退到输入价 (官方 5.6 是输入的 1.25 倍).
// 查价用客户端模型名. 映射后的 target_model 不参与, 缓存未命中时返回 () 交给下一层.
// 不含工具调用费: 宿主 usage 没有 tool call 次数.

// 计费规则适配上游: https://oinkie.top 的 Openai 类分组.

fn estimate(ctx) {
    if ctx.price == () && ctx.builtin == () {
        return ();
    }
    let prompt = ctx.usage.uncached_input_tokens
        + ctx.usage.cache_creation_tokens
        + ctx.usage.cache_read_tokens;
    let long = prompt > 272000;
    let fx = if ctx.fx != () { ctx.fx.usd_cny } else { 7.2 };
    let cny = token_cny(ctx, long);
    if cny == () {
        return ();
    }
    let billed = cny * ctx.multiplier;
    // estimate 必须返回 USD. 这条中转的余额数字是人民币, 即使 unit 标成 USD 也按人民币扣.
    if ctx.balance != () {
        charge(billed);
    }
    billed / fx
}

fn token_cny(ctx, long) {
    if ctx.price == () || ctx.price.input == () || ctx.price.output == () {
        if long {
            return ();
        }
        return ctx.builtin;
    }
    let input = ctx.price.input;
    let cached = if ctx.price.cached_input != () { ctx.price.cached_input } else { input };
    let write = if ctx.price.cache_write != () { ctx.price.cache_write } else { input };
    let output = ctx.price.output;
    let in_scale = if long { 2.0 } else { 1.0 };
    let out_scale = if long { 1.5 } else { 1.0 };
    usd_for_tokens(ctx.usage.uncached_input_tokens, input * in_scale)
        + usd_for_tokens(ctx.usage.cache_read_tokens, cached * in_scale)
        + usd_for_tokens(ctx.usage.cache_creation_tokens, write * in_scale)
        + usd_for_tokens(ctx.usage.output_tokens, output * out_scale)
}
