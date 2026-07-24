use super::{ChatResponseContext, response_tool_item_from_chat_name};
use super::shared::{
    chat_message_text, custom_input, encode_reasoning, message_item_with_id, new_call_id,
    new_item_id, normalize_chat_usage, reasoning_from_chat,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) struct ChatSseConverter {
    context: ChatResponseContext,
    response_id: String,
    model: String,
    sequence_number: u64,
    next_output_index: usize,
    reasoning: Option<ReasoningState>,
    text: Option<TextState>,
    tools: BTreeMap<usize, ToolState>,
    usage: Value,
    outputs_finalized: bool,
    completed: bool,
}

struct ReasoningState {
    output_index: usize,
    item_id: String,
    content: String,
}

struct TextState {
    output_index: usize,
    item_id: String,
    content: String,
}

#[derive(Default)]
struct ToolState {
    output_index: Option<usize>,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
    added: bool,
}

impl ChatSseConverter {
    pub(crate) fn new(context: ChatResponseContext) -> Self {
        Self {
            model: context.model.clone().unwrap_or_else(|| "unknown".to_string()),
            context,
            response_id: new_item_id("resp"),
            sequence_number: 0,
            next_output_index: 0,
            reasoning: None,
            text: None,
            tools: BTreeMap::new(),
            usage: normalize_chat_usage(None),
            outputs_finalized: false,
            completed: false,
        }
    }

    pub(crate) fn initial_events(&mut self) -> Vec<u8> {
        let response = json!({
            "id":self.response_id,
            "object":"response",
            "model":self.model,
            "status":"in_progress",
            "output":[]
        });
        let mut out = String::new();
        out.push_str(&self.event("response.created", json!({"response":response})));
        out.push_str(&self.event("response.in_progress", json!({"response":response})));
        out.into_bytes()
    }

