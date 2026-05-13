use bytes::Bytes;
use futures::{Stream, StreamExt};
use reqwest::{header, StatusCode};

use crate::{
    error::ProxyError,
    openai::chat::{ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse},
    state::AppState,
};

pub async fn create_chat_completion(
    state: &AppState,
    bearer_token: Option<&str>,
    payload: &ChatCompletionRequest,
) -> Result<ChatCompletionResponse, ProxyError> {
    let request = state
        .client
        .post(format!("{}/v1/chat/completions", state.config.openai_base_url))
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", resolve_bearer(state, bearer_token)?),
        )
        .json(payload);

    let response = request.send().await.map_err(map_transport_error)?;
    let status = response.status();

    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<unavailable>".to_string());
        return Err(map_upstream_status(status, body));
    }

    response
        .json::<ChatCompletionResponse>()
        .await
        .map_err(|source| {
            ProxyError::upstream(
                StatusCode::BAD_GATEWAY,
                "upstream_invalid_response",
                format!("failed to decode upstream response JSON: {source}"),
            )
        })
}

pub async fn create_chat_completion_stream(
    state: &AppState,
    bearer_token: Option<&str>,
    payload: &ChatCompletionRequest,
) -> Result<impl Stream<Item = Result<ChatCompletionChunk, ProxyError>>, ProxyError> {
    let response = state
        .client
        .post(format!("{}/v1/chat/completions", state.config.openai_base_url))
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", resolve_bearer(state, bearer_token)?),
        )
        .header(header::ACCEPT, "text/event-stream")
        .json(payload)
        .send()
        .await
        .map_err(map_transport_error)?;

    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<unavailable>".to_string());
        return Err(map_upstream_status(status, body));
    }

    Ok(response.bytes_stream().map(parse_sse_bytes))
}

fn parse_sse_bytes(
    chunk: Result<Bytes, reqwest::Error>,
) -> Result<ChatCompletionChunk, ProxyError> {
    let bytes = chunk.map_err(map_transport_error)?;
    let text = std::str::from_utf8(&bytes).map_err(|source| {
        ProxyError::upstream(
            StatusCode::BAD_GATEWAY,
            "upstream_invalid_response",
            format!("upstream SSE payload was not valid UTF-8: {source}"),
        )
    })?;

    let normalized = text.replace("\r\n", "\n");

    for frame in normalized.split("\n\n") {
        if frame.trim().is_empty() {
            continue;
        }

        let mut payload_lines = Vec::new();

        for line in frame.lines() {
            if let Some(payload) = line.strip_prefix("data:") {
                let payload = payload.trim_start();
                if payload == "[DONE]" {
                    continue;
                }

                if !payload.is_empty() {
                    payload_lines.push(payload);
                }
            }
        }

        if !payload_lines.is_empty() {
            let payload = payload_lines.join("\n");
            return serde_json::from_str(&payload).map_err(|source| {
                ProxyError::upstream(
                    StatusCode::BAD_GATEWAY,
                    "upstream_invalid_response",
                    format!("failed to decode upstream SSE event JSON: {source}"),
                )
            });
        }
    }

    Err(ProxyError::upstream(
        StatusCode::BAD_GATEWAY,
        "upstream_invalid_response",
        "received SSE chunk without data payload",
    ))
}

fn resolve_bearer<'a>(
    state: &'a AppState,
    inbound_bearer: Option<&'a str>,
) -> Result<&'a str, ProxyError> {
    inbound_bearer
        .or(state.config.openai_api_key.as_deref())
        .ok_or_else(|| ProxyError::unauthorized("missing bearer token for upstream request"))
}

fn map_transport_error(source: reqwest::Error) -> ProxyError {
    if source.is_timeout() {
        ProxyError::upstream(
            StatusCode::GATEWAY_TIMEOUT,
            "upstream_timeout",
            format!("upstream request timed out: {source}"),
        )
    } else {
        ProxyError::upstream(
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream_unavailable",
            format!("upstream transport error: {source}"),
        )
    }
}

fn map_upstream_status(status: StatusCode, body: String) -> ProxyError {
    let code = match status {
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::TOO_MANY_REQUESTS => "upstream_error",
        StatusCode::SERVICE_UNAVAILABLE => "upstream_unavailable",
        StatusCode::GATEWAY_TIMEOUT => "upstream_timeout",
        _ => "upstream_error",
    };

    ProxyError::upstream(status, code, format!("upstream returned {status}: {body}"))
}
