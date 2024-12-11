use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum ScrapingBeeResponse {
    Text(String),
    Binary(Vec<u8>)
}

pub struct ScrapingBeeClient {
    client: Client,
    api_key: String,
    render_js: bool,
}

impl ScrapingBeeClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.to_string(),
            render_js: false,
        }
    }

    pub fn url(self, _url: &str) -> Self {
        self
    }

    pub fn render_js(mut self, render: bool) -> Self {
        self.render_js = render;
        self
    }

    pub async fn execute(self) -> Result<ScrapingBeeResponse> {
        Ok(ScrapingBeeResponse::Text("Example response".to_string()))
    }
}
