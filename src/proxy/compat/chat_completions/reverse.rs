use serde_json::{Map, Value, json};

pub(crate) fn chat_to_responses_request_json(body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(body)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("chat request body must be a JSON object"))?;
    if object.get("n").and_then(Value::as_u64).unwrap_or(1) != 1 {
        anyhow::bail!("chat requests with n greater than 1 cannot be converted");
    }
    for unsupported in ["audio", "modalities", "logprobs"] {
        if object.get(unsupported).is_some_and(|value| !value.is_null()) {
            anyhow::bail!("chat field {unsupported} cannot be converted");
        }
    }

    let mut input = Vec::new();
    if let Some(messages) = object.get("messages").and_then(Value::as_array) {
        for message in messages {
            append_chat_message(message, &mut input)?;
        }
    }
    let mut result = Map::new();
    copy_field(object, &mut result, "model", "model");
    result.insert("input".to_string(), Value::Array(input));
    result.insert(
        "stream".to_string(),
        json!(object.get("stream").and_then(Value::as_bool).unwrap_or(false)),
    );
    result.insert("store".to_string(), Value::Bool(false));
    copy_field(object, &mut result, "temperature", "temperature");
    copy_field(object, &mut result, "top_p", "top_p");
    copy_field(object, &mut result, "max_tokens", "max_output_tokens");
    copy_field(object, &mut result, "max_completion_tokens", "max_output_tokens");
    copy_field(object, &mut result, "user", "user");
    if object.get("stop").is_some_and(|value| !value.is_null()) {
        anyhow::bail!("chat stop sequences cannot be converted");
    }
    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        let mut converted = Vec::new();
        for tool in tools {
            match tool.get("type").and_then(Value::as_str).unwrap_or("function") {
                "function" => {
                    let function = tool.get("function").ok_or_else(|| {
                        anyhow::anyhow!("chat function tool is missing function definition")
                    })?;
                    converted.push(json!({
                        "type":"function",
                        "name":function.get("name").cloned().unwrap_or_else(|| json!("")),
                        "description":function.get("description").cloned().unwrap_or(Value::Null),
                        "parameters":function.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object","properties":{}})),
                        "strict":function.get("strict").cloned().unwrap_or(Value::Bool(false))
                    }));
                }
                "web_search" | "web_search_preview" => converted.push(tool.clone()),
                kind => anyhow::bail!("chat server tool {kind} cannot be converted"),
            }
        }
        if !converted.is_empty() {
            result.insert("tools".to_string(), Value::Array(converted));
        }
    }
    if let Some(choice) = object.get("tool_choice") {
        result.insert("tool_choice".to_string(), chat_tool_choice_to_responses(choice));
    }
    if let Some(parallel) = object.get("parallel_tool_calls") {
        result.insert("parallel_tool_calls".to_string(), parallel.clone());
    }
    if let Some(format) = object.get("response_format") {
        result.insert("text".to_string(), json!({"format":chat_format_to_responses(format)}));
    }
    if let Some(effort) = object.get("reasoning_effort") {
        result.insert("reasoning".to_string(), json!({"effort":effort,"summary":"auto"}));
        result.insert("include".to_string(), json!(["reasoning.encrypted_content"]));
    }
    Ok(serde_json::to_vec(&Value::Object(result))?)
}

fn append_chat_message(message: &Value, input: &mut Vec<Value>) -> anyhow::Result<()> {
    let role = message.get("role").and_then(Value::as_str).unwrap_or("user");
    match role {
        "tool" => {
            let call_id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            input.push(json!({
                "type":"function_call_output",
                "call_id":call_id,
                "output":chat_text(message.get("content"))
            }));
        }
        "assistant" => {
            if let Some(reasoning) = message
                .get("reasoning_content")
                .or_else(|| message.get("reasoning"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                input.push(json!({
                    "type":"reasoning",
                    "summary":[{"type":"summary_text","text":reasoning}]
                }));
            }
            let parts = chat_content_to_responses(message.get("content"), true)?;
            if !parts.is_empty() {
                input.push(json!({"type":"message","role":"assistant","content":parts}));
            }
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let function = call.get("function").unwrap_or(&Value::Null);
                    input.push(json!({
                        "type":"function_call",
                        "call_id":call.get("id").cloned().unwrap_or_else(|| json!(uuid::Uuid::new_v4().to_string())),
                        "name":function.get("name").cloned().unwrap_or_else(|| json!("")),
                        "arguments":argument_string(function.get("arguments"))
                    }));
                }
            }
        }
        "system" | "developer" | "user" => {
            let output = role == "assistant";
            let parts = chat_content_to_responses(message.get("content"), output)?;
            if !parts.is_empty() {
                let mapped_role = if role == "developer" { "developer" } else { role };
                input.push(json!({"type":"message","role":mapped_role,"content":parts}));
            }
        }
        _ => anyhow::bail!("chat role {role} cannot be converted"),
    }
    Ok(())
}

