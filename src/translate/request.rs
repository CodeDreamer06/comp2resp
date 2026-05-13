use crate::{
    config::Config,
    error::ProxyError,
    openai::{
        chat::{
            AssistantToolCall, ChatCompletionRequest, ChatMessage, ChatMessageContent,
            ChatTool, ChatToolFunction, ContentPart, ToolChoice, ToolChoiceFunction,
            ToolChoiceNamed, ToolFunctionCall,
        },
        responses::{
            ResponseInputContent, ResponseInputItem, ResponseTool, ResponseToolChoice,
            ResponsesRequest,
        },
    },
};

pub fn translate_chat_request(
    request: ResponsesRequest,
    _config: &Config,
) -> Result<ChatCompletionRequest, ProxyError> {
    validate_request(&request)?;

    let messages = request
        .input
        .into_iter()
        .map(translate_input_item)
        .collect::<Result<Vec<_>, _>>()?;

    let max_tokens = request.max_output_tokens;

    let tools = request
        .tools
        .map(|tools| tools.into_iter().map(translate_tool).collect())
        .transpose()?;

    let tool_choice = request.tool_choice.map(translate_tool_choice).transpose()?;

    Ok(ChatCompletionRequest {
        model: request.model,
        messages,
        stream: request.stream.unwrap_or(false),
        temperature: request.temperature,
        top_p: request.top_p,
        max_tokens,
        max_completion_tokens: None,
        tools,
        tool_choice,
        user: None,
        metadata: None,
        n: None,
        logprobs: None,
        top_logprobs: None,
        logit_bias: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        response_format: None,
        parallel_tool_calls: None,
        functions: None,
        function_call: None,
        audio: None,
        modalities: None,
        prediction: None,
        service_tier: None,
        store: None,
        reasoning_effort: None,
    })
}

fn validate_request(request: &ResponsesRequest) -> Result<(), ProxyError> {
    if request.model.trim().is_empty() {
        return Err(ProxyError::invalid_param(
            "missing_required_field",
            "model",
            "model must not be empty",
        ));
    }

    if request.input.is_empty() {
        return Err(ProxyError::invalid_param(
            "missing_required_field",
            "input",
            "input must contain at least one item",
        ));
    }

    if request.metadata.is_some() {
        return Err(ProxyError::unsupported_feature(
            "metadata",
            "metadata is not supported in v1",
        ));
    }

    if let Some(temperature) = request.temperature {
        if !temperature.is_finite() {
            return Err(ProxyError::invalid_param(
                "invalid_parameter",
                "temperature",
                "temperature must be a finite number",
            ));
        }
    }

    if let Some(top_p) = request.top_p {
        if !top_p.is_finite() || !(0.0..=1.0).contains(&top_p) {
            return Err(ProxyError::invalid_param(
                "invalid_parameter",
                "top_p",
                "top_p must be a finite number between 0 and 1",
            ));
        }
    }

    for (index, item) in request.input.iter().enumerate() {
        validate_input_item(index, item)?;
    }

    Ok(())
}

fn validate_input_item(index: usize, item: &ResponseInputItem) -> Result<(), ProxyError> {
    let prefix = format!("input[{index}]");
    match item {
        ResponseInputItem::Message { role, content } => {
            if role.trim().is_empty() {
                return Err(ProxyError::invalid_param(
                    "invalid_parameter",
                    format!("{prefix}.role"),
                    "message role must not be empty",
                ));
            }
            match content {
                ResponseInputContent::Text(text) => {
                    if text.is_empty() {
                        return Err(ProxyError::invalid_param(
                            "invalid_parameter",
                            format!("{prefix}.content"),
                            "text content must not be empty",
                        ));
                    }
                }
                ResponseInputContent::Parts(parts) => {
                    if parts.is_empty() {
                        return Err(ProxyError::invalid_param(
                            "invalid_parameter",
                            format!("{prefix}.content"),
                            "content parts must not be empty",
                        ));
                    }
                }
            }
        }
        ResponseInputItem::FunctionCall {
            call_id, name, ..
        } => {
            if call_id.trim().is_empty() {
                return Err(ProxyError::invalid_param(
                    "invalid_parameter",
                    format!("{prefix}.call_id"),
                    "function_call call_id must not be empty",
                ));
            }
            if name.trim().is_empty() {
                return Err(ProxyError::invalid_param(
                    "invalid_parameter",
                    format!("{prefix}.name"),
                    "function_call name must not be empty",
                ));
            }
        }
        ResponseInputItem::FunctionCallOutput {
            call_id, output, ..
        } => {
            if call_id.trim().is_empty() {
                return Err(ProxyError::invalid_param(
                    "invalid_parameter",
                    format!("{prefix}.call_id"),
                    "function_call_output call_id must not be empty",
                ));
            }
            if output.is_empty() {
                return Err(ProxyError::invalid_param(
                    "invalid_parameter",
                    format!("{prefix}.output"),
                    "function_call_output output must not be empty",
                ));
            }
        }
    }

    Ok(())
}

