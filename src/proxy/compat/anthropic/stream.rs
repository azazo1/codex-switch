use super::super::ChatResponseContext;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) struct AnthropicToResponsesSseConverter {
    response_id: String,
    model: String,
    sequence_number: u64,
    next_output_index: usize,
    blocks: BTreeMap<usize, AnthropicBlockState>,
    output: Vec<(usize, Value)>,
    usage: AnthropicUsage,
    stop_reason: Option<String>,
    context: Option<ChatResponseContext>,
    created: bool,
    completed: bool,
}

struct AnthropicBlockState {
    output_index: usize,
    item_id: String,
    kind: String,
    call_id: String,
    name: String,
    content: String,
}

#[derive(Default)]
struct AnthropicUsage {
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
}

impl AnthropicToResponsesSseConverter {
    pub(crate) fn new(context: Option<ChatResponseContext>) -> Self {
        Self {
            response_id: new_id("resp"),
            model: "unknown".to_string(),
            sequence_number: 0,
            next_output_index: 0,
            blocks: BTreeMap::new(),
            output: Vec::new(),
            usage: AnthropicUsage::default(),
            stop_reason: None,
            context,
            created: false,
            completed: false,
        }
    }

    pub(crate) fn push(&mut self, block: &str) -> Vec<u8> {
        let Some((event_name, value)) = sse_event(block) else {
            return Vec::new();
        };
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(event_name.as_deref().unwrap_or_default());
        let mut output = String::new();
        match event_type {
            "message_start" => self.message_start(&value, &mut output),
            "content_block_start" => self.content_block_start(&value, &mut output),
            "content_block_delta" => self.content_block_delta(&value, &mut output),
            "content_block_stop" => self.content_block_stop(&value, &mut output),
            "message_delta" => self.message_delta(&value),
            "message_stop" => output.push_str(&self.complete()),
            "error" => output.push_str(&self.fail(&value)),
            "ping" => {}
            _ => {}
        }
        output.into_bytes()
    }

    pub(crate) fn finish(&mut self) -> Vec<u8> {
        if !self.created || self.completed {
            Vec::new()
        } else {
            self.complete().into_bytes()
        }
    }

    fn message_start(&mut self, value: &Value, output: &mut String) {
        if let Some(message) = value.get("message") {
            if let Some(id) = message.get("id").and_then(Value::as_str) {
                self.response_id = id.to_string();
            }
            if let Some(model) = message.get("model").and_then(Value::as_str) {
                self.model = model.to_string();
            }
            self.merge_usage(message.get("usage"));
        }
        if self.created {
            return;
        }
        self.created = true;
        let response = self.response("in_progress");
        output.push_str(&self.event("response.created", json!({"response":response})));
        let response = self.response("in_progress");
        output.push_str(&self.event("response.in_progress", json!({"response":response})));
    }

    fn ensure_created(&mut self, output: &mut String) {
        if self.created {
            return;
        }
        self.created = true;
        let response = self.response("in_progress");
        output.push_str(&self.event("response.created", json!({"response":response})));
    }

