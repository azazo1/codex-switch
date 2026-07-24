use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub(super) const TOOL_SEARCH_CHAT_NAME: &str = "tool_search";
const CUSTOM_TOOL_INPUT_FIELD: &str = "input";
const CHAT_TOOL_NAME_MAX_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolKind {
    Function,
    Namespace,
    Custom,
    ToolSearch,
}

#[derive(Debug, Clone)]
pub(super) struct ToolSpec {
    pub(super) kind: ToolKind,
    pub(super) name: String,
    pub(super) namespace: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ToolContext {
    chat_tools: Vec<Value>,
    chat_name_to_spec: HashMap<String, ToolSpec>,
    namespace_name_to_chat_name: HashMap<(String, String), String>,
}

impl ToolContext {
    pub(super) fn from_request(body: &Value) -> Self {
        let mut context = Self::default();
        if let Some(tools) = body.get("tools").and_then(Value::as_array) {
            context.add_tools(tools);
        }
        if let Some(input) = body.get("input") {
            collect_input_tools(input, "additional_tools", &mut context);
            collect_input_tools(input, "tool_search_output", &mut context);
        }
        context
    }

    pub(super) fn chat_tools(&self) -> &[Value] {
        &self.chat_tools
    }

    pub(super) fn lookup_chat_name(&self, chat_name: &str) -> Option<&ToolSpec> {
        self.chat_name_to_spec.get(chat_name)
    }

    pub(super) fn is_custom_tool(&self, chat_name: &str) -> bool {
        self.lookup_chat_name(chat_name)
            .is_some_and(|spec| spec.kind == ToolKind::Custom)
    }

    pub(super) fn chat_name_for_function(&self, name: &str, namespace: Option<&str>) -> String {
        let Some(namespace) = namespace.filter(|value| !value.is_empty()) else {
            return name.to_string();
        };
        self.namespace_name_to_chat_name
            .get(&(namespace.to_string(), name.to_string()))
            .cloned()
            .unwrap_or_else(|| flatten_namespace_tool_name(namespace, name))
    }

    pub(super) fn tool_choice_to_chat(&self, value: &Value) -> Value {
        let Some(object) = value.as_object() else {
            return value.clone();
        };
        let tool_type = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if tool_type == "tool_search" {
            return json!({
                "type":"function",
                "function":{"name":TOOL_SEARCH_CHAT_NAME}
            });
        }
        let name = object
            .get("name")
            .or_else(|| value.pointer("/function/name"))
            .and_then(Value::as_str);
        let Some(name) = name else {
            return value.clone();
        };
        let namespace = object.get("namespace").and_then(Value::as_str);
        let chat_name = if tool_type == "function" {
            self.chat_name_for_function(name, namespace)
        } else {
            name.to_string()
        };
        json!({"type":"function","function":{"name":chat_name}})
    }

    fn add_tools(&mut self, tools: &[Value]) {
        for tool in tools {
            self.add_response_tool(tool);
        }
    }

    fn add_response_tool(&mut self, tool: &Value) {
        match tool {
            Value::String(name) => {
                self.add_custom_tool(&json!({"type":"custom","name":name}));
            }
            Value::Object(_) => match tool.get("type").and_then(Value::as_str) {
                Some("function") => self.add_function_tool(tool, None),
                Some("custom") => self.add_custom_tool(tool),
                Some("namespace") => self.add_namespace_tool(tool),
                Some("tool_search") => self.add_tool_search_tool(),
                Some("web_search" | "web_search_preview") => {
                    self.chat_tools.push(tool.clone());
                }
                Some(tool_type) => {
                    tracing::debug!(tool_type, "忽略不支持的 Responses 工具类型");
                }
                None => {}
            },
            _ => {}
        }
    }

    fn add_function_tool(&mut self, tool: &Value, namespace: Option<&str>) {
        let Some(original_name) = response_tool_name(tool) else {
            return;
        };
        let chat_name = namespace
            .map(|namespace| flatten_namespace_tool_name(namespace, &original_name))
            .unwrap_or_else(|| original_name.clone());
        let Some(chat_tool) = response_function_tool_to_chat(tool, &chat_name) else {
            return;
        };
        let spec = ToolSpec {
            kind: if namespace.is_some() {
                ToolKind::Namespace
            } else {
                ToolKind::Function
            },
            name: original_name,
            namespace: namespace.map(str::to_string),
        };
        self.add_chat_tool(chat_name, spec, chat_tool);
    }

