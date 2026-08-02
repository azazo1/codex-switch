use super::{
    AnthropicToResponsesSseConverter, ChatResponseContext, ChatSseConverter,
    ResponsesToAnthropicSseConverter, ResponsesToChatSseConverter,
    anthropic_to_responses_request_json, anthropic_to_responses_response_json,
    chat_to_responses_json, chat_to_responses_request_json, normalize_chat_request_json,
    responses_response_to_chat_json, responses_to_anthropic_request_json,
    responses_to_anthropic_response_json, responses_to_chat_json,
};
use super::namespace::NamespaceToolMap;
use crate::core::models::WireApi;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct ProtocolConversionError(String);

pub(crate) struct PreparedProtocolRequest {
    pub body: Vec<u8>,
    pub sse_bridge: ProtocolSseBridge,
    client_api: WireApi,
    upstream_api: WireApi,
    chat_context: Option<ChatResponseContext>,
    model: Option<String>,
    response_model: Option<String>,
}

impl PreparedProtocolRequest {
    pub(crate) fn new(
        client_api: WireApi,
        upstream_api: WireApi,
        body: &[u8],
        model: Option<String>,
    ) -> Result<Self, ProtocolConversionError> {
        if client_api == upstream_api {
            let body = if matches!(client_api, WireApi::Responses | WireApi::ChatCompletions) {
                convert(normalize_chat_request_json(body))?
            } else {
                body.to_vec()
            };
            let namespace_tools =
                NamespaceToolMap::from_request(&serde_json::from_slice(&body).unwrap_or(Value::Null));
            return Ok(Self {
                body,
                sse_bridge: ProtocolSseBridge::passthrough(
                    client_api,
                    model.clone(),
                    namespace_tools,
                ),
                client_api,
                upstream_api,
                chat_context: None,
                model,
                response_model: None,
            });
        }

        let canonical = match client_api {
            WireApi::Responses => body.to_vec(),
            WireApi::ChatCompletions => convert(chat_to_responses_request_json(body))?,
            WireApi::AnthropicMessages => convert(anthropic_to_responses_request_json(body))?,
        };
        let canonical_value = serde_json::from_slice::<Value>(&canonical)
            .map_err(|error| ProtocolConversionError(error.to_string()))?;
        let mut response_context = ChatResponseContext::from_responses_request(&canonical_value);
        let namespace_tools = NamespaceToolMap::from_request(&canonical_value);
        let body = match upstream_api {
            WireApi::Responses => canonical,
            WireApi::ChatCompletions => {
                let converted = convert(responses_to_chat_json(&canonical))?;
                response_context = converted.response_context;
                converted.body
            }
            WireApi::AnthropicMessages => convert(responses_to_anthropic_request_json(&canonical))?,
        };
        let sse_bridge = ProtocolSseBridge::new(
            client_api,
            upstream_api,
            Some(response_context.clone()),
            model.clone(),
            namespace_tools,
        );
        Ok(Self {
            body,
            sse_bridge,
            client_api,
            upstream_api,
            chat_context: Some(response_context),
            model,
            response_model: None,
        })
    }

    pub(crate) fn convert_json_response(
        &self,
        value: &Value,
    ) -> Result<Value, ProtocolConversionError> {
        if self.client_api == self.upstream_api {
            let mut value = value.clone();
            self.sse_bridge.restore_response_namespaces(&mut value);
            return Ok(value);
        }
        let canonical = match self.upstream_api {
            WireApi::Responses => value.clone(),
            WireApi::ChatCompletions => chat_to_responses_json(
                value,
                self.chat_context
                    .as_ref()
                    .ok_or_else(|| ProtocolConversionError("missing Chat response context".to_string()))?,
            ),
            WireApi::AnthropicMessages => anthropic_to_responses_response_json(
                value,
                self.chat_context.as_ref(),
            ),
        };
        Ok(match self.client_api {
            WireApi::Responses => canonical,
            WireApi::ChatCompletions => responses_response_to_chat_json(&canonical),
            WireApi::AnthropicMessages => {
                responses_to_anthropic_response_json(&canonical, self.model.as_deref())
            }
        })
    }

    pub(crate) fn is_passthrough(&self) -> bool {
        self.client_api == self.upstream_api
    }

    pub(crate) fn override_response_model(&mut self, model: String) {
        self.response_model = Some(model.clone());
        self.sse_bridge.response_model = Some(model);
    }

    pub(crate) fn response_model_override(&self) -> Option<&str> {
        self.response_model.as_deref()
    }
}

pub(crate) struct ProtocolSseBridge {
    decoder: UpstreamDecoder,
    encoder: ClientEncoder,
    canonical_buffer: Vec<u8>,
    passthrough: bool,
    response_model: Option<String>,
    namespace_tools: NamespaceToolMap,
}

enum UpstreamDecoder {
    Responses,
    Chat(ChatSseConverter),
    Anthropic(AnthropicToResponsesSseConverter),
}

