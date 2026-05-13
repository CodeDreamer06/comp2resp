use crate::{
    error::ProxyError,
    openai::{
        chat::ChatCompletionResponse,
        responses::{
            ResponseOutputContentPart, ResponseOutputItem, ResponseUsage, ResponsesApiResponse,
        },
    },
};

pub fn translate_response(
    response: ChatCompletionResponse,
) -> Result<ResponsesApiResponse, ProxyError> {
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| {
            ProxyError::upstream(
                axum::http::StatusCode::BAD_GATEWAY,
                "upstream_translation_failed",
                "upstream response missing choices",
            )
        })?;

    let created_at = response.created;
    let finish_reason = choice.finish_reason.as_deref();
    let (output, status, incomplete_details) = build_output(&choice.message, finish_reason)?;

    Ok(ResponsesApiResponse {
        id: response.id,
        created_at: Some(created_at),
        model: response.model,
        output,
        usage: response.usage.map(|usage| ResponseUsage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        }),
        status: Some(status),
        incomplete_details,
    })
}

fn build_output(
    message: &crate::openai::chat::ChatAssistantMessage,
    finish_reason: Option<&str>,
) -> Result<
    (
        Vec<ResponseOutputItem>,
        String,
        Option<serde_json::Value>,
    ),
    ProxyError,
> {
    let mut output = Vec::new();

    if let Some(tool_calls) = &message.tool_calls {
        for tool_call in tool_calls {
            output.push(ResponseOutputItem {
                id: Some(tool_call.id.clone()),
                kind: "function_call".to_string(),
                role: None,
                content: vec![],
                name: Some(tool_call.function.name.clone()),
                arguments: Some(tool_call.function.arguments.clone()),
                call_id: Some(tool_call.id.clone()),
            });
        }
    }

    if let Some(content) = &message.content {
        if !content.is_empty() {
            output.push(ResponseOutputItem {
                id: None,
                kind: "message".to_string(),
                role: Some("assistant".to_string()),
                content: vec![ResponseOutputContentPart {
                    kind: "output_text".to_string(),
                    text: Some(content.clone()),
                }],
                name: None,
                arguments: None,
                call_id: None,
            });
        }
    }

    if finish_reason == Some("tool_calls") && message.tool_calls.is_none() {
        return Err(ProxyError::upstream(
            axum::http::StatusCode::BAD_GATEWAY,
            "upstream_translation_failed",
            "upstream finish_reason is 'tool_calls' but message has no tool_calls",
        ));
    }

    let (status, incomplete_details) = map_finish_reason(finish_reason)?;

    Ok((output, status, incomplete_details))
}

pub(crate) fn map_finish_reason(
    finish_reason: Option<&str>,
) -> Result<(String, Option<serde_json::Value>), ProxyError> {
    match finish_reason {
        None | Some("stop") | Some("tool_calls") => Ok(("completed".to_string(), None)),
        Some("length") => Ok((
            "incomplete".to_string(),
            Some(serde_json::json!({ "reason": "max_output_tokens" })),
        )),
        Some("content_filter") => Ok((
            "incomplete".to_string(),
            Some(serde_json::json!({ "reason": "content_filter" })),
        )),
        Some(other) => Err(ProxyError::upstream(
            axum::http::StatusCode::BAD_GATEWAY,
            "upstream_translation_failed",
            format!("unsupported upstream finish_reason: {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::chat::{ChatAssistantMessage, ChatChoice, ChatCompletionResponse, ChatUsage};

    #[test]
    fn translates_text_response() {
        let response = ChatCompletionResponse {
            id: "chatcmpl_1".to_string(),
            object: "chat.completion".to_string(),
            created: 1,
            model: "gpt-4.1".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatAssistantMessage {
                    role: "assistant".to_string(),
                    content: Some("hello".to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(ChatUsage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
            }),
            system_fingerprint: None,
        };

        let translated = translate_response(response).unwrap();
        assert_eq!(translated.status.as_deref(), Some("completed"));
        assert_eq!(translated.output.len(), 1);
        assert_eq!(
            translated.output[0].content[0].text.as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn translates_tool_calls_response() {
        let response = ChatCompletionResponse {
            id: "chatcmpl_1".to_string(),
            object: "chat.completion".to_string(),
            created: 1,
            model: "gpt-4.1".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatAssistantMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: Some(vec![crate::openai::chat::AssistantToolCall {
                        id: "call_123".to_string(),
                        kind: "function".to_string(),
                        function: crate::openai::chat::ToolFunctionCall {
                            name: "get_weather".to_string(),
                            arguments: "{\"city\":\"NYC\"}".to_string(),
                        },
                    }]),
                },
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: None,
            system_fingerprint: None,
        };

        let translated = translate_response(response).unwrap();
        assert_eq!(translated.status.as_deref(), Some("completed"));
        assert_eq!(translated.output.len(), 1);
        assert_eq!(translated.output[0].kind, "function_call");
        assert_eq!(translated.output[0].name.as_deref(), Some("get_weather"));
        assert_eq!(
            translated.output[0].arguments.as_deref(),
            Some("{\"city\":\"NYC\"}")
        );
    }

    #[test]
    fn rejects_tool_calls_finish_without_tool_calls() {
        let response = ChatCompletionResponse {
            id: "chatcmpl_1".to_string(),
            object: "chat.completion".to_string(),
            created: 1,
            model: "gpt-4.1".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatAssistantMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: None,
                },
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: None,
            system_fingerprint: None,
        };

        let error = translate_response(response).unwrap_err();
        assert_eq!(error.code, "upstream_translation_failed");
    }

    #[test]
    fn maps_length_finish_reason() {
        let response = ChatCompletionResponse {
            id: "chatcmpl_1".to_string(),
            object: "chat.completion".to_string(),
            created: 1,
            model: "gpt-4.1".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatAssistantMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: None,
                },
                finish_reason: Some("length".to_string()),
            }],
            usage: None,
            system_fingerprint: None,
        };

        let translated = translate_response(response).unwrap();
        assert_eq!(translated.status.as_deref(), Some("incomplete"));
        assert_eq!(
            translated.incomplete_details,
            Some(serde_json::json!({ "reason": "max_output_tokens" }))
        );
    }
}
