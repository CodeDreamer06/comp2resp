use std::collections::BTreeMap;

use crate::{
    config::Config,
    error::ProxyError,
    openai::{
        chat::{ChatCompletionRequest, ChatMessage, ChatMessageContent, ChatTool, ToolChoice},
        responses::{
            ResponseContentPart, ResponseInputItem, ResponseTool, ResponseToolChoice,
            ResponseToolChoiceNamed, ResponsesRequest,
        },
    },
};

pub fn translate_chat_request(
    request: ChatCompletionRequest,
    config: &Config,
) -> Result<ResponsesRequest, ProxyError> {
    validate_request(&request, config)?;

    let input = request
        .messages
        .iter()
        .map(translate_message)
        .collect::<Result<Vec<_>, _>>()?;

    let max_output_tokens = match (request.max_completion_tokens, request.max_tokens) {
        (Some(left), Some(right)) if left != right => {
            return Err(ProxyError::invalid_param(
                "conflicting_parameters",
                "max_tokens",
                "max_tokens and max_completion_tokens must match when both are provided",
            ));
        }
        (Some(value), _) => Some(value),
        (None, value) => value,
    };

    let tools = request
        .tools
        .map(|tools| tools.into_iter().map(translate_tool).collect())
        .transpose()?;

    let tool_choice = request.tool_choice.map(translate_tool_choice).transpose()?;

    let metadata = merge_metadata(request.metadata, request.user, config)?;

    Ok(ResponsesRequest {
        model: request.model,
        input,
        stream: Some(request.stream),
        temperature: request.temperature,
        top_p: request.top_p,
        max_output_tokens,
        tools,
        tool_choice,
        metadata,
    })
}

fn validate_request(request: &ChatCompletionRequest, config: &Config) -> Result<(), ProxyError> {
    if request.model.trim().is_empty() {
        return Err(ProxyError::invalid_param(
            "missing_required_field",
            "model",
            "model must not be empty",
        ));
    }

    if request.messages.is_empty() {
        return Err(ProxyError::invalid_param(
            "missing_required_field",
            "messages",
            "messages must contain at least one message",
        ));
    }

    if let Some(n) = request.n {
        if n != 1 {
            return Err(ProxyError::unsupported_feature(
                "n",
                "only n=1 is supported in v1",
            ));
        }
    }

    reject_if_some(&request.logprobs, "logprobs")?;
    reject_if_some(&request.top_logprobs, "top_logprobs")?;
    reject_if_some(&request.logit_bias, "logit_bias")?;
    reject_if_some(&request.presence_penalty, "presence_penalty")?;
    reject_if_some(&request.frequency_penalty, "frequency_penalty")?;
    reject_if_some(&request.seed, "seed")?;
    reject_if_some(&request.response_format, "response_format")?;
    reject_if_some(&request.parallel_tool_calls, "parallel_tool_calls")?;
    reject_if_some(&request.functions, "functions")?;
    reject_if_some(&request.function_call, "function_call")?;
    reject_if_some(&request.audio, "audio")?;
    reject_if_some(&request.modalities, "modalities")?;
    reject_if_some(&request.prediction, "prediction")?;
    reject_if_some(&request.service_tier, "service_tier")?;
    reject_if_some(&request.store, "store")?;
    reject_if_some(&request.reasoning_effort, "reasoning_effort")?;

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

    if let Some(metadata) = &request.metadata {
        for (key, value) in metadata {
            if key.trim().is_empty() {
                return Err(ProxyError::invalid_param(
                    "invalid_parameter",
                    "metadata",
                    "metadata keys must not be empty",
                ));
            }

            if value.trim().is_empty() {
                return Err(ProxyError::invalid_param(
                    "invalid_parameter",
                    format!("metadata.{key}"),
                    "metadata values must not be empty",
                ));
            }
        }
    }

    if request.user.is_some() && !config.forward_user_field {
        return Err(ProxyError::unsupported_feature(
            "user",
            "user forwarding is disabled by proxy configuration",
        ));
    }

    for (index, message) in request.messages.iter().enumerate() {
        validate_message(index, message)?;
    }

    Ok(())
}