    pub(crate) fn convert_block(&mut self, block: &str) -> Vec<u8> {
        let mut out = String::new();
        for line in block.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                out.push_str(&self.finish_events());
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if let Some(error) = value.get("error") {
                self.completed = true;
                let response = json!({
                    "id":self.response_id,
                    "object":"response",
                    "model":self.model,
                    "status":"failed",
                    "output":[],
                    "error":{
                        "code":error.get("code").or_else(|| error.get("type")).cloned().unwrap_or_else(|| json!("api_error")),
                        "message":error.get("message").cloned().unwrap_or_else(|| json!("Chat stream failed"))
                    }
                });
                out.push_str(&self.event("response.failed", json!({"response":response})));
                continue;
            }
            if let Some(model) = value.get("model").and_then(Value::as_str) {
                self.model = model.to_string();
            }
            if let Some(usage) = value.get("usage") {
                self.usage = normalize_chat_usage(Some(usage));
            }
            let Some(choice) = value.pointer("/choices/0") else {
                continue;
            };
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            if let Some(reasoning) = reasoning_from_chat(delta)
                && !reasoning.is_empty()
            {
                self.ensure_reasoning(&mut out);
                if let Some(state) = &mut self.reasoning {
                    state.content.push_str(&reasoning);
                }
            }
            if let Some(content) = chat_message_text(delta.get("content"))
                && !content.is_empty()
            {
                self.ensure_text(&mut out);
                let (output_index, item_id) = {
                    let state = self.text.as_mut().expect("text state exists");
                    state.content.push_str(&content);
                    (state.output_index, state.item_id.clone())
                };
                out.push_str(&self.event(
                    "response.output_text.delta",
                    json!({
                        "output_index":output_index,
                        "content_index":0,
                        "item_id":item_id,
                        "delta":content
                    }),
                ));
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool_call in tool_calls {
                    self.append_tool_delta(tool_call, &mut out);
                }
            }
            if choice.get("finish_reason").is_some_and(|value| !value.is_null()) {
                out.push_str(&self.finalize_outputs());
            }
        }
        out.into_bytes()
    }

    pub(crate) fn finish(&mut self) -> Vec<u8> {
        self.finish_events().into_bytes()
    }

    fn ensure_reasoning(&mut self, out: &mut String) {
        if self.reasoning.is_some() {
            return;
        }
        let output_index = self.allocate_output_index();
        let item_id = new_item_id("rs");
        self.reasoning = Some(ReasoningState {
            output_index,
            item_id: item_id.clone(),
            content: String::new(),
        });
        out.push_str(&self.event(
            "response.output_item.added",
            json!({
                "output_index":output_index,
                "item":{"id":item_id,"type":"reasoning","summary":[],"status":"in_progress"}
            }),
        ));
    }

    fn ensure_text(&mut self, out: &mut String) {
        if self.text.is_some() {
            return;
        }
        let output_index = self.allocate_output_index();
        let item_id = new_item_id("msg");
        self.text = Some(TextState {
            output_index,
            item_id: item_id.clone(),
            content: String::new(),
        });
        out.push_str(&self.event(
            "response.output_item.added",
            json!({
                "output_index":output_index,
                "item":{"id":item_id,"type":"message","role":"assistant","content":[],"status":"in_progress"}
            }),
        ));
        out.push_str(&self.event(
            "response.content_part.added",
            json!({
                "output_index":output_index,
                "content_index":0,
                "item_id":item_id,
                "part":{"type":"output_text","text":"","annotations":[]}
            }),
        ));
    }

    fn append_tool_delta(&mut self, tool_call: &Value, out: &mut String) {
        let index = tool_call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let name_delta = tool_call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let argument_delta = tool_call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id_delta = tool_call.get("id").and_then(Value::as_str);
        let (should_add, already_added, current_name, pending_arguments, output_index, item_id) = {
            let state = self.tools.entry(index).or_default();
            if let Some(call_id) = id_delta
                && state.call_id.is_empty()
            {
                state.call_id = call_id.to_string();
            }
            if !name_delta.is_empty() {
                state.name.push_str(name_delta);
            }
            if !argument_delta.is_empty() {
                state.arguments.push_str(argument_delta);
            }
            (
                !state.added && !state.name.is_empty(),
                state.added,
                state.name.clone(),
                state.arguments.clone(),
                state.output_index,
                state.item_id.clone(),
            )
        };

        let should_add = should_add
            && (self
                .context
                .tool_context
                .lookup_chat_name(&current_name)
                .is_some()
                || !pending_arguments.is_empty());
        let custom = self.context.is_custom_tool(&current_name);
        if should_add {
            let assigned_index = self.allocate_output_index();
            let item_prefix = self.context.item_id_prefix(&current_name);
            let (item_id, call_id, name) = {
                let state = self.tools.get_mut(&index).expect("tool state exists");
                if state.call_id.is_empty() {
                    state.call_id = new_call_id();
                }
                state.output_index = Some(assigned_index);
                state.item_id = new_item_id(item_prefix);
                state.added = true;
                (
                    state.item_id.clone(),
                    state.call_id.clone(),
                    state.name.clone(),
                )
            };
            let item = response_tool_item_from_chat_name(
                &item_id,
                "in_progress",
                &call_id,
                &name,
                "",
                &self.context,
            );
            out.push_str(&self.event(
                "response.output_item.added",
                json!({"output_index":assigned_index,"item":item}),
            ));
            if !pending_arguments.is_empty() && !custom {
                out.push_str(&self.event(
                    "response.function_call_arguments.delta",
                    json!({
                        "output_index":assigned_index,
                        "item_id":item_id,
                        "delta":pending_arguments
                    }),
                ));
            }
        } else if already_added && !argument_delta.is_empty() && !custom
            && let Some(output_index) = output_index
        {
            out.push_str(&self.event(
                "response.function_call_arguments.delta",
                json!({
                    "output_index":output_index,
                    "item_id":item_id,
                    "delta":argument_delta
                }),
            ));
        }
    }

    fn finalize_outputs(&mut self) -> String {
        if self.outputs_finalized {
            return String::new();
        }
        self.outputs_finalized = true;
        let mut out = String::new();
        if let Some(state) = &self.reasoning {
            let output_index = state.output_index;
            let item = json!({
                "id":state.item_id,
                "type":"reasoning",
                "summary":[],
                "encrypted_content":encode_reasoning(&state.content),
                "status":"completed"
            });
            out.push_str(&self.event(
                "response.output_item.done",
                json!({"output_index":output_index,"item":item}),
            ));
        }
        if let Some(state) = &self.text {
            let output_index = state.output_index;
            let item_id = state.item_id.clone();
            let content = state.content.clone();
            out.push_str(&self.event(
                "response.output_text.done",
                json!({
                    "output_index":output_index,
                    "content_index":0,
                    "item_id":item_id,
                    "text":content
                }),
            ));
            out.push_str(&self.event(
                "response.content_part.done",
                json!({
                    "output_index":output_index,
                    "content_index":0,
                    "item_id":item_id,
                    "part":{"type":"output_text","text":content,"annotations":[]}
                }),
            ));
            out.push_str(&self.event(
                "response.output_item.done",
                json!({
                    "output_index":output_index,
                    "item":message_item_with_id(&item_id, &content)
                }),
            ));
        }
        let tool_indexes = self.tools.keys().copied().collect::<Vec<_>>();
        for index in tool_indexes {
            let Some(state) = self.tools.get(&index) else {
                continue;
            };
            if !state.added {
                tracing::warn!(tool_index = index, "忽略缺少名称的流式工具调用");
                continue;
            }
            let Some(output_index) = state.output_index else {
                continue;
            };
            let item_id = state.item_id.clone();
            let call_id = state.call_id.clone();
            let name = state.name.clone();
            let arguments = state.arguments.clone();
            let custom = self.context.is_custom_tool(&name);
            let item = response_tool_item_from_chat_name(
                &item_id,
                "completed",
                &call_id,
                &name,
                &arguments,
                &self.context,
            );
            if custom {
                let input = custom_input(&arguments);
                if !input.is_empty() {
                    out.push_str(&self.event(
                        "response.custom_tool_call_input.delta",
                        json!({"output_index":output_index,"item_id":item_id,"delta":input}),
                    ));
                }
                out.push_str(&self.event(
                    "response.custom_tool_call_input.done",
                    json!({"output_index":output_index,"item_id":item_id,"input":input}),
                ));
                out.push_str(&self.event(
                    "response.output_item.done",
                    json!({"output_index":output_index,"item":item}),
                ));
            } else {
                out.push_str(&self.event(
                    "response.function_call_arguments.done",
                    json!({
                        "output_index":output_index,
                        "item_id":item_id,
                        "name":name,
                        "arguments":arguments
                    }),
                ));
                out.push_str(&self.event(
                    "response.output_item.done",
                    json!({"output_index":output_index,"item":item}),
                ));
            }
        }
        out
    }

    fn finish_events(&mut self) -> String {
        if self.completed {
            return String::new();
        }
        let mut out = self.finalize_outputs();
        self.completed = true;
        let output = self.completed_output();
        out.push_str(&self.event(
            "response.completed",
            json!({
                "response":{
                    "id":self.response_id,
                    "object":"response",
                    "model":self.model,
                    "status":"completed",
                    "output":output,
                    "usage":self.usage
                }
            }),
        ));
        out
    }

    fn completed_output(&self) -> Vec<Value> {
        let mut output = Vec::new();
        if let Some(state) = &self.reasoning {
            output.push((
                state.output_index,
                json!({
                    "id":state.item_id,
                    "type":"reasoning",
                    "summary":[],
                    "encrypted_content":encode_reasoning(&state.content),
                    "status":"completed"
                }),
            ));
        }
        if let Some(state) = &self.text {
            output.push((
                state.output_index,
                message_item_with_id(&state.item_id, &state.content),
            ));
        }
        for state in self.tools.values() {
            if !state.added {
                continue;
            }
            let Some(output_index) = state.output_index else {
                continue;
            };
            let item = response_tool_item_from_chat_name(
                &state.item_id,
                "completed",
                &state.call_id,
                &state.name,
                &state.arguments,
                &self.context,
            );
            output.push((output_index, item));
        }
        output.sort_by_key(|(index, _)| *index);
        output.into_iter().map(|(_, item)| item).collect()
    }

    fn allocate_output_index(&mut self) -> usize {
        let index = self.next_output_index;
        self.next_output_index += 1;
        index
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
