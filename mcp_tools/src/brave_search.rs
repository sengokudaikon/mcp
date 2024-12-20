use serde::{Deserialize, Serialize};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT};
use anyhow::Result;
use serde_json::json;

use shared_protocol_objects::ToolInfo;

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

pub fn search_tool_info() -> ToolInfo {
    ToolInfo {
        name: "brave_search".into(),
        description: Some(
            "Searches the internet using the Brave search engine. 
            
            **PROACTIVE USAGE:**
            - CONTINUOUSLY search for relevant information during conversations
            - AUTOMATICALLY research topics as they come up
            - PROACTIVELY gather supporting facts and statistics
            - ACTIVELY find related resources and documentation
            - IMMEDIATELY search when external knowledge is needed
            - CONSISTENTLY gather context and evidence
            
            **When to Use:**
            - ANY TIME the conversation touches on topics that could benefit from external information
            - WHENEVER claims or statements could use verification
            - BEFORE responding to questions that might need factual support
            - DURING discussions to enrich the conversation with relevant data
            - AS SOON AS new topics or concepts are introduced

            **Input:**
             - Expects a single search query string in the 'query' field.
                - Be specific and use keywords that are likely to be found in the content you are seeking.
                - The 'count' parameter (optional) controls how many results to return. Use more results (up to 20) for broad queries and fewer for specific facts.

            **Output:**
            - Returns a list of search results that includes:
               - The title of the page
               - The URL of the page
               - An excerpt describing the page content.
               - Page age.
               - Page fetched date.

            **Usage Constraints:**
            - Do not use for personal information.
            - Use this tool as a first step. You can follow up with `scrape_url` for more detailed information from the search results.
            - ALWAYS choose results that are most relevant to the user's question or task.
           ".into()
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query - be specific and include relevant keywords",
                    "minLength": 1
                },
                "count": {
                    "type": "integer",
                    "description": "Number of results to return (max 20). Use more results for broad research, fewer for specific queries.",
                    "default": 10,
                    "minimum": 1,
                    "maximum": 20
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    }
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

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("API request failed with status {}", error_text);
        }

        // Parse the JSON response
        let search_response = response.json::<SearchResponse>().await
            .map_err(|e| anyhow::anyhow!("Failed to parse response: {}", e))?;

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