enum ClientEncoder {
    Responses,
    Chat(ResponsesToChatSseConverter),
    Anthropic(ResponsesToAnthropicSseConverter),
}

impl ProtocolSseBridge {
    fn passthrough(api: WireApi, model: Option<String>, namespace_tools: NamespaceToolMap) -> Self {
        Self {
            decoder: decoder(api, None),
            encoder: encoder(api, model),
            canonical_buffer: Vec::new(),
            passthrough: true,
            response_model: None,
            namespace_tools,
        }
    }

    fn new(
        client_api: WireApi,
        upstream_api: WireApi,
        chat_context: Option<ChatResponseContext>,
        model: Option<String>,
        namespace_tools: NamespaceToolMap,
    ) -> Self {
        Self {
            decoder: decoder(upstream_api, chat_context),
            encoder: encoder(client_api, model),
            canonical_buffer: Vec::new(),
            passthrough: client_api == upstream_api,
            response_model: None,
            namespace_tools,
        }
    }

    pub(crate) fn is_passthrough(&self) -> bool {
        self.passthrough
    }

    pub(crate) fn initial_events(&mut self) -> Vec<u8> {
        if self.passthrough {
            return Vec::new();
        }
        let canonical = match &mut self.decoder {
            UpstreamDecoder::Chat(converter) => converter.initial_events(),
            _ => Vec::new(),
        };
        self.encode_canonical(&canonical)
    }

    pub(crate) fn push_block(&mut self, block: &[u8]) -> Vec<u8> {
        if self.passthrough {
            let mut output = match &self.response_model {
                Some(model) => rewrite_sse_model(block, model),
                None => block.to_vec(),
            };
            output = self.namespace_tools.rewrite_sse_block(&output);
            output.extend_from_slice(b"\n\n");
            return output;
        }
        let text = String::from_utf8_lossy(block);
        let canonical = match &mut self.decoder {
            UpstreamDecoder::Responses => {
                let mut output = block.to_vec();
                output.extend_from_slice(b"\n\n");
                output
            }
            UpstreamDecoder::Chat(converter) => converter.convert_block(&text),
            UpstreamDecoder::Anthropic(converter) => converter.push(&text),
        };
        self.encode_canonical(&canonical)
    }

    fn restore_response_namespaces(&self, value: &mut Value) -> bool {
        self.namespace_tools.restore_response_value(value)
    }

    pub(crate) fn finish(&mut self) -> Vec<u8> {
        if self.passthrough {
            return Vec::new();
        }
        let canonical = match &mut self.decoder {
            UpstreamDecoder::Responses => Vec::new(),
            UpstreamDecoder::Chat(converter) => converter.finish(),
            UpstreamDecoder::Anthropic(converter) => converter.finish(),
        };
        let mut output = self.encode_canonical(&canonical);
        let remaining = std::mem::take(&mut self.canonical_buffer);
        if !remaining.is_empty() {
            output.extend_from_slice(&self.encode_block(&remaining));
        }
        match &mut self.encoder {
            ClientEncoder::Responses => {}
            ClientEncoder::Chat(converter) => {
                output.extend_from_slice(converter.finish_eof().as_bytes());
            }
            ClientEncoder::Anthropic(converter) => {
                output.extend_from_slice(&converter.finish());
            }
        }
        output
    }

    fn encode_canonical(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.canonical_buffer.extend_from_slice(bytes);
        let mut output = Vec::new();
        while let Some((index, separator_len)) = find_sse_separator(&self.canonical_buffer) {
            let block = self.canonical_buffer[..index].to_vec();
            self.canonical_buffer.drain(..index + separator_len);
            output.extend_from_slice(&self.encode_block(&block));
        }
        output
    }

    fn encode_block(&mut self, block: &[u8]) -> Vec<u8> {
        let mut output = match &mut self.encoder {
            ClientEncoder::Responses => {
                let mut output = block.to_vec();
                output.extend_from_slice(b"\n\n");
                output
            }
            ClientEncoder::Chat(converter) => converter
                .push(&String::from_utf8_lossy(block))
                .into_bytes(),
            ClientEncoder::Anthropic(converter) => {
                converter.push(&String::from_utf8_lossy(block))
            }
        };
        if let Some(model) = &self.response_model {
            output = rewrite_sse_model(&output, model);
        }
        output
    }
}

fn rewrite_sse_model(block: &[u8], model: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(block);
    let mut rewritten = Vec::new();
    for line in text.split('\n') {
        let trimmed = line.trim_end_matches('\r');
        if let Some(data) = trimmed.strip_prefix("data:") {
            let payload = data.trim();
            if !payload.is_empty()
                && payload != "[DONE]"
                && let Ok(json) = crate::proxy::transform::rewrite_response_model(payload.as_bytes(), model)
            {
                let json = String::from_utf8_lossy(&json);
                rewritten.extend_from_slice(format!("data: {json}\n").as_bytes());
                continue;
            }
        }
        rewritten.extend_from_slice(line.as_bytes());
        rewritten.push(b'\n');
    }
    rewritten
}

