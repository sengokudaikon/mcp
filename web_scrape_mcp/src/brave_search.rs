use serde::{Deserialize, Serialize};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_ENCODING};
use anyhow::Result;

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    #[serde(rename = "type")]
    pub type_: String,
    pub web: Option<Search>,
    pub query: Option<Query>,
}

#[derive(Debug, Deserialize)]
pub struct Search {
    #[serde(rename = "type")]
    pub type_: String,
    pub results: Vec<SearchResult>,
    pub family_friendly: bool,
}

#[derive(Debug, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub description: Option<String>,
    pub page_age: Option<String>,
    pub page_fetched: Option<String>,
    pub language: Option<String>,
    pub family_friendly: bool,
    pub is_source_local: bool,
    pub is_source_both: bool,
    pub extra_snippets: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct Query {
    pub original: String,
    pub show_strict_warning: Option<bool>,
    pub altered: Option<String>,
    pub safesearch: Option<bool>,
    pub is_navigational: Option<bool>,
    pub is_geolocal: Option<bool>,
    pub local_decision: Option<String>,
    pub local_locations_idx: Option<i32>,
    pub is_trending: Option<bool>,
    pub is_news_breaking: Option<bool>,
    pub ask_for_location: Option<bool>,
    pub spellcheck_off: Option<bool>,
    pub country: Option<String>,
    pub bad_results: Option<bool>,
    pub should_fallback: Option<bool>,
    pub lat: Option<String>,
    pub long: Option<String>,
    pub postal_code: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub header_country: Option<String>,
    pub more_results_available: Option<bool>,
    pub custom_location_label: Option<String>,
    pub reddit_cluster: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchParams {
    pub q: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safesearch: Option<String>,
}

pub struct BraveSearchClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl BraveSearchClient {
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::new();
        let base_url = "https://api.search.brave.com/res/v1/web/search".to_string();
        
        Self {
            client,
            api_key,
            base_url,
        }
    }

    pub async fn search(&self, query: &str) -> Result<SearchResponse> {
        let params = SearchParams {
            q: query.to_string(),
            count: Some(20),  // maximum results
            offset: None,
            safesearch: Some("moderate".to_string()),
        };

        // Log request details
        println!("Making request to: {}", self.base_url);
        println!("Query params: {:?}", params);

        // Set up headers
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            "X-Subscription-Token",
            HeaderValue::from_str(&self.api_key)?,
        );

        // Make the request
        let response = self.client
            .get(&self.base_url)
            .headers(headers)
            .query(&params)
            .send()
            .await?;

        // Check status and content type
        let status = response.status();
        let content_type = response.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown");

        println!("Response status: {}", status);
        println!("Content-Type: {}", content_type);

        if !status.is_success() {
            let error_text = response.text().await?;
            println!("Error response: {}", error_text);
            anyhow::bail!("API request failed with status {}: {}", status, error_text);
        }

        // Get the raw response body first
        let body = response.text().await?;
        println!("Response body (first 100 chars): {}", &body[..body.len().min(100)]);

        // Try to parse the JSON
        let search_response = serde_json::from_str::<SearchResponse>(&body)
            .map_err(|e| anyhow::anyhow!("Failed to parse response: {}. Body: {}", e, body))?;

        Ok(search_response)
    }
}

// Example error type for API errors
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("API request failed: {0}")]
    RequestFailed(String),
    #[error("Failed to parse response: {0}")]
    ParseError(String),
    #[error("Network error: {0}")]
    NetworkError(#[from] reqwest::Error),
}