fn chat_content_to_responses(content: Option<&Value>, output: bool) -> anyhow::Result<Vec<Value>> {
    let text_type = if output { "output_text" } else { "input_text" };
    match content {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(if text.is_empty() {
            Vec::new()
        } else {
            vec![json!({"type":text_type,"text":text})]
        }),
        Some(Value::Array(parts)) => {
            let mut result = Vec::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str).unwrap_or("text") {
                    "text" => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            result.push(json!({"type":text_type,"text":text}));
                        }
                    }
                    "image_url" if !output => {
                        let url = part
                            .pointer("/image_url/url")
                            .or_else(|| part.get("image_url"))
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow::anyhow!("chat image_url is missing a URL"))?;
                        result.push(json!({"type":"input_image","image_url":url}));
                    }
                    kind => anyhow::bail!("chat content block {kind} cannot be converted"),
                }
            }
            Ok(result)
        }
        Some(_) => anyhow::bail!("chat message content cannot be converted"),
    }
}

pub(crate) fn responses_response_to_chat_json(value: &Value) -> Value {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str).unwrap_or_default() {
                "message" => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            if part.get("type").and_then(Value::as_str) == Some("output_text")
                                && let Some(text) = part.get("text").and_then(Value::as_str)
                            {
                                content.push_str(text);
                            }
                        }
                    }
                }
                "reasoning" => {
                    if let Some(parts) = item.get("summary").and_then(Value::as_array) {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                reasoning.push_str(text);
                            }
                        }
                    }
                }
                "function_call" | "custom_tool_call" => {
                    let arguments = if item.get("type").and_then(Value::as_str)
                        == Some("custom_tool_call")
                    {
                        json!({"input":item.get("input").cloned().unwrap_or(Value::Null)}).to_string()
                    } else {
                        argument_string(item.get("arguments"))
                    };
                    tool_calls.push(json!({
                        "id":item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or_else(|| json!(uuid::Uuid::new_v4().to_string())),
                        "type":"function",
                        "function":{
                            "name":item.get("name").cloned().unwrap_or_else(|| json!("")),
                            "arguments":arguments
                        }
                    }));
                }
                _ => {}
            }
        }
    }
    let mut message = Map::new();
    message.insert("role".to_string(), json!("assistant"));
    message.insert("content".to_string(), json!(content));
    if !reasoning.is_empty() {
        message.insert("reasoning_content".to_string(), json!(reasoning));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_string(), Value::Array(tool_calls.clone()));
    }
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else if value.get("status").and_then(Value::as_str) == Some("incomplete") {
        "length"
    } else {
        "stop"
    };
    json!({
        "id":value.get("id").cloned().unwrap_or_else(|| json!(format!("chatcmpl-{}", uuid::Uuid::new_v4()))),
        "object":"chat.completion",
        "created":chrono::Utc::now().timestamp(),
        "model":value.get("model").cloned().unwrap_or_else(|| json!("unknown")),
        "choices":[{"index":0,"message":Value::Object(message),"finish_reason":finish_reason}],
        "usage":responses_usage_to_chat(value.get("usage"))
    })
}

pub(crate) struct ResponsesToChatSseConverter {
    id: String,
    model: String,
    role_sent: bool,
    done: bool,
    tool_indexes: std::collections::BTreeMap<usize, usize>,
    next_tool_index: usize,
}

impl ResponsesToChatSseConverter {
    pub(crate) fn new() -> Self {
        Self {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            model: "unknown".to_string(),
            role_sent: false,
            done: false,
            tool_indexes: std::collections::BTreeMap::new(),
            next_tool_index: 0,
        }
    }

