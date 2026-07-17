use super::*;

#[test]
fn downgrades_developer_role_for_chat_upstreams() {
    let body = normalize_chat_request_json(
        br#"{"model":"chat-model","messages":[{"role":"developer","content":"rules"},{"role":"user","content":"hello"}]}"#,
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(value["messages"][0]["role"], "system");
    assert_eq!(value["messages"][1]["role"], "user");
}

#[test]
fn downgrades_developer_role_for_unknown_responses_item_type() {
    let request = json!({
        "model":"chat-model",
        "input":[{
            "type":"input_message",
            "role":"developer",
            "content":[{"type":"input_text","text":"rules"}]
        }]
    });
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let value: Value = serde_json::from_slice(&converted.body).unwrap();

    assert_eq!(value["messages"][0]["role"], "system");
}

#[test]
fn converts_codex_tools_and_round_trips_reasoning() {
    let context = ChatResponseContext {
        tool_context: ToolContext::from_request(&json!({
            "tools":[{"type":"custom","name":"apply_patch"}]
        })),
        model: Some("deepseek-v4-pro".to_string()),
    };
    let first_response = chat_to_responses_json(
        &json!({
            "model":"deepseek-v4-pro",
            "choices":[{"message":{
                "role":"assistant",
                "content":null,
                "reasoning_content":"需要先读取文件",
                "tool_calls":[{
                    "id":"call_read",
                    "type":"function",
                    "function":{"name":"read_file","arguments":"{\"path\":\"src/main.rs\"}"}
                }]
            },"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14}
        }),
        &context,
    );
    let reasoning = first_response["output"][0].clone();
    let call = first_response["output"][1].clone();
    let request = json!({
        "model":"deepseek-v4-pro",
        "instructions":"完成代码任务",
        "input":[
            reasoning,
            call,
            {"type":"function_call_output","call_id":"call_read","output":"fn main() {}"}
        ],
        "tools":[
            {"type":"function","name":"read_file","description":"读取文件","parameters":{"type":"object"}},
            {"type":"custom","name":"apply_patch","description":"修改文件","format":{"type":"text"}}
        ],
        "stream":true
    });
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let body: Value = serde_json::from_slice(&converted.body).unwrap();

    assert_eq!(body["messages"][1]["reasoning_content"], "需要先读取文件");
    assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call_read");
    assert_eq!(body["messages"][2]["role"], "tool");
    assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    assert_eq!(body["tools"][1]["function"]["name"], "apply_patch");
    assert!(converted.response_context.is_custom_tool("apply_patch"));
}

#[test]
fn converts_multimodal_message_parts() {
    let request = json!({
        "model":"vision-model",
        "input":[{"type":"message","role":"user","content":[
            {"type":"input_text","text":"描述图片"},
            {"type":"input_image","image_url":"data:image/png;base64,abc"}
        ]}]
    });
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let body: Value = serde_json::from_slice(&converted.body).unwrap();

    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
}

#[test]
fn converts_streaming_tool_call_to_responses_events() {
    let context = ChatResponseContext {
        tool_context: ToolContext::default(),
        model: Some("deepseek-v4-pro".to_string()),
    };
    let mut converter = ChatSseConverter::new(context);
    let mut events = String::from_utf8(converter.initial_events()).unwrap();
    events.push_str(
        &String::from_utf8(converter.convert_block(
            r#"data: {"choices":[{"delta":{"reasoning_content":"先检查"}}]}"#,
        ))
        .unwrap(),
    );
    events.push_str(
        &String::from_utf8(converter.convert_block(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
        ))
        .unwrap(),
    );
    events.push_str(
        &String::from_utf8(converter.convert_block(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"src/main.rs\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        ))
        .unwrap(),
    );
    events.push_str(
        &String::from_utf8(converter.convert_block(
            r#"data: {"choices":[],"usage":{"prompt_tokens":8,"completion_tokens":3,"total_tokens":11}}"#,
        ))
        .unwrap(),
    );
    events.push_str(&String::from_utf8(converter.convert_block("data: [DONE]")).unwrap());

    assert!(events.contains("response.function_call_arguments.delta"));
    assert!(events.contains("response.function_call_arguments.done"));
    assert!(events.contains("\"call_id\":\"call_1\""));
    assert!(events.contains("response.completed"));
    assert!(events.contains("\"input_tokens\":8"));
}

#[test]
fn converts_additional_tools_without_creating_empty_messages() {
    let request = additional_tools_request();
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let body: Value = serde_json::from_slice(&converted.body).unwrap();
    let names = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(names.len(), 9);
    assert!(names.contains(&"exec"));
    assert!(names.contains(&"wait"));
    assert!(names.contains(&"request_user_input"));
    assert!(names.contains(&"collaboration__spawn_agent"));
    assert!(names.contains(&"collaboration__wait_agent"));
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], false);

    let exec = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "exec")
        .unwrap();
    let description = exec["function"]["description"].as_str().unwrap();
    assert!(description.contains("\"format\""));
    assert!(description.contains("\"syntax\":\"lark\""));
    assert_eq!(
        exec["function"]["parameters"]["required"][0],
        "input"
    );
    assert_eq!(
        body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == "wait")
            .unwrap()["function"]["strict"],
        true
    );
}

#[test]
fn drops_tool_controls_when_no_tools_are_available() {
    let request = json!({
        "model":"chat-model",
        "input":"hello",
        "tool_choice":"auto",
        "parallel_tool_calls":true
    });
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let body: Value = serde_json::from_slice(&converted.body).unwrap();

    assert!(body.get("tools").is_none());
    assert!(body.get("tool_choice").is_none());
    assert!(body.get("parallel_tool_calls").is_none());
}

#[test]
fn restores_custom_namespace_and_tool_search_non_streaming_calls() {
    let mut request = additional_tools_request();
    request["tools"] = json!([{"type":"tool_search"}]);
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let response = chat_to_responses_json(
        &json!({
            "model":"domestic-coder",
            "choices":[{
                "message":{
                    "role":"assistant",
                    "content":null,
                    "tool_calls":[
                        {
                            "id":"call_exec",
                            "type":"function",
                            "function":{"name":"exec","arguments":"{\"input\":\"await tools.apply_patch()\"}"}
                        },
                        {
                            "id":"call_spawn",
                            "type":"function",
                            "function":{"name":"collaboration__spawn_agent","arguments":"{\"task_name\":\"check\",\"message\":\"inspect\"}"}
                        },
                        {
                            "id":"call_search",
                            "type":"function",
                            "function":{"name":"tool_search","arguments":"{\"query\":\"mail\",\"limit\":3}"}
                        }
                    ]
                },
                "finish_reason":"tool_calls"
            }]
        }),
        &converted.response_context,
    );

    assert_eq!(response["output"][0]["type"], "custom_tool_call");
    assert_eq!(response["output"][0]["name"], "exec");
    assert_eq!(
        response["output"][0]["input"],
        "await tools.apply_patch()"
    );
    assert_eq!(response["output"][1]["type"], "function_call");
    assert_eq!(response["output"][1]["namespace"], "collaboration");
    assert_eq!(response["output"][1]["name"], "spawn_agent");
    assert_eq!(response["output"][2]["type"], "tool_search_call");
    assert_eq!(response["output"][2]["arguments"]["query"], "mail");
}

#[test]
fn converts_namespace_history_and_loaded_tool_search_tools() {
    let request = json!({
        "model":"chat-model",
        "tools":[{"type":"tool_search"}],
        "input":[
            {
                "type":"tool_search_call",
                "call_id":"call_search",
                "arguments":{"query":"mail"}
            },
            {
                "type":"tool_search_output",
                "call_id":"call_search",
                "tools":[{
                    "type":"namespace",
                    "name":"mail",
                    "tools":[{
                        "type":"function",
                        "name":"search",
                        "description":"Search mail",
                        "parameters":{"type":"object"}
                    }]
                }]
            },
            {
                "type":"function_call",
                "call_id":"call_mail",
                "namespace":"mail",
                "name":"search",
                "arguments":"{\"query\":\"unread\"}"
            },
            {
                "type":"function_call_output",
                "call_id":"call_mail",
                "output":"done"
            }
        ]
    });
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let body: Value = serde_json::from_slice(&converted.body).unwrap();

    assert_eq!(body["tools"].as_array().unwrap().len(), 2);
    assert_eq!(body["tools"][1]["function"]["name"], "mail__search");
    assert_eq!(
        body["messages"][2]["tool_calls"][0]["function"]["name"],
        "mail__search"
    );
    assert_eq!(body["messages"][3]["tool_call_id"], "call_mail");
}

#[test]
fn restores_fragmented_custom_and_namespace_stream_calls() {
    let request = additional_tools_request();
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let mut converter = ChatSseConverter::new(converted.response_context);
    let mut events = String::from_utf8(converter.initial_events()).unwrap();
    for block in [
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_exec","function":{"name":"ex"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"ec","arguments":"{\"input\":\"await "}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"tools.apply_patch()\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ] {
        events.push_str(&String::from_utf8(converter.convert_block(block)).unwrap());
    }

    assert!(events.contains("response.custom_tool_call_input.delta"));
    assert!(events.contains("response.custom_tool_call_input.done"));
    assert!(events.contains("\"type\":\"custom_tool_call\""));
    assert!(events.contains("\"name\":\"exec\""));
    assert!(events.contains("await tools.apply_patch()"));

    let request = additional_tools_request();
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let mut converter = ChatSseConverter::new(converted.response_context);
    let mut namespace_events = String::new();
    for block in [
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_spawn","function":{"name":"collaboration__spawn_"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"agent","arguments":"{\"task_name\":\"check\",\"message\":\"inspect\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ] {
        namespace_events
            .push_str(&String::from_utf8(converter.convert_block(block)).unwrap());
    }

    assert!(namespace_events.contains("\"namespace\":\"collaboration\""));
    assert!(namespace_events.contains("\"name\":\"spawn_agent\""));
    assert!(namespace_events.contains("response.function_call_arguments.done"));
}

#[test]
fn hashes_long_namespace_names_and_restores_original_identity() {
    let namespace = "namespace_with_a_name_that_is_longer_than_the_chat_tool_limit";
    let function = "function_with_an_equally_long_name";
    let request = json!({
        "model":"chat-model",
        "input":[{
            "type":"additional_tools",
            "role":"developer",
            "tools":[{
                "type":"namespace",
                "name":namespace,
                "tools":[{
                    "type":"function",
                    "name":function,
                    "parameters":{"type":"object"}
                }]
            }]
        }]
    });
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let body: Value = serde_json::from_slice(&converted.body).unwrap();
    let chat_name = body["tools"][0]["function"]["name"].as_str().unwrap();

    assert_eq!(chat_name.len(), 64);
    assert!(!chat_name.contains(function));
    let response = chat_to_responses_json(
        &json!({
            "choices":[{
                "message":{
                    "tool_calls":[{
                        "id":"call_long",
                        "function":{"name":chat_name,"arguments":"{}"}
                    }]
                },
                "finish_reason":"tool_calls"
            }]
        }),
        &converted.response_context,
    );
    assert_eq!(response["output"][0]["namespace"], namespace);
    assert_eq!(response["output"][0]["name"], function);
}

#[test]
fn streams_tool_search_calls_with_structured_arguments() {
    let request = json!({
        "model":"chat-model",
        "tools":[{"type":"tool_search"}],
        "stream":true
    });
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let mut converter = ChatSseConverter::new(converted.response_context);
    let mut events = String::new();
    for block in [
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_search","function":{"name":"tool_search","arguments":"{\"query\":"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"mail\",\"limit\":2}"}}]},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ] {
        events.push_str(&String::from_utf8(converter.convert_block(block)).unwrap());
    }

    assert!(events.contains("\"type\":\"tool_search_call\""));
    assert!(events.contains("\"query\":\"mail\""));
    assert!(events.contains("\"limit\":2"));
    assert!(events.contains("response.function_call_arguments.done"));
}

