use axum::{extract::State, Json};
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

pub async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub async fn readyz(State(_state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse { status: "ready" })
}
