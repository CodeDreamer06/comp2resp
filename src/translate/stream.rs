use std::collections::BTreeMap;

use crate::{
    error::ProxyError,
    openai::{
        chat::ChatCompletionChunk,
        responses::{
            ResponseOutputContentPart, ResponseOutputItem, ResponsesApiResponse, ResponsesStreamEvent,
        },
    },
};

#[derive(Debug, Clone)]
pub struct StreamContext {
    pub response_id: String,
    pub model: String,
    pub created: u64,
    text_accumulated: String,
    text_started: bool,
    role_emitted: bool,
    done_emitted: bool,
    tool_calls: BTreeMap<u32, PartialToolCall>,
}

#[derive(Debug, Clone, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    item_emitted: bool,
}

impl StreamContext {
    pub fn new(response_id: String, model: String, created: u64) -> Self {
        Self {
            response_id,
            model,
            created,
            text_accumulated: String::new(),
            text_started: false,
            role_emitted: false,
            done_emitted: false,
            tool_calls: BTreeMap::new(),
        }
    }
}

pub fn translate_stream_event(
    context: &mut StreamContext,
    chunk: ChatCompletionChunk,
) -> Result<Vec<ResponsesStreamEvent>, ProxyError> {
    let mut events = Vec::new();

    let choice = chunk
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| {
            ProxyError::upstream(
                axum::http::StatusCode::BAD_GATEWAY,
                "upstream_invalid_response",
                "upstream chunk missing choices",
            )
        })?;

    if !context.role_emitted && choice.delta.role.is_some() {
        context.role_emitted = true;
        events.push(ResponsesStreamEvent {
            event_type: "response.created".to_string(),
            response: Some(ResponsesApiResponse {
                id: chunk.id.clone(),
                created_at: Some(chunk.created),
                model: chunk.model.clone(),
                output: vec![],
                usage: None,
                status: Some("in_progress".to_string()),
                incomplete_details: None,
            }),
            item_id: None,
            output_index: None,
            content_index: None,
            delta: None,
            arguments: None,
            item: None,
        });
    }

    if let Some(content) = choice.delta.content {
        if !content.is_empty() {
            if !context.text_started {
                context.text_started = true;
                events.push(ResponsesStreamEvent {
                    event_type: "response.output_item.added".to_string(),
                    response: None,
                    item_id: Some(format!("{}_msg", chunk.id)),
                    output_index: Some(0),
                    content_index: None,
                    delta: None,
                    arguments: None,
                    item: Some(ResponseOutputItem {
                        id: Some(format!("{}_msg", chunk.id)),
                        kind: "message".to_string(),
                        role: Some("assistant".to_string()),
                        content: vec![],
                        name: None,
                        arguments: None,
                        call_id: None,
                    }),
                });
                events.push(ResponsesStreamEvent {
                    event_type: "response.content_part.added".to_string(),
                    response: None,
                    item_id: Some(format!("{}_msg", chunk.id)),
                    output_index: Some(0),
                    content_index: Some(0),
                    delta: None,
                    arguments: None,
                    item: Some(ResponseOutputItem {
                        id: Some(format!("{}_part", chunk.id)),
                        kind: "output_text".to_string(),
                        role: None,
                        content: vec![ResponseOutputContentPart {
                            kind: "output_text".to_string(),
                            text: Some(String::new()),
                        }],
                        name: None,
                        arguments: None,
                        call_id: None,
                    }),
                });
            }
            context.text_accumulated.push_str(&content);
            events.push(ResponsesStreamEvent {
                event_type: "response.output_text.delta".to_string(),
                response: None,
                item_id: Some(format!("{}_msg", chunk.id)),
                output_index: Some(0),
                content_index: Some(0),
                delta: Some(content),
                arguments: None,
                item: None,
            });
        }
    }

    if let Some(tool_call_deltas) = choice.delta.tool_calls {
        for delta in tool_call_deltas {
            let index = delta.index;
            let tc = context.tool_calls.entry(index).or_default();

            if let Some(id) = delta.id {
                tc.id = Some(id);
            }
            if let Some(kind) = delta.kind {
                if kind != "function" {
                    return Err(ProxyError::upstream(
                        axum::http::StatusCode::BAD_GATEWAY,
                        "stream_translation_failed",
                        format!("unsupported tool call type in stream: {kind}"),
                    ));
                }
            }
            if let Some(ref function) = delta.function {
                if let Some(name) = function.name.clone() {
                    tc.name = Some(name);
                }
                if let Some(arguments) = function.arguments.clone() {
                    tc.arguments.push_str(&arguments);
                }
            }

            if !tc.item_emitted && tc.id.is_some() && tc.name.is_some() {
                tc.item_emitted = true;
                events.push(ResponsesStreamEvent {
                    event_type: "response.output_item.added".to_string(),
                    response: None,
                    item_id: tc.id.clone(),
                    output_index: Some(index),
                    content_index: None,
                    delta: None,
                    arguments: None,
                    item: Some(ResponseOutputItem {
                        id: tc.id.clone(),
                        kind: "function_call".to_string(),
                        role: None,
                        content: vec![],
                        name: tc.name.clone(),
                        arguments: Some(String::new()),
                        call_id: tc.id.clone(),
                    }),
                });
            }

            if tc.item_emitted {
                if let Some(ref function) = delta.function {
                    if let Some(ref arguments) = function.arguments {
                        if !arguments.is_empty() {
                            events.push(ResponsesStreamEvent {
                                event_type: "response.function_call_arguments.delta".to_string(),
                                response: None,
                                item_id: tc.id.clone(),
                                output_index: Some(index),
                                content_index: None,
                                delta: Some(arguments.clone()),
                                arguments: None,
                                item: None,
                            });
                        }
                    }
                }
            }
        }
    }

    if let Some(finish_reason) = choice.finish_reason {
        if !context.done_emitted {
            context.done_emitted = true;

            let (status, incomplete_details) =
                super::response::map_finish_reason(Some(&finish_reason))?;

            if context.text_started {
                events.push(ResponsesStreamEvent {
                    event_type: "response.output_text.done".to_string(),
                    response: None,
                    item_id: Some(format!("{}_msg", chunk.id)),
                    output_index: Some(0),
                    content_index: Some(0),
                    delta: None,
                    arguments: None,
                    item: None,
                });
                events.push(ResponsesStreamEvent {
                    event_type: "response.content_part.done".to_string(),
                    response: None,
                    item_id: Some(format!("{}_msg", chunk.id)),
                    output_index: Some(0),
                    content_index: Some(0),
                    delta: None,
                    arguments: None,
                    item: Some(ResponseOutputItem {
                        id: Some(format!("{}_part", chunk.id)),
                        kind: "output_text".to_string(),
                        role: None,
                        content: vec![ResponseOutputContentPart {
                            kind: "output_text".to_string(),
                            text: Some(context.text_accumulated.clone()),
                        }],
                        name: None,
                        arguments: None,
                        call_id: None,
                    }),
                });
                events.push(ResponsesStreamEvent {
                    event_type: "response.output_item.done".to_string(),
                    response: None,
                    item_id: Some(format!("{}_msg", chunk.id)),
                    output_index: Some(0),
                    content_index: None,
                    delta: None,
                    arguments: None,
                    item: Some(ResponseOutputItem {
                        id: Some(format!("{}_msg", chunk.id)),
                        kind: "message".to_string(),
                        role: Some("assistant".to_string()),
                        content: vec![ResponseOutputContentPart {
                            kind: "output_text".to_string(),
                            text: Some(context.text_accumulated.clone()),
                        }],
                        name: None,
                        arguments: None,
                        call_id: None,
                    }),
                });
            }

            for (index, tc) in &context.tool_calls {
                events.push(ResponsesStreamEvent {
                    event_type: "response.function_call_arguments.done".to_string(),
                    response: None,
                    item_id: tc.id.clone(),
                    output_index: Some(*index),
                    content_index: None,
                    delta: None,
                    arguments: Some(tc.arguments.clone()),
                    item: None,
                });
                events.push(ResponsesStreamEvent {
                    event_type: "response.output_item.done".to_string(),
                    response: None,
                    item_id: tc.id.clone(),
                    output_index: Some(*index),
                    content_index: None,
                    delta: None,
                    arguments: None,
                    item: Some(ResponseOutputItem {
                        id: tc.id.clone(),
                        kind: "function_call".to_string(),
                        role: None,
                        content: vec![],
                        name: tc.name.clone(),
                        arguments: Some(tc.arguments.clone()),
                        call_id: tc.id.clone(),
                    }),
                });
            }

            let mut output = Vec::new();
            if context.text_started {
                output.push(ResponseOutputItem {
                    id: Some(format!("{}_msg", context.response_id)),
                    kind: "message".to_string(),
                    role: Some("assistant".to_string()),
                    content: vec![ResponseOutputContentPart {
                        kind: "output_text".to_string(),
                        text: Some(context.text_accumulated.clone()),
                    }],
                    name: None,
                    arguments: None,
                    call_id: None,
                });
            }
            for (_index, tc) in &context.tool_calls {
                output.push(ResponseOutputItem {
                    id: tc.id.clone(),
                    kind: "function_call".to_string(),
                    role: None,
                    content: vec![],
                    name: tc.name.clone(),
                    arguments: Some(tc.arguments.clone()),
                    call_id: tc.id.clone(),
                });
            }

            events.push(ResponsesStreamEvent {
                event_type: "response.completed".to_string(),
                response: Some(ResponsesApiResponse {
                    id: chunk.id,
                    created_at: Some(context.created),
                    model: context.model.clone(),
                    output,
                    usage: None,
                    status: Some(status),
                    incomplete_details,
                }),
                item_id: None,
                output_index: None,
                content_index: None,
                delta: None,
                arguments: None,
                item: None,
            });
        }
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::chat::{
        ChatChunkChoice, ChatChunkDelta, StreamingToolCallDelta, StreamingToolFunctionDelta,
    };

    #[test]
    fn emits_response_created_on_role() {
        let mut context = StreamContext::new("chatcmpl_1".to_string(), "gpt-4.1".to_string(), 1);

        let events = translate_stream_event(
            &mut context,
            ChatCompletionChunk {
                id: "chatcmpl_1".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 1,
                model: "gpt-4.1".to_string(),
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatChunkDelta {
                        role: Some("assistant".to_string()),
                        content: None,
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
            },
        )
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "response.created");
    }

    #[test]
    fn emits_text_events() {
        let mut context = StreamContext::new("chatcmpl_1".to_string(), "gpt-4.1".to_string(), 1);
        context.role_emitted = true;

        let events = translate_stream_event(
            &mut context,
            ChatCompletionChunk {
                id: "chatcmpl_1".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 1,
                model: "gpt-4.1".to_string(),
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatChunkDelta {
                        role: None,
                        content: Some("hello".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
            },
        )
        .unwrap();

        assert_eq!(events[0].event_type, "response.output_item.added");
        assert_eq!(events[1].event_type, "response.content_part.added");
        assert_eq!(events[2].event_type, "response.output_text.delta");
        assert_eq!(events[2].delta.as_deref(), Some("hello"));
    }

    #[test]
    fn emits_tool_call_events() {
        let mut context = StreamContext::new("chatcmpl_1".to_string(), "gpt-4.1".to_string(), 1);
        context.role_emitted = true;

        let events = translate_stream_event(
            &mut context,
            ChatCompletionChunk {
                id: "chatcmpl_1".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 1,
                model: "gpt-4.1".to_string(),
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatChunkDelta {
                        role: None,
                        content: None,
                        tool_calls: Some(vec![StreamingToolCallDelta {
                            index: 0,
                            id: Some("call_123".to_string()),
                            kind: Some("function".to_string()),
                            function: Some(StreamingToolFunctionDelta {
                                name: Some("get_weather".to_string()),
                                arguments: None,
                            }),
                        }]),
                    },
                    finish_reason: None,
                }],
            },
        )
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "response.output_item.added");
        assert_eq!(events[0].item.as_ref().unwrap().kind, "function_call");
        assert_eq!(
            events[0].item.as_ref().unwrap().name.as_deref(),
            Some("get_weather")
        );
    }

    #[test]
    fn emits_tool_call_argument_delta() {
        let mut context = StreamContext::new("chatcmpl_1".to_string(), "gpt-4.1".to_string(), 1);
        context.role_emitted = true;
        context.tool_calls.insert(
            0,
            PartialToolCall {
                id: Some("call_123".to_string()),
                name: Some("get_weather".to_string()),
                arguments: String::new(),
                item_emitted: true,
            },
        );

        let events = translate_stream_event(
            &mut context,
            ChatCompletionChunk {
                id: "chatcmpl_1".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 1,
                model: "gpt-4.1".to_string(),
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatChunkDelta {
                        role: None,
                        content: None,
                        tool_calls: Some(vec![StreamingToolCallDelta {
                            index: 0,
                            id: None,
                            kind: None,
                            function: Some(StreamingToolFunctionDelta {
                                name: None,
                                arguments: Some("{\"city\":\"NYC\"}".to_string()),
                            }),
                        }]),
                    },
                    finish_reason: None,
                }],
            },
        )
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].event_type,
            "response.function_call_arguments.delta"
        );
        assert_eq!(
            events[0].delta.as_deref(),
            Some("{\"city\":\"NYC\"}")
        );
    }

    #[test]
    fn emits_completion_events_on_text_finish() {
        let mut context = StreamContext::new("chatcmpl_1".to_string(), "gpt-4.1".to_string(), 1);
        context.role_emitted = true;
        context.text_started = true;
        context.text_accumulated = "hello".to_string();

        let events = translate_stream_event(
            &mut context,
            ChatCompletionChunk {
                id: "chatcmpl_1".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 1,
                model: "gpt-4.1".to_string(),
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatChunkDelta::default(),
                    finish_reason: Some("stop".to_string()),
                }],
            },
        )
        .unwrap();

        let types: Vec<_> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(types.contains(&"response.output_text.done"));
        assert!(types.contains(&"response.content_part.done"));
        assert!(types.contains(&"response.output_item.done"));
        assert!(types.contains(&"response.completed"));
    }

    #[test]
    fn emits_completion_events_on_tool_calls_finish() {
        let mut context = StreamContext::new("chatcmpl_1".to_string(), "gpt-4.1".to_string(), 1);
        context.role_emitted = true;
        context.tool_calls.insert(
            0,
            PartialToolCall {
                id: Some("call_123".to_string()),
                name: Some("get_weather".to_string()),
                arguments: "{\"city\":\"NYC\"}".to_string(),
                item_emitted: true,
            },
        );

        let events = translate_stream_event(
            &mut context,
            ChatCompletionChunk {
                id: "chatcmpl_1".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 1,
                model: "gpt-4.1".to_string(),
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatChunkDelta::default(),
                    finish_reason: Some("tool_calls".to_string()),
                }],
            },
        )
        .unwrap();

        let types: Vec<_> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(types.contains(&"response.function_call_arguments.done"));
        assert!(types.contains(&"response.output_item.done"));
        assert!(types.contains(&"response.completed"));
        let completed = events
            .iter()
            .find(|e| e.event_type == "response.completed")
            .unwrap();
        assert_eq!(
            completed.response.as_ref().unwrap().status.as_deref(),
            Some("completed")
        );
    }
}
