use axum::{
    extract::rejection::{BytesRejection, JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, Serialize)]
pub struct ErrorEnvelope {
    pub error: ApiErrorBody,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: &'static str,
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    pub request_id: String,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ProxyError {
    pub status: StatusCode,
    pub error_type: &'static str,
    pub code: &'static str,
    pub message: String,
    pub param: Option<String>,
}

impl ProxyError {
    pub fn invalid_request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error_type: "invalid_request_error",
            code,
            message: message.into(),
            param: None,
        }
    }

    pub fn invalid_request_with_param(
        code: &'static str,
        param: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error_type: "invalid_request_error",
            code,
            message: message.into(),
            param: Some(param.into()),
        }
    }

    pub fn invalid_param(
        code: &'static str,
        param: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            error_type: "invalid_request_error",
            code,
            message: message.into(),
            param: Some(param.into()),
        }
    }

    pub fn unsupported_feature(param: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            error_type: "invalid_request_error",
            code: "unsupported_feature",
            message: message.into(),
            param: Some(param.into()),
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            error_type: "authentication_error",
            code: "unauthorized",
            message: message.into(),
            param: None,
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            error_type: "permission_error",
            code: "forbidden",
            message: message.into(),
            param: None,
        }
    }

    pub fn upstream(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        let error_type = match status {
            StatusCode::UNAUTHORIZED => "authentication_error",
            StatusCode::FORBIDDEN => "permission_error",
            StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
            _ => "api_error",
        };

        Self {
            status,
            error_type,
            code,
            message: message.into(),
            param: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error_type: "api_error",
            code: "internal_error",
            message: message.into(),
            param: None,
        }
    }

    pub fn internal_with_source(
        message: impl Into<String>,
        source: impl std::error::Error,
    ) -> Self {
        Self::internal(format!("{}: {}", message.into(), source))
    }

    pub fn with_status(mut self, status: StatusCode, code: &'static str) -> Self {
        self.status = status;
        self.code = code;
        self
    }

    pub fn into_envelope(self, request_id: String) -> ErrorEnvelope {
        ErrorEnvelope {
            error: ApiErrorBody {
                message: self.message,
                error_type: self.error_type,
                code: self.code,
                param: self.param,
                request_id,
            },
        }
    }
}

impl From<JsonRejection> for ProxyError {
    fn from(rejection: JsonRejection) -> Self {
        match rejection {
            JsonRejection::MissingJsonContentType(_) => Self {
                status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                error_type: "invalid_request_error",
                code: "unsupported_media_type",
                message: "Content-Type must be application/json".to_string(),
                param: None,
            },
            JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_) => Self {
                status: StatusCode::BAD_REQUEST,
                error_type: "invalid_request_error",
                code: "invalid_json",
                message: "request body contained invalid JSON".to_string(),
                param: None,
            },
            JsonRejection::BytesRejection(bytes) => Self::from(bytes),
            _ => Self::invalid_request("invalid_json", rejection.body_text()),
        }
    }
}

impl From<BytesRejection> for ProxyError {
    fn from(rejection: BytesRejection) -> Self {
        let body_text = rejection.body_text();
        if body_text.to_ascii_lowercase().contains("body too large") {
            Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                error_type: "invalid_request_error",
                code: "body_too_large",
                message: "request body exceeded configured size limit".to_string(),
                param: None,
            }
        } else {
            Self::invalid_request("invalid_json", body_text)
        }
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let request_id = "unknown".to_string();
        (self.status, Json(self.into_envelope(request_id))).into_response()
    }
}
