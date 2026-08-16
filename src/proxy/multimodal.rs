use crate::core::models::WireApi;
pub(crate) use crate::core::model_capabilities::model_multimodal_from_item;
use base64::Engine;
use serde_json::{Value, json};

pub(crate) struct StripMultimodalResult {
    pub body: Vec<u8>,
    pub removed: usize,
}

pub(crate) fn strip_multimodal_input(
    body: &[u8],
    wire_api: WireApi,
) -> anyhow::Result<StripMultimodalResult> {
    let mut value: Value = serde_json::from_slice(body)?;
    let removed = match wire_api {
        WireApi::Responses => strip_responses_input(&mut value),
        WireApi::ChatCompletions => strip_chat_messages(&mut value),
        WireApi::AnthropicMessages => strip_anthropic_messages(&mut value),
    };
    Ok(StripMultimodalResult {
        body: serde_json::to_vec(&value)?,
        removed,
    })
}

fn strip_responses_input(value: &mut Value) -> usize {
    let Some(input) = value.get_mut("input") else {
        return 0;
    };
    match input {
        Value::Array(items) => items.iter_mut().map(strip_responses_item).sum(),
        _ => 0,
    }
}

fn strip_responses_item(item: &mut Value) -> usize {
    let mut removed = 0;
    let text_type = if item.get("role").and_then(Value::as_str) == Some("assistant") {
        "output_text"
    } else {
        "input_text"
    };
    if let Some(parts) = item.get_mut("content").and_then(Value::as_array_mut) {
        removed += strip_content_parts(parts, text_type);
    }
    if let Some(kind) = item.get("type").and_then(Value::as_str)
        && let Some(description) = multimodal_description(item, kind)
    {
        *item = json!({
            "type":"message",
            "role":"user",
            "content":[{"type":"input_text","text":description}]
        });
        removed += 1;
    }
    if matches!(
        item.get("type").and_then(Value::as_str),
        Some("function_call_output" | "custom_tool_call_output" | "tool_search_output")
    ) {
        removed += strip_output_field(item, "output", "text");
    }
    removed
}

fn strip_chat_messages(value: &mut Value) -> usize {
    let Some(messages) = value.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    messages
        .iter_mut()
        .map(|message| {
            let mut removed = 0;
            if let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) {
                removed += strip_content_parts(parts, "text");
            } else if message.get("role").and_then(Value::as_str) == Some("tool") {
                removed += strip_output_field(message, "content", "text");
            }
            removed
        })
        .sum()
}

fn strip_anthropic_messages(value: &mut Value) -> usize {
    let Some(messages) = value.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    messages
        .iter_mut()
        .map(|message| {
            message
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .map_or(0, strip_anthropic_content_blocks)
        })
        .sum()
}

fn strip_anthropic_content_blocks(blocks: &mut Vec<Value>) -> usize {
    let mut removed = 0;
    for block in blocks {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("text");
        if let Some(description) = multimodal_description(block, kind) {
            *block = json!({"type":"text","text":description});
            removed += 1;
        } else if kind == "tool_result" {
            match block.get_mut("content") {
                Some(Value::Array(content)) => {
                    removed += strip_anthropic_content_blocks(content);
                }
                Some(field) => {
                    removed += strip_embedded_media_value(field, "text");
                }
                None => {}
            }
        }
    }
    removed
}

fn strip_content_parts(parts: &mut Vec<Value>, text_type: &str) -> usize {
    let mut removed = 0;
    for part in parts {
        let kind = part.get("type").and_then(Value::as_str).unwrap_or("text");
        if let Some(description) = multimodal_description(part, kind) {
            *part = json!({"type":text_type,"text":description});
            removed += 1;
        }
    }
    removed
}

fn strip_output_field(value: &mut Value, key: &str, text_type: &str) -> usize {
    let Some(field) = value.get_mut(key) else {
        return 0;
    };
    match field {
        Value::String(text) => {
            let (cleaned, removed) = strip_embedded_media_json(text, text_type);
            if removed > 0 {
                *field = Value::String(cleaned);
            }
            removed
        }
        other => strip_embedded_media_value(other, text_type),
    }
}

fn strip_embedded_media_json(text: &str, text_type: &str) -> (String, usize) {
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        return (text.to_string(), 0);
    };
    let removed = strip_embedded_media_value(&mut value, text_type);
    if removed == 0 {
        return (text.to_string(), 0);
    }
    (
        serde_json::to_string(&value).unwrap_or_else(|_| text.to_string()),
        removed,
    )
}