fn decoder(api: WireApi, chat_context: Option<ChatResponseContext>) -> UpstreamDecoder {
    match api {
        WireApi::Responses => UpstreamDecoder::Responses,
        WireApi::ChatCompletions => UpstreamDecoder::Chat(
            ChatSseConverter::new(chat_context.unwrap_or_default()),
        ),
        WireApi::AnthropicMessages => {
            UpstreamDecoder::Anthropic(AnthropicToResponsesSseConverter::new(chat_context))
        }
    }
}

fn encoder(api: WireApi, model: Option<String>) -> ClientEncoder {
    match api {
        WireApi::Responses => ClientEncoder::Responses,
        WireApi::ChatCompletions => ClientEncoder::Chat(ResponsesToChatSseConverter::new()),
        WireApi::AnthropicMessages => {
            ClientEncoder::Anthropic(ResponsesToAnthropicSseConverter::new(model))
        }
    }
}

fn find_sse_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left <= right { (left, 2) } else { (right, 4) }),
        (Some(index), None) => Some((index, 2)),
        (None, Some(index)) => Some((index, 4)),
        (None, None) => None,
    }
}

fn convert<T>(result: anyhow::Result<T>) -> Result<T, ProtocolConversionError> {
    result.map_err(|error| ProtocolConversionError(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_bridge_restores_custom_and_namespace_tool_identity() {
        let request = serde_json::to_vec(&json!({
            "model":"claude-test",
            "input":"patch the file",
            "tools":[
                {
                    "type":"custom",
                    "name":"exec",
                    "description":"Run freeform input",
                    "format":{"type":"grammar","syntax":"lark","definition":"start: source"}
                },
                {
                    "type":"namespace",
                    "name":"collaboration",
                    "tools":[{
                        "type":"function",
                        "name":"spawn_agent",
                        "parameters":{"type":"object","properties":{}}
                    }]
                }
            ]
        }))
        .unwrap();
        let mut prepared = PreparedProtocolRequest::new(
            WireApi::Responses,
            WireApi::AnthropicMessages,
            &request,
            Some("claude-test".to_string()),
        )
        .unwrap();
        let upstream_request: Value = serde_json::from_slice(&prepared.body).unwrap();

        assert_eq!(upstream_request.pointer("/tools/0/name"), Some(&json!("exec")));
        assert_eq!(
            upstream_request.pointer("/tools/1/name"),
            Some(&json!("collaboration__spawn_agent"))
        );

        let response = json!({
            "id":"msg_1",
            "type":"message",
            "model":"claude-test",
            "content":[
                {"type":"tool_use","id":"toolu_1","name":"exec","input":{"input":"await tools.apply_patch()"}},
                {"type":"tool_use","id":"toolu_2","name":"collaboration__spawn_agent","input":{"task":"inspect"}}
            ],
            "stop_reason":"tool_use",
            "usage":{"input_tokens":1,"output_tokens":1}
        });
        let converted = prepared.convert_json_response(&response).unwrap();

        assert_eq!(converted.pointer("/output/0/type"), Some(&json!("custom_tool_call")));
        assert_eq!(converted.pointer("/output/0/name"), Some(&json!("exec")));
        assert_eq!(
            converted.pointer("/output/0/input"),
            Some(&json!("await tools.apply_patch()"))
        );
        assert_eq!(converted.pointer("/output/1/type"), Some(&json!("function_call")));
        assert_eq!(
            converted.pointer("/output/1/namespace"),
            Some(&json!("collaboration"))
        );
        assert_eq!(converted.pointer("/output/1/name"), Some(&json!("spawn_agent")));

        let blocks = [
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_2\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":1}}}",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_3\",\"name\":\"exec\",\"input\":{}}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"input\\\":\\\"stream patch\\\"}\"}}",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":1}}",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}",
        ];
        let mut stream = Vec::new();
        for block in blocks {
            stream.extend_from_slice(&prepared.sse_bridge.push_block(block.as_bytes()));
        }
        stream.extend_from_slice(&prepared.sse_bridge.finish());
        let stream = String::from_utf8(stream).unwrap();

        assert!(stream.contains("\"type\":\"custom_tool_call\""));
        assert!(stream.contains("response.custom_tool_call_input.delta"));
        assert!(stream.contains("stream patch"));
    }

    #[test]
    fn passthrough_bridge_rewrites_response_model() {
        let request = br#"{"model":"gpt-5.4","input":"hello"}"#;
        let mut prepared = PreparedProtocolRequest::new(
            WireApi::Responses,
            WireApi::Responses,
            request,
            Some("gpt-5.4".to_string()),
        )
        .unwrap();
        prepared.override_response_model("gpt-5.4".to_string());

        let block = b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\",\"model\":\"deepseek-v4-flash\"}}";
        let output = prepared.sse_bridge.push_block(block);
        let text = String::from_utf8(output).unwrap();

        assert!(text.contains("\"model\":\"gpt-5.4\""));
        assert!(!text.contains("deepseek-v4-flash"));
    }
}
