use std::time::Duration;

use axum::{
    extract::DefaultBodyLimit,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    response::IntoResponse,
    Json, Router,
};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use crate::{error::ProxyError, routes, state::AppState};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(routes::health::healthz))
        .route("/readyz", get(routes::health::readyz))
        .route(
            "/v1/responses",
            post(routes::responses::create_response),
        )
        .fallback(handler_404)
        .layer(DefaultBodyLimit::max(state.config.max_request_body_bytes))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(state.config.request_timeout.as_secs()),
        ))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn handler_404(headers: HeaderMap) -> impl IntoResponse {
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_string();
    let error = ProxyError {
        status: StatusCode::NOT_FOUND,
        error_type: "invalid_request_error",
        code: "invalid_parameter",
        message: "route not found".to_string(),
        param: None,
    };

    (error.status, Json(error.into_envelope(request_id)))
}
