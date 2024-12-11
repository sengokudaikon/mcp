use anyhow::{anyhow, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT};
use serde::{Deserialize, Serialize};
use log::{info, warn, error, debug};

#[derive(Debug)]
pub enum ScrapingBeeResponse {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Debug, Serialize)]
struct ScrapingBeeRequest {
    url: String,
    render_js: bool,
}

pub struct ScrapingBeeClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    url: Option<String>,
    render_js: bool,
}

impl ScrapingBeeClient {
    pub fn new(api_key: String) -> Self {
        ScrapingBeeClient {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://app.scrapingbee.com/api/v1/".to_string(),
            url: None,
            render_js: false,
        }
    }

    pub fn url(mut self, url: &str) -> Self {
        self.url = Some(url.to_string());
        self
    }

    pub fn render_js(mut self, enabled: bool) -> Self {
        self.render_js = enabled;
        self
    }

    pub async fn execute(self) -> Result<ScrapingBeeResponse> {
        let url = self.url.ok_or_else(|| anyhow!("URL not set"))?;

        let request_body = ScrapingBeeRequest {
            url,
            render_js: self.render_js,
        };

        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("*/*"));

        let response = self.client
            .post(&self.base_url)
            .headers(headers)
            .query(&[("api_key", &self.api_key)])
            .query(&[("url", &request_body.url)])
            .query(&[("render_js", &request_body.render_js.to_string())])
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "ScrapingBee API request failed with status: {}",
                response.status()
            ));
        }

        let content_type = response.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if content_type.starts_with("text") || content_type.contains("json") {
            Ok(ScrapingBeeResponse::Text(response.text().await?))
        } else {
            Ok(ScrapingBeeResponse::Binary(response.bytes().await?.to_vec()))
        }
    }
}