fn reject_if_some<T>(value: &Option<T>, param: &str) -> Result<(), ProxyError> {
    if value.is_some() {
        return Err(ProxyError::unsupported_feature(
            param,
            format!("{param} is not supported in v1"),
        ));
    }
    Ok(())
}

fn validate_message(index: usize, message: &ChatMessage) -> Result<(), ProxyError> {
    let prefix = format!("messages[{index}]");
    match message.role.as_str() {
        "system" => {
            let content = message.content.as_ref().ok_or_else(|| {
                ProxyError::invalid_param(
                    "missing_required_field",
                    format!("{prefix}.content"),
                    "message content is required",
                )
            })?;

            if !matches!(content, ChatMessageContent::Text(_)) {
                return Err(ProxyError::unsupported_feature(
                    format!("{prefix}.content"),
                    "system messages only support string content",
                ));
            }

            validate_text_content(&prefix, content)?;
        }
        "user" => {
            let content = message.content.as_ref().ok_or_else(|| {
                ProxyError::invalid_param(
                    "missing_required_field",
                    format!("{prefix}.content"),
                    "message content is required",
                )
            })?;
            validate_text_content(&prefix, content)?;
        }
        "assistant" => {
            if message.content.is_none() && message.tool_calls.is_none() {
                return Err(ProxyError::invalid_param(
                    "missing_required_field",
                    format!("{prefix}.content"),
                    "assistant message must provide content or tool_calls",
                ));
            }

            if let Some(content) = &message.content {
                if !matches!(content, ChatMessageContent::Text(_)) {
                    return Err(ProxyError::unsupported_feature(
                        format!("{prefix}.content"),
                        "assistant messages only support string content in v1",
                    ));
                }
                validate_text_content(&prefix, content)?;
            }

            if let Some(tool_calls) = &message.tool_calls {
                if tool_calls.is_empty() {
                    return Err(ProxyError::invalid_param(
                        "invalid_parameter",
                        format!("{prefix}.tool_calls"),
                        "tool_calls must not be empty",
                    ));
                }

                for (tool_index, tool_call) in tool_calls.iter().enumerate() {
                    if tool_call.kind != "function" {
                        return Err(ProxyError::unsupported_feature(
                            format!("{prefix}.tool_calls[{tool_index}].type"),
                            "only function tool calls are supported",
                        ));
                    }

                    if tool_call.id.trim().is_empty() {
                        return Err(ProxyError::invalid_param(
                            "invalid_parameter",
                            format!("{prefix}.tool_calls[{tool_index}].id"),
                            "tool call id must not be empty",
                        ));
                    }

                    if tool_call.function.name.trim().is_empty() {
                        return Err(ProxyError::invalid_param(
                            "invalid_parameter",
                            format!("{prefix}.tool_calls[{tool_index}].function.name"),
                            "tool call function name must not be empty",
                        ));
                    }
                }
            }
        }
        "tool" => {
            let content = message.content.as_ref().ok_or_else(|| {
                ProxyError::invalid_param(
                    "missing_required_field",
                    format!("{prefix}.content"),
                    "tool message content is required",
                )
            })?;
            validate_text_content(&prefix, content)?;

            if message
                .tool_call_id
                .as_deref()
                .unwrap_or_default()
                .is_empty()
            {
                return Err(ProxyError::invalid_param(
                    "missing_required_field",
                    format!("{prefix}.tool_call_id"),
                    "tool_call_id is required for tool messages",
                ));
            }
        }
        _ => {
            return Err(ProxyError::invalid_param(
                "invalid_parameter",
                format!("{prefix}.role"),
                "unsupported message role",
            ));
        }
    }

    Ok(())
}

