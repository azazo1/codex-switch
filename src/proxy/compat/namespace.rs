use serde_json::{Map, Value, json};
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub(crate) struct NamespaceToolMap {
    flattened_to_identity: HashMap<String, (String, String)>,
    original_to_namespaces: HashMap<String, Vec<String>>,
}

impl NamespaceToolMap {
    pub(crate) fn from_request(body: &Value) -> Self {
        let mut map = Self::default();
        if let Some(tools) = body.get("tools").and_then(Value::as_array) {
            map.collect_tools(tools);
        }
        if let Some(input) = body.get("input") {
            for item_type in ["additional_tools", "tool_search_output"] {
                map.collect_nested(input, item_type);
            }
        }
        map
    }

    pub(crate) fn restore_response_value(&self, value: &mut Value) -> bool {
        let mut changed = false;
        match value {
            Value::Array(items) => {
                for item in items {
                    changed |= self.restore_response_value(item);
                }
            }
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("function_call") {
                    changed |= self.restore_function_call(object);
                }
                for nested in object.values_mut() {
                    changed |= self.restore_response_value(nested);
                }
            }
            _ => {}
        }
        changed
    }

    pub(crate) fn rewrite_sse_block(&self, block: &[u8]) -> Vec<u8> {
        let text = String::from_utf8_lossy(block);
        let mut rewritten = Vec::new();
        for line in text.split('\n') {
            let trimmed = line.trim_end_matches('\r');
            if let Some(data) = trimmed.strip_prefix("data:") {
                let payload = data.trim();
                if !payload.is_empty()
                    && payload != "[DONE]"
                    && let Ok(mut value) = serde_json::from_str::<Value>(payload)
                    && self.restore_response_value(&mut value)
                    && let Ok(serialized) = serde_json::to_string(&value)
                {
                    rewritten.extend_from_slice(format!("data: {serialized}\n").as_bytes());
                    continue;
                }
            }
            rewritten.extend_from_slice(line.as_bytes());
            rewritten.push(b'\n');
        }
        rewritten
    }

    fn collect_tools(&mut self, tools: &[Value]) {
        for tool in tools {
            if let Some(object) = tool.as_object()
                && object.get("type").and_then(Value::as_str) == Some("namespace")
            {
                self.collect_namespace_tool(object);
            }
        }
    }

    fn collect_namespace_tool(&mut self, tool: &Map<String, Value>) {
        let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
            return;
        };
        let Some(children) = tool
            .get("tools")
            .or_else(|| tool.get("children"))
            .and_then(Value::as_array)
        else {
            return;
        };
        for child in children {
            if let Some(name) = response_tool_name(child) {
                self.add(namespace, &name);
            }
        }
    }

    fn collect_nested(&mut self, value: &Value, item_type: &str) {
        match value {
            Value::Array(items) => {
                for item in items {
                    self.collect_nested(item, item_type);
                }
            }
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some(item_type)
                    && let Some(tools) = object.get("tools").and_then(Value::as_array)
                {
                    self.collect_tools(tools);
                }
                for nested in object.values() {
                    self.collect_nested(nested, item_type);
                }
            }
            _ => {}
        }
    }

    fn add(&mut self, namespace: &str, name: &str) {
        if namespace.is_empty() || name.is_empty() {
            return;
        }
        let flattened = format!("{namespace}__{name}");
        self.flattened_to_identity
            .entry(flattened)
            .or_insert_with(|| (namespace.to_string(), name.to_string()));
        let namespaces = self.original_to_namespaces.entry(name.to_string()).or_default();
        if !namespaces.contains(&namespace.to_string()) {
            namespaces.push(namespace.to_string());
        }
    }

    fn restore_function_call(&self, object: &mut Map<String, Value>) -> bool {
        if object
            .get("namespace")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
        {
            return false;
        }
        let Some(name) = object.get("name").and_then(Value::as_str) else {
            return false;
        };
        let identity = self
            .flattened_to_identity
            .get(name)
            .cloned()
            .or_else(|| parse_flattened_name(name))
            .or_else(|| namespace_from_arguments(object).map(|namespace| (namespace, name.to_string())))
            .or_else(|| {
                self.original_to_namespaces.get(name).and_then(|namespaces| {
                    if namespaces.len() == 1 {
                        Some((namespaces[0].clone(), name.to_string()))
                    } else {
                        None
                    }
                })
            });
        let Some((namespace, original_name)) = identity else {
            return false;
        };
        object.insert("name".to_string(), json!(original_name));
        object.insert("namespace".to_string(), json!(namespace));
        normalize_namespace_arguments(object);
        true
    }
}

