use super::super::ChatResponseContext;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub(crate) fn anthropic_to_responses_request_json(body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(body)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("anthropic request body must be a JSON object"))?;
    let mut input = Vec::new();
    if let Some(system) = object.get("system") {
        let parts = anthropic_system_parts(system)?;
        if !parts.is_empty() {
            input.push(json!({"type":"message","role":"developer","content":parts}));
        }
    }
    if let Some(messages) = object.get("messages").and_then(Value::as_array) {
        for message in messages {
            append_anthropic_message(message, &mut input)?;
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
    result.insert("parallel_tool_calls".to_string(), Value::Bool(true));
    result.insert("include".to_string(), json!(["reasoning.encrypted_content"]));
    copy_field(object, &mut result, "max_tokens", "max_output_tokens");
    copy_field(object, &mut result, "temperature", "temperature");
    copy_field(object, &mut result, "top_p", "top_p");
    if object.get("top_k").is_some_and(non_null) {
        anyhow::bail!("anthropic top_k cannot be converted to Responses");
    }
    if object
        .get("stop_sequences")
        .and_then(Value::as_array)
        .is_some_and(|values| !values.is_empty())
    {
        anyhow::bail!("anthropic stop_sequences cannot be converted to Responses");
    }
    if let Some(user) = value.pointer("/metadata/user_id") {
        result.insert("user".to_string(), user.clone());
    }
    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        let mut converted = Vec::new();
        for tool in tools {
            converted.push(anthropic_tool_to_responses(tool)?);
        }
        if !converted.is_empty() {
            result.insert("tools".to_string(), Value::Array(converted));
        }
    }
    if let Some(choice) = object.get("tool_choice") {
        result.insert(
            "tool_choice".to_string(),
            anthropic_tool_choice_to_responses(choice),
        );
        if let Some(disable_parallel) = choice.get("disable_parallel_tool_use").and_then(Value::as_bool) {
            result.insert("parallel_tool_calls".to_string(), Value::Bool(!disable_parallel));
        }
    }
    let effort = value
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
        .map(|effort| if effort == "max" { "xhigh" } else { effort })
        .or_else(|| {
            object.get("thinking").and_then(|thinking| {
                (thinking.get("type").and_then(Value::as_str) == Some("enabled"))
                    .then_some("high")
            })
        });
    if let Some(effort) = effort {
        result.insert(
            "reasoning".to_string(),
            json!({"effort":effort,"summary":"auto"}),
        );
    }
    Ok(serde_json::to_vec(&Value::Object(result))?)
}

pub(crate) fn responses_to_anthropic_request_json(body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(body)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("responses request body must be a JSON object"))?;
    let mut system = Vec::new();
    if let Some(instructions) = object.get("instructions") {
        append_system_value(instructions, &mut system)?;
    }
    let mut messages = Vec::new();
    match object.get("input") {
        Some(Value::String(text)) => {
            messages.push(message("user", vec![json!({"type":"text","text":text})]));
        }
        Some(Value::Array(items)) => {
            for item in items {
                append_responses_item(item, &mut system, &mut messages)?;
            }
        }
        Some(Value::Null) | None => {}
        Some(_) => anyhow::bail!("Responses input cannot be converted to Anthropic Messages"),
    }
    messages = normalize_tool_pairing(merge_messages(messages));
    messages = merge_messages(messages);
    if messages.is_empty() {
        messages.push(message("user", vec![json!({"type":"text","text":""})]));
    } else if messages[0].get("role").and_then(Value::as_str) != Some("user") {
        messages.insert(0, message("user", vec![json!({"type":"text","text":""})]));
    }

    let mut result = Map::new();
    copy_field(object, &mut result, "model", "model");
    result.insert("messages".to_string(), Value::Array(messages));
    result.insert(
        "max_tokens".to_string(),
        object
            .get("max_output_tokens")
            .cloned()
            .unwrap_or_else(|| json!(8192)),
    );
    result.insert(
        "stream".to_string(),
        json!(object.get("stream").and_then(Value::as_bool).unwrap_or(false)),
    );
    if !system.is_empty() {
        result.insert("system".to_string(), Value::Array(system));
    }
    copy_field(object, &mut result, "temperature", "temperature");
    copy_field(object, &mut result, "top_p", "top_p");
    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        validate_responses_tools(tools)?;
    }
    let tool_context = ChatResponseContext::from_responses_request(&value);
    let mut converted = tool_context
        .compatible_function_tools()
        .iter()
        .filter_map(chat_function_tool_to_anthropic)
        .collect::<Vec<_>>();
    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        for tool in tools {
            if matches!(
                tool.get("type").and_then(Value::as_str),
                Some("web_search" | "web_search_preview")
            ) {
                converted.push(responses_tool_to_anthropic(tool)?);
            }
        }
    }
    if !converted.is_empty() {
        result.insert("tools".to_string(), Value::Array(converted));
    }
    if let Some(choice) = object.get("tool_choice") {
        let mut converted = responses_tool_choice_to_anthropic(choice);
        if object.get("parallel_tool_calls").and_then(Value::as_bool) == Some(false)
            && let Some(choice) = converted.as_object_mut()
        {
            choice.insert("disable_parallel_tool_use".to_string(), Value::Bool(true));
        }
        result.insert("tool_choice".to_string(), converted);
    }
    if let Some(effort) = value.pointer("/reasoning/effort").and_then(Value::as_str) {
        let effort = if effort == "xhigh" { "max" } else { effort };
        result.insert("output_config".to_string(), json!({"effort":effort}));
        if effort != "low" {
            result.insert(
                "thinking".to_string(),
                json!({"type":"enabled","budget_tokens":thinking_budget(effort)}),
            );
        }
    }
    Ok(serde_json::to_vec(&Value::Object(result))?)
}