#[test]
fn keeps_first_tool_when_flattened_names_collide() {
    let request = json!({
        "model":"chat-model",
        "tools":[{
            "type":"function",
            "name":"collaboration__spawn_agent",
            "parameters":{"type":"object"}
        }],
        "input":[{
            "type":"additional_tools",
            "role":"developer",
            "tools":[{
                "type":"namespace",
                "name":"collaboration",
                "tools":[{
                    "type":"function",
                    "name":"spawn_agent",
                    "parameters":{"type":"object"}
                }]
            }]
        }],
        "tool_choice":{
            "type":"function",
            "namespace":"collaboration",
            "name":"spawn_agent"
        }
    });
    let converted = responses_to_chat_json(&serde_json::to_vec(&request).unwrap()).unwrap();
    let body: Value = serde_json::from_slice(&converted.body).unwrap();

    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["tool_choice"]["function"]["name"],
        "collaboration__spawn_agent"
    );
    let response = chat_to_responses_json(
        &json!({
            "choices":[{
                "message":{
                    "tool_calls":[{
                        "id":"call_collision",
                        "function":{
                            "name":"collaboration__spawn_agent",
                            "arguments":"{}"
                        }
                    }]
                },
                "finish_reason":"tool_calls"
            }]
        }),
        &converted.response_context,
    );
    assert_eq!(response["output"][0]["name"], "collaboration__spawn_agent");
    assert!(response["output"][0].get("namespace").is_none());
}

