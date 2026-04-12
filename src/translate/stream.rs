use std::collections::BTreeMap;

use crate::{
    error::ProxyError,
    openai::{
        chat::{
            ChatChunkChoice, ChatChunkDelta, ChatCompletionChunk, StreamingToolCallDelta,
            StreamingToolFunctionDelta,
        },
        responses::ResponsesStreamEvent,
    },
};

#[derive(Debug, Clone)]
pub struct StreamContext {
    pub response_id: String,
    pub model: String,
    pub created: u64,
    tool_indexes: BTreeMap<String, u32>,
    role_emitted: bool,
}

impl StreamContext {
    pub fn new(response_id: String, model: String, created: u64) -> Self {
        Self {
            response_id,
            model,
            created,
            tool_indexes: BTreeMap::new(),
            role_emitted: false,
        }
    }

    pub fn initial_chunk(&mut self) -> ChatCompletionChunk {
        self.role_emitted = true;
        ChatCompletionChunk {
            id: self.response_id.clone(),
            object: "chat.completion.chunk",
            created: self.created,
            model: self.model.clone(),
            choices: vec![ChatChunkChoice {
                index: 0,
                delta: ChatChunkDelta {
                    role: Some("assistant"),
                    content: None,
                    tool_calls: None,
                },
                finish_reason: None,
            }],
        }
    }
}

pub fn translate_stream_event(
    context: &mut StreamContext,
    event: ResponsesStreamEvent,
) -> Result<Vec<ChatCompletionChunk>, ProxyError> {
    let mut chunks = Vec::new();

    if !context.role_emitted {
        chunks.push(context.initial_chunk());
    }

    match event.event_type.as_str() {
        "response.output_text.delta" => {
            let delta = event.delta.ok_or_else(|| {
                ProxyError::upstream(
                    axum::http::StatusCode::BAD_GATEWAY,
                    "upstream_invalid_response",
                    "response.output_text.delta missing delta",
                )
            })?;

            chunks.push(simple_chunk(
                context,
                ChatChunkDelta {
                    role: None,
                    content: Some(delta),
                    tool_calls: None,
                },
                None,
            ));
        }
        "response.function_call_arguments.delta" => {
            let item_id = event.item_id.ok_or_else(|| {
                ProxyError::upstream(
                    axum::http::StatusCode::BAD_GATEWAY,
                    "upstream_invalid_response",
                    "response.function_call_arguments.delta missing item_id",
                )
            })?;
            let delta = event.delta.ok_or_else(|| {
                ProxyError::upstream(
                    axum::http::StatusCode::BAD_GATEWAY,
                    "upstream_invalid_response",
                    "response.function_call_arguments.delta missing delta",
                )
            })?;

            let index = next_tool_index(&mut context.tool_indexes, &item_id);
            chunks.push(simple_chunk(
                context,
                ChatChunkDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![StreamingToolCallDelta {
                        index,
                        id: Some(item_id),
                        kind: Some("function".to_string()),
                        function: Some(StreamingToolFunctionDelta {
                            name: None,
                            arguments: Some(delta),
                        }),
                    }]),
                },
                None,
            ));
        }
        "response.output_item.added" => {
            if let Some(item) = event.item {
                if item.kind == "function_call" {
                    let item_id = item.call_id.or(item.id).ok_or_else(|| {
                        ProxyError::upstream(
                            axum::http::StatusCode::BAD_GATEWAY,
                            "upstream_invalid_response",
                            "function_call item missing id",
                        )
                    })?;
                    let index = next_tool_index(&mut context.tool_indexes, &item_id);
                    chunks.push(simple_chunk(
                        context,
                        ChatChunkDelta {
                            role: None,
                            content: None,
                            tool_calls: Some(vec![StreamingToolCallDelta {
                                index,
                                id: Some(item_id),
                                kind: Some("function".to_string()),
                                function: Some(StreamingToolFunctionDelta {
                                    name: item.name,
                                    arguments: None,
                                }),
                            }]),
                        },
                        None,
                    ));
                }
            }
        }
        "response.completed" => {
            let finish_reason = match event
                .response
                .as_ref()
                .and_then(|response| response.status.as_deref())
            {
                None | Some("completed") => Some("stop".to_string()),
                Some("incomplete") => {
                    let reason = event
                        .response
                        .as_ref()
                        .and_then(|response| response.incomplete_details.as_ref())
                        .and_then(|details| details.get("reason"))
                        .and_then(|value| value.as_str());

                    match reason {
                        Some("max_output_tokens") | Some("max_completion_tokens") => {
                            Some("length".to_string())
                        }
                        Some("content_filter") => Some("content_filter".to_string()),
                        Some(other) => {
                            return Err(ProxyError::upstream(
                                axum::http::StatusCode::BAD_GATEWAY,
                                "stream_translation_failed",
                                format!("unsupported upstream stream completion reason: {other}"),
                            ));
                        }
                        None => {
                            return Err(ProxyError::upstream(
                                axum::http::StatusCode::BAD_GATEWAY,
                                "stream_translation_failed",
                                "upstream incomplete stream response missing incomplete_details.reason",
                            ));
                        }
                    }
                }
                Some(other) => {
                    return Err(ProxyError::upstream(
                        axum::http::StatusCode::BAD_GATEWAY,
                        "stream_translation_failed",
                        format!("unsupported upstream stream status: {other}"),
                    ));
                }
            };

            chunks.push(simple_chunk(
                context,
                ChatChunkDelta::default(),
                finish_reason,
            ));
        }
        "response.failed" => {
            return Err(ProxyError::upstream(
                axum::http::StatusCode::BAD_GATEWAY,
                "stream_translation_failed",
                "upstream stream reported response.failed",
            ));
        }
        "response.created"
        | "response.in_progress"
        | "response.output_item.done"
        | "response.content_part.added"
        | "response.content_part.done"
        | "response.output_text.done"
        | "response.function_call_arguments.done" => {}
        other => {
            return Err(ProxyError::upstream(
                axum::http::StatusCode::BAD_GATEWAY,
                "stream_translation_failed",
                format!("unsupported upstream stream event type: {other}"),
            ));
        }
    }

    Ok(chunks)
}