fn anthropic_system_parts(value: &Value) -> anyhow::Result<Vec<Value>> {
    match value {
        Value::String(text) => Ok(vec![json!({"type":"input_text","text":text})]),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str).unwrap_or("text") {
                    "text" => parts.push(json!({
                        "type":"input_text",
                        "text":block.get("text").cloned().unwrap_or_else(|| json!(""))
                    })),
                    kind => anyhow::bail!("anthropic system block {kind} cannot be converted"),
                }
            }
            Ok(parts)
        }
        _ => anyhow::bail!("anthropic system must be a string or text block array"),
    }
}

fn append_anthropic_message(message: &Value, input: &mut Vec<Value>) -> anyhow::Result<()> {
    let role = message.get("role").and_then(Value::as_str).unwrap_or("user");
    let content = message.get("content").unwrap_or(&Value::Null);
    if let Value::String(text) = content {
        let kind = if role == "assistant" { "output_text" } else { "input_text" };
        input.push(json!({"type":"message","role":role,"content":[{"type":kind,"text":text}]}));
        return Ok(());
    }
    let blocks = content
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("anthropic message content must be a string or block array"))?;
    let mut message_parts = Vec::new();
    for block in blocks {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("text");
        match (role, kind) {
            (_, "text") => {
                let part_type = if role == "assistant" { "output_text" } else { "input_text" };
                message_parts.push(json!({
                    "type":part_type,
                    "text":block.get("text").cloned().unwrap_or_else(|| json!(""))
                }));
            }
            ("user", "image") => {
                message_parts.push(anthropic_image_to_responses(block)?);
            }
            ("user", "tool_result") => {
                flush_message_parts(role, &mut message_parts, input);
                let (output, images) = anthropic_tool_result_content(block.get("content"))?;
                input.push(json!({
                    "type":"function_call_output",
                    "call_id":block.get("tool_use_id").cloned().unwrap_or_else(|| json!("")),
                    "output":output
                }));
                if !images.is_empty() {
                    input.push(json!({"type":"message","role":"user","content":images}));
                }
            }
            ("assistant", "tool_use") => {
                flush_message_parts(role, &mut message_parts, input);
                input.push(json!({
                    "type":"function_call",
                    "call_id":block.get("id").cloned().unwrap_or_else(|| json!(uuid::Uuid::new_v4().to_string())),
                    "name":block.get("name").cloned().unwrap_or_else(|| json!("")),
                    "arguments":argument_string(block.get("input"))
                }));
            }
            ("assistant", "thinking") => {
                flush_message_parts(role, &mut message_parts, input);
                if block.get("signature").is_some() {
                    tracing::debug!("dropping Anthropic thinking signature during protocol conversion");
                }
                if let Some(thinking) = block.get("thinking").and_then(Value::as_str) {
                    input.push(json!({
                        "type":"reasoning",
                        "summary":[{"type":"summary_text","text":thinking}]
                    }));
                }
            }
            (_, "document" | "audio") => {
                anyhow::bail!("anthropic {kind} content cannot be converted");
            }
            _ => anyhow::bail!("anthropic content block {kind} cannot be converted"),
        }
    }
    flush_message_parts(role, &mut message_parts, input);
    Ok(())
}