#[test]
fn keeps_textual_tool_tags_as_plain_output() {
    let context = ChatResponseContext {
        tool_context: ToolContext::from_request(&json!({
            "tools":[{"type":"function","name":"search","parameters":{"type":"object"}}]
        })),
        model: Some("chat-model".to_string()),
    };
    let mut converter = ChatSseConverter::new(context);
    let mut events = String::new();
    events.push_str(
        &String::from_utf8(converter.convert_block(
            r#"data: {"choices":[{"delta":{"content":"<tool_call name=\"search\">{}</tool_call>"},"finish_reason":"stop"}]}"#,
        ))
        .unwrap(),
    );
    events.push_str(&String::from_utf8(converter.convert_block("data: [DONE]")).unwrap());

    assert!(events.contains("response.output_text.delta"));
    assert!(!events.contains("&lt;tool_call"));
    assert!(events.contains("<tool_call name=\\\"search\\\">"));
    assert!(!events.contains("response.function_call_arguments.done"));
    assert!(!events.contains("\"type\":\"function_call\""));
}

fn additional_tools_request() -> Value {
    let namespace_children = [
        "followup_task",
        "interrupt_agent",
        "list_agents",
        "send_message",
        "spawn_agent",
        "wait_agent",
    ]
    .into_iter()
    .map(|name| {
        json!({
            "type":"function",
            "name":name,
            "description":format!("Call {name}"),
            "parameters":{"type":"object","properties":{}},
            "strict":false
        })
    })
    .collect::<Vec<_>>();
    json!({
        "model":"domestic-coder",
        "input":[
            {
                "type":"additional_tools",
                "role":"developer",
                "tools":[
                    {
                        "type":"custom",
                        "name":"exec",
                        "description":"Run JavaScript tools",
                        "format":{
                            "type":"grammar",
                            "syntax":"lark",
                            "definition":"start: source"
                        }
                    },
                    {
                        "type":"function",
                        "name":"wait",
                        "description":"Wait for a task",
                        "parameters":{"type":"object","properties":{}},
                        "strict":true
                    },
                    {
                        "type":"function",
                        "name":"request_user_input",
                        "description":"Ask the user",
                        "parameters":{"type":"object","properties":{}},
                        "strict":false
                    },
                    {
                        "type":"namespace",
                        "name":"collaboration",
                        "description":"Agent collaboration tools",
                        "tools":namespace_children
                    }
                ]
            },
            {
                "type":"message",
                "role":"user",
                "content":[{"type":"input_text","text":"run the tool"}]
            }
        ],
        "tool_choice":"auto",
        "parallel_tool_calls":false,
        "stream":true
    })
}
