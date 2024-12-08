mod processor;
use shared_protocol_objects::{
    ResourceInfo, ToolInfo, ServerCapabilities, Implementation, 
    InitializeResult, ClientCapabilities,
    ResourcesCapability, ToolsCapability,
    JsonRpcRequest, JsonRpcResponse,
    ListResourcesResult, ListToolsResult, ReadResourceParams,
    ResourceContent, ReadResourceResult,
    ToolResponseContent, CallToolResult, CallToolParams,
    LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
    PARSE_ERROR, INVALID_PARAMS, INTERNAL_ERROR, error_response, success_response
};

use processor::{OpenAIClient, Processor};
mod process_html;
use process_html::extract_text_from_html;

mod bash;
use bash::{BashExecutor, BashParams};

mod brave_search;
mod scraping_bee;

use brave_search::BraveSearchClient;
use scraping_bee::{ScrapingBeeClient, ScrapingBeeResponse};
use serde_json::{json, Value};
use std::collections::HashMap;

use std::sync::Arc;
use tokio::io::stdout;
use tokio::io::AsyncWriteExt;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::task;
use tokio_stream::wrappers::LinesStream;
use tokio_stream::StreamExt;


#[derive(Debug)]
struct MCPServerState {
    resources: Vec<ResourceInfo>,
    tools: Vec<ToolInfo>,
    client_capabilities: Option<ClientCapabilities>,
    client_info: Option<Implementation>,
}

#[tokio::main]
async fn main() {
    // Verify required environment variables are present
    let _ = std::env::var("SCRAPINGBEE_API_KEY")
        .expect("SCRAPINGBEE_API_KEY environment variable must be set");
    let _ = std::env::var("BRAVE_API_KEY").expect("BRAVE_API_KEY environment variable must be set");

    let state = Arc::new(Mutex::new(MCPServerState {
        resources: vec![ResourceInfo {
            uri: "file:///example.txt".into(),
            name: "Example Text File".into(),
            mime_type: Some("text/plain".into()),
            description: Some("An example text resource".into()),
        }],
        tools: vec![
            ToolInfo {
                name: BashExecutor::new().tool_info().name,
                description: Some(BashExecutor::new().tool_info().description),
                input_schema: Some(BashExecutor::new().tool_info().input_schema),
            },

            ToolInfo {
                name: "scrape_url".into(),
                description: Some(
                    "Use this tool when you need to read, analyze, or extract information from a webpage. \
                    Perfect for when a user asks you to:\n\
                    - Read and summarize an article or blog post\n\
                    - Extract text from a documentation page\n\
                    - Analyze the content of a product page\n\
                    - Get information from a news site\n\
                    - Read technical documentation\n\
                    - Extract code examples from tutorials\n\
                    - Get text from pages that might block normal scraping\n\
                    - Capture text from dynamic JavaScript-rendered content\n\
                    The tool takes a screenshot of the full webpage, processes it with OCR, and returns all visible text. \
                    Use this when you need to actually read the contents of a webpage, not just search for information.".into()
                ),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "url": { 
                            "type": "string",
                            "description": "The complete URL of the webpage to read and analyze"
                        }
                    },
                    "required": ["url"]
                })),
            },
            ToolInfo {
                name: "brave_search".into(),
                description: Some(
                    "Use this tool when you need to search the internet for information. Perfect for when a user:\n\
                    - Asks about current events or news\n\
                    - Needs up-to-date information about a topic\n\
                    - Wants to find specific websites or resources\n\
                    - Needs to verify facts or claims\n\
                    - Wants to discover articles about a subject\n\
                    - Needs to find documentation or tutorials\n\
                    - Wants to research products or services\n\
                    - Needs recent information not in your training data\n\
                    - Asks about trends or developments\n\
                    - Wants to find discussions or forums about a topic\n\
                    Use this tool first to find relevant URLs, then use scrape_url to read specific pages in detail. \
                    This tool returns a list of search results with titles, URLs, and brief descriptions.".into()
                ),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query - be specific and include relevant keywords"
                        },
                        "count": {
                            "type": "number",
                            "description": "Number of results to return (max 20). Use more results for broad research, fewer for specific queries.",
                            "default": 10
                        }
                    },
                    "required": ["query"]
                })),
            }
        ],
        client_capabilities: None,
        client_info: None
    }));

    let (tx_out, mut rx_out) = mpsc::unbounded_channel::<JsonRpcResponse>();

    let printer_handle = tokio::spawn(async move {
        let mut out = stdout();
        while let Some(resp) = rx_out.recv().await {
            let serialized = serde_json::to_string(&resp).unwrap();
            let _ = out.write_all(serialized.as_bytes()).await;
            let _ = out.write_all(b"\n").await;
            let _ = out.flush().await;
        }
    });

    let stdin = io::stdin();
    let reader = BufReader::new(stdin);
    let lines = reader.lines();
    let mut lines = LinesStream::new(lines);

    while let Some(Ok(line)) = lines.next().await {
        if line.trim().is_empty() {
            continue;
        }

        let parsed: Result<JsonRpcRequest, _> = serde_json::from_str(&line);
        let req = match parsed {
            Ok(req) => req,
            Err(_) => {
                let resp = error_response(None, PARSE_ERROR, "Parse error");
                let _ = tx_out.send(resp);
                continue;
            }
        };

        let state = Arc::clone(&state);
        let tx_out_clone = tx_out.clone();

        task::spawn(async move {
            let resp = handle_request(req, &state, tx_out_clone.clone()).await;
            if let Some(resp) = resp {
                let _ = tx_out_clone.send(resp);
            }
        });
    }

    drop(tx_out);
    let _ = printer_handle.await;
}