fn validate_text_content(prefix: &str, content: &ChatMessageContent) -> Result<(), ProxyError> {
    match content {
        ChatMessageContent::Text(text) => {
            if text.is_empty() {
                return Err(ProxyError::invalid_param(
                    "invalid_parameter",
                    format!("{prefix}.content"),
                    "content must not be empty",
                ));
            }
        }
        ChatMessageContent::Parts(parts) => {
            if parts.is_empty() {
                return Err(ProxyError::invalid_param(
                    "invalid_parameter",
                    format!("{prefix}.content"),
                    "content parts must not be empty",
                ));
            }

            for (part_index, part) in parts.iter().enumerate() {
                if part.kind != "text" || part.text.is_none() {
                    return Err(ProxyError::unsupported_feature(
                        format!("{prefix}.content[{part_index}]"),
                        "only text content parts are supported",
                    ));
                }
            }
        }
    }

    Ok(())
}

fn translate_message(message: &ChatMessage) -> Result<ResponseInputItem, ProxyError> {
    match message.role.as_str() {
        "assistant" => {
            if let Some(tool_calls) = &message.tool_calls {
                if !tool_calls.is_empty() {
                    if message.content.is_some() {
                        return Err(ProxyError::unsupported_feature(
                            "messages.content",
                            "assistant messages with tool_calls must not also include content in v1",
                        ));
                    }

                    if tool_calls.len() != 1 {
                        return Err(ProxyError::unsupported_feature(
                            "messages.tool_calls",
                            "only a single assistant tool call per message is supported in v1",
                        ));
                    }

                    let tool_call = &tool_calls[0];
                    return Ok(ResponseInputItem::FunctionCall {
                        kind: "function_call",
                        call_id: tool_call.id.clone(),
                        name: tool_call.function.name.clone(),
                        arguments: tool_call.function.arguments.clone(),
                    });
                }
            }
        }
        "tool" => {
            let output = flatten_content(message.content.as_ref().ok_or_else(|| {
                ProxyError::invalid_param(
                    "missing_required_field",
                    "messages.content",
                    "tool message content is required",
                )
            })?)?;

            return Ok(ResponseInputItem::FunctionCallOutput {
                kind: "function_call_output",
                call_id: message.tool_call_id.clone().ok_or_else(|| {
                    ProxyError::invalid_param(
                        "missing_required_field",
                        "messages.tool_call_id",
                        "tool_call_id is required for tool messages",
                    )
                })?,
                output,
            });
        }
        _ => {}
    }

    let text = message
        .content
        .as_ref()
        .map(flatten_content)
        .transpose()?
        .unwrap_or_default();

    Ok(ResponseInputItem::Message {
        role: message.role.clone(),
        content: vec![ResponseContentPart::InputText { text }],
    })
}

fn flatten_content(content: &ChatMessageContent) -> Result<String, ProxyError> {
    match content {
        ChatMessageContent::Text(text) => Ok(text.clone()),
        ChatMessageContent::Parts(parts) => {
            let mut merged = String::new();
            for part in parts {
                if part.kind != "text" {
                    return Err(ProxyError::unsupported_feature(
                        "messages.content",
                        "only text content parts are supported",
                    ));
                }

                let text = part.text.as_ref().ok_or_else(|| {
                    ProxyError::invalid_param(
                        "invalid_parameter",
                        "messages.content",
                        "text content parts must contain text",
                    )
                })?;
                merged.push_str(text);
            }
            Ok(merged)
        }
    }
}

fn translate_tool(tool: ChatTool) -> Result<ResponseTool, ProxyError> {
    if tool.kind != "function" {
        return Err(ProxyError::unsupported_feature(
            "tools.type",
            "only function tools are supported",
        ));
    }

    if tool.function.name.trim().is_empty() {
        return Err(ProxyError::invalid_param(
            "invalid_parameter",
            "tools.function.name",
            "tool function name must not be empty",
        ));
    }

    Ok(ResponseTool {
        kind: "function",
        name: tool.function.name,
        description: tool.function.description,
        parameters: tool.function.parameters,
    })
}

