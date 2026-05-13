use std::convert::Infallible;

use axum::{
    body::Body,
    extract::{rejection::JsonRejection, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use futures::{stream, StreamExt};
use tracing::error;

use crate::{
    config::InboundAuthMode,
    error::ProxyError,
    openai::{chat::ChatCompletionChunk, responses::ResponsesStreamEvent},
    state::AppState,
    translate::{
        request::translate_chat_request,
        response::translate_response,
        stream::{translate_stream_event, StreamContext},
    },
    upstream,
};

pub async fn create_response(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<crate::openai::responses::ResponsesRequest>, JsonRejection>,
) -> Response {
    let request_id = extract_request_id(&state, &headers);

    let request = match payload {
        Ok(Json(request)) => request,
        Err(rejection) => {
            let error = ProxyError::from(rejection);
            return (error.status, Json(error.into_envelope(request_id))).into_response();
        }
    };

    match handle_request(state, headers, request_id.clone(), request).await {
        Ok(response) => response,
        Err(error) => {
            error!(request_id = %request_id, status = %error.status, code = error.code, message = %error.message, "request failed");
            (error.status, Json(error.into_envelope(request_id))).into_response()
        }
    }
}

fn extract_request_id(state: &AppState, headers: &HeaderMap) -> String {
    if state.config.trust_inbound_x_request_id {
        headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown")
            .to_string()
    } else {
        "unknown".to_string()
    }
}

async fn handle_request(
    state: AppState,
    headers: HeaderMap,
    request_id: String,
    request: crate::openai::responses::ResponsesRequest,
) -> Result<Response, ProxyError> {
    let inbound_bearer = authenticate(&state, &headers)?;
    let upstream_request = translate_chat_request(request, &state.config)?;

    if upstream_request.stream {
        let upstream_stream = upstream::create_chat_completion_stream(
            &state,
            inbound_bearer.as_deref(),
            &upstream_request,
        )
        .await?;
        let stream = stream_response(request_id, upstream_stream);

        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            )
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
            .body(Body::from_stream(stream))
            .map_err(|source| {
                ProxyError::internal_with_source("failed to build stream response", source)
            })?);
    }

    let upstream_response = upstream::create_chat_completion(
        &state,
        inbound_bearer.as_deref(),
        &upstream_request,
    )
    .await?;
    let responses_response = translate_response(upstream_response)?;

    Ok((StatusCode::OK, Json(responses_response)).into_response())
}

fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Option<String>, ProxyError> {
    let bearer = extract_bearer(headers)?;

    match state.config.inbound_auth_mode {
        InboundAuthMode::None => Ok(None),
        InboundAuthMode::StaticBearer => {
            let presented =
                bearer.ok_or_else(|| ProxyError::unauthorized("missing bearer token"))?;
            let expected = state
                .config
                .inbound_bearer_token
                .as_deref()
                .ok_or_else(|| ProxyError::internal("missing configured static bearer token"))?;

            if presented != expected {
                return Err(ProxyError::forbidden("invalid bearer token"));
            }

            Ok(None)
        }
        InboundAuthMode::PassthroughBearer => {
            let presented =
                bearer.ok_or_else(|| ProxyError::unauthorized("missing bearer token"))?;
            Ok(Some(presented))
        }
    }
}

fn extract_bearer(headers: &HeaderMap) -> Result<Option<String>, ProxyError> {
    let Some(header_value) = headers.get(header::AUTHORIZATION) else {
        return Ok(None);
    };

    let raw = header_value
        .to_str()
        .map_err(|_| ProxyError::unauthorized("invalid authorization header encoding"))?;

    let token = raw
        .strip_prefix("Bearer ")
        .ok_or_else(|| ProxyError::unauthorized("authorization header must use Bearer scheme"))?;

    if token.is_empty() {
        return Err(ProxyError::unauthorized("bearer token must not be empty"));
    }

    Ok(Some(token.to_string()))
}

fn stream_response<S>(
    request_id: String,
    upstream_stream: S,
) -> impl futures::Stream<Item = Result<Bytes, Infallible>>
where
    S: futures::Stream<Item = Result<ChatCompletionChunk, ProxyError>> + Send + 'static,
{
    let mut context: Option<StreamContext> = None;

    upstream_stream.flat_map(move |chunk_result| {
        let request_id = request_id.clone();

        let frames = match chunk_result {
            Ok(chunk) => {
                if context.is_none() {
                    context = Some(StreamContext::new(
                        chunk.id.clone(),
                        chunk.model.clone(),
                        chunk.created,
                    ));
                }

                match translate_stream_event(context.as_mut().expect("context initialized"), chunk)
                {
                    Ok(events) => {
                        let is_done = events
                            .iter()
                            .any(|event| event.event_type == "response.completed");

                        let mut frames = events
                            .into_iter()
                            .map(|event| {
                                let payload = serde_json::to_string(&event)
                                    .unwrap_or_else(|_| "{}".to_string());
                                Ok(Bytes::from(format!("data: {payload}\n\n")))
                            })
                            .collect::<Vec<_>>();

                        if is_done {
                            frames.push(Ok(Bytes::from_static(b"data: [DONE]\n\n")));
                        }

                        frames
                    }
                    Err(error) => vec![Ok(Bytes::from(format_sse_error(error, &request_id)))],
                }
            }
            Err(error) => vec![Ok(Bytes::from(format_sse_error(error, &request_id)))],
        };

        stream::iter(frames)
    })
}

fn format_sse_error(error: ProxyError, request_id: &str) -> String {
    let envelope = error.into_envelope(request_id.to_string());
    let _event = ResponsesStreamEvent {
        event_type: "error".to_string(),
        response: None,
        item_id: None,
        output_index: None,
        content_index: None,
        delta: None,
        arguments: None,
        item: None,
    };
    let payload = serde_json::json!({
        "type": "error",
        "error": envelope.error,
    });
    format!("data: {}\n\n", payload)
}