fn response_tool_name(tool: &Value) -> Option<String> {
    tool.pointer("/function/name")
        .or_else(|| tool.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn parse_flattened_name(name: &str) -> Option<(String, String)> {
    if let Some(rest) = name.strip_prefix("functions.") {
        return parse_dot_name(rest);
    }
    if let Some(rest) = name.strip_prefix("mcp__")
        && let Some(index) = rest.find("__")
    {
        return Some((
            format!("mcp__{}", &rest[..index]),
            rest[index + 2..].to_string(),
        ));
    }
    if let Some(index) = name.find("__")
        && index > 0
        && index + 2 < name.len()
    {
        return Some((name[..index].to_string(), name[index + 2..].to_string()));
    }
    parse_dot_name(name)
}

fn parse_dot_name(name: &str) -> Option<(String, String)> {
    let index = name.find('.')?;
    if index == 0 || index + 1 >= name.len() {
        return None;
    }
    Some((name[..index].to_string(), name[index + 1..].to_string()))
}

fn namespace_from_arguments(object: &Map<String, Value>) -> Option<String> {
    let Value::String(arguments) = object.get("arguments")? else {
        return None;
    };
    let Value::Object(args) = serde_json::from_str::<Value>(arguments).ok()? else {
        return None;
    };
    args.get("namespace")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn normalize_namespace_arguments(object: &mut Map<String, Value>) {
    let Some(arguments) = object.get("arguments").cloned() else {
        return;
    };
    let Value::String(arguments_text) = arguments else {
        return;
    };
    let Ok(mut parsed) = serde_json::from_str::<Value>(&arguments_text) else {
        return;
    };
    let Some(args) = parsed.as_object_mut() else {
        return;
    };
    let has_namespace = args.contains_key("namespace");
    let has_arguments = args.get("arguments").is_some();
    if !has_namespace && !has_arguments {
        return;
    }
    if let Some(nested) = args.remove("arguments") {
        if args.len() <= 1 {
            parsed = nested;
        } else {
            args.insert("arguments".to_string(), nested);
        }
    }
    if let Some(args) = parsed.as_object_mut() {
        args.remove("namespace");
    }
    if let Ok(serialized) = serde_json::to_string(&parsed) {
        object.insert("arguments".to_string(), Value::String(serialized));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn restores_namespace_from_request_tools() {
        let request = json!({
            "tools": [{
                "type": "namespace",
                "name": "codex_app",
                "tools": [{
                    "type": "function",
                    "name": "open_in_codex",
                    "parameters": {"type": "object"}
                }]
            }]
        });
        let map = NamespaceToolMap::from_request(&request);
        let mut value = json!({
            "type": "function_call",
            "name": "codex_app__open_in_codex",
            "arguments": "{}"
        });

        assert!(map.restore_response_value(&mut value));
        assert_eq!(value["name"], "open_in_codex");
        assert_eq!(value["namespace"], "codex_app");
    }

    #[test]
    fn restores_mcp_namespace_from_flattened_name() {
        let map = NamespaceToolMap::default();
        let block = "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"name\":\"mcp__node_repl__js\",\"arguments\":\"\"}}\n\n";

        let rewritten = map.rewrite_sse_block(block.as_bytes());
        let text = String::from_utf8(rewritten).unwrap();

        assert!(text.contains("\"name\":\"js\""));
        assert!(text.contains("\"namespace\":\"mcp__node_repl\""));
    }

    #[test]
    fn normalizes_misplaced_namespace_arguments() {
        let request = json!({
            "input": [{
                "type": "tool_search_output",
                "tools": [{
                    "type": "namespace",
                    "name": "codex_app",
                    "tools": [{"type": "function", "name": "open_in_codex"}]
                }]
            }]
        });
        let map = NamespaceToolMap::from_request(&request);
        let mut value = json!({
            "type": "function_call",
            "name": "open_in_codex",
            "arguments": "{\"arguments\":{\"placement\":\"right\",\"target\":{\"type\":\"browser\",\"url\":\"https://www.baidu.com/\"}},\"namespace\":\"codex_app\"}"
        });

        assert!(map.restore_response_value(&mut value));
        assert_eq!(value["namespace"], "codex_app");
        let arguments: Value = serde_json::from_str(value["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(arguments["placement"], "right");
        assert!(arguments.get("namespace").is_none());
        assert!(arguments.get("arguments").is_none());
    }

    #[test]
    fn restores_nonstreaming_response_output() {
        let request = json!({
            "tools": [{
                "type": "namespace",
                "name": "collaboration",
                "tools": [{"type": "function", "name": "spawn_agent"}]
            }]
        });
        let map = NamespaceToolMap::from_request(&request);
        let mut value = json!({
            "type": "response.completed",
            "response": {
                "output": [{
                    "type": "function_call",
                    "name": "collaboration__spawn_agent",
                    "arguments": "{}"
                }]
            }
        });

        assert!(map.restore_response_value(&mut value));
        let item = &value["response"]["output"][0];
        assert_eq!(item["name"], "spawn_agent");
        assert_eq!(item["namespace"], "collaboration");
    }
}
