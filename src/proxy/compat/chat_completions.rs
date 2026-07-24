mod shared;
mod stream;
mod reverse;
mod tools;
#[cfg(test)]
mod tests;

pub(crate) use stream::ChatSseConverter;
pub(crate) use reverse::{
    ResponsesToChatSseConverter, chat_to_responses_request_json,
    responses_response_to_chat_json,
};

use self::shared::*;
use self::tools::{TOOL_SEARCH_CHAT_NAME, ToolContext, ToolKind};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Default)]
pub(crate) struct ChatResponseContext {
    tool_context: ToolContext,
    model: Option<String>,
}

impl ChatResponseContext {
    pub(crate) fn from_responses_request(value: &Value) -> Self {
        Self {
            tool_context: ToolContext::from_request(value),
            model: value.get("model").and_then(Value::as_str).map(str::to_string),
        }
    }

    pub(crate) fn compatible_function_tools(&self) -> &[Value] {
        self.tool_context.chat_tools()
    }

    pub(crate) fn restore_tool_item(
        &self,
        item_id: &str,
        status: &str,
        call_id: &str,
        name: &str,
        arguments: &str,
    ) -> Value {
        response_tool_item_from_chat_name(
            item_id,
            status,
            call_id,
            name,
            arguments,
            self,
        )
    }

    fn is_custom_tool(&self, name: &str) -> bool {
        self.tool_context.is_custom_tool(name)
    }

    fn item_id_prefix(&self, name: &str) -> &'static str {
        if self.is_custom_tool(name) {
            "ctc"
        } else {
            "fc"
        }
    }
}

fn normalize_chat_role(role: &str) -> &str {
    if role == "developer" { "system" } else { role }
}

#[derive(Debug, Clone)]
pub(crate) struct ConvertedChatRequest {
    pub body: Vec<u8>,
    pub response_context: ChatResponseContext,
}

#[derive(Default)]
struct PendingAssistant {
    content: Option<Value>,
    reasoning_content: Option<String>,
    tool_calls: Vec<Value>,
}

impl PendingAssistant {
    fn has_content(&self) -> bool {
        self.content.is_some() || self.reasoning_content.is_some() || !self.tool_calls.is_empty()
    }

    fn flush(&mut self, messages: &mut Vec<Value>) {
        if !self.has_content() {
            return;
        }
        let mut message = Map::new();
        message.insert("role".to_string(), json!("assistant"));
        message.insert(
            "content".to_string(),
            self.content.take().unwrap_or(Value::Null),
        );
        if let Some(reasoning_content) = self.reasoning_content.take() {
            message.insert("reasoning_content".to_string(), json!(reasoning_content));
        }
        if !self.tool_calls.is_empty() {
            message.insert(
                "tool_calls".to_string(),
                Value::Array(std::mem::take(&mut self.tool_calls)),
            );
        }
        messages.push(Value::Object(message));
    }
}

pub(crate) fn normalize_chat_request_json(body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(body)?;
    let mut normalized_roles = 0;
    if let Some(messages) = value.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if message.get("role").and_then(Value::as_str) == Some("developer") {
                message["role"] = json!("system");
                normalized_roles += 1;
            }
        }
    }
    tracing::debug!(normalized_roles, "chat request roles normalized");
    Ok(serde_json::to_vec(&value)?)
}

pub(crate) fn responses_to_chat_json(body: &[u8]) -> anyhow::Result<ConvertedChatRequest> {
    let value: Value = serde_json::from_slice(body)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("responses request body must be a JSON object"))?;
    validate_responses_tools(object.get("tools"))?;
    let model = object
        .get("model")
        .cloned()
        .unwrap_or_else(|| json!("unknown"));
    let context = ChatResponseContext::from_responses_request(&value);
    let mut messages = Vec::new();
    if let Some(instructions) = object.get("instructions").and_then(Value::as_str) {
        messages.push(json!({"role":"system","content":instructions}));
    }

    let mut pending = PendingAssistant::default();
    match object.get("input") {
        Some(Value::String(text)) => {
            messages.push(json!({"role":"user","content":text}));
        }
        Some(Value::Array(items)) => {
            for item in items {
                append_input_item(item, &mut pending, &mut messages, &context)?;
            }
        }
        Some(other) => {
            messages.push(json!({"role":"user","content":json_text(other)}));
        }
        None => {}
    }
    pending.flush(&mut messages);
    if messages.is_empty() {
        messages.push(json!({"role":"user","content":""}));
    }

    let tools = context.tool_context.chat_tools();
    let mut result = Map::new();
    result.insert("model".to_string(), model);
    result.insert("messages".to_string(), Value::Array(messages));
    result.insert(
        "stream".to_string(),
        json!(object.get("stream").and_then(Value::as_bool).unwrap_or(false)),
    );
    if !tools.is_empty() {
        result.insert("tools".to_string(), Value::Array(tools.to_vec()));
        if let Some(parallel_tool_calls) = object.get("parallel_tool_calls") {
            result.insert(
                "parallel_tool_calls".to_string(),
                parallel_tool_calls.clone(),
            );
        }
        if let Some(tool_choice) = object.get("tool_choice") {
            result.insert(
                "tool_choice".to_string(),
                context.tool_context.tool_choice_to_chat(tool_choice),
            );
        }
    }
    copy_field(object, &mut result, "temperature", "temperature");
    copy_field(object, &mut result, "top_p", "top_p");
    copy_field(object, &mut result, "max_output_tokens", "max_tokens");
    copy_field(object, &mut result, "user", "user");
    if let Some(effort) = value.pointer("/reasoning/effort") {
        result.insert("reasoning_effort".to_string(), effort.clone());
    }
    if let Some(format) = value.pointer("/text/format") {
        result.insert("response_format".to_string(), convert_text_format(format));
    }
    if result.get("stream").and_then(Value::as_bool) == Some(true) {
        result.insert("stream_options".to_string(), json!({"include_usage":true}));
    }

    Ok(ConvertedChatRequest {
        body: serde_json::to_vec(&Value::Object(result))?,
        response_context: context,
    })
}