    fn content_block_start(&mut self, value: &Value, output: &mut String) {
        self.ensure_created(output);
        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let Some(block) = value.get("content_block") else {
            return;
        };
        let kind = block.get("type").and_then(Value::as_str).unwrap_or_default();
        let output_index = self.next_output_index;
        self.next_output_index += 1;
        let (item_id, call_id, name) = match kind {
            "text" => (new_id("msg"), String::new(), String::new()),
            "thinking" => (new_id("rs"), String::new(), String::new()),
            "tool_use" => (
                new_id("fc"),
                block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ),
            "server_tool_use" if block.get("name").and_then(Value::as_str) == Some("web_search") => (
                block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("web_search")
                    .to_string(),
                String::new(),
                "web_search".to_string(),
            ),
            _ => return,
        };
        let item = match kind {
            "text" => json!({
                "id":item_id,
                "type":"message",
                "role":"assistant",
                "content":[],
                "status":"in_progress"
            }),
            "thinking" => json!({
                "id":item_id,
                "type":"reasoning",
                "summary":[],
                "status":"in_progress"
            }),
            "tool_use" => match &self.context {
                Some(context) => context.restore_tool_item(
                    &item_id,
                    "in_progress",
                    &call_id,
                    &name,
                    "",
                ),
                None => json!({
                    "id":item_id,
                    "type":"function_call",
                    "call_id":call_id,
                    "name":name,
                    "arguments":"",
                    "status":"in_progress"
                }),
            },
            _ => json!({
                "id":item_id,
                "type":"web_search_call",
                "action":block.get("input").cloned().unwrap_or_else(|| json!({})),
                "status":"in_progress"
            }),
        };
        output.push_str(&self.event(
            "response.output_item.added",
            json!({"output_index":output_index,"item":item}),
        ));
        if kind == "text" {
            output.push_str(&self.event(
                "response.content_part.added",
                json!({
                    "output_index":output_index,
                    "content_index":0,
                    "item_id":item_id,
                    "part":{"type":"output_text","text":"","annotations":[]}
                }),
            ));
        }
        let initial_content = match kind {
            "text" => block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            "thinking" => block
                .get("thinking")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            "tool_use" => String::new(),
            _ => String::new(),
        };
        let state_kind = if kind == "tool_use" {
            match item.get("type").and_then(Value::as_str) {
                Some("custom_tool_call") => "custom_tool_call",
                Some("tool_search_call") => "tool_search_call",
                _ => "tool_use",
            }
        } else {
            kind
        };
        self.blocks.insert(
            index,
            AnthropicBlockState {
                output_index,
                item_id,
                kind: state_kind.to_string(),
                call_id,
                name,
                content: initial_content,
            },
        );
    }

    fn content_block_delta(&mut self, value: &Value, output: &mut String) {
        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let Some(delta) = value.get("delta") else {
            return;
        };
        let delta_type = delta.get("type").and_then(Value::as_str).unwrap_or_default();
        let text = match delta_type {
            "text_delta" => delta.get("text").and_then(Value::as_str),
            "thinking_delta" => delta.get("thinking").and_then(Value::as_str),
            "input_json_delta" => delta.get("partial_json").and_then(Value::as_str),
            "signature_delta" => {
                tracing::debug!("dropping Anthropic thinking signature delta during protocol conversion");
                None
            }
            _ => None,
        };
        let Some(text) = text.filter(|value| !value.is_empty()) else {
            return;
        };
        let Some(state) = self.blocks.get_mut(&index) else {
            return;
        };
        state.content.push_str(text);
        let event = match delta_type {
            "text_delta" => "response.output_text.delta",
            "thinking_delta" => "response.reasoning_summary_text.delta",
            "input_json_delta" if state.kind == "custom_tool_call" => return,
            "input_json_delta" if state.kind == "tool_search_call" => return,
            "input_json_delta" => "response.function_call_arguments.delta",
            _ => return,
        };
        let data = json!({
            "output_index":state.output_index,
            "content_index":0,
            "summary_index":0,
            "item_id":state.item_id,
            "call_id":state.call_id,
            "name":state.name,
            "delta":text
        });
        output.push_str(&self.event(event, data));
    }

    fn content_block_stop(&mut self, value: &Value, output: &mut String) {
        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        output.push_str(&self.close_block(index));
    }

