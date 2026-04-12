use reqwest::{header, Client};

use crate::{config::Config, error::ProxyError};

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub client: Client,
}

impl AppState {
    pub fn from_config(config: Config) -> Result<Self, ProxyError> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );

        let client = Client::builder()
            .default_headers(headers)
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .build()
            .map_err(|source| {
                ProxyError::internal_with_source("failed to build reqwest client", source)
            })?;

        Ok(Self { config, client })
    }
}