fn translate_tool_choice(tool_choice: ToolChoice) -> Result<ResponseToolChoice, ProxyError> {
    match tool_choice {
        ToolChoice::Mode(mode) => match mode.as_str() {
            "auto" | "none" => Ok(ResponseToolChoice::Mode(mode)),
            _ => Err(ProxyError::unsupported_feature(
                "tool_choice",
                "unsupported tool_choice mode",
            )),
        },
        ToolChoice::Named(named) => {
            if named.kind != "function" {
                return Err(ProxyError::unsupported_feature(
                    "tool_choice.type",
                    "only function tool choice is supported",
                ));
            }

            if named.function.name.trim().is_empty() {
                return Err(ProxyError::invalid_param(
                    "invalid_parameter",
                    "tool_choice.function.name",
                    "tool choice function name must not be empty",
                ));
            }

            Ok(ResponseToolChoice::Named(ResponseToolChoiceNamed {
                kind: "function",
                name: named.function.name,
            }))
        }
    }
}

fn merge_metadata(
    metadata: Option<BTreeMap<String, String>>,
    user: Option<String>,
    config: &Config,
) -> Result<Option<BTreeMap<String, String>>, ProxyError> {
    let mut merged = metadata.unwrap_or_default();

    if let Some(user) = user {
        if !config.forward_user_field {
            return Err(ProxyError::unsupported_feature(
                "user",
                "user forwarding is disabled by proxy configuration",
            ));
        }
        merged.insert("chat_user".to_string(), user);
    }

    Ok((!merged.is_empty()).then_some(merged))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, InboundAuthMode};
    use std::{collections::BTreeMap, net::SocketAddr, time::Duration};

    fn config() -> Config {
        Config {
            listen_addr: "127.0.0.1:3000".parse::<SocketAddr>().unwrap(),
            openai_base_url: "https://api.openai.com".to_string(),
            openai_api_key: Some("test".to_string()),
            request_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(5),
            max_request_body_bytes: 1024,
            inbound_auth_mode: InboundAuthMode::None,
            inbound_bearer_token: None,
            forward_user_field: true,
            trust_inbound_x_request_id: false,
            log_json: false,
        }
    }

    #[test]
    fn translates_simple_request() {
        let request = ChatCompletionRequest {
            model: "gpt-4.1".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(ChatMessageContent::Text("hello".to_string())),
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: false,
            temperature: Some(0.2),
            top_p: Some(0.9),
            max_tokens: Some(42),
            max_completion_tokens: None,
            tools: None,
            tool_choice: None,
            user: Some("abc".to_string()),
            metadata: Some(BTreeMap::from([(String::from("k"), String::from("v"))])),
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
        };

        let translated = translate_chat_request(request, &config()).unwrap();
        assert_eq!(translated.max_output_tokens, Some(42));
        assert_eq!(translated.input.len(), 1);
        assert_eq!(
            translated.metadata.unwrap().get("chat_user").unwrap(),
            "abc"
        );
    }

    #[test]
    fn rejects_system_array_content() {
        let request = ChatCompletionRequest {
            model: "gpt-4.1".to_string(),
            messages: vec![ChatMessage {
                role: "system".to_string(),
                content: Some(ChatMessageContent::Parts(vec![
                    crate::openai::chat::ContentPart {
                        kind: "text".to_string(),
                        text: Some("hello".to_string()),
                    },
                ])),
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: false,
            temperature: None,
            top_p: None,
            max_tokens: None,
            max_completion_tokens: None,
            tools: None,
            tool_choice: None,
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
        };

        let error = translate_chat_request(request, &config()).unwrap_err();
        assert_eq!(error.code, "unsupported_feature");
    }

    #[test]
    fn translates_tool_message_to_function_output() {
        let request = ChatCompletionRequest {
            model: "gpt-4.1".to_string(),
            messages: vec![ChatMessage {
                role: "tool".to_string(),
                content: Some(ChatMessageContent::Text("result".to_string())),
                tool_calls: None,
                tool_call_id: Some("call_123".to_string()),
            }],
            stream: false,
            temperature: None,
            top_p: None,
            max_tokens: None,
            max_completion_tokens: None,
            tools: None,
            tool_choice: None,
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
        };

        let translated = translate_chat_request(request, &config()).unwrap();
        match &translated.input[0] {
            crate::openai::responses::ResponseInputItem::FunctionCallOutput {
                call_id,
                output,
                ..
            } => {
                assert_eq!(call_id, "call_123");
                assert_eq!(output, "result");
            }
            _ => panic!("expected function call output item"),
        }
    }
}
