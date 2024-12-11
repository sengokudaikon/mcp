use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ScrapingBeeResponse {
    pub url: String,
    pub html: String,
}

pub struct ScrapingBeeClient {
    client: Client,
    api_key: String,
}

impl ScrapingBeeClient {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }

    pub async fn scrape(&self, url: &str) -> Result<ScrapingBeeResponse> {
        let response = self.client
            .get(&format!("https://app.scrapingbee.com/api/v1/?api_key={}&url={}", self.api_key, url))
            .send()
            .await?;

        let html = response.text().await?;
        
        Ok(ScrapingBeeResponse {
            url: url.to_string(),
            html,
        })
    }
}