fn flush_message_parts(role: &str, parts: &mut Vec<Value>, input: &mut Vec<Value>) {
    if !parts.is_empty() {
        input.push(json!({
            "type":"message",
            "role":role,
            "content":std::mem::take(parts)
        }));
    }
}

fn anthropic_image_to_responses(block: &Value) -> anyhow::Result<Value> {
    let source = block
        .get("source")
        .ok_or_else(|| anyhow::anyhow!("anthropic image is missing source"))?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str).unwrap_or_default();
            Ok(json!({"type":"input_image","image_url":format!("data:{media_type};base64,{data}")}))
        }
        Some("url") => Ok(json!({
            "type":"input_image",
            "image_url":source.get("url").cloned().unwrap_or_else(|| json!(""))
        })),
        _ => anyhow::bail!("anthropic image source cannot be converted"),
    }
}

fn anthropic_tool_result_content(content: Option<&Value>) -> anyhow::Result<(String, Vec<Value>)> {
    match content {
        None | Some(Value::Null) => Ok(("(empty)".to_string(), Vec::new())),
        Some(Value::String(text)) => Ok((non_empty_output(text), Vec::new())),
        Some(Value::Array(blocks)) => {
            let mut text = Vec::new();
            let mut images = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str).unwrap_or("text") {
                    "text" => {
                        if let Some(value) = block.get("text").and_then(Value::as_str) {
                            text.push(value.to_string());
                        }
                    }
                    "image" => images.push(anthropic_image_to_responses(block)?),
                    "document" | "audio" => {
                        anyhow::bail!("anthropic tool result media cannot be converted");
                    }
                    kind => anyhow::bail!("anthropic tool result block {kind} cannot be converted"),
                }
            }
            Ok((non_empty_output(&text.join("\n")), images))
        }
        Some(value) => Ok((value.to_string(), Vec::new())),
    }
}

fn anthropic_tool_to_responses(tool: &Value) -> anyhow::Result<Value> {
    match tool.get("type").and_then(Value::as_str) {
        None | Some("custom") => Ok(json!({
            "type":"function",
            "name":tool.get("name").cloned().unwrap_or_else(|| json!("")),
            "description":tool.get("description").cloned().unwrap_or(Value::Null),
            "parameters":tool.get("input_schema").cloned().unwrap_or_else(empty_schema),
            "strict":false
        })),
        Some("web_search_20250305" | "web_search_20260209") => {
            let mut result = tool.as_object().cloned().unwrap_or_default();
            result.insert("type".to_string(), json!("web_search"));
            result.remove("input_schema");
            Ok(Value::Object(result))
        }
        Some(kind) => anyhow::bail!("anthropic server tool {kind} cannot be converted"),
    }
}

fn anthropic_tool_choice_to_responses(choice: &Value) -> Value {
    match choice.get("type").and_then(Value::as_str) {
        Some("any") => json!("required"),
        Some("none") => json!("none"),
        Some("tool") => json!({
            "type":"function",
            "name":choice.get("name").cloned().unwrap_or_else(|| json!(""))
        }),
        _ => json!("auto"),
    }
}

fn append_system_value(value: &Value, system: &mut Vec<Value>) -> anyhow::Result<()> {
    match value {
        Value::String(text) => system.push(json!({"type":"text","text":text})),
        Value::Array(parts) => {
            for part in parts {
                match part.get("type").and_then(Value::as_str).unwrap_or("text") {
                    "input_text" | "text" => system.push(json!({
                        "type":"text",
                        "text":part.get("text").cloned().unwrap_or_else(|| json!(""))
                    })),
                    kind => anyhow::bail!("Responses system content {kind} cannot be converted"),
                }
            }
        }
        Value::Null => {}
        _ => anyhow::bail!("Responses instructions cannot be converted"),
    }
    Ok(())
}