async fn handle_request(
    req: JsonRpcRequest,
    state: &Arc<Mutex<MCPServerState>>,
    tx_out: mpsc::UnboundedSender<JsonRpcResponse>,
) -> Option<JsonRpcResponse> {
    let id = if req.id.is_null() {
        None
    } else {
        Some(req.id.clone())
    };

    match req.method.as_str() {
        "initialize" => {
            let params = match req.params {
                Some(p) => p,
                None => return Some(error_response(id, -32602, "Missing params")),
            };

            let protocol_version = params.get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(LATEST_PROTOCOL_VERSION);

            if !SUPPORTED_PROTOCOL_VERSIONS.contains(&protocol_version) {
                return Some(error_response(id, -32602, "Unsupported protocol version"));
            }

            // Store client info and capabilities
            if let Some(client_info) = params.get("clientInfo") {
                if let Ok(info) = serde_json::from_value(client_info.clone()) {
                    let mut guard = state.lock().await;
                    guard.client_info = Some(info);
                }
            }

            if let Some(capabilities) = params.get("capabilities") {
                if let Ok(caps) = serde_json::from_value(capabilities.clone()) {
                    let mut guard = state.lock().await;
                    guard.client_capabilities = Some(caps);
                }
            }

            let result = InitializeResult {
                protocol_version: protocol_version.to_string(),
                capabilities: ServerCapabilities {
                    experimental: Some(HashMap::new()),
                    logging: Some(json!({})),
                    prompts: Some(PromptsCapability {
                        list_changed: false
                    }),
                    resources: Some(ResourcesCapability {
                        subscribe: false,
                        list_changed: true,
                    }),
                    tools: Some(ToolsCapability {
                        list_changed: true,
                    }),
                },
                server_info: Implementation {
                    name: "rust-mcp-server".into(),
                    version: "1.0.0".into(),
                },
                _meta: None,
            };

            Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!({
                    "protocolVersion": result.protocol_version,
                    "serverInfo": result.server_info,
                    "capabilities": {
                        "experimental": result.capabilities.experimental,
                        "logging": result.capabilities.logging,
                        "prompts": result.capabilities.prompts,
                        "resources": result.capabilities.resources,
                        "tools": result.capabilities.tools
                    }
                })),
                error: None
            })
        }

        "resources/list" => {
            let guard = state.lock().await;
            let result = ListResourcesResult {
                resources: guard.resources.clone(),
                _meta: None,
            };
            Some(success_response(id, json!(result)))
        }

        "resources/read" => {
            let params_res: Result<ReadResourceParams, _> = serde_json::from_value(req.params.unwrap_or(Value::Null));
            let params = match params_res {
                Ok(p) => p,
                Err(e) => {
                    return Some(error_response(
                        id,
                        -32602,
                        &format!("Invalid params: {}", e),
                    ))
                }
            };

            let guard = state.lock().await;
            let res = guard.resources.iter().find(|r| r.uri == params.uri);
            match res {
                Some(r) => {
                    let content = ResourceContent {
                        uri: r.uri.clone(),
                        mime_type: r.mime_type.clone(),
                        text: Some("Example file contents.\n".into()),
                        blob: None,
                    };
                    let result = ReadResourceResult {
                        contents: vec![content],
                        _meta: None,
                    };
                    Some(success_response(id, json!(result)))
                }
                None => Some(error_response(id, -32601, "Resource not found")),
            }
        }

        "tools/list" => {
            let guard = state.lock().await;
            let result = ListToolsResult {
                tools: guard.tools.clone(),
                _meta: None,
            };
            Some(success_response(id, json!(result)))
        }

        "tools/call" => {
            let params_res: Result<CallToolParams, _> = serde_json::from_value(req.params.clone().unwrap_or(Value::Null));
            let params = match params_res {
                Ok(p) => p,
                Err(e) => {
                    return Some(error_response(
                        id,
                        -32602,
                        &format!("Invalid params: {}", e),
                    ))
                }
            };

            let tool = {
                let guard = state.lock().await;
                guard.tools.iter().find(|t| t.name == params.name).cloned()
            };

            match tool {
                Some(t) => {
                    if t.name == "scrape_url" {
                        // Handle scrape_url tool
                        let url = params
                            .arguments
                            .get("url")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if url.is_empty() {
                            return Some(error_response(
                                id,
                                -32602,
                                "Missing required argument: url",
                            ));
                        }

                        let scrapingbee_api_key = std::env::var("SCRAPINGBEE_API_KEY")
                            .expect("SCRAPINGBEE_API_KEY environment variable must be set");
                        let client = ScrapingBeeClient::new(&scrapingbee_api_key)
                            .url(&url)
                            .render_js(true);

                        let result = client.execute().await;

                        match result {
                            Ok(ScrapingBeeResponse::Text(body)) => {
                                // Send progress notification if requested
                                let meta = if let Some(params) = &req.params {
                                    params.get("_meta").cloned()
                                } else {
                                    None
                                };
                                if let Some(meta) = meta {
                                    if let Some(token) = meta.get("progressToken") {
                                        let progress_notification = JsonRpcResponse {
                                            jsonrpc: "2.0".into(),
                                            id: None,
                                            result: Some(json!({
                                                "method": "notifications/progress",
                                                "params": {
                                                    "progressToken": token,
                                                    "progress": 50,
                                                    "total": 100
                                                }
                                            })),
                                            error: None,
                                        };
                                        let _ = tx_out.send(progress_notification);
                                    }
                                }

                                let tool_res = CallToolResult {
                                    content: vec![ToolResponseContent {
                                        type_: "text".into(),
                                        text: extract_text_from_html(&body, Some(&url)),
                                    }],
                                    is_error: None,
                                    _meta: None,
                                    progress: None,
                                    total: None,
                                };
                                Some(success_response(id, json!(tool_res)))
                            }
                            Ok(ScrapingBeeResponse::Binary(bytes)) => {
                                // Save screenshot to temporary file
                                let temp_filename =
                                    format!("temp_screenshot_{}.png", uuid::Uuid::new_v4());
                                if let Err(e) = std::fs::write(&temp_filename, &bytes) {
                                    return Some(error_response(
                                        id,
                                        -32603,
                                        &format!("Failed to write temp file: {}", e),
                                    ));
                                }

                                // Initialize OpenAI client and processor
                                let openai_api_key = match std::env::var("OPENAI_API_KEY") {
                                    Ok(key) => key,
                                    Err(_) => {
                                        return Some(error_response(
                                            id,
                                            -32603,
                                            "OPENAI_API_KEY not set",
                                        ))
                                    }
                                };
                                let openai_client = OpenAIClient::new(openai_api_key);
                                let processor =
                                    Processor::new(&openai_client, "metadata.json", "output");

                                // Process the screenshot
                                match processor.process_image(&temp_filename).await {
                                    Ok(()) => {
                                        // Read the processed output
                                        let metadata =
                                            match processor::Metadata::load("metadata.json") {
                                                Ok(m) => m,
                                                Err(e) => {
                                                    return Some(error_response(
                                                        id,
                                                        -32603,
                                                        &format!("Failed to load metadata: {}", e),
                                                    ))
                                                }
                                            };
                                        if let Some(img_meta) = metadata.images.last() {
                                            if let Some(output_file) = &img_meta.output_file {
                                                let processed_text =
                                                    match std::fs::read_to_string(output_file) {
                                                        Ok(text) => text,
                                                        Err(e) => {
                                                            return Some(error_response(
                                                                id,
                                                                -32603,
                                                                &format!(
                                                                "Failed to read output file: {}",
                                                                e
                                                            ),
                                                            ))
                                                        }
                                                    };
                                                let tool_res = CallToolResult {
                                                    content: vec![ToolResponseContent {
                                                        type_: "text".into(),
                                                        text: processed_text,
                                                    }],
                                                    is_error: None,
                                                    _meta: None,
                                                    progress: None,
                                                    total: None,
                                                };
                                                // Clean up temp file
                                                let _ = std::fs::remove_file(&temp_filename);
                                                Some(success_response(id, json!(tool_res)))
                                            } else {
                                                Some(error_response(
                                                    id,
                                                    -32603,
                                                    "No output file generated",
                                                ))
                                            }
                                        } else {
                                            Some(error_response(
                                                id,
                                                -32603,
                                                "No metadata entry found",
                                            ))
                                        }
                                    }
                                    Err(e) => {
                                        let _ = std::fs::remove_file(&temp_filename);
                                        Some(error_response(
                                            id,
                                            -32603,
                                            &format!("Failed to process image: {}", e),
                                        ))
                                    }
                                }
                            }
                            Err(e) => {
                                let tool_res = CallToolResult {
                                    content: vec![ToolResponseContent {
                                        type_: "text".into(),
                                        text: format!("Error: {}", e),
                                    }],
                                    is_error: Some(true),
                                    _meta: None,
                                    progress: None,
                                    total: None,
                                };
                                Some(success_response(id, json!(tool_res)))
                            }
                        }
                    } else if t.name == "bash" {
                        // Parse bash params
                        let bash_params: BashParams = match serde_json::from_value(params.arguments)
                        {
                            Ok(p) => p,
                            Err(e) => {
                                return Some(error_response(id, INVALID_PARAMS, &e.to_string()))
                            }
                        };

                        let executor = BashExecutor::new();

                        // Execute command
                        match executor.execute(bash_params).await {
                            Ok(result) => {
                                let tool_res = CallToolResult {
                                content: vec![ToolResponseContent {
                                    type_: "text".into(),
                                    text: format!(
                                        "Command completed with status {}\n\nSTDOUT:\n{}\n\nSTDERR:\n{}", 
                                        result.status,
                                        result.stdout,
                                        result.stderr
                                    ),
                                }],
                                is_error: Some(!result.success),
                                _meta: None,
                                progress: None,
                                total: None,
                            };
                                Some(success_response(id, json!(tool_res)))
                            }
                            Err(e) => Some(error_response(id, INTERNAL_ERROR, &e.to_string())),
                        }
                    } else if t.name == "brave_search" {
                        let query = match params.arguments.get("query").and_then(Value::as_str) {
                            Some(q) => q.to_string(),
                            None => {
                                return Some(error_response(
                                    id,
                                    -32602,
                                    "Missing required argument: query",
                                ))
                            }
                        };

                        let count = params
                            .arguments
                            .get("count")
                            .and_then(Value::as_u64)
                            .unwrap_or(10)
                            .min(20) as u8;

                        let brave_search_api_key = std::env::var("BRAVE_API_KEY")
                            .expect("BRAVE_API_KEY environment variable must be set");
                        let client = BraveSearchClient::new(brave_search_api_key);
                        match client.search(&query).await {
                            Ok(response) => {
                                let results = match response.web {
                                    Some(web) => web
                                        .results
                                        .iter()
                                        .take(count as usize)
                                        .map(|result| {
                                            format!(
                                                "Title: {}\nURL: {}\nDescription: {}\n\n",
                                                result.title,
                                                result.url,
                                                result
                                                    .description
                                                    .as_deref()
                                                    .unwrap_or("No description available")
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .join("---\n"),
                                    None => "No web results found".to_string(),
                                };

                                let tool_res = CallToolResult {
                                    content: vec![ToolResponseContent {
                                        type_: "text".into(),
                                        text: results,
                                    }],
                                    is_error: None,
                                    _meta: None,
                                    progress: None,
                                    total: None,
                                };
                                Some(success_response(id, json!(tool_res)))
                            }
                            Err(e) => {
                                let tool_res = CallToolResult {
                                    content: vec![ToolResponseContent {
                                        type_: "text".into(),
                                        text: format!("Search error: {}", e),
                                    }],
                                    is_error: Some(true),
                                    _meta: None,
                                    progress: None,
                                    total: None,
                                };
                                Some(success_response(id, json!(tool_res)))
                            }
                        }
                    } else {
                        Some(error_response(id, -32601, "Tool not implemented"))
                    }
                }
                None => Some(error_response(id, -32601, "Tool not found")),
            }
        }

        _ => Some(error_response(id, -32601, "Method not found")),
    }
}