fn append_input_item(
    item: &Value,
    pending: &mut PendingAssistant,
    messages: &mut Vec<Value>,
    context: &ChatResponseContext,
) -> anyhow::Result<()> {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("message");
    match item_type {
        "reasoning" => {
            if let Some(reasoning) = reasoning_from_response_item(item) {
                pending.reasoning_content = Some(reasoning);
            }
        }
        "function_call" => {
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
            let namespace = item.get("namespace").and_then(Value::as_str);
            let chat_name = context
                .tool_context
                .chat_name_for_function(name, namespace);
            let call_id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(new_call_id);
            let arguments = argument_string(item.get("arguments"));
            pending.tool_calls.push(json!({
                "id":call_id,
                "type":"function",
                "function":{"name":chat_name,"arguments":arguments}
            }));
        }
        "custom_tool_call" => {
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
            let call_id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(new_call_id);
            let input = item.get("input").cloned().unwrap_or_else(|| json!(""));
            let arguments = json!({"input":input}).to_string();
            pending.tool_calls.push(json!({
                "id":call_id,
                "type":"function",
                "function":{"name":name,"arguments":arguments}
            }));
        }
        "tool_search_call" => {
            let call_id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(new_call_id);
            let arguments = argument_string(item.get("arguments"));
            pending.tool_calls.push(json!({
                "id":call_id,
                "type":"function",
                "function":{"name":TOOL_SEARCH_CHAT_NAME,"arguments":arguments}
            }));
        }
        "function_call_output" | "custom_tool_call_output" => {
            pending.flush(messages);
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let output = item.get("output").map(json_text).unwrap_or_default();
            messages.push(json!({"role":"tool","tool_call_id":call_id,"content":output}));
        }
        "tool_search_output" => {
            pending.flush(messages);
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            messages.push(json!({
                "role":"tool",
                "tool_call_id":call_id,
                "content":item.to_string()
            }));
        }
        "additional_tools" => {}
        "message" => {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = chat_content(item.get("content"))?;
            if role == "assistant" {
                if pending.content.is_some() {
                    pending.flush(messages);
                }
                pending.content = Some(content);
            } else {
                pending.flush(messages);
                let role = normalize_chat_role(role);
                messages.push(json!({"role":role,"content":content}));
            }
        }
        _ => {
            if item.get("role").is_some() {
                pending.flush(messages);
                let role = item
                    .get("role")
                    .and_then(Value::as_str)
                    .map(normalize_chat_role)
                    .unwrap_or("user");
                messages.push(json!({"role":role,"content":chat_content(item.get("content"))?}));
            }
        }
    }
    Ok(())
}

fn chat_content(content: Option<&Value>) -> anyhow::Result<Value> {
    let Some(content) = content else {
        return Ok(json!(""));
    };
    match content {
        Value::String(text) => Ok(json!(text)),
        Value::Array(parts) => {
            let mut converted = Vec::new();
            let mut has_media = false;
            for part in parts {
                let part_type = part.get("type").and_then(Value::as_str).unwrap_or("text");
                match part_type {
                    "input_text" | "output_text" | "text" => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            converted.push(json!({"type":"text","text":text}));
                        }
                    }
                    "input_image" | "image_url" => {
                        let url = part
                            .get("image_url")
                            .or_else(|| part.get("url"))
                            .and_then(|value| match value {
                                Value::String(url) => Some(url.as_str()),
                                Value::Object(object) => object.get("url").and_then(Value::as_str),
                                _ => None,
                            })
                            .ok_or_else(|| anyhow::anyhow!("image input is missing image_url"))?;
                        let mut image_url = json!({"url":url});
                        if let Some(detail) = part.get("detail") {
                            image_url["detail"] = detail.clone();
                        }
                        converted.push(json!({"type":"image_url","image_url":image_url}));
                        has_media = true;
                    }
                    kind => anyhow::bail!(
                        "Responses content block {kind} cannot be converted to Chat Completions"
                    ),
                }
            }
            if has_media {
                Ok(Value::Array(converted))
            } else {
                Ok(json!(converted
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")))
            }
        }
        other => Ok(json!(json_text(other))),
    }
}