    fn close_block(&mut self, index: usize) -> String {
        let Some(state) = self.blocks.remove(&index) else {
            return String::new();
        };
        let mut output = String::new();
        let item = match state.kind.as_str() {
            "text" => {
                output.push_str(&self.event(
                    "response.output_text.done",
                    json!({
                        "output_index":state.output_index,
                        "content_index":0,
                        "item_id":state.item_id,
                        "text":state.content
                    }),
                ));
                output.push_str(&self.event(
                    "response.content_part.done",
                    json!({
                        "output_index":state.output_index,
                        "content_index":0,
                        "item_id":state.item_id,
                        "part":{"type":"output_text","text":state.content,"annotations":[]}
                    }),
                ));
                json!({
                    "id":state.item_id,
                    "type":"message",
                    "role":"assistant",
                    "content":[{"type":"output_text","text":state.content,"annotations":[]}],
                    "status":"completed"
                })
            }
            "thinking" => {
                output.push_str(&self.event(
                    "response.reasoning_summary_text.done",
                    json!({
                        "output_index":state.output_index,
                        "summary_index":0,
                        "item_id":state.item_id,
                        "text":state.content
                    }),
                ));
                json!({
                    "id":state.item_id,
                    "type":"reasoning",
                    "summary":[{"type":"summary_text","text":state.content}],
                    "status":"completed"
                })
            }
            "tool_use" => {
                output.push_str(&self.event(
                    "response.function_call_arguments.done",
                    json!({
                        "output_index":state.output_index,
                        "item_id":state.item_id,
                        "call_id":state.call_id,
                        "name":state.name,
                        "arguments":state.content
                    }),
                ));
                match &self.context {
                    Some(context) => context.restore_tool_item(
                        &state.item_id,
                        "completed",
                        &state.call_id,
                        &state.name,
                        &state.content,
                    ),
                    None => json!({
                        "id":state.item_id,
                        "type":"function_call",
                        "call_id":state.call_id,
                        "name":state.name,
                        "arguments":state.content,
                        "status":"completed"
                    }),
                }
            }
            "custom_tool_call" => {
                let item = self.context.as_ref().map_or_else(
                    || json!({
                        "id":state.item_id,
                        "type":"custom_tool_call",
                        "call_id":state.call_id,
                        "name":state.name,
                        "input":state.content,
                        "status":"completed"
                    }),
                    |context| context.restore_tool_item(
                        &state.item_id,
                        "completed",
                        &state.call_id,
                        &state.name,
                        &state.content,
                    ),
                );
                let input = item.get("input").and_then(Value::as_str).unwrap_or_default();
                if !input.is_empty() {
                    output.push_str(&self.event(
                        "response.custom_tool_call_input.delta",
                        json!({
                            "output_index":state.output_index,
                            "item_id":state.item_id,
                            "delta":input
                        }),
                    ));
                }
                output.push_str(&self.event(
                    "response.custom_tool_call_input.done",
                    json!({
                        "output_index":state.output_index,
                        "item_id":state.item_id,
                        "input":input
                    }),
                ));
                item
            }
            "tool_search_call" => self.context.as_ref().map_or_else(
                || json!({
                    "type":"tool_search_call",
                    "call_id":state.call_id,
                    "status":"completed",
                    "execution":"client",
                    "arguments":serde_json::from_str::<Value>(&state.content).unwrap_or_else(|_| json!({}))
                }),
                |context| context.restore_tool_item(
                    &state.item_id,
                    "completed",
                    &state.call_id,
                    &state.name,
                    &state.content,
                ),
            ),
            _ => json!({
                "id":state.item_id,
                "type":"web_search_call",
                "action":serde_json::from_str::<Value>(&state.content).unwrap_or_else(|_| json!({})),
                "status":"completed"
            }),
        };
        output.push_str(&self.event(
            "response.output_item.done",
            json!({"output_index":state.output_index,"item":item}),
        ));
        self.output.push((state.output_index, item));
        output
    }

    fn message_delta(&mut self, value: &Value) {
        if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
            self.stop_reason = Some(reason.to_string());
        }
        self.merge_usage(value.get("usage"));
    }

    fn merge_usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else {
            return;
        };
        self.usage.input_tokens = self
            .usage
            .input_tokens
            .max(int_field(usage, "input_tokens"));
        self.usage.output_tokens = self
            .usage
            .output_tokens
            .max(int_field(usage, "output_tokens"));
        self.usage.cache_read_input_tokens = self
            .usage
            .cache_read_input_tokens
            .max(int_field(usage, "cache_read_input_tokens"));
        self.usage.cache_creation_input_tokens = self
            .usage
            .cache_creation_input_tokens
            .max(int_field(usage, "cache_creation_input_tokens"));
    }

    fn complete(&mut self) -> String {
        if self.completed {
            return String::new();
        }
        let mut output = String::new();
        for index in self.blocks.keys().copied().collect::<Vec<_>>() {
            output.push_str(&self.close_block(index));
        }
        self.completed = true;
        let status = if self.stop_reason.as_deref() == Some("max_tokens") {
            "incomplete"
        } else {
            "completed"
        };
        let mut response = self.response(status);
        if status == "incomplete" {
            response["incomplete_details"] = json!({"reason":"max_output_tokens"});
        }
        output.push_str(&self.event(
            if status == "incomplete" {
                "response.incomplete"
            } else {
                "response.completed"
            },
            json!({"response":response}),
        ));
        output
    }

    fn fail(&mut self, value: &Value) -> String {
        if self.completed {
            return String::new();
        }
        self.completed = true;
        let error = value.get("error").cloned().unwrap_or_else(|| json!({
            "type":"api_error",
            "message":"Anthropic stream failed"
        }));
        let response = json!({
            "id":self.response_id,
            "object":"response",
            "model":self.model,
            "status":"failed",
            "output":[],
            "error":{
                "code":error.get("type").cloned().unwrap_or_else(|| json!("api_error")),
                "message":error.get("message").cloned().unwrap_or_else(|| json!("Anthropic stream failed"))
            }
        });
        self.event("response.failed", json!({"response":response}))
    }

    fn response(&self, status: &str) -> Value {
        let mut output = self.output.clone();
        output.sort_by_key(|(index, _)| *index);
        json!({
            "id":self.response_id,
            "object":"response",
            "model":self.model,
            "status":status,
            "output":output.into_iter().map(|(_, item)| item).collect::<Vec<_>>(),
            "usage":self.responses_usage()
        })
    }

    fn responses_usage(&self) -> Value {
        let total_input = self.usage.input_tokens
            + self.usage.cache_read_input_tokens
            + self.usage.cache_creation_input_tokens;
        json!({
            "input_tokens":total_input,
            "output_tokens":self.usage.output_tokens,
            "total_tokens":total_input + self.usage.output_tokens,
            "input_tokens_details":{"cached_tokens":self.usage.cache_read_input_tokens},
            "cache_creation_input_tokens":self.usage.cache_creation_input_tokens
        })
    }

    fn event(&mut self, event_type: &str, mut data: Value) -> String {
        if let Some(object) = data.as_object_mut() {
            object.insert("type".to_string(), json!(event_type));
            object.insert("sequence_number".to_string(), json!(self.sequence_number));
        }
        self.sequence_number += 1;
        format!("event: {event_type}\ndata: {data}\n\n")
    }
}