fn strip_embedded_media_value(value: &mut Value, text_type: &str) -> usize {
    match value {
        Value::Array(items) => items
            .iter_mut()
            .map(|item| strip_embedded_media_value(item, text_type))
            .sum(),
        Value::Object(_) => {
            let media_description = value
                .get("type")
                .and_then(Value::as_str)
                .and_then(|kind| multimodal_description(value, kind));
            if let Some(description) = media_description {
                *value = json!({"type":text_type,"text":description});
                return 1;
            }
            let mut removed = 0;
            if let Some(content) = value.get_mut("content").and_then(Value::as_array_mut) {
                removed += strip_content_parts(content, text_type);
            }
            if let Some(object) = value.as_object_mut() {
                for (key, field) in object.iter_mut() {
                    if key == "content" && field.is_array() {
                        continue;
                    }
                    if let Value::String(text) = field {
                        let (cleaned, count) = strip_embedded_media_json(text, text_type);
                        if count > 0 {
                            *field = Value::String(cleaned);
                        }
                        removed += count;
                    } else {
                        removed += strip_embedded_media_value(field, text_type);
                    }
                }
            }
            removed
        }
        Value::String(text) => {
            let (cleaned, removed) = strip_embedded_media_json(text, text_type);
            if removed > 0 {
                *text = cleaned;
            }
            removed
        }
        _ => 0,
    }
}

fn multimodal_description(part: &Value, kind: &str) -> Option<String> {
    let label = match kind {
        "input_image" | "image_url" | "image" => "图片",
        "input_audio" | "audio" => "音频",
        "input_file" | "file" | "document" => "文件",
        _ => return None,
    };
    let media_type = media_type_of(part, kind);
    let data = media_data(part);
    let mut details = Vec::new();
    if let Some(media_type) = media_type {
        details.push(media_type);
    }
    if label == "图片"
        && let Some((width, height)) = data.as_deref().and_then(image_dimensions)
    {
        details.push(format!("{width}x{height}"));
    }
    if let Some(data) = data {
        details.push(format!("{} bytes", data.len()));
    } else {
        details.push("远程".to_string());
    }
    if let Some(filename) = part
        .get("filename")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        details.push(filename.to_string());
    }
    if let Some(detail) = part
        .get("detail")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        details.push(format!("detail={detail}"));
    }
    Some(format!(
        "[该模型不支持{label}输入, 已移除: {}]",
        details.join(", ")
    ))
}

