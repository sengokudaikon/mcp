use anyhow::{anyhow, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
pub struct ScrapingBeeResponse {
    #[serde(default)]
    pub content: String,
}

#[derive(Debug, Serialize)]
struct ScrapingBeeRequest {
    url: String,
    render_js: bool,
    // Add other parameters as needed for your specific usage
}

pub struct ScrapingBeeClient {
    api_key: String,
    base_url: String,
}

impl ScrapingBeeClient {
    pub fn new(api_key: String) -> Self {
        ScrapingBeeClient {
            api_key,
            base_url: "https://app.scrapingbee.com/api/v1/".to_string(),
        }
    }

    pub async fn scrape(&self, url: &str) -> Result<ScrapingBeeResponse> {
        let request_body = ScrapingBeeRequest {
            url: url.to_string(),
            render_js: true,
        };

        // Set up headers
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8"),
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;

        let response = client
            .post(&self.base_url)
            .query(&[("api_key", &self.api_key)])
            .query(&[("render_js", &request_body.render_js.to_string())])
            .query(&[("url", &request_body.url)])
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "ScrapingBee API request failed with status: {}",
                response.status()
            ));
        }

        let response_json: Value = response.json().await?;
        let content = response_json
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        Ok(ScrapingBeeResponse { content })
    }
}