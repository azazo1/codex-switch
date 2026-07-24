mod request;
mod response;
mod stream;

pub(crate) use request::{
    anthropic_to_responses_request_json, responses_to_anthropic_request_json,
};
pub(crate) use response::{
    anthropic_to_responses_response_json, error_response_json,
    responses_to_anthropic_response_json,
};
pub(crate) use stream::{
    AnthropicToResponsesSseConverter, ResponsesToAnthropicSseConverter,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn converts_anthropic_request_with_image_tools_and_results() {
        let body = anthropic_to_responses_request_json(
            serde_json::to_vec(&json!({
                "model":"claude-test",
                "system":[{"type":"text","text":"rules","cache_control":{"type":"ephemeral"}}],
                "messages":[
                    {"role":"assistant","content":[
                        {"type":"thinking","thinking":"plan","signature":"signed"},
                        {"type":"tool_use","id":"toolu_1","name":"lookup","input":{"q":"x"}}
                    ]},
                    {"role":"user","content":[
                        {"type":"tool_result","tool_use_id":"toolu_1","content":"done"},
                        {"type":"image","source":{"type":"url","url":"https://example.com/a.png"}}
                    ]}
                ],
                "tools":[{"name":"lookup","input_schema":{"type":"object","properties":{}}}],
                "tool_choice":{"type":"tool","name":"lookup"},
                "output_config":{"effort":"max"},
                "max_tokens":64
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["max_output_tokens"], 64);
        assert_eq!(value.pointer("/reasoning/effort"), Some(&json!("xhigh")));
        assert_eq!(value.pointer("/tools/0/type"), Some(&json!("function")));
        assert!(value["input"].as_array().unwrap().iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str) == Some("toolu_1")
        }));
        assert!(value["input"].as_array().unwrap().iter().any(|item| {
            item.pointer("/content/0/image_url").and_then(Value::as_str)
                == Some("https://example.com/a.png")
        }));
    }

    #[test]
    fn converts_responses_request_and_repairs_parallel_tool_pairing() {
        let body = responses_to_anthropic_request_json(
            serde_json::to_vec(&json!({
                "model":"gpt-test",
                "input":[
                    {"type":"function_call","call_id":"call_1","name":"a","arguments":"{}"},
                    {"type":"function_call","call_id":"call_2","name":"b","arguments":"{}"},
                    {"type":"message","role":"developer","content":[{"type":"input_text","text":"notice"}]},
                    {"type":"function_call_output","call_id":"call_2","output":"ok"},
                    {"type":"function_call_output","call_id":"call_1","output":"done"}
                ],
                "max_output_tokens":128
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value.pointer("/system/0/text"), Some(&json!("notice")));
        assert_eq!(value.pointer("/messages/0/role"), Some(&json!("user")));
        assert_eq!(value.pointer("/messages/1/role"), Some(&json!("assistant")));
        assert_eq!(value.pointer("/messages/1/content/0/id"), Some(&json!("call_1")));
        assert_eq!(value.pointer("/messages/1/content/1/id"), Some(&json!("call_2")));
        assert_eq!(value.pointer("/messages/2/content/0/tool_use_id"), Some(&json!("call_1")));
        assert_eq!(value.pointer("/messages/2/content/1/tool_use_id"), Some(&json!("call_2")));
    }

    #[test]
    fn rejects_cross_protocol_document_content() {
        let error = anthropic_to_responses_request_json(
            br#"{"model":"claude-test","messages":[{"role":"user","content":[{"type":"document","source":{"type":"base64","data":"x"}}]}],"max_tokens":16}"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("document"));
    }

    #[test]
    fn converts_fragmented_anthropic_sse_with_thinking_and_tool_input() {
        let stream = concat!(
            "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":3,\"cache_read_input_tokens\":5}}}\r\n\r\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"lookup\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\\\"x\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        let mut converter = AnthropicToResponsesSseConverter::new(None);
        let mut buffer = Vec::new();
        let mut output = Vec::new();
        for chunk in stream.as_bytes().chunks(17) {
            buffer.extend_from_slice(chunk);
            while let Some((index, length)) = separator(&buffer) {
                let block = buffer[..index].to_vec();
                buffer.drain(..index + length);
                output.extend_from_slice(&converter.push(&String::from_utf8(block).unwrap()));
            }
        }
        output.extend_from_slice(&converter.finish());
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("response.reasoning_summary_text.delta"));
        assert!(output.contains("response.function_call_arguments.delta"));
        assert!(output.contains("\"cached_tokens\":5"));
        assert!(output.contains("response.completed"));
    }

    #[test]
    fn converts_responses_failure_to_anthropic_error_event() {
        let mut converter = ResponsesToAnthropicSseConverter::new(Some("claude-test".to_string()));
        let output = converter.push(concat!(
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_is_overloaded\",\"message\":\"busy\"}}}"
        ));
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("event: error"));
        assert!(output.contains("server_is_overloaded"));
        assert!(!output.contains("message_stop"));
    }

    fn separator(buffer: &[u8]) -> Option<(usize, usize)> {
        let lf = buffer.windows(2).position(|window| window == b"\n\n");
        let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
        match (lf, crlf) {
            (Some(left), Some(right)) => Some(if left <= right { (left, 2) } else { (right, 4) }),
            (Some(index), None) => Some((index, 2)),
            (None, Some(index)) => Some((index, 4)),
            (None, None) => None,
        }
    }
}