fn append_responses_item(
    item: &Value,
    system: &mut Vec<Value>,
    messages: &mut Vec<Value>,
) -> anyhow::Result<()> {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("message");
    match item_type {
        "message" => {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            if matches!(role, "system" | "developer") {
                append_system_value(item.get("content").unwrap_or(&Value::Null), system)?;
            } else {
                let content = responses_content_to_anthropic(
                    item.get("content").unwrap_or(&Value::Null),
                    role,
                )?;
                if !content.is_empty() {
                    messages.push(message(role, content));
                }
            }
        }
        "function_call" => messages.push(message(
            "assistant",
            vec![json!({
                "type":"tool_use",
                "id":anthropic_call_id(item),
                "name":item.get("name").cloned().unwrap_or_else(|| json!("")),
                "input":argument_value(item.get("arguments"))
            })],
        )),
        "custom_tool_call" => messages.push(message(
            "assistant",
            vec![json!({
                "type":"tool_use",
                "id":anthropic_call_id(item),
                "name":item.get("name").cloned().unwrap_or_else(|| json!("")),
                "input":custom_input_value(item.get("input"))
            })],
        )),
        "function_call_output" | "custom_tool_call_output" => messages.push(message(
            "user",
            vec![json!({
                "type":"tool_result",
                "tool_use_id":anthropic_call_id(item),
                "content":[{"type":"text","text":output_string(item.get("output"))}]
            })],
        )),
        "reasoning" => {
            if item.get("encrypted_content").is_some() {
                tracing::debug!("dropping Responses reasoning signature during protocol conversion");
            }
        }
        "web_search_call" => messages.push(message(
            "assistant",
            vec![json!({
                "type":"server_tool_use",
                "id":format!("srvtoolu_{}", item.get("id").and_then(Value::as_str).unwrap_or_default()),
                "name":"web_search",
                "input":item.get("action").cloned().unwrap_or_else(|| json!({}))
            })],
        )),
        "input_file" | "input_audio" | "audio" => {
            anyhow::bail!("Responses {item_type} cannot be converted to Anthropic Messages");
        }
        kind => anyhow::bail!("Responses input item {kind} cannot be converted to Anthropic Messages"),
    }
    Ok(())
}

fn responses_content_to_anthropic(
    content: &Value,
    role: &str,
) -> anyhow::Result<Vec<Value>> {
    match content {
        Value::String(text) => Ok(vec![json!({"type":"text","text":text})]),
        Value::Array(parts) => {
            let mut blocks = Vec::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str).unwrap_or("text") {
                    "input_text" | "output_text" | "text" => blocks.push(json!({
                        "type":"text",
                        "text":part.get("text").cloned().unwrap_or_else(|| json!(""))
                    })),
                    "input_image" if role == "user" => {
                        blocks.push(responses_image_to_anthropic(part)?);
                    }
                    "input_file" | "input_audio" | "audio" => {
                        anyhow::bail!("Responses media content cannot be converted to Anthropic Messages");
                    }
                    kind => anyhow::bail!("Responses content block {kind} cannot be converted"),
                }
            }
            Ok(blocks)
        }
        Value::Null => Ok(Vec::new()),
        _ => anyhow::bail!("Responses message content cannot be converted"),
    }
}

