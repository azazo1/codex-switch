# 费用估算与计价脚本

Codex Switch 的费用只用于本地统计和展示, 不会向下游或上游发送账单.

## 内置公式

未启用脚本时:

```
估算 USD = estimate_usage_cost(usage, models.dev 价格) * 上游价格倍率
```

`estimate_usage_cost` 按百万 token 单价拆成未缓存输入, 缓存读取, 缓存写入和输出. 价格来自 `https://models.dev/api.json`, 用仪表盘 `获取模型信息` 刷新. 查价只用客户端模型名, 不用调度映射后的 `target_model`.

上游编辑器里的 `价格倍率` 只作用于这条内置路径. 默认 `1.0`. 它只影响成本统计, 不改变转发.

汇率缓存只用于界面 USD / CNY 切换. 内置公式始终产出 USD, 除非脚本自己读取 `ctx.fx`.

## 启用脚本

仪表盘价格缓存一行有 `计价脚本`, 编辑的是全局脚本. 上游编辑器里同一行的 `计价脚本` 编辑该上游独立脚本. 保存后立即替换运行中的估算逻辑, 不用重编译或重启. 点 `文档` 可在应用内查阅本页.

估算按固定优先级取第一份有效结果:

```
上游脚本 > 全局脚本 > 内置公式
```

- 某一层未启用, 源码为空, 编译失败, `estimate` 返回 `()`, 运行失败或死循环: 进入下一层.
- 某一层返回数字: 该数字就是最终 USD. 宿主不再自动乘价格倍率.
- 无上游或上游没有独立脚本时, 跳过上游层, 直接走全局.
- 运行失败不会自动关闭开关.
- 单条日志费用是写入时的快照. 仪表盘汇总用当前全局/上游脚本链路, 当前价格缓存, 当前汇率和当前时间重算.

缓存保活是否发送请求仍按内置价格比较, 不受脚本影响. 保活请求写入日志时的费用走脚本.

## estimate(ctx)

必须提供 `fn estimate(ctx)`. 返回数字或 `()`.

Rhai 自带的 `timestamp()` 只是单调时钟. 日历时间使用宿主注入的 `DateTime` 结构体: `ctx.utc` 和 `ctx.local`.

| 字段 | 含义 |
| --- | --- |
| `ctx.model` | 客户端模型名 |
| `ctx.target_model` | 调度映射后的上游模型, 可能为 `()` |
| `ctx.usage.input_tokens` / `output_tokens` / `cache_read_tokens` / `cache_creation_tokens` / `total_tokens` / `uncached_input_tokens` | token |
| `ctx.price` | 命中的 models.dev 价格. 无缓存时为 `()` |
| `ctx.price.input` / `cached_input` / `cache_write` / `output` | USD / million, 缺失为 `()` |
| `ctx.price.model_id` / `provider_id` / `official` | 价格缓存元数据 |
| `ctx.upstream` | 上游. 无上游时为 `()` |
| `ctx.upstream.id` / `name` / `kind` / `base_url` / `multiplier` | 上游字段. 上游已删除时 `base_url` 为空字符串 |
| `ctx.builtin` | 内置 `estimate_usage_cost` 结果, 尚未乘倍率. 无价格时为 `()` |
| `ctx.multiplier` | 上游价格倍率, 无上游为 `1.0` |
| `ctx.fx` | 汇率缓存. 尚未获取时为 `()` |
| `ctx.fx.usd_cny` | 1 USD 兑 CNY |
| `ctx.fx.fetched_at` | 汇率缓存 unix 秒 |
| `ctx.now` | 本次估算使用的 unix 秒 (UTC) |
| `ctx.utc` / `ctx.local` | `DateTime`: `unix`, `year`, `month`, `day`, `hour`, `minute`, `second`, `weekday` (ISO, 1=周一) |

写请求日志时 `ctx.now` 用该条日志的完成时间. 仪表盘汇总和试算用估算当下.

宿主还注册 `usd_for_tokens(tokens, usd_per_million)`.

脚本可以写日志:

| 调用 | 去向 |
| --- | --- |
| `print(x)` / `log(x)` | 应用主日志 info, 试算窗口也会显示 |
| `warn(x)` | 应用主日志 warn |
| `debug(x)` | 应用主日志 debug, 需打开完整调试日志 |

`estimate` 会在每次请求写入和仪表盘汇总时执行. 不要无条件打印大量内容, 否则主日志会涨得很快.

脚本不能访问文件, 网络或 `sleep` / `eval`. 操作数上限为 10000.

## 示例

包装内置公式并使用倍率:

```rhai
fn estimate(ctx) {
    if ctx.builtin != () {
        return ctx.builtin * ctx.multiplier;
    }
    ()
}
```

补全缺失模型, 人民币报价换算成 USD:

```rhai
fn estimate(ctx) {
    let fx = if ctx.fx != () { ctx.fx.usd_cny } else { 7.2 };
    if ctx.model.contains("deepseek") {
        let cny = usd_for_tokens(ctx.usage.uncached_input_tokens, 2.0)
            + usd_for_tokens(ctx.usage.output_tokens, 8.0);
        return cny / fx;
    }
    if ctx.builtin != () { ctx.builtin * ctx.multiplier } else { () }
}
```

本地高峰加价:

```rhai
fn estimate(ctx) {
    let cost = if ctx.builtin != () { ctx.builtin * ctx.multiplier } else { () };
    if cost != () && ctx.local.hour >= 19 && ctx.local.hour < 23 {
        return cost * 1.2;
    }
    cost
}
```
