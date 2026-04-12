use crate::{
    error::ProxyError,
    openai::{
        chat::{
            AssistantToolCall, ChatAssistantMessage, ChatChoice, ChatCompletionResponse, ChatUsage,
            ToolFunctionCall,
        },
        responses::{ResponseOutputItem, ResponsesApiResponse},
    },
};

pub fn translate_response(
    response: ResponsesApiResponse,
) -> Result<ChatCompletionResponse, ProxyError> {
    let created = response.created_at.unwrap_or(0);
    let status = response.status.as_deref();
    let (content, tool_calls, finish_reason) = collect_output(
        &response.output,
        status,
        response.incomplete_details.as_ref(),
    )?;

    Ok(ChatCompletionResponse {
        id: response.id,
        object: "chat.completion",
        created,
        model: response.model,
        choices: vec![ChatChoice {
            index: 0,
            message: ChatAssistantMessage {
                role: "assistant",
                content,
                tool_calls,
            },
            finish_reason,
        }],
        usage: response.usage.map(|usage| ChatUsage {
            prompt_tokens: usage.input_tokens,
            completion_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
        }),
        system_fingerprint: None,
    })
}

fn collect_output(
    output: &[ResponseOutputItem],
    status: Option<&str>,
    incomplete_details: Option<&serde_json::Value>,
) -> Result<
    (
        Option<String>,
        Option<Vec<AssistantToolCall>>,
        Option<String>,
    ),
    ProxyError,
> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();

    for item in output {
        match item.kind.as_str() {
            "message" => {
                for part in &item.content {
                    match part.kind.as_str() {
                        "output_text" | "text" => {
                            if let Some(part_text) = &part.text {
                                text.push_str(part_text);
                            }
                        }
                        other => {
                            return Err(ProxyError::upstream(
                                axum::http::StatusCode::BAD_GATEWAY,
                                "upstream_translation_failed",
                                format!("unsupported upstream message content part: {other}"),
                            ));
                        }
                    }
                }
            }
            "function_call" => {
                let id = item
                    .call_id
                    .clone()
                    .or_else(|| item.id.clone())
                    .ok_or_else(|| {
                        ProxyError::upstream(
                            axum::http::StatusCode::BAD_GATEWAY,
                            "upstream_invalid_response",
                            "upstream function_call item missing call id",
                        )
                    })?;
                let name = item.name.clone().ok_or_else(|| {
                    ProxyError::upstream(
                        axum::http::StatusCode::BAD_GATEWAY,
                        "upstream_invalid_response",
                        "upstream function_call item missing name",
                    )
                })?;

                tool_calls.push(AssistantToolCall {
                    id,
                    kind: "function".to_string(),
                    function: ToolFunctionCall {
                        name,
                        arguments: item.arguments.clone().unwrap_or_default(),
                    },
                });
            }
            "reasoning" => {}
            other => {
                return Err(ProxyError::upstream(
                    axum::http::StatusCode::BAD_GATEWAY,
                    "upstream_translation_failed",
                    format!("unsupported upstream output item type: {other}"),
                ));
            }
        }
    }

    let content = (!text.is_empty()).then_some(text);
    let tool_calls = (!tool_calls.is_empty()).then_some(tool_calls);
    let finish_reason = if tool_calls.is_some() {
        Some("tool_calls".to_string())
    } else {
        map_finish_reason(status, incomplete_details)?
    };

    Ok((content, tool_calls, finish_reason))
}

fn map_finish_reason(
    status: Option<&str>,
    incomplete_details: Option<&serde_json::Value>,
) -> Result<Option<String>, ProxyError> {
    match status {
        None | Some("completed") => Ok(Some("stop".to_string())),
        Some("incomplete") => {
            let Some(details) = incomplete_details else {
                return Err(ProxyError::upstream(
                    axum::http::StatusCode::BAD_GATEWAY,
                    "upstream_translation_failed",
                    "upstream incomplete response missing incomplete_details",
                ));
            };

            match details.get("reason").and_then(|value| value.as_str()) {
                Some("max_output_tokens") | Some("max_completion_tokens") => {
                    Ok(Some("length".to_string()))
                }
                Some("content_filter") => Ok(Some("content_filter".to_string())),
                Some(other) => Err(ProxyError::upstream(
                    axum::http::StatusCode::BAD_GATEWAY,
                    "upstream_translation_failed",
                    format!("unsupported upstream incomplete reason: {other}"),
                )),
                None => Err(ProxyError::upstream(
                    axum::http::StatusCode::BAD_GATEWAY,
                    "upstream_translation_failed",
                    "upstream incomplete response missing incomplete_details.reason",
                )),
            }
        }
        Some(other) => Err(ProxyError::upstream(
            axum::http::StatusCode::BAD_GATEWAY,
            "upstream_translation_failed",
            format!("unsupported upstream response status: {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::responses::{ResponseOutputContentPart, ResponseUsage};

    #[test]
    fn translates_text_response() {
        let response = ResponsesApiResponse {
            id: "resp_1".to_string(),
            created_at: Some(1),
            model: "gpt-4.1".to_string(),
            output: vec![ResponseOutputItem {
                id: None,
                kind: "message".to_string(),
                role: Some("assistant".to_string()),
                content: vec![ResponseOutputContentPart {
                    kind: "output_text".to_string(),
                    text: Some("hello".to_string()),
                }],
                name: None,
                arguments: None,
                call_id: None,
            }],
            usage: Some(ResponseUsage {
                input_tokens: 1,
                output_tokens: 2,
                total_tokens: 3,
            }),
            status: Some("completed".to_string()),
            incomplete_details: None,
        };

        let translated = translate_response(response).unwrap();
        assert_eq!(
            translated.choices[0].message.content.as_deref(),
            Some("hello")
        );
        assert_eq!(translated.choices[0].finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn maps_incomplete_length_finish_reason() {
        let response = ResponsesApiResponse {
            id: "resp_1".to_string(),
            created_at: Some(1),
            model: "gpt-4.1".to_string(),
            output: vec![],
            usage: None,
            status: Some("incomplete".to_string()),
            incomplete_details: Some(serde_json::json!({ "reason": "max_output_tokens" })),
        };

        let translated = translate_response(response).unwrap();
        assert_eq!(
            translated.choices[0].finish_reason.as_deref(),
            Some("length")
        );
    }
}