fn responses_image_to_anthropic(part: &Value) -> anyhow::Result<Value> {
    let url = part
        .get("image_url")
        .or_else(|| part.get("url"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Responses image is missing image_url"))?;
    if let Some(data) = url.strip_prefix("data:")
        && let Some((media_type, encoded)) = data.split_once(";base64,")
    {
        return Ok(json!({
            "type":"image",
            "source":{"type":"base64","media_type":media_type,"data":encoded}
        }));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Ok(json!({"type":"image","source":{"type":"url","url":url}}));
    }
    anyhow::bail!("Responses image URL cannot be converted to Anthropic Messages")
}

fn responses_tool_to_anthropic(tool: &Value) -> anyhow::Result<Value> {
    match tool.get("type").and_then(Value::as_str).unwrap_or("function") {
        "function" | "custom" => Ok(json!({
            "name":tool.get("name").cloned().unwrap_or_else(|| json!("")),
            "description":tool.get("description").cloned().unwrap_or(Value::Null),
            "input_schema":normalize_schema(tool.get("parameters"))
        })),
        "web_search" | "web_search_preview" => {
            let mut result = tool.as_object().cloned().unwrap_or_default();
            result.insert("type".to_string(), json!("web_search_20250305"));
            result.insert("name".to_string(), json!("web_search"));
            result.remove("parameters");
            Ok(Value::Object(result))
        }
        kind => anyhow::bail!("Responses server tool {kind} cannot be converted to Anthropic Messages"),
    }
}

fn validate_responses_tools(tools: &[Value]) -> anyhow::Result<()> {
    for tool in tools {
        match tool.get("type").and_then(Value::as_str).unwrap_or("function") {
            "function" | "custom" | "namespace" | "tool_search" | "web_search"
            | "web_search_preview" => {}
            kind => anyhow::bail!(
                "Responses server tool {kind} cannot be converted to Anthropic Messages"
            ),
        }
    }
    Ok(())
}

fn chat_function_tool_to_anthropic(tool: &Value) -> Option<Value> {
    let function = tool.get("function")?;
    Some(json!({
        "name":function.get("name").cloned().unwrap_or_else(|| json!("")),
        "description":function.get("description").cloned().unwrap_or(Value::Null),
        "input_schema":normalize_schema(function.get("parameters"))
    }))
}

fn responses_tool_choice_to_anthropic(choice: &Value) -> Value {
    match choice.as_str() {
        Some("required") => json!({"type":"any"}),
        Some("none") => json!({"type":"none"}),
        Some(_) => json!({"type":"auto"}),
        None if choice.get("type").and_then(Value::as_str) == Some("function") => json!({
            "type":"tool",
            "name":choice.get("name").or_else(|| choice.pointer("/function/name")).cloned().unwrap_or_else(|| json!(""))
        }),
        None => choice.clone(),
    }
}

fn normalize_tool_pairing(messages: Vec<Value>) -> Vec<Value> {
    let mut results = BTreeMap::new();
    for message in &messages {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        for block in content_blocks(message) {
            if block.get("type").and_then(Value::as_str) == Some("tool_result")
                && let Some(id) = block.get("tool_use_id").and_then(Value::as_str)
            {
                results.insert(id.to_string(), block.clone());
            }
        }
    }
    let mut output = Vec::new();
    for item in messages {
        let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
        let blocks = content_blocks(&item);
        if role == "assistant" {
            let mut kept = Vec::new();
            let mut matched_results = Vec::new();
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                    if let Some(result) = results.get(id) {
                        kept.push(block.clone());
                        matched_results.push(result.clone());
                    }
                } else {
                    kept.push(block.clone());
                }
            }
            if !kept.is_empty() {
                output.push(message("assistant", kept));
            }
            if !matched_results.is_empty() {
                output.push(message("user", matched_results));
            }
        } else if role == "user" {
            let blocks = blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) != Some("tool_result"))
                .cloned()
                .collect::<Vec<_>>();
            if !blocks.is_empty() {
                output.push(message("user", blocks));
            }
        } else {
            output.push(item);
        }
    }
    output
}

fn merge_messages(messages: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for item in messages {
        let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
        if let Some(last) = merged.last_mut()
            && last.get("role").and_then(Value::as_str) == Some(role)
        {
            let extra = content_blocks(&item).to_vec();
            if let Some(content) = last.get_mut("content").and_then(Value::as_array_mut) {
                content.extend(extra);
                continue;
            }
        }
        merged.push(item);
    }
    merged
}

fn content_blocks(message: &Value) -> &[Value] {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn message(role: &str, content: Vec<Value>) -> Value {
    json!({"role":role,"content":content})
}

fn anthropic_call_id(item: &Value) -> String {
    let id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if id.starts_with("toolu_") || id.starts_with("call_") {
        id.to_string()
    } else {
        format!("toolu_{id}")
    }
}

fn argument_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(value) if !value.is_null() => value.to_string(),
        _ => "{}".to_string(),
    }
}

fn argument_value(value: Option<&Value>) -> Value {
    let text = argument_string(value);
    serde_json::from_str(&text).unwrap_or_else(|_| json!({"input":text}))
}

fn custom_input_value(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or_else(|_| json!({"input":text})),
        Some(value) => value.clone(),
        None => json!({}),
    }
}

fn output_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => non_empty_output(text),
        Some(value) if !value.is_null() => value.to_string(),
        _ => "(empty)".to_string(),
    }
}

fn non_empty_output(value: &str) -> String {
    if value.is_empty() { "(empty)".to_string() } else { value.to_string() }
}

fn normalize_schema(value: Option<&Value>) -> Value {
    let mut schema = value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    schema.insert("type".to_string(), json!("object"));
    schema.entry("properties".to_string()).or_insert_with(|| json!({}));
    Value::Object(schema)
}

fn empty_schema() -> Value {
    json!({"type":"object","properties":{}})
}

fn thinking_budget(effort: &str) -> u64 {
    match effort {
        "medium" => 4096,
        "high" => 10240,
        "max" => 32768,
        _ => 1024,
    }
}

fn copy_field(source: &Map<String, Value>, target: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = source.get(from) {
        target.insert(to.to_string(), value.clone());
    }
}

fn non_null(value: &Value) -> bool {
    !value.is_null()
}
