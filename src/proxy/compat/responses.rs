use serde_json::{Value, json};

/// 历史里完全没有思维链时, 补进去的占位文本. 官方只要求非空.
const SYNTHETIC_REASONING_TEXT: &str = "(reasoning not recorded)";

/// reasoning 项在转发前的归一化形态.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReasoningNormalize {
    /// 把思维链文本搬进 `summary` 并清空 `content`, 这是 OpenAI 系 Responses 上游的常规形态.
    Summary,
    /// 把思维链文本还原进 `content[].reasoning_text`, 并保留已有的 `content`.
    ///
    /// DeepSeek 官方 Responses 接口在思考模式下要求把它认不出的工具调用所属轮次的
    /// 思维链随请求传回 (见官方 Thinking Mode 文档的 Tool Calls 一节), 只给 `summary`
    /// 会被 400 拒绝, 而且它不把 `summary` 计入输入. 该形态下还会在整段历史一个
    /// reasoning 项都没有时补占位项, 因为"没有思维链"同样会被拒绝.
    ReasoningText,
}

/// reasoning 归一化的改动统计, 供调用方打日志.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ReasoningNormalizeStats {
    /// 被写回 `content[].reasoning_text` 的 reasoning 项数.
    pub(crate) restored: usize,
    /// 合成的占位 reasoning 项数.
    pub(crate) synthesized: usize,
}

pub(crate) fn normalize_responses_request(
    body: &[u8],
    mode: ReasoningNormalize,
) -> anyhow::Result<(Vec<u8>, ReasoningNormalizeStats)> {
    let mut value: Value = serde_json::from_slice(body)?;
    let mut stats = ReasoningNormalizeStats::default();
    if let Some(input) = value.get_mut("input").and_then(Value::as_array_mut) {
        match mode {
            ReasoningNormalize::Summary => {
                for item in input.iter_mut() {
                    normalize_reasoning_item(item);
                }
            }
            ReasoningNormalize::ReasoningText => {
                for item in input.iter_mut() {
                    if restore_reasoning_text(item) {
                        stats.restored += 1;
                    }
                }
                stats.synthesized = synthesize_missing_reasoning(input);
            }
        }
    }
    Ok((serde_json::to_vec(&value)?, stats))
}

fn normalize_reasoning_item(item: &mut Value) {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return;
    }
    let decoded_internal = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .and_then(super::decode_reasoning);
    let content_text = content_reasoning_text(item);
    let Some(object) = item.as_object_mut() else {
        return;
    };
    object.insert("content".to_string(), json!([]));
    if let Some(text) = decoded_internal.or(content_text) {
        object.insert(
            "summary".to_string(),
            json!([{"type":"summary_text","text":text}]),
        );
        object.remove("encrypted_content");
    }
}

/// 把思维链文本还原进 `content[].reasoning_text`, 返回是否写入了内容.
///
/// 取料顺序: 已有的 `content` 文本 > 代理自存的思维链编码 > `summary` 文本.
/// 已经有 `reasoning_text` 的项原样保留, 避免破坏上游原生返回的内容.
fn restore_reasoning_text(item: &mut Value) -> bool {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return false;
    }
    if content_reasoning_text(item).is_some() {
        return false;
    }
    let decoded_internal = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .and_then(super::decode_reasoning);
    let text = decoded_internal.or_else(|| summary_reasoning_text(item));
    let Some(object) = item.as_object_mut() else {
        return false;
    };
    object.remove("encrypted_content");
    let Some(text) = text else {
        return false;
    };
    object.insert(
        "content".to_string(),
        json!([{"type":"reasoning_text","text":text}]),
    );
    object.remove("summary");
    true
}

/// 整段历史一个 reasoning 项都没有, 但存在工具调用时, 为每个调用步补一个占位项, 返回补了几个.
fn synthesize_missing_reasoning(input: &mut Vec<Value>) -> usize {
    let has_reasoning = input
        .iter()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"));
    let has_function_call = input
        .iter()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call"));
    if has_reasoning || !has_function_call {
        return 0;
    }
    let mut rebuilt: Vec<Value> = Vec::with_capacity(input.len());
    let mut synthesized = 0;
    for item in input.drain(..) {
        let is_function_call = item.get("type").and_then(Value::as_str) == Some("function_call");
        let previous_is_reasoning = rebuilt.last().is_some_and(|previous: &Value| {
            previous.get("type").and_then(Value::as_str) == Some("reasoning")
        });
        if is_function_call && !previous_is_reasoning {
            rebuilt.push(json!({
                "type": "reasoning",
                "id": format!("codex-switch-synthetic-{synthesized}"),
                "status": "completed",
                "content": [{"type": "reasoning_text", "text": SYNTHETIC_REASONING_TEXT}],
            }));
            synthesized += 1;
        }
        rebuilt.push(item);
    }
    *input = rebuilt;
    synthesized
}