fn media_type_of(part: &Value, kind: &str) -> Option<String> {
    for key in ["media_type", "mime_type"] {
        if let Some(value) = part
            .get(key)
            .or_else(|| part.pointer("/source/media_type"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }
    if matches!(kind, "input_audio" | "audio")
        && let Some(format) = part
            .get("format")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    {
        return Some(format!("audio/{format}"));
    }
    media_string(part)
        .as_deref()
        .and_then(data_url_media_type)
}

fn media_data(part: &Value) -> Option<Vec<u8>> {
    let value = media_string(part)?;
    if let Some(rest) = value.strip_prefix("data:") {
        let encoded = rest.split_once(";base64,")?.1;
        return base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok();
    }
    let raw_base64 = part.pointer("/source/type").and_then(Value::as_str) == Some("base64")
        || part.get("file_data").is_some()
        || part.get("data").is_some();
    raw_base64
        .then(|| {
            base64::engine::general_purpose::STANDARD
                .decode(value)
                .ok()
        })
        .flatten()
}

fn media_string(part: &Value) -> Option<String> {
    if let Some(image_url) = part.get("image_url") {
        match image_url {
            Value::String(value) => return Some(value.clone()),
            Value::Object(object) => {
                if let Some(value) = object.get("url").and_then(Value::as_str) {
                    return Some(value.to_string());
                }
            }
            _ => {}
        }
    }
    for key in ["file_data", "data", "input_audio", "url"] {
        if let Some(value) = part.get(key).and_then(Value::as_str) {
            return Some(value.to_string());
        }
    }
    if let Some(source) = part.get("source") {
        for key in ["data", "url"] {
            if let Some(value) = source.get(key).and_then(Value::as_str) {
                return Some(value.to_string());
            }
        }
    }
    if let Some(object) = part.get("input_audio").and_then(Value::as_object)
        && let Some(value) = object.get("data").and_then(Value::as_str)
    {
        return Some(value.to_string());
    }
    None
}

fn data_url_media_type(value: &str) -> Option<String> {
    let rest = value.strip_prefix("data:")?;
    rest.split(';').next().map(str::to_string)
}

fn image_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() >= 24 && data.starts_with(b"\x89PNG\r\n\x1a\n") && &data[12..16] == b"IHDR" {
        let width = u32::from_be_bytes(data[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(data[20..24].try_into().ok()?);
        return Some((width, height));
    }
    if data.len() >= 10 && (data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) {
        let width = u16::from_le_bytes([data[6], data[7]]) as u32;
        let height = u16::from_le_bytes([data[8], data[9]]) as u32;
        return Some((width, height));
    }
    if data.len() >= 2 && data.starts_with(&[0xff, 0xd8]) {
        return jpeg_dimensions(data);
    }
    if data.len() >= 30 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return webp_dimensions(data);
    }
    if data.len() >= 26 && data.starts_with(b"BM") {
        let width = u32::from_le_bytes(data[18..22].try_into().ok()?);
        let height = u32::from_le_bytes(data[22..26].try_into().ok()?);
        return Some((width, height));
    }
    None
}

fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let mut offset = 2;
    while offset + 4 <= data.len() {
        if data[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = *data.get(offset + 1)?;
        if marker == 0xd8 {
            offset += 2;
            continue;
        }
        if matches!(marker, 0xd9 | 0xda) {
            return None;
        }
        let length = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
        if length < 2 {
            return None;
        }
        let segment = offset + 4;
        if matches!(
            marker,
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf
        ) {
            if segment + 5 > data.len() {
                return None;
            }
            let height = u16::from_be_bytes([data[segment + 1], data[segment + 2]]) as u32;
            let width = u16::from_be_bytes([data[segment + 3], data[segment + 4]]) as u32;
            return Some((width, height));
        }
        offset += 2 + length;
    }
    None
}

fn webp_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    match &data[12..16] {
        b"VP8X" if data.len() >= 30 => {
            let width =
                u32::from_le_bytes([data[24], data[25], data[26], 0]) + 1;
            let height =
                u32::from_le_bytes([data[27], data[28], data[29], 0]) + 1;
            Some((width, height))
        }
        b"VP8 " if data.len() >= 30 => {
            let width = u16::from_le_bytes([data[26], data[27]]) & 0x3fff;
            let height = u16::from_le_bytes([data[28], data[29]]) & 0x3fff;
            Some((width as u32, height as u32))
        }
        b"VP8L" if data.len() >= 25 => {
            let bits = u32::from_le_bytes([data[21], data[22], data[23], data[24]]);
            let width = (bits & 0x3fff) + 1;
            let height = ((bits >> 14) & 0x3fff) + 1;
            Some((width, height))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_capabilities_from_model_items() {
        assert_eq!(
            model_multimodal_from_item(&json!({"id":"m","capabilities":{"supports_image_input":true}})),
            Some(true)
        );
        assert_eq!(
            model_multimodal_from_item(&json!({"id":"m","architecture":{"modality":"text->text"}})),
            Some(false)
        );
        assert_eq!(
            model_multimodal_from_item(&json!({"id":"m","modalities":["text","image"]})),
            Some(true)
        );
        assert_eq!(
            model_multimodal_from_item(&json!({"id":"m","input_modalities":{"text":true,"image":false}})),
            Some(false)
        );
        assert_eq!(
            model_multimodal_from_item(&json!({"id":"m","object":"model"})),
            None
        );
    }

    #[test]
    fn strips_responses_media_parts() {
        let body = json!({
            "model":"deepseek-v4-flash",
            "input":[{
                "type":"message",
                "role":"user",
                "content":[
                    {"type":"input_text","text":"describe this"},
                    {"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="},
                    {"type":"input_audio","input_audio":"data:audio/wav;base64,YXVkaW8="},
                    {"type":"input_file","file_data":"data:application/pdf;base64,ZmlsZQ==","filename":"a.pdf"}
                ]
            }]
        });
        let stripped =
            strip_multimodal_input(&serde_json::to_vec(&body).unwrap(), WireApi::Responses)
                .unwrap();
        let value: Value = serde_json::from_slice(&stripped.body).unwrap();
        let content = value["input"][0]["content"].as_array().unwrap();
        assert_eq!(stripped.removed, 3);
        assert_eq!(content[0]["text"], "describe this");
        assert!(content[1]["text"]
            .as_str()
            .unwrap()
            .contains("不支持图片输入"));
        assert!(content[2]["text"]
            .as_str()
            .unwrap()
            .contains("不支持音频输入"));
        assert!(content[3]["text"].as_str().unwrap().contains("a.pdf"));
    }

    #[test]
    fn strips_chat_and_anthropic_media() {
        let chat = json!({
            "model":"deepseek-v4-flash",
            "messages":[{"role":"user","content":[
                {"type":"text","text":"keep"},
                {"type":"image_url","image_url":{"url":"https://example.com/a.png"}}
            ]}]
        });
        let stripped =
            strip_multimodal_input(&serde_json::to_vec(&chat).unwrap(), WireApi::ChatCompletions)
                .unwrap();
        let value: Value = serde_json::from_slice(&stripped.body).unwrap();
        assert_eq!(stripped.removed, 1);
        assert_eq!(value["messages"][0]["content"][1]["type"], "text");
        assert!(value["messages"][0]["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("不支持图片输入"));
        assert!(value["messages"][0]["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("远程"));

        let anthropic = json!({
            "model":"deepseek-v4-flash",
            "messages":[{"role":"user","content":[
                {"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"aGVsbG8="}},
                {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"ZmlsZQ=="}},
                {"type":"tool_result","tool_use_id":"call_1","content":[
                    {"type":"image","source":{"type":"base64","media_type":"image/gif","data":"R0lGODlhAQABAIAAAP8AAP8AACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw=="}}
                ]}
            ]}]
        });
        let stripped = strip_multimodal_input(
            &serde_json::to_vec(&anthropic).unwrap(),
            WireApi::AnthropicMessages,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&stripped.body).unwrap();
        assert_eq!(stripped.removed, 3);
        assert!(value["messages"][0]["content"][2]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("不支持图片输入"));
    }

    #[test]
    fn strips_media_inside_responses_tool_output() {
        let body = json!({
            "model":"deepseek-v4-flash",
            "input":[
                {
                    "type":"function_call",
                    "call_id":"call_1",
                    "name":"view_image",
                    "arguments":"{}"
                },
                {
                    "type":"function_call_output",
                    "call_id":"call_1",
                    "output": r#"[{"detail":"high","image_url":"data:image/png;base64,aGVsbG8=","type":"input_image"}]"#
                }
            ]
        });
        let stripped =
            strip_multimodal_input(&serde_json::to_vec(&body).unwrap(), WireApi::Responses)
                .unwrap();
        let value: Value = serde_json::from_slice(&stripped.body).unwrap();
        assert_eq!(stripped.removed, 1);
        let output = value["input"][1]["output"].as_str().unwrap();
        assert!(!output.contains("base64"));
        assert!(!output.contains("aGVsbG8="));
        assert!(output.contains("不支持图片输入"));
        assert!(output.contains("image/png"));
    }

    #[test]
    fn strips_media_inside_chat_tool_content() {
        let chat = json!({
            "model":"deepseek-v4-flash",
            "messages":[{
                "role":"tool",
                "tool_call_id":"call_1",
                "content": r#"[{"type":"image_url","image_url":{"url":"https://example.com/a.png"}}]"#
            }]
        });
        let stripped =
            strip_multimodal_input(&serde_json::to_vec(&chat).unwrap(), WireApi::ChatCompletions)
                .unwrap();
        let value: Value = serde_json::from_slice(&stripped.body).unwrap();
        assert_eq!(stripped.removed, 1);
        let content = value["messages"][0]["content"].as_str().unwrap();
        assert!(!content.contains("example.com"));
        assert!(content.contains("不支持图片输入"));
        assert!(content.contains("远程"));
    }

    #[test]
    fn strips_media_inside_anthropic_tool_result() {
        let anthropic = json!({
            "model":"deepseek-v4-flash",
            "messages":[{
                "role":"user",
                "content":[{
                    "type":"tool_result",
                    "tool_use_id":"call_1",
                    "content": r#"[{"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"aGVsbG8="}}]"#
                }]
            }]
        });
        let stripped = strip_multimodal_input(
            &serde_json::to_vec(&anthropic).unwrap(),
            WireApi::AnthropicMessages,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&stripped.body).unwrap();
        assert_eq!(stripped.removed, 1);
        let content = value["messages"][0]["content"][0]["content"].as_str().unwrap();
        assert!(!content.contains("aGVsbG8="));
        assert!(content.contains("不支持图片输入"));
    }

    #[test]
    fn keeps_plain_tool_output_unchanged() {
        let body = json!({
            "model":"deepseek-v4-flash",
            "input":[{
                "type":"function_call_output",
                "call_id":"call_1",
                "output":"plain text"
            }]
        });
        let stripped =
            strip_multimodal_input(&serde_json::to_vec(&body).unwrap(), WireApi::Responses)
                .unwrap();
        let value: Value = serde_json::from_slice(&stripped.body).unwrap();
        assert_eq!(stripped.removed, 0);
        assert_eq!(value["input"][0]["output"], "plain text");
    }

    #[test]
    fn parses_png_dimensions() {
        let mut png = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&640_u32.to_be_bytes());
        png.extend_from_slice(&480_u32.to_be_bytes());
        assert_eq!(image_dimensions(&png), Some((640, 480)));
    }
}
