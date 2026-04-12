use std::{env, net::SocketAddr, str::FromStr, time::Duration};

use crate::error::ProxyError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundAuthMode {
    None,
    StaticBearer,
    PassthroughBearer,
}

impl FromStr for InboundAuthMode {
    type Err = ProxyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "static_bearer" => Ok(Self::StaticBearer),
            "passthrough_bearer" => Ok(Self::PassthroughBearer),
            _ => Err(ProxyError::internal(format!(
                "invalid INBOUND_AUTH_MODE: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub openai_base_url: String,
    pub openai_api_key: Option<String>,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub max_request_body_bytes: usize,
    pub inbound_auth_mode: InboundAuthMode,
    pub inbound_bearer_token: Option<String>,
    pub forward_user_field: bool,
    pub trust_inbound_x_request_id: bool,
    pub log_json: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, ProxyError> {
        let listen_addr = parse_socket_addr("LISTEN_ADDR", "127.0.0.1:3000")?;
        let openai_base_url = env_required("OPENAI_BASE_URL")?;
        let request_timeout = parse_duration_secs("REQUEST_TIMEOUT_SECS", 60)?;
        let connect_timeout = parse_duration_secs("CONNECT_TIMEOUT_SECS", 10)?;
        let max_request_body_bytes = parse_usize("MAX_REQUEST_BODY_BYTES", 1_048_576)?;
        let inbound_auth_mode = env::var("INBOUND_AUTH_MODE")
            .unwrap_or_else(|_| "none".to_string())
            .parse()?;
        let inbound_bearer_token = env::var("INBOUND_BEARER_TOKEN").ok();
        let forward_user_field = parse_bool("FORWARD_USER_FIELD", false)?;
        let trust_inbound_x_request_id = parse_bool("TRUST_INBOUND_X_REQUEST_ID", false)?;
        let log_json = parse_bool("LOG_JSON", false)?;
        let openai_api_key = env::var("OPENAI_API_KEY").ok();

        if !openai_base_url.starts_with("http://") && !openai_base_url.starts_with("https://") {
            return Err(ProxyError::internal(
                "OPENAI_BASE_URL must start with http:// or https://",
            ));
        }

        if request_timeout.is_zero() {
            return Err(ProxyError::internal(
                "REQUEST_TIMEOUT_SECS must be greater than 0",
            ));
        }

        if connect_timeout.is_zero() {
            return Err(ProxyError::internal(
                "CONNECT_TIMEOUT_SECS must be greater than 0",
            ));
        }

        if max_request_body_bytes == 0 {
            return Err(ProxyError::internal(
                "MAX_REQUEST_BODY_BYTES must be greater than 0",
            ));
        }

        match inbound_auth_mode {
            InboundAuthMode::StaticBearer if inbound_bearer_token.is_none() => {
                return Err(ProxyError::internal(
                    "INBOUND_BEARER_TOKEN is required when INBOUND_AUTH_MODE=static_bearer",
                ));
            }
            InboundAuthMode::None
            | InboundAuthMode::PassthroughBearer
            | InboundAuthMode::StaticBearer => {}
        }

        if inbound_auth_mode != InboundAuthMode::PassthroughBearer && openai_api_key.is_none() {
            return Err(ProxyError::internal(
                "OPENAI_API_KEY is required unless INBOUND_AUTH_MODE=passthrough_bearer",
            ));
        }

        Ok(Self {
            listen_addr,
            openai_base_url: openai_base_url.trim_end_matches('/').to_string(),
            openai_api_key,
            request_timeout,
            connect_timeout,
            max_request_body_bytes,
            inbound_auth_mode,
            inbound_bearer_token,
            forward_user_field,
            trust_inbound_x_request_id,
            log_json,
        })
    }
}

fn env_required(name: &str) -> Result<String, ProxyError> {
    env::var(name).map_err(|_| ProxyError::internal(format!("missing required env var {name}")))
}

fn parse_socket_addr(name: &str, default: &str) -> Result<SocketAddr, ProxyError> {
    env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse()
        .map_err(|_| ProxyError::internal(format!("invalid socket address in {name}")))
}

fn parse_duration_secs(name: &str, default: u64) -> Result<Duration, ProxyError> {
    let secs = env::var(name)
        .ok()
        .map(|raw| {
            raw.parse::<u64>()
                .map_err(|_| ProxyError::internal(format!("invalid integer value in {name}")))
        })
        .transpose()?
        .unwrap_or(default);
    Ok(Duration::from_secs(secs))
}

fn parse_usize(name: &str, default: usize) -> Result<usize, ProxyError> {
    env::var(name)
        .ok()
        .map(|raw| {
            raw.parse::<usize>()
                .map_err(|_| ProxyError::internal(format!("invalid integer value in {name}")))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_bool(name: &str, default: bool) -> Result<bool, ProxyError> {
    match env::var(name).ok().as_deref() {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(_) => Err(ProxyError::internal(format!(
            "invalid boolean value in {name}"
        ))),
    }
}