    fn add_custom_tool(&mut self, tool: &Value) {
        let Some(name) = response_tool_name(tool) else {
            return;
        };
        let description = custom_tool_description(tool);
        let chat_tool = json!({
            "type":"function",
            "function":{
                "name":name,
                "description":description,
                "parameters":{
                    "type":"object",
                    "properties":{
                        CUSTOM_TOOL_INPUT_FIELD:{
                            "type":"string",
                            "description":"原始 custom tool 的自由格式输入. 必须严格保留格式, 并遵循工具定义"
                        }
                    },
                    "required":[CUSTOM_TOOL_INPUT_FIELD],
                    "additionalProperties":false
                }
            }
        });
        let spec = ToolSpec {
            kind: ToolKind::Custom,
            name: name.clone(),
            namespace: None,
        };
        self.add_chat_tool(name, spec, chat_tool);
    }

    fn add_namespace_tool(&mut self, tool: &Value) {
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
            if child.get("type").and_then(Value::as_str) == Some("function") {
                self.add_function_tool(child, Some(namespace));
            }
        }
    }

    fn add_tool_search_tool(&mut self) {
        let chat_tool = json!({
            "type":"function",
            "function":{
                "name":TOOL_SEARCH_CHAT_NAME,
                "description":"搜索并加载当前任务可用的 Codex 工具, 插件, connector 和 MCP namespace",
                "parameters":{
                    "type":"object",
                    "properties":{
                        "query":{"type":"string","description":"工具或 connector 搜索词"},
                        "limit":{"type":"integer","description":"最多返回的工具组数量"}
                    },
                    "required":["query"]
                }
            }
        });
        let spec = ToolSpec {
            kind: ToolKind::ToolSearch,
            name: TOOL_SEARCH_CHAT_NAME.to_string(),
            namespace: None,
        };
        self.add_chat_tool(TOOL_SEARCH_CHAT_NAME.to_string(), spec, chat_tool);
    }

    fn add_chat_tool(&mut self, chat_name: String, spec: ToolSpec, chat_tool: Value) {
        if chat_name.trim().is_empty() {
            return;
        }
        if self.chat_name_to_spec.contains_key(&chat_name) {
            tracing::warn!(
                chat_name,
                original_name = spec.name,
                namespace = ?spec.namespace,
                "Chat 工具名称冲突, 保留第一个定义"
            );
            return;
        }
        if let Some(namespace) = spec.namespace.as_ref() {
            self.namespace_name_to_chat_name.insert(
                (namespace.clone(), spec.name.clone()),
                chat_name.clone(),
            );
        }
        self.chat_name_to_spec.insert(chat_name, spec);
        self.chat_tools.push(chat_tool);
    }
}

fn collect_input_tools(value: &Value, item_type: &str, context: &mut ToolContext) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_input_tools(item, item_type, context);
            }
        }
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some(item_type)
                && let Some(tools) = object.get("tools").and_then(Value::as_array)
            {
                context.add_tools(tools);
            }
            for nested in object.values() {
                collect_input_tools(nested, item_type, context);
            }
        }
        _ => {}
    }
}

fn response_tool_name(tool: &Value) -> Option<String> {
    tool.get("function")
        .and_then(|function| function.get("name"))
        .or_else(|| tool.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn response_function_tool_to_chat(tool: &Value, chat_name: &str) -> Option<Value> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    if let Some(function) = tool.get("function") {
        let mut function = function.clone();
        let object = function.as_object_mut()?;
        object.insert("name".to_string(), json!(chat_name));
        if let Some(strict) = tool.get("strict") {
            object
                .entry("strict".to_string())
                .or_insert_with(|| strict.clone());
        }
        return Some(json!({"type":"function","function":function}));
    }
    let mut function = json!({
        "name":chat_name,
        "description":tool.get("description").cloned().unwrap_or(Value::Null),
        "parameters":tool.get("parameters").cloned().unwrap_or_else(|| json!({}))
    });
    if let Some(strict) = tool.get("strict") {
        function["strict"] = strict.clone();
    }
    Some(json!({"type":"function","function":function}))
}

fn custom_tool_description(tool: &Value) -> String {
    let definition = serde_json::to_string(tool).unwrap_or_else(|_| "{}".to_string());
    format!("原始工具定义:\n```json\n{definition}\n```")
}

fn flatten_namespace_tool_name(namespace: &str, name: &str) -> String {
    let full_name = format!("{namespace}__{name}");
    if full_name.len() <= CHAT_TOOL_NAME_MAX_LEN {
        return full_name;
    }
    let digest = Sha256::digest(full_name.as_bytes());
    let hash = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let suffix = format!("__{hash}");
    let prefix_len = CHAT_TOOL_NAME_MAX_LEN.saturating_sub(suffix.len());
    let mut prefix = String::new();
    for character in full_name.chars() {
        if prefix.len() + character.len_utf8() > prefix_len {
            break;
        }
        prefix.push(character);
    }
    format!("{prefix}{suffix}")
}