fn translate_input_item(item: ResponseInputItem) -> Result<ChatMessage, ProxyError> {
    match item {
        ResponseInputItem::Message { role, content } => {
            let chat_content = match content {
                ResponseInputContent::Text(text) => Some(ChatMessageContent::Text(text)),
                ResponseInputContent::Parts(parts) => {
                    let chat_parts: Vec<ContentPart> = parts
                        .into_iter()
                        .map(|part| match part {
                            crate::openai::responses::ResponseContentPart::InputText { text } => {
                                ContentPart {
                                    kind: "text".to_string(),
                                    text: Some(text),
                                }
                            }
                        })
                        .collect();
                    Some(ChatMessageContent::Parts(chat_parts))
                }
            };
            Ok(ChatMessage {
                role,
                content: chat_content,
                tool_calls: None,
                tool_call_id: None,
            })
        }
        ResponseInputItem::FunctionCall {
            call_id,
            name,
            arguments,
            ..
        } => Ok(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![AssistantToolCall {
                id: call_id,
                kind: "function".to_string(),
                function: ToolFunctionCall { name, arguments },
            }]),
            tool_call_id: None,
        }),
        ResponseInputItem::FunctionCallOutput {
            call_id, output, ..
        } => Ok(ChatMessage {
            role: "tool".to_string(),
            content: Some(ChatMessageContent::Text(output)),
            tool_calls: None,
            tool_call_id: Some(call_id),
        }),
    }
}

fn translate_tool(tool: ResponseTool) -> Result<ChatTool, ProxyError> {
    if tool.kind != "function" {
        return Err(ProxyError::unsupported_feature(
            "tools.type",
            "only function tools are supported",
        ));
    }

    if tool.name.trim().is_empty() {
        return Err(ProxyError::invalid_param(
            "invalid_parameter",
            "tools.name",
            "tool name must not be empty",
        ));
    }

    Ok(ChatTool {
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: tool.name,
            description: tool.description,
            parameters: tool.parameters,
        },
    })
}

fn translate_tool_choice(
    tool_choice: ResponseToolChoice,
) -> Result<ToolChoice, ProxyError> {
    match tool_choice {
        ResponseToolChoice::Mode(mode) => match mode.as_str() {
            "auto" | "none" => Ok(ToolChoice::Mode(mode)),
            "required" => Err(ProxyError::unsupported_feature(
                "tool_choice",
                "tool_choice 'required' is not supported in v1",
            )),
            _ => Err(ProxyError::unsupported_feature(
                "tool_choice",
                "unsupported tool_choice mode",
            )),
        },
        ResponseToolChoice::Named(named) => {
            if named.kind != "function" {
                return Err(ProxyError::unsupported_feature(
                    "tool_choice.type",
                    "only function tool choice is supported",
                ));
            }

            if named.name.trim().is_empty() {
                return Err(ProxyError::invalid_param(
                    "invalid_parameter",
                    "tool_choice.name",
                    "tool choice function name must not be empty",
                ));
            }

            Ok(ToolChoice::Named(ToolChoiceNamed {
                kind: "function".to_string(),
                function: ToolChoiceFunction { name: named.name },
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, InboundAuthMode};
    use std::{net::SocketAddr, time::Duration};

    fn config() -> Config {
        Config {
            listen_addr: "127.0.0.1:3000".parse::<SocketAddr>().unwrap(),
            openai_base_url: "https://api.openai.com".to_string(),
            openai_api_key: Some("test".to_string()),
            request_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(5),
            max_request_body_bytes: 1024 * 1024,
            inbound_auth_mode: InboundAuthMode::None,
            inbound_bearer_token: None,
            forward_user_field: false,
            trust_inbound_x_request_id: false,
            log_json: false,
        }
    }

    #[test]
    fn translates_simple_request() {
        let request = ResponsesRequest {
            model: "gpt-4.1".to_string(),
            input: vec![ResponseInputItem::Message {
                role: "user".to_string(),
                content: ResponseInputContent::Text("hello".to_string()),
            }],
            stream: Some(false),
            temperature: Some(0.2),
            top_p: Some(0.9),
            max_output_tokens: Some(42),
            tools: None,
            tool_choice: None,
            metadata: None,
        };

        let translated = translate_chat_request(request, &config()).unwrap();
        assert_eq!(translated.model, "gpt-4.1");
        assert_eq!(translated.messages.len(), 1);
        assert_eq!(translated.messages[0].role, "user");
        assert_eq!(
            translated.messages[0].content.as_ref().unwrap(),
            &ChatMessageContent::Text("hello".to_string())
        );
        assert_eq!(translated.max_tokens, Some(42));
    }

    #[test]
    fn translates_function_call_input() {
        let request = ResponsesRequest {
            model: "gpt-4.1".to_string(),
            input: vec![ResponseInputItem::FunctionCall {
                kind: "function_call".to_string(),
                call_id: "call_123".to_string(),
                name: "get_weather".to_string(),
                arguments: "{\"city\":\"NYC\"}".to_string(),
            }],
            stream: Some(false),
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            tools: None,
            tool_choice: None,
            metadata: None,
        };

        let translated = translate_chat_request(request, &config()).unwrap();
        assert_eq!(translated.messages[0].role, "assistant");
        assert!(translated.messages[0].tool_calls.is_some());
    }

    #[test]
    fn rejects_metadata() {
        let request = ResponsesRequest {
            model: "gpt-4.1".to_string(),
            input: vec![ResponseInputItem::Message {
                role: "user".to_string(),
                content: ResponseInputContent::Text("hi".to_string()),
            }],
            stream: Some(false),
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            tools: None,
            tool_choice: None,
            metadata: Some(
                [("key".to_string(), "value".to_string())]
                    .into_iter()
                    .collect(),
            ),
        };

        let error = translate_chat_request(request, &config()).unwrap_err();
        assert_eq!(error.code, "unsupported_feature");
    }
}