pub(crate) struct ResponsesToAnthropicSseConverter {
    response_id: String,
    model: String,
    next_block_index: usize,
    blocks: BTreeMap<usize, ResponsesBlockState>,
    started: bool,
    stopped: bool,
    has_tool: bool,
}

struct ResponsesBlockState {
    block_index: usize,
    kind: String,
    content: String,
    had_delta: bool,
}

impl ResponsesToAnthropicSseConverter {
    pub(crate) fn new(model: Option<String>) -> Self {
        Self {
            response_id: new_id("msg"),
            model: model.unwrap_or_else(|| "unknown".to_string()),
            next_block_index: 0,
            blocks: BTreeMap::new(),
            started: false,
            stopped: false,
            has_tool: false,
        }
    }

    pub(crate) fn push(&mut self, block: &str) -> Vec<u8> {
        let Some((event_name, value)) = sse_event(block) else {
            return Vec::new();
        };
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(event_name.as_deref().unwrap_or_default());
        let mut output = String::new();
        match event_type {
            "response.created" | "response.in_progress" => {
                if let Some(response) = value.get("response") {
                    if let Some(id) = response.get("id").and_then(Value::as_str) {
                        self.response_id = id.to_string();
                    }
                    if let Some(model) = response.get("model").and_then(Value::as_str) {
                        self.model = model.to_string();
                    }
                }
                output.push_str(&self.ensure_started());
            }
            "response.output_item.added" => {
                output.push_str(&self.ensure_started());
                output.push_str(&self.output_item_added(&value));
            }
            "response.output_text.delta" => {
                output.push_str(&self.ensure_started());
                output.push_str(&self.text_delta(&value));
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                output.push_str(&self.ensure_started());
                output.push_str(&self.reasoning_delta(&value));
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                output.push_str(&self.tool_delta(&value, event_type));
            }
            "response.function_call_arguments.done" | "response.custom_tool_call_input.done" => {
                output.push_str(&self.tool_done(&value));
            }
            "response.output_item.done" => output.push_str(&self.output_item_done(&value)),
            "response.output_text.done" | "response.reasoning_summary_text.done" => {
                if let Some(index) = value.get("output_index").and_then(Value::as_u64) {
                    output.push_str(&self.close_block(index as usize));
                }
            }
            "response.completed" | "response.done" | "response.incomplete" => {
                output.push_str(&self.complete(&value));
            }
            "response.failed" => output.push_str(&self.fail(&value)),
            _ => {}
        }
        output.into_bytes()
    }

    pub(crate) fn finish(&mut self) -> Vec<u8> {
        if !self.started || self.stopped {
            return Vec::new();
        }
        self.complete(&Value::Null).into_bytes()
    }