fn content_reasoning_text(item: &Value) -> Option<String> {
    match item.get("content") {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<String>();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn summary_reasoning_text(item: &Value) -> Option<String> {
    let parts = item.get("summary")?;
    let text = match parts {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<String>(),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

fn tool_call_id(item: &Value) -> Option<&str> {
    match item.get("type").and_then(Value::as_str)? {
        "function_call" | "custom_tool_call" | "tool_search_call" => {
            item.get("call_id").and_then(Value::as_str)
        }
        _ => None,
    }
}

fn tool_output_call_id(item: &Value) -> Option<&str> {
    match item.get("type").and_then(Value::as_str)? {
        "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
            item.get("call_id").and_then(Value::as_str)
        }
        _ => None,
    }
}

/// 把每个工具输出挪到它自己的工具调用后面, 返回被移动的输出项数.
///
/// 只按相邻项配对的上游 (DeepSeek 官方 `/v1/responses`) 要求输出紧跟调用: 中间夹了另一个调用,
/// developer 消息或 reasoning 项都会被 400 拒绝 (`No tool output found for tool call ...`),
/// 而这段历史之后每轮都会原样重放, 会话再也接不下去. 客户端习惯把并行调用排成
/// `call, call, output, output`, 所以转发前先交错一次. 已经在正确位置的项不动, 没有输出的调用
/// 也不会凭空补齐; 没有改动时原样返回入参字节.
pub(crate) fn attach_tool_outputs_to_calls(body: &[u8]) -> anyhow::Result<(Vec<u8>, usize)> {
    let mut value: Value = serde_json::from_slice(body)?;
    let Some(input) = value.get_mut("input").and_then(Value::as_array_mut) else {
        return Ok((body.to_vec(), 0));
    };
    let mut moved = 0;
    let mut index = 0;
    while index < input.len() {
        let Some(call_id) = tool_call_id(&input[index]).map(str::to_string) else {
            index += 1;
            continue;
        };
        let adjacent = input
            .get(index + 1)
            .and_then(tool_output_call_id)
            .is_some_and(|id| id == call_id.as_str());
        if !adjacent
            && let Some(found) = (index + 1..input.len())
                .find(|&candidate| tool_output_call_id(&input[candidate]) == Some(call_id.as_str()))
        {
            let output = input.remove(found);
            input.insert(index + 1, output);
            moved += 1;
            index += 2;
            continue;
        }
        index += 1;
    }
    if moved == 0 {
        return Ok((body.to_vec(), 0));
    }
    Ok((serde_json::to_vec(&value)?, moved))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalize(body: &Value, mode: ReasoningNormalize) -> Value {
        let (normalized, _) =
            normalize_responses_request(&serde_json::to_vec(body).unwrap(), mode).unwrap();
        serde_json::from_slice(&normalized).unwrap()
    }

    fn normalize_with_stats(
        body: &Value,
        mode: ReasoningNormalize,
    ) -> (Value, ReasoningNormalizeStats) {
        let (normalized, stats) =
            normalize_responses_request(&serde_json::to_vec(body).unwrap(), mode).unwrap();
        (serde_json::from_slice(&normalized).unwrap(), stats)
    }

    fn reorder(body: &Value) -> (Value, usize) {
        let (value, moved) =
            attach_tool_outputs_to_calls(&serde_json::to_vec(body).unwrap()).unwrap();
        (serde_json::from_slice(&value).unwrap(), moved)
    }

    fn item_types(value: &Value) -> Vec<&str> {
        value["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["type"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn attaches_parallel_tool_outputs_to_their_calls() {
        let body = json!({
            "model":"deepseek-flash",
            "input":[
                {"type":"message","role":"assistant","content":[]},
                {"type":"function_call","call_id":"call_00","name":"skill","arguments":"{}"},
                {"type":"function_call","call_id":"call_01","name":"bash","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_00","output":"skill ok"},
                {"type":"function_call_output","call_id":"call_01","output":"bash ok"}
            ]
        });

        let (value, moved) = reorder(&body);

        // 把 call_00 的输出搬到它后面之后, call_01 正好接上自己的输出, 所以只搬一次.
        assert_eq!(moved, 1);
        assert_eq!(
            item_types(&value),
            vec![
                "message",
                "function_call",
                "function_call_output",
                "function_call",
                "function_call_output"
            ]
        );
        assert_eq!(value["input"][2]["call_id"], "call_00");
        assert_eq!(value["input"][2]["output"], "skill ok");
        assert_eq!(value["input"][4]["call_id"], "call_01");
    }

    #[test]
    fn attaches_tool_output_across_intervening_items() {
        let body = json!({
            "model":"deepseek-flash",
            "input":[
                {"type":"function_call","call_id":"call_1","name":"bash","arguments":"{}"},
                {"type":"developer","content":"补充说明"},
                {"type":"function_call_output","call_id":"call_1","output":"ok"}
            ]
        });

        let (value, moved) = reorder(&body);

        assert_eq!(moved, 1);
        assert_eq!(
            item_types(&value),
            vec!["function_call", "function_call_output", "developer"]
        );
        assert_eq!(value["input"][2]["content"], "补充说明");
    }

    #[test]
    fn leaves_adjacent_tool_history_untouched() {
        let body = json!({
            "model":"deepseek-flash",
            "input":[
                {"type":"function_call","call_id":"call_1","name":"bash","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_1","output":"ok"}
            ]
        });

        let (value, moved) = reorder(&body);

        assert_eq!(moved, 0);
        assert_eq!(value, body);
    }

    #[test]
    fn keeps_unanswered_tool_call_without_synthesizing_output() {
        let body = json!({
            "model":"deepseek-flash",
            "input":[
                {"type":"function_call","call_id":"call_1","name":"bash","arguments":"{}"},
                {"type":"function_call","call_id":"call_2","name":"read","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_1","output":"ok"}
            ]
        });

        let (value, moved) = reorder(&body);

        assert_eq!(moved, 1);
        assert_eq!(
            item_types(&value),
            vec!["function_call", "function_call_output", "function_call"]
        );
        assert_eq!(value["input"][0]["call_id"], "call_1");
        assert_eq!(value["input"][1]["call_id"], "call_1");
        assert_eq!(value["input"][2]["call_id"], "call_2");
    }

    #[test]
    fn restores_internal_reasoning_as_summary_text() {
        let body = json!({
            "model":"deepseek-v4-flash",
            "input":[
                {
                    "type":"reasoning",
                    "id":"rs_1",
                    "summary":[],
                    "encrypted_content":"codex-switch-reasoning-v1:aGVsbG8"
                },
                {
                    "type":"function_call",
                    "call_id":"call_1",
                    "name":"exec",
                    "arguments":"{}"
                }
            ]
        });

        let value = normalize(&body, ReasoningNormalize::Summary);

        assert_eq!(
            value["input"][0]["summary"][0],
            json!({"type":"summary_text","text":"hello"})
        );
        assert_eq!(value["input"][0]["content"], json!([]));
        assert!(value["input"][0].get("encrypted_content").is_none());
    }

    #[test]
    fn keeps_summary_reasoning_with_empty_content() {
        let body = json!({
            "model":"responses-model",
            "input":[{
                "type":"reasoning",
                "id":"rs_1",
                "summary":[{"type":"summary_text","text":"short summary"}]
            }]
        });

        let value = normalize(&body, ReasoningNormalize::Summary);

        assert_eq!(
            value["input"][0]["summary"][0],
            json!({"type":"summary_text","text":"short summary"})
        );
        assert_eq!(value["input"][0]["content"], json!([]));
    }

    #[test]
    fn moves_existing_reasoning_content_to_summary() {
        let body = json!({
            "model":"responses-model",
            "input":[{
                "type":"reasoning",
                "id":"rs_1",
                "summary":[],
                "content":[{"type":"reasoning_text","text":"think step by step"}]
            }]
        });

        let value = normalize(&body, ReasoningNormalize::Summary);

        assert_eq!(
            value["input"][0]["summary"][0],
            json!({"type":"summary_text","text":"think step by step"})
        );
        assert_eq!(value["input"][0]["content"], json!([]));
    }

    #[test]
    fn reasoning_text_mode_restores_summary_into_content() {
        let body = json!({
            "model":"deepseek-flash",
            "input":[
                {
                    "type":"reasoning",
                    "id":"item_1",
                    "status":"completed",
                    "summary":[{"type":"summary_text","text":"先看代码再动手"}]
                },
                {"type":"function_call","call_id":"call_1","name":"bash","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_1","output":"ok"}
            ]
        });

        let value = normalize(&body, ReasoningNormalize::ReasoningText);

        assert_eq!(
            value["input"][0]["content"][0],
            json!({"type":"reasoning_text","text":"先看代码再动手"})
        );
        assert!(value["input"][0].get("summary").is_none());
        assert_eq!(value["input"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn reasoning_text_mode_keeps_native_content() {
        let body = json!({
            "model":"deepseek-flash",
            "input":[{
                "type":"reasoning",
                "id":"11111111-2222-3333-4444-555555555555",
                "status":"completed",
                "summary":[],
                "content":[{"type":"reasoning_text","text":"原生思维链"}],
                "encrypted_content":"c86da72c-d95f-44e6-b660-6466483eed96-0"
            }]
        });

        let value = normalize(&body, ReasoningNormalize::ReasoningText);

        assert_eq!(
            value["input"][0]["content"][0],
            json!({"type":"reasoning_text","text":"原生思维链"})
        );
        assert_eq!(
            value["input"][0]["encrypted_content"],
            json!("c86da72c-d95f-44e6-b660-6466483eed96-0")
        );
    }

    #[test]
    fn reasoning_text_mode_decodes_internal_reasoning_first() {
        let body = json!({
            "model":"deepseek-flash",
            "input":[
                {
                    "type":"reasoning",
                    "id":"rs_1",
                    "summary":[{"type":"summary_text","text":"摘要文本"}],
                    "encrypted_content":"codex-switch-reasoning-v1:aGVsbG8"
                },
                {"type":"function_call","call_id":"call_1","name":"bash","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_1","output":"ok"}
            ]
        });

        let value = normalize(&body, ReasoningNormalize::ReasoningText);

        assert_eq!(
            value["input"][0]["content"][0],
            json!({"type":"reasoning_text","text":"hello"})
        );
        assert!(value["input"][0].get("encrypted_content").is_none());
    }

    #[test]
    fn reasoning_text_mode_synthesizes_when_history_has_none() {
        let body = json!({
            "model":"deepseek-flash",
            "input":[
                {"role":"user","content":"跑一下"},
                {"type":"function_call","call_id":"call_1","name":"bash","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_1","output":"ok"}
            ]
        });

        let value = normalize(&body, ReasoningNormalize::ReasoningText);
        let input = value["input"].as_array().unwrap();

        assert_eq!(input.len(), 4);
        assert_eq!(input[1]["type"], json!("reasoning"));
        assert_eq!(
            input[1]["content"][0],
            json!({"type":"reasoning_text","text":SYNTHETIC_REASONING_TEXT})
        );
    }

    #[test]
    fn reasoning_text_mode_skips_synthesis_when_reasoning_exists() {
        let body = json!({
            "model":"deepseek-flash",
            "input":[
                {"role":"user","content":"跑一下"},
                {"type":"reasoning","id":"rs_1","summary":[]},
                {"type":"function_call","call_id":"call_1","name":"bash","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_1","output":"ok"}
            ]
        });

        let value = normalize(&body, ReasoningNormalize::ReasoningText);
        let input = value["input"].as_array().unwrap();

        assert_eq!(input.len(), 4);
        assert_eq!(input[1]["type"], json!("reasoning"));
    }

    #[test]
    fn reasoning_text_mode_reports_restored_and_synthesized_counts() {
        let restored_body = json!({
            "model":"deepseek-flash",
            "input":[
                {"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"甲"}]},
                {"type":"reasoning","id":"rs_2","summary":[{"type":"summary_text","text":"乙"}]},
                {"type":"function_call","call_id":"call_1","name":"bash","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_1","output":"ok"}
            ]
        });
        let (_, stats) = normalize_with_stats(&restored_body, ReasoningNormalize::ReasoningText);
        assert_eq!(stats.restored, 2);
        assert_eq!(stats.synthesized, 0);

        let synthesized_body = json!({
            "model":"deepseek-flash",
            "input":[
                {"role":"user","content":"跑一下"},
                {"type":"function_call","call_id":"call_1","name":"bash","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_1","output":"ok"},
                {"type":"function_call","call_id":"call_2","name":"bash","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_2","output":"ok"}
            ]
        });
        let (value, stats) =
            normalize_with_stats(&synthesized_body, ReasoningNormalize::ReasoningText);
        assert_eq!(stats.restored, 0);
        assert_eq!(stats.synthesized, 2);
        assert_eq!(value["input"].as_array().unwrap().len(), 7);
    }
}