fn simple_chunk(
    context: &StreamContext,
    delta: ChatChunkDelta,
    finish_reason: Option<String>,
) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: context.response_id.clone(),
        object: "chat.completion.chunk",
        created: context.created,
        model: context.model.clone(),
        choices: vec![ChatChunkChoice {
            index: 0,
            delta,
            finish_reason,
        }],
    }
}

fn next_tool_index(indexes: &mut BTreeMap<String, u32>, item_id: &str) -> u32 {
    if let Some(index) = indexes.get(item_id) {
        return *index;
    }

    let index = indexes.len() as u32;
    indexes.insert(item_id.to_string(), index);
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_initial_role_and_delta() {
        let mut context = StreamContext::new("resp_1".to_string(), "gpt-4.1".to_string(), 1);
        let chunks = translate_stream_event(
            &mut context,
            ResponsesStreamEvent {
                event_type: "response.output_text.delta".to_string(),
                response: None,
                item_id: None,
                output_index: None,
                content_index: None,
                delta: Some("he".to_string()),
                item: None,
            },
        )
        .unwrap();

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].choices[0].delta.role, Some("assistant"));
        assert_eq!(chunks[1].choices[0].delta.content.as_deref(), Some("he"));
    }

    #[test]
    fn maps_incomplete_stream_finish_reason() {
        let mut context = StreamContext::new("resp_1".to_string(), "gpt-4.1".to_string(), 1);
        let chunks = translate_stream_event(
            &mut context,
            ResponsesStreamEvent {
                event_type: "response.completed".to_string(),
                response: Some(crate::openai::responses::ResponsesApiResponse {
                    id: "resp_1".to_string(),
                    created_at: Some(1),
                    model: "gpt-4.1".to_string(),
                    output: vec![],
                    usage: None,
                    status: Some("incomplete".to_string()),
                    incomplete_details: Some(serde_json::json!({ "reason": "content_filter" })),
                }),
                item_id: None,
                output_index: None,
                content_index: None,
                delta: None,
                item: None,
            },
        )
        .unwrap();

        assert_eq!(
            chunks.last().unwrap().choices[0].finish_reason.as_deref(),
            Some("content_filter")
        );
    }
}