    fn ensure_started(&mut self) -> String {
        if self.started {
            return String::new();
        }
        self.started = true;
        anthropic_event(
            "message_start",
            json!({
                "type":"message_start",
                "message":{
                    "id":self.response_id,
                    "type":"message",
                    "role":"assistant",
                    "model":self.model,
                    "content":[],
                    "stop_reason":Value::Null,
                    "stop_sequence":Value::Null,
                    "usage":{"input_tokens":0,"output_tokens":0}
                }
            }),
        )
    }

    fn output_item_added(&mut self, value: &Value) -> String {
        let Some(item) = value.get("item") else {
            return String::new();
        };
        let output_index = value.get("output_index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "reasoning" => self.open_block(
                output_index,
                "thinking",
                json!({"type":"thinking","thinking":""}),
            ),
            "function_call" | "custom_tool_call" => {
                self.has_tool = true;
                let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
                let id = anthropic_call_id(item);
                let output = self.open_block(
                    output_index,
                    if kind == "custom_tool_call" { "custom_tool" } else { "tool_use" },
                    json!({"type":"tool_use","id":id,"name":name,"input":{}}),
                );
                if let Some(arguments) = item.get("arguments").and_then(Value::as_str).filter(|value| !value.is_empty())
                    && let Some(state) = self.blocks.get_mut(&output_index)
                {
                    state.content.push_str(arguments);
                }
                output
            }
            "web_search_call" => {
                self.has_tool = true;
                let id = format!("srvtoolu_{}", item.get("id").and_then(Value::as_str).unwrap_or_default());
                self.open_block(
                    output_index,
                    "server_tool_use",
                    json!({
                        "type":"server_tool_use",
                        "id":id,
                        "name":"web_search",
                        "input":item.get("action").cloned().unwrap_or_else(|| json!({}))
                    }),
                )
            }
            _ => String::new(),
        }
    }

    fn open_block(
        &mut self,
        output_index: usize,
        kind: &str,
        content_block: Value,
    ) -> String {
        if self.blocks.contains_key(&output_index) {
            return String::new();
        }
        let block_index = self.next_block_index;
        self.next_block_index += 1;
        self.blocks.insert(
            output_index,
            ResponsesBlockState {
                block_index,
                kind: kind.to_string(),
                content: String::new(),
                had_delta: false,
            },
        );
        anthropic_event(
            "content_block_start",
            json!({
                "type":"content_block_start",
                "index":block_index,
                "content_block":content_block
            }),
        )
    }

    fn text_delta(&mut self, value: &Value) -> String {
        let output_index = value.get("output_index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let mut output = String::new();
        if !self.blocks.contains_key(&output_index) {
            output.push_str(&self.open_block(
                output_index,
                "text",
                json!({"type":"text","text":""}),
            ));
        }
        let delta = value.get("delta").and_then(Value::as_str).unwrap_or_default();
        if let Some(state) = self.blocks.get_mut(&output_index) {
            state.content.push_str(delta);
            state.had_delta = true;
            output.push_str(&anthropic_event(
                "content_block_delta",
                json!({
                    "type":"content_block_delta",
                    "index":state.block_index,
                    "delta":{"type":"text_delta","text":delta}
                }),
            ));
        }
        output
    }

    fn reasoning_delta(&mut self, value: &Value) -> String {
        let output_index = value.get("output_index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let mut output = String::new();
        if !self.blocks.contains_key(&output_index) {
            output.push_str(&self.open_block(
                output_index,
                "thinking",
                json!({"type":"thinking","thinking":""}),
            ));
        }
        let delta = value.get("delta").and_then(Value::as_str).unwrap_or_default();
        if let Some(state) = self.blocks.get_mut(&output_index) {
            state.content.push_str(delta);
            state.had_delta = true;
            output.push_str(&anthropic_event(
                "content_block_delta",
                json!({
                    "type":"content_block_delta",
                    "index":state.block_index,
                    "delta":{"type":"thinking_delta","thinking":delta}
                }),
            ));
        }
        output
    }

    fn tool_delta(&mut self, value: &Value, event_type: &str) -> String {
        let output_index = value.get("output_index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let delta = value.get("delta").and_then(Value::as_str).unwrap_or_default();
        let Some(state) = self.blocks.get_mut(&output_index) else {
            return String::new();
        };
        state.content.push_str(delta);
        if event_type == "response.custom_tool_call_input.delta" {
            return String::new();
        }
        state.had_delta = true;
        anthropic_event(
            "content_block_delta",
            json!({
                "type":"content_block_delta",
                "index":state.block_index,
                "delta":{"type":"input_json_delta","partial_json":delta}
            }),
        )
    }

    fn tool_done(&mut self, value: &Value) -> String {
        let output_index = value.get("output_index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let Some(state) = self.blocks.get_mut(&output_index) else {
            return String::new();
        };
        if state.content.is_empty() {
            state.content = value
                .get("arguments")
                .or_else(|| value.get("input"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
        let mut output = String::new();
        if !state.had_delta && !state.content.is_empty() {
            let partial = if state.kind == "custom_tool" {
                json!({"input":state.content}).to_string()
            } else {
                state.content.clone()
            };
            output.push_str(&anthropic_event(
                "content_block_delta",
                json!({
                    "type":"content_block_delta",
                    "index":state.block_index,
                    "delta":{"type":"input_json_delta","partial_json":partial}
                }),
            ));
        }
        output.push_str(&self.close_block(output_index));
        output
    }

    fn output_item_done(&mut self, value: &Value) -> String {
        let output_index = value.get("output_index").and_then(Value::as_u64).unwrap_or(0) as usize;
        if let Some(item) = value.get("item")
            && let Some(state) = self.blocks.get_mut(&output_index)
            && state.content.is_empty()
        {
            state.content = item
                .get("arguments")
                .or_else(|| item.get("input"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
        self.close_block(output_index)
    }

    fn close_block(&mut self, output_index: usize) -> String {
        let Some(state) = self.blocks.remove(&output_index) else {
            return String::new();
        };
        anthropic_event(
            "content_block_stop",
            json!({"type":"content_block_stop","index":state.block_index}),
        )
    }

    fn complete(&mut self, value: &Value) -> String {
        if self.stopped {
            return String::new();
        }
        let mut output = self.ensure_started();
        for index in self.blocks.keys().copied().collect::<Vec<_>>() {
            output.push_str(&self.close_block(index));
        }
        let response = value.get("response").unwrap_or(value);
        let incomplete = response.get("status").and_then(Value::as_str) == Some("incomplete")
            || value.get("type").and_then(Value::as_str) == Some("response.incomplete");
        let stop_reason = if incomplete {
            "max_tokens"
        } else if self.has_tool {
            "tool_use"
        } else {
            "end_turn"
        };
        output.push_str(&anthropic_event(
            "message_delta",
            json!({
                "type":"message_delta",
                "delta":{"stop_reason":stop_reason,"stop_sequence":Value::Null},
                "usage":responses_usage_to_anthropic(response.get("usage"))
            }),
        ));
        output.push_str(&anthropic_event("message_stop", json!({"type":"message_stop"})));
        self.stopped = true;
        output
    }

    fn fail(&mut self, value: &Value) -> String {
        if self.stopped {
            return String::new();
        }
        self.stopped = true;
        let error = value
            .pointer("/response/error")
            .or_else(|| value.get("error"))
            .cloned()
            .unwrap_or_else(|| json!({"code":"api_error","message":"Responses stream failed"}));
        anthropic_event(
            "error",
            json!({
                "type":"error",
                "error":{
                    "type":error.get("code").or_else(|| error.get("type")).cloned().unwrap_or_else(|| json!("api_error")),
                    "message":error.get("message").cloned().unwrap_or_else(|| json!("Responses stream failed"))
                }
            }),
        )
    }
}

fn sse_event(block: &str) -> Option<(Option<String>, Value)> {
    let mut event = None;
    let mut data = String::new();
    for line in block.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    serde_json::from_str(&data).ok().map(|value| (event, value))
}

fn anthropic_event(event_type: &str, value: Value) -> String {
    format!("event: {event_type}\ndata: {value}\n\n")
}

fn responses_usage_to_anthropic(value: Option<&Value>) -> Value {
    let input = value
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let output = value
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let cache_read = value
        .and_then(|usage| usage.pointer("/input_tokens_details/cached_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let cache_creation = value
        .and_then(|usage| usage.get("cache_creation_input_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    json!({
        "input_tokens":input.saturating_sub(cache_read + cache_creation),
        "output_tokens":output,
        "cache_read_input_tokens":cache_read,
        "cache_creation_input_tokens":cache_creation
    })
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

fn int_field(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}