fn validate_responses_tools(tools: Option<&Value>) -> anyhow::Result<()> {
    let Some(tools) = tools.and_then(Value::as_array) else {
        return Ok(());
    };
    for tool in tools {
        match tool.get("type").and_then(Value::as_str).unwrap_or("function") {
            "function" | "custom" | "namespace" | "tool_search" | "web_search"
            | "web_search_preview" => {}
            kind => anyhow::bail!(
                "Responses server tool {kind} cannot be converted to Chat Completions"
            ),
        }
    }
    Ok(())
}

fn convert_text_format(value: &Value) -> Value {
    if value.get("type").and_then(Value::as_str) != Some("json_schema") {
        return value.clone();
    }
    json!({
        "type":"json_schema",
        "json_schema":{
            "name":value.get("name").cloned().unwrap_or_else(|| json!("response")),
            "schema":value.get("schema").cloned().unwrap_or_else(|| json!({})),
            "strict":value.get("strict").cloned().unwrap_or(Value::Bool(false))
        }
    })
}

fn copy_field(source: &Map<String, Value>, target: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = source.get(from) {
        target.insert(to.to_string(), value.clone());
    }
}

pub(crate) fn chat_to_responses_json(value: &Value, context: &ChatResponseContext) -> Value {
    let response_id = response_id(value);
    let model = value
        .get("model")
        .cloned()
        .or_else(|| context.model.clone().map(Value::String))
        .unwrap_or_else(|| json!("unknown"));
    let message = value.pointer("/choices/0/message").unwrap_or(&Value::Null);
    let mut output = Vec::new();
    if let Some(reasoning) = reasoning_from_chat(message) {
        output.push(reasoning_item(&reasoning));
    }
    if let Some(text) = chat_message_text(message.get("content"))
        && !text.is_empty()
    {
        output.push(message_item(&text));
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        output.extend(
            tool_calls
                .iter()
                .map(|tool_call| response_tool_item(tool_call, context)),
        );
    }
    let incomplete = value.pointer("/choices/0/finish_reason").and_then(Value::as_str)
        == Some("length");
    let mut response = json!({
        "id":response_id,
        "object":"response",
        "model":model,
        "status":if incomplete { "incomplete" } else { "completed" },
        "output":output,
        "usage":normalize_chat_usage(value.get("usage"))
    });
    if incomplete {
        response["incomplete_details"] = json!({"reason":"max_output_tokens"});
    }
    response
}

fn response_tool_item(tool_call: &Value, context: &ChatResponseContext) -> Value {
    let name = tool_call
        .pointer("/function/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let call_id = tool_call
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(new_call_id);
    let arguments = argument_string(tool_call.pointer("/function/arguments"));
    let item_id = new_item_id(context.item_id_prefix(name));
    response_tool_item_from_chat_name(
        &item_id,
        "completed",
        &call_id,
        name,
        &arguments,
        context,
    )
}

pub(super) fn response_tool_item_from_chat_name(
    item_id: &str,
    status: &str,
    call_id: &str,
    chat_name: &str,
    arguments: &str,
    context: &ChatResponseContext,
) -> Value {
    match context.tool_context.lookup_chat_name(chat_name) {
        Some(spec) if spec.kind == ToolKind::Custom => json!({
            "id":item_id,
            "type":"custom_tool_call",
            "call_id":call_id,
            "name":spec.name,
            "input":custom_input(arguments),
            "status":status
        }),
        Some(spec) if spec.kind == ToolKind::ToolSearch => {
            let parsed_arguments = if arguments.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(arguments)
                    .ok()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({"query":arguments}))
            };
            json!({
                "type":"tool_search_call",
                "call_id":call_id,
                "status":status,
                "execution":"client",
                "arguments":parsed_arguments
            })
        }
        Some(spec) => {
            let mut item = json!({
                "id":item_id,
                "type":"function_call",
                "call_id":call_id,
                "name":spec.name,
                "arguments":arguments,
                "status":status
            });
            if let Some(namespace) = spec.namespace.as_ref() {
                item["namespace"] = json!(namespace);
            }
            item
        }
        None => json!({
            "id":item_id,
            "type":"function_call",
            "call_id":call_id,
            "name":chat_name,
            "arguments":arguments,
            "status":status
        }),
    }
}
