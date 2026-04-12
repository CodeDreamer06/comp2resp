use std::{net::SocketAddr, time::Duration};

use axum::http::StatusCode;
use axum_test::TestServer;
use comp2resp::{
    app::build_router,
    config::{Config, InboundAuthMode},
    state::AppState,
};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn config(base_url: String) -> Config {
    Config {
        listen_addr: "127.0.0.1:3000".parse::<SocketAddr>().unwrap(),
        openai_base_url: base_url,
        openai_api_key: Some("test-key".to_string()),
        request_timeout: Duration::from_secs(10),
        connect_timeout: Duration::from_secs(5),
        max_request_body_bytes: 1024 * 1024,
        inbound_auth_mode: InboundAuthMode::None,
        inbound_bearer_token: None,
        forward_user_field: false,
        trust_inbound_x_request_id: false,
        log_json: false,
    }
}

#[tokio::test]
async fn proxies_non_streaming_request() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "resp_123",
            "created_at": 1,
            "model": "gpt-4.1",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        { "type": "output_text", "text": "hello from upstream" }
                    ]
                }
            ],
            "usage": {
                "input_tokens": 1,
                "output_tokens": 2,
                "total_tokens": 3
            },
            "status": "completed"
        })))
        .mount(&mock_server)
        .await;

    let state = AppState::from_config(config(mock_server.uri())).unwrap();
    let server = TestServer::new(build_router(state)).unwrap();

    let response = server
        .post("/v1/chat/completions")
        .json(&serde_json::json!({
            "model": "gpt-4.1",
            "messages": [
                { "role": "user", "content": "hi" }
            ]
        }))
        .await;

    response.assert_status_ok();
    assert_eq!(
        response.json::<serde_json::Value>()["choices"][0]["message"]["content"],
        "hello from upstream"
    );
}

#[tokio::test]
async fn rejects_unsupported_field() {
    let mock_server = MockServer::start().await;
    let state = AppState::from_config(config(mock_server.uri())).unwrap();
    let server = TestServer::new(build_router(state)).unwrap();

    let response = server
        .post("/v1/chat/completions")
        .json(&serde_json::json!({
            "model": "gpt-4.1",
            "messages": [
                { "role": "user", "content": "hi" }
            ],
            "response_format": { "type": "json_object" }
        }))
        .await;

    response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn rejects_non_json_content_type() {
    let mock_server = MockServer::start().await;
    let state = AppState::from_config(config(mock_server.uri())).unwrap();
    let server = TestServer::new(build_router(state)).unwrap();

    let response = server
        .post("/v1/chat/completions")
        .add_header("content-type", "text/plain")
        .text("not json")
        .await;

    response.assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn rejects_malformed_upstream_json() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string("{not valid json"),
        )
        .mount(&mock_server)
        .await;

    let state = AppState::from_config(config(mock_server.uri())).unwrap();
    let server = TestServer::new(build_router(state)).unwrap();

    let response = server
        .post("/v1/chat/completions")
        .json(&serde_json::json!({
            "model": "gpt-4.1",
            "messages": [
                { "role": "user", "content": "hi" }
            ]
        }))
        .await;

    response.assert_status(StatusCode::BAD_GATEWAY);
    assert_eq!(response.json::<serde_json::Value>()["error"]["code"], "upstream_invalid_response");
}