    pub(crate) fn push(&mut self, block: &str) -> String {
        let Some(value) = sse_value(block) else {
            return String::new();
        };
        let event_type = value.get("type").and_then(Value::as_str).unwrap_or_default();
        match event_type {
            "response.created" => {
                if let Some(response) = value.get("response") {
                    if let Some(id) = response.get("id").and_then(Value::as_str) {
                        self.id = id.to_string();
                    }
                    if let Some(model) = response.get("model").and_then(Value::as_str) {
                        self.model = model.to_string();
                    }
                }
                self.role_chunk()
            }
            "response.output_text.delta" => {
                let mut output = self.role_chunk();
                output.push_str(&self.chunk(json!({"content":value.get("delta").cloned().unwrap_or_else(|| json!(""))}), Value::Null, None));
                output
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let mut output = self.role_chunk();
                output.push_str(&self.chunk(json!({"reasoning_content":value.get("delta").cloned().unwrap_or_else(|| json!(""))}), Value::Null, None));
                output
            }
            "response.output_item.added" => self.tool_start(&value),
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                self.tool_delta(&value)
            }
            "response.completed" | "response.done" | "response.incomplete" => {
                self.finish(&value)
            }
            "response.failed" => self.fail(&value),
            _ => String::new(),
        }
    }

    pub(crate) fn finish_eof(&mut self) -> String {
        if self.done {
            String::new()
        } else {
            self.finish_with_reason("stop", None)
        }
    }

    fn role_chunk(&mut self) -> String {
        if self.role_sent {
            return String::new();
        }
        self.role_sent = true;
        self.chunk(json!({"role":"assistant","content":""}), Value::Null, None)
    }

    fn tool_start(&mut self, value: &Value) -> String {
        let Some(item) = value.get("item") else {
            return String::new();
        };
        if !matches!(item.get("type").and_then(Value::as_str), Some("function_call" | "custom_tool_call")) {
            return String::new();
        }
        let output_index = value.get("output_index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let tool_index = self.next_tool_index;
        self.next_tool_index += 1;
        self.tool_indexes.insert(output_index, tool_index);
        let mut output = self.role_chunk();
        output.push_str(&self.chunk(json!({"tool_calls":[{
            "index":tool_index,
            "id":item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or_else(|| json!(uuid::Uuid::new_v4().to_string())),
            "type":"function",
            "function":{"name":item.get("name").cloned().unwrap_or_else(|| json!("")),"arguments":""}
        }]}), Value::Null, None));
        output
    }

    fn tool_delta(&self, value: &Value) -> String {
        let output_index = value.get("output_index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let Some(tool_index) = self.tool_indexes.get(&output_index) else {
            return String::new();
        };
        self.chunk(json!({"tool_calls":[{
            "index":tool_index,
            "function":{"arguments":value.get("delta").cloned().unwrap_or_else(|| json!(""))}
        }]}), Value::Null, None)
    }

    fn finish(&mut self, value: &Value) -> String {
        let response = value.get("response").unwrap_or(&Value::Null);
        let reason = if response.get("status").and_then(Value::as_str) == Some("incomplete") {
            "length"
        } else if !self.tool_indexes.is_empty() {
            "tool_calls"
        } else {
            "stop"
        };
        self.finish_with_reason(reason, response.get("usage").or_else(|| value.get("usage")))
    }

    fn finish_with_reason(&mut self, reason: &str, usage: Option<&Value>) -> String {
        if self.done {
            return String::new();
        }
        self.done = true;
        let mut output = self.role_chunk();
        output.push_str(&self.chunk(json!({}), json!(reason), usage));
        output.push_str("data: [DONE]\n\n");
        output
    }

    fn fail(&mut self, value: &Value) -> String {
        if self.done {
            return String::new();
        }
        self.done = true;
        let error = value
            .pointer("/response/error")
            .or_else(|| value.get("error"))
            .cloned()
            .unwrap_or_else(|| json!({"type":"api_error","message":"Responses stream failed"}));
        format!("data: {}\n\ndata: [DONE]\n\n", json!({"error":error}))
    }

    fn chunk(&self, delta: Value, finish_reason: Value, usage: Option<&Value>) -> String {
        let value = json!({
            "id":self.id,
            "object":"chat.completion.chunk",
            "created":chrono::Utc::now().timestamp(),
            "model":self.model,
            "choices":[{"index":0,"delta":delta,"finish_reason":finish_reason}],
            "usage":responses_usage_to_chat(usage)
        });
        format!("data: {value}\n\n")
    }
}

fn responses_usage_to_chat(usage: Option<&Value>) -> Value {
    let input = usage.and_then(|value| value.get("input_tokens")).and_then(Value::as_i64).unwrap_or(0);
    let output = usage.and_then(|value| value.get("output_tokens")).and_then(Value::as_i64).unwrap_or(0);
    let cached = usage
        .and_then(|value| value.pointer("/input_tokens_details/cached_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    json!({
        "prompt_tokens":input,
        "completion_tokens":output,
        "total_tokens":input + output,
        "prompt_tokens_details":{"cached_tokens":cached}
    })
}

fn sse_value(block: &str) -> Option<Value> {
    block.lines().find_map(|line| {
        line.strip_prefix("data:")
            .map(str::trim)
            .filter(|data| !data.is_empty() && *data != "[DONE]")
            .and_then(|data| serde_json::from_str(data).ok())
    })
}

fn chat_tool_choice_to_responses(value: &Value) -> Value {
    if let Some(kind) = value.as_str() {
        return json!(match kind {
            "required" => "required",
            "none" => "none",
            _ => "auto",
        });
    }
    if value.get("type").and_then(Value::as_str) == Some("function") {
        return json!({
            "type":"function",
            "name":value.pointer("/function/name").cloned().unwrap_or_else(|| json!(""))
        });
    }
    value.clone()
}

fn chat_format_to_responses(value: &Value) -> Value {
    if value.get("type").and_then(Value::as_str) == Some("json_schema") {
        let schema = value.get("json_schema").unwrap_or(&Value::Null);
        return json!({
            "type":"json_schema",
            "name":schema.get("name").cloned().unwrap_or_else(|| json!("response")),
            "schema":schema.get("schema").cloned().unwrap_or_else(|| json!({})),
            "strict":schema.get("strict").cloned().unwrap_or(Value::Bool(false))
        });
    }
    value.clone()
}

fn copy_field(source: &Map<String, Value>, target: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = source.get(from) {
        target.insert(to.to_string(), value.clone());
    }
}

fn chat_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn argument_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(value) if !value.is_null() => value.to_string(),
        _ => "{}".to_string(),
    }
}
