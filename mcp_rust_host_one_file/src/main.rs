use anyhow::Result;
use console::{style, Term};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use serde::{Deserialize, Serialize};
use log::info;
use tokio::time::Duration;

#[derive(Debug, Deserialize, Serialize)]
struct ServerConfig {
    command: String,
    #[serde(default)]
    env: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    #[serde(rename = "mcpServers")]
    servers: HashMap<String, ServerConfig>,
}
mod openai;
use openai::OpenAIClient;
use shared_protocol_objects::Role;
mod conversation_state;
use conversation_state::ConversationState;
use std::io;
use anyhow::anyhow;
use log::{error,debug};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::time::timeout;
use uuid::Uuid;
use regex::Regex;
use lazy_static::lazy_static;

async fn with_progress<F, T>(msg: String, future: F) -> T 
where
    F: std::future::Future<Output = T>,
{
    let term = Term::stderr();
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut i = 0;
    
    // Clone the message and term for the spawned task
    let progress_msg = msg.clone();
    let progress_term = term.clone();
    
    let handle = tokio::spawn(async move {
        loop {
            // Write the spinner and message, staying on same line
            progress_term.write_str(&format!("\r{} {}", spinner[i], progress_msg))
                .unwrap_or_default();
            // Ensure the line is flushed
            progress_term.flush().unwrap_or_default();
            
            i = (i + 1) % spinner.len();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let result = future.await;
    handle.abort();
    // Clear the progress line completely
    term.clear_line().unwrap_or_default();
    result
}

// Helper functions for parsing tool calls
fn extract_json_after_position(text: &str, pos: usize) -> Option<Value> {
    if let Some(json_start) = text[pos..].find('{') {
        let start_pos = pos + json_start;
        let mut brace_count = 0;
        let mut end_pos = start_pos;
        
        for (i, c) in text[start_pos..].chars().enumerate() {
            match c {
                '{' => brace_count += 1,
                '}' => {
                    brace_count -= 1;
                    if brace_count == 0 {
                        end_pos = start_pos + i + 1;
                        break;
                    }
                }
                _ => continue,
            }
        }

        if brace_count == 0 {
            if let Ok(json) = serde_json::from_str(&text[start_pos..end_pos]) {
                return Some(json);
            }
        }
    }
    None
}

fn find_any_json(text: &str) -> Option<Value> {
    let mut start_indices: Vec<usize> = text.match_indices('{').map(|(i, _)| i).collect();
    start_indices.sort_unstable(); // Sort in case there are multiple JSON objects

    for start in start_indices {
        let mut brace_count = 0;
        let mut end_pos = start;
        
        for (i, c) in text[start..].chars().enumerate() {
            match c {
                '{' => brace_count += 1,
                '}' => {
                    brace_count -= 1;
                    if brace_count == 0 {
                        end_pos = start + i + 1;
                        break;
                    }
                }
                _ => continue,
            }
        }

        if brace_count == 0 {
            if let Ok(json) = serde_json::from_str(&text[start..end_pos]) {
                return Some(json);
            }
        }
    }
    None
}

fn infer_tool_from_json(json: &Value) -> Option<(String, Value)> {
    // Common patterns to identify tools
    if json.get("action").is_some() {
        return Some(("graph_tool".to_string(), json.clone()));
    }
    if json.get("query").is_some() {
        return Some(("brave_search".to_string(), json.clone()));
    }
    if json.get("url").is_some() {
        return Some(("scrape_url".to_string(), json.clone()));
    }
    if json.get("command").is_some() {
        return Some(("bash".to_string(), json.clone()));
    }
    
    // If we can't infer the tool, return None
    None
}

// Main tool call parsing function
fn parse_tool_call(response: &str) -> Option<(String, Value)> {
    lazy_static! {
        // Multiple patterns to match different formats
        static ref TOOL_PATTERNS: Vec<Regex> = vec![
            // Standard format with tool keyword
            Regex::new(
                r"(?s)Let me call the `([^`]+)` tool with these parameters:\s*```(?:json)?\s*(\{.*?\})\s*```"
            ).unwrap(),
            // Without "tool" keyword
            Regex::new(
                r"(?s)Let me call `([^`]+)` with these parameters:\s*```(?:json)?\s*(\{.*?\})\s*```"
            ).unwrap(),
            // More variations
            Regex::new(
                r"(?s)Using the `([^`]+)` tool:?\s*```(?:json)?\s*(\{.*?\})\s*```"
            ).unwrap(),
            Regex::new(
                r"(?s)I'll use `([^`]+)`:?\s*```(?:json)?\s*(\{.*?\})\s*```"
            ).unwrap(),
            // Tool name with JSON anywhere
            Regex::new(
                r"(?s)`([^`]+)`.*?```(?:json)?\s*(\{.*?\})\s*```"
            ).unwrap(),
            // Just the JSON block
            Regex::new(
                r"(?s)```(?:json)?\s*(\{.*?\})\s*```"
            ).unwrap(),
        ];
    }

    // First try: Look for explicit tool name and JSON patterns
    for pattern in TOOL_PATTERNS.iter() {
        if let Some(captures) = pattern.captures(response) {
            match captures.len() {
                // Just JSON block
                2 => {
                    if let Ok(json) = serde_json::from_str(&captures[1]) {
                        // Try to infer tool from JSON content
                        return infer_tool_from_json(&json);
                    }
                },
                // Tool name and JSON
                3 => {
                    let tool_name = captures[1].to_string();
                    if let Ok(args) = serde_json::from_str(&captures[2]) {
                        return Some((tool_name, args));
                    }
                },
                _ => continue,
            }
        }
    }

    // Second try: Look for backtick-wrapped tool names and nearby JSON
    if let Some(tool_start) = response.find('`') {
        if let Some(tool_end) = response[tool_start + 1..].find('`') {
            let tool_name = response[tool_start + 1..tool_start + 1 + tool_end].to_string();
            // Look for JSON after the tool name
            if let Some(json) = extract_json_after_position(response, tool_start + tool_end) {
                return Some((tool_name, json));
            }
        }
    }

    // Third try: Look for any JSON object and try to infer the tool
    if let Some(json) = find_any_json(response) {
        return infer_tool_from_json(&json);
    }

    None
}

use shared_protocol_objects::{
    JsonRpcRequest, JsonRpcResponse, ServerCapabilities, Implementation,
    ToolInfo, CallToolResult, RequestId, ListToolsResult
};

// Server Management Types
#[derive(Debug)]
#[allow(dead_code)]
struct ManagedServer {
    name: String, 
    process: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: Arc<Mutex<ChildStdout>>,
    capabilities: Option<ServerCapabilities>,
    initialized: bool,
}

pub struct MCPHost {
    servers: Arc<Mutex<HashMap<String, ManagedServer>>>,
    client_info: Implementation,
    request_timeout: std::time::Duration,
    openai_client: Option<OpenAIClient>,
}

impl MCPHost {
    pub async fn enter_chat_mode(&self, server_name: &str) -> Result<ConversationState> {
        // Fetch tools from the server
        let tool_info_list = self.list_server_tools(server_name).await?;

        // Convert our tool list to a JSON structure
        let tools_json: Vec<serde_json::Value> = tool_info_list.iter().map(|t| {
            json!({
                "name": t.name,
                "description": t.description.as_ref().unwrap_or(&"".to_string()),
                "inputSchema": t.input_schema
            })
        }).collect();

        let system_prompt = self.generate_system_prompt(&tools_json);

        // Create the conversation state
        let mut state = ConversationState::new(system_prompt, tool_info_list.clone());
        
        // Create a hidden instruction message that combines static guidance with dynamic tool info
        let hidden_instruction = format!(
            "[ASSISTANT INSTRUCTION - FOLLOW THESE GUIDELINES STRICTLY]\n\
            GENERAL PRINCIPLES:\n\
            - Use tools proactively but discreetly in each response\n\
            - Chain tools together to build internal context\n\
            - Run preliminary tools silently before responding\n\
            - Follow each tool's usage patterns exactly as described\n\
            - Maintain natural conversation flow - don't dump tool outputs\n\
            - Don't wait for explicit user requests to use tools\n\n\
            INFORMATION USAGE:\n\
            - DO NOT repeat tool outputs verbatim to the user\n\
            - Use retrieved information to inform and personalize responses\n\
            - Keep tool usage subtle and behind-the-scenes\n\
            - Focus on natural conversation, not data reporting\n\
            - Only reference known information when contextually relevant\n\n\
            TOOL USAGE PATTERN:\n\
            1. Silently gather context from tools first\n\
            2. Process and analyze results internally\n\
            3. Use insights to shape natural responses\n\
            4. Store new information continuously\n\n\
            EXAMPLE INTERACTIONS:\n\
            - BAD: \"I see from the graph that you like pizza and work as a developer\"\n\
            - GOOD: \"Since you're familiar with software development, you might find this interesting...\"\n\
            - BAD: \"According to my records, you mentioned having a dog named Max\"\n\
            - GOOD: \"How's Max doing? Still enjoying those long walks?\"\n\n\
            AVAILABLE TOOLS AND THEIR REQUIRED USAGE PATTERNS:\n{}",
            tool_info_list.iter().map(|tool| {
                format!(
                    "Tool: {}\n\
                    Description: {}\n\
                    Usage Requirements: {}\n\
                    Schema: {}\n",
                    tool.name,
                    tool.description.as_ref().unwrap_or(&"".to_string()),
                    // Extract any usage requirements from description (usually in caps or after "ALWAYS")
                    tool.description.as_ref()
                        .unwrap_or(&"".to_string())
                        .lines()
                        .filter(|line| line.contains("ALWAYS") || line.contains("MUST") || line.contains("NEVER"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default()
                )
            }).collect::<Vec<_>>().join("\n\n")
        );
        
        // Add the hidden instruction as a user message instead of a system message
        state.add_user_message(&hidden_instruction);

        Ok(state)
    }

    fn generate_system_prompt(&self, tools: &[serde_json::Value]) -> String {
        let tools_section = serde_json::to_string_pretty(&json!({ "tools": tools })).unwrap_or("".to_string());

        let prompt = format!(r#"You are an assistant with access to tools to help answer questions and complete tasks.

TOOL INVOCATION FORMAT - THIS IS CRITICAL:
When you want to use a tool, simply output EXACTLY this format and the system will automatically parse and execute it:

Let me call the `tool_name` tool with these parameters:
```json
{{ "parameter": "value" }}
```

EXAMPLE INVOCATIONS (these will be automatically detected and executed):

1. To search the web:
Let me call the `brave_search` tool with these parameters:
```json
{{
    "query": "latest news about AI",
    "count": 5
}}
```

2. To scrape a webpage:
Let me call the `scrape_url` tool with these parameters:
```json
{{
    "url": "https://example.com/article"
}}
```

3. To run a shell command:
Let me call the `bash` tool with these parameters:
```json
{{
    "command": "uname -a"
}}
```

IMPORTANT:
- Just output the tool invocation exactly as shown - the system will automatically detect and execute it
- The backticks around the tool name are REQUIRED
- The json block MUST be wrapped in ```json and ``` markers
- You can make multiple tool calls in sequence
- Each tool call will be automatically detected and executed
- Wait for each tool's response before making additional calls
- The system handles all parsing and execution - just output the format

Available tools and their schemas:
{tools_section}

REMEMBER: Just output the tool invocation format exactly as shown above. The system will handle everything else automatically.
"#);

        prompt
    }

    pub async fn new() -> Result<Self> {
        let openai_client = match std::env::var("OPENAI_API_KEY") {
            Ok(api_key) => {
                info!("Found OpenAI API key in environment");
                // Test the API key with a simple request
                let client = OpenAIClient::new(api_key.clone());
                match tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    client.raw_builder()
                        .model("gpt-4o")
                        .system("Test message")
                        .user("Echo test")
                        .execute()
                ).await {
                    Ok(Ok(_)) => {
                        info!("Successfully validated OpenAI API key");
                        Some(client)
                    }
                    Ok(Err(e)) => {
                        info!("Warning: OpenAI API key validation failed: {}", e);
                        info!("Chat functionality may not work correctly");
                        Some(client)
                    }
                    Err(_) => {
                        info!("Warning: OpenAI API key validation timed out");
                        info!("Chat functionality may not work correctly");
                        Some(client)
                    }
                }
            }
            Err(_) => {
                info!("Warning: OPENAI_API_KEY environment variable not set");
                info!("Chat functionality will not be available");
                None
            }
        };

        Ok(Self {
            servers: Arc::new(Mutex::new(HashMap::new())),
            client_info: Implementation {
                name: "mcp-host".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            request_timeout: std::time::Duration::from_secs(120), // Increased timeout for long-running operations
            openai_client,
        })
    }

    pub async fn load_config(&self, config_path: &str) -> Result<()> {
        info!("Loading configuration from: {}", config_path);
        
        // Ensure config directory exists
        if let Some(parent) = std::path::Path::new(config_path).parent() {
            info!("Creating config directory if it doesn't exist: {}", parent.display());
            fs::create_dir_all(parent)?;
        }

        // Try to read existing config or create default
        let config_str = match fs::read_to_string(config_path) {
            Ok(content) => {
                info!("Found existing config file");
                content
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!("Config file not found, creating default");
                let default_config = Config {
                    servers: HashMap::new(),
                };
                let default_str = serde_json::to_string_pretty(&default_config)?;
                fs::write(config_path, &default_str)?;
                default_str
            }
            Err(e) => return Err(e.into()),
        };

        info!("Parsing configuration JSON");
        let config: Config = serde_json::from_str(&config_str)?;
        
        info!("Found {} servers in config", config.servers.len());
        for (name, server_config) in config.servers {
            // Start each configured server
            let mut command = Command::new(&server_config.command);
            
            // Set environment variables if specified
            for (key, value) in server_config.env {
                command.env(key, value);
            }
            
            self.start_server_with_command(&name, command).await?;
        }
        
        Ok(())
    }

    async fn start_server_with_command(&self, name: &str, mut command: Command) -> Result<()> {
        info!("Starting server '{}' with command: {:?}", name, command);
        command.stdin(Stdio::piped())
               .stdout(Stdio::piped())
               .stderr(Stdio::piped());

        info!("Spawning server process");
        let mut child = command.spawn()?;
        let child_stdin = child.stdin.take().expect("Failed to get stdin");
        let stdin = Arc::new(Mutex::new(ChildStdin::from_std(child_stdin)?));

        let stdout = child.stdout.take().expect("Failed to get stdout");
        let stdout = Arc::new(Mutex::new(ChildStdout::from_std(stdout)?));

        let server = ManagedServer {
            name: name.to_string(),
            process: child,
            stdin,
            stdout,
            capabilities: None,
            initialized: false,
        };

        {
            let mut servers = self.servers.lock().await;
            servers.insert(name.to_string(), server);
        }

        self.initialize_server(name).await?;

        Ok(())
    }

    pub async fn start_server(&self, name: &str, command: &str, args: &[String]) -> Result<()> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        self.start_server_with_command(name, cmd).await
    }

    async fn initialize_server(&self, name: &str) -> Result<()> {
        info!("Initializing server '{}'", name);
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: RequestId::String(Uuid::new_v4().to_string()).into(),
            method: "initialize".to_string(),
            params: Some(json!({
                "capabilities": {
                    "roots": { "listChanged": true },
                    "sampling": {}
                },
                "clientInfo": self.client_info,
                "protocolVersion": "2024-11-05"
            })),
        };

        let response = self.send_request(name, request).await?;

        // Check for error response
        if let Some(error) = response.error {
            error!("RPC Error {}: {}", error.code, error.message);
            return Err(anyhow!("RPC Error {}: {}", error.code, error.message));
        }

        if let Some(result) = response.result {
            let capabilities: ServerCapabilities = serde_json::from_value(result)?;
            let mut servers = self.servers.lock().await;
            if let Some(server) = servers.get_mut(name) {
                server.capabilities = Some(capabilities);
                server.initialized = true;
            }
        }

        // Send initialized notification
        let notification = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: RequestId::String(Uuid::new_v4().to_string()).into(),
            method: "notifications/initialized".to_string(),
            params: None,
        };

        self.send_request(name, notification).await?;

        Ok(())
    }

    async fn send_request(&self, server_name: &str, request: JsonRpcRequest) -> Result<JsonRpcResponse> {
        debug!("\n=== Starting send_request ===");
        debug!("Server: {}", server_name);
        debug!("Request method: {}", request.method);
        let request_str = serde_json::to_string(&request)? + "\n";
        debug!("DEBUG: Sending request: {}", request_str.trim());
        
        // Create channels for stdin/stdout communication
        let (tx, mut rx) = mpsc::channel(1);
        
        // Get the server's I/O handles
        let (stdin, stdout) = {
            let servers = self.servers.lock().await;
            let server = servers.get(server_name)
                .ok_or_else(|| anyhow::anyhow!("Server not found: {}", server_name))?;
            
            (Arc::clone(&server.stdin), Arc::clone(&server.stdout))
        };

        debug!("Spawning async task for request/response handling");
        // Write request and read response in a separate task
        tokio::spawn(async move {
            debug!("Async task started");
            // Write request
            {
                let request_bytes = request_str.as_bytes().to_vec(); // Clone the data
                debug!("Acquiring stdin lock");
                let mut stdin_guard = stdin.lock().await;
                debug!("Acquired stdin lock");
                if let Err(e) = stdin_guard.write_all(&request_bytes).await {
                    let _ = tx.send(Err(anyhow::anyhow!("Failed to write to stdin: {}", e))).await;
                    return;
                }
                if let Err(e) = stdin_guard.flush().await {
                    let _ = tx.send(Err(anyhow::anyhow!("Failed to flush stdin: {}", e))).await;
                    return;
                }
                // stdin_guard is dropped here
            }

            // Read response
            debug!("Starting response read");
            let mut response_line = String::new();
            {
                let mut stdout_guard = stdout.lock().await;
                let mut reader = BufReader::new(&mut *stdout_guard);
                
                match reader.read_line(&mut response_line).await {
                    Ok(0) => {
                        let _ = tx.send(Err(anyhow::anyhow!("Server closed connection"))).await;
                    }
                    Ok(_) => {
                        debug!("DEBUG: Received response: {}", response_line.trim());
                        match serde_json::from_str(&response_line) {
                            Ok(response) => { let _ = tx.send(Ok(response)).await; }
                            Err(e) => { 
                                let _ = tx.send(Err(anyhow::anyhow!("Failed to parse response '{}': {}", response_line.trim(), e))).await; 
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!("Failed to read response: {}", e))).await;
                    }
                }
                // stdout_guard is dropped here
            }
        });

        // Wait for response with timeout
        match timeout(self.request_timeout, rx.recv()).await {
            Ok(Some(result)) => result,
            Ok(None) => Err(anyhow::anyhow!("Response channel closed")),
            Err(_) => Err(anyhow::anyhow!("Request timed out")),
        }
    }

    pub async fn list_server_tools(&self, server_name: &str) -> Result<Vec<ToolInfo>> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: RequestId::String(Uuid::new_v4().to_string()).into(),
            method: "tools/list".to_string(),
            params: None,
        };

        info!("Sending tool call request to server");
        let response = self.send_request(server_name, request).await?;
        info!("Received response from server");
        let tools: ListToolsResult = serde_json::from_value(response.result.unwrap_or_default())?;
        Ok(tools.tools)
    }

    pub async fn call_tool(&self, server_name: &str, tool_name: &str, args: Value) -> Result<String> {
        debug!("call_tool started");
        debug!("Server: {}", server_name);
        debug!("Tool: {}", tool_name);
        debug!("Arguments: {}", serde_json::to_string_pretty(&args).unwrap_or_default());
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: RequestId::String(Uuid::new_v4().to_string()).into(),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": tool_name,
                "arguments": args
            })).into(),
        };

        let response = self.send_request(server_name, request).await?;
        let result: CallToolResult = serde_json::from_value(response.result.unwrap_or_default())?;

        let mut output = String::new();
        for content in result.content {
            match content {
                content => {
                    match content.type_.as_str() {
                        "text" => {
                            output.push_str(&content.text);
                            output.push('\n');
                        }
                        _ => {
                            output.push_str(&format!("Unknown content type: {}\n", content.type_));
                        }
                    }
                }
            }
        }

        Ok(output)
    }

    pub async fn stop_server(&self, name: &str) -> Result<()> {
        let mut servers = self.servers.lock().await;
        if let Some(mut server) = servers.remove(name) {
            server.process.kill()?;
        }
        Ok(())
    }

    async fn handle_assistant_response(
        &self,
        response: &str,
        server_name: &str,
        state: &mut ConversationState,
        client: &OpenAIClient,
    ) -> Result<()> {
        state.add_assistant_message(response);
        
        // Initialize a loop for multiple tool calls
        let mut current_response = response.to_string();
        let mut iteration = 0;
        const MAX_ITERATIONS: i32 = 10; // Increased from 5 to allow more tool calls
        
        while iteration < MAX_ITERATIONS {
            debug!("\nStarting iteration {} of response handling", iteration + 1);
            
            // Try to find all tool calls in the current response
            let mut found_tool_call = false;
            
            // Split response into chunks that might contain tool calls
            let chunks: Vec<&str> = current_response.split("```").collect();
            for (i, chunk) in chunks.iter().enumerate() {
                if i % 2 == 1 { // Only look at content between ``` marks
                    if let Some((tool_name, args)) = parse_tool_call(chunk) {
                        found_tool_call = true;
                        debug!("Found tool call in chunk {}:", i);
                        debug!("Tool: {}", tool_name);
                        debug!("Arguments: {}", serde_json::to_string_pretty(&args).unwrap_or_default());
                        
                        // Execute the tool call
                        println!("{}", style("\nTool Call:").green().bold());
                        println!("└─ {}: {}\n", 
                            style(&tool_name).yellow(),
                            conversation_state::format_json_output(&serde_json::to_string_pretty(&args)?));

                        match self.call_tool(server_name, &tool_name, args).await {
                            Ok(result) => {
                                println!("{}", conversation_state::format_tool_response(&tool_name, &result));
                                state.add_system_message(&result);
                            }
                            Err(e) => {
                                println!("{}: {}\n", style("Error").red().bold(), e);
                                state.add_system_message(&format!("Error: {}", e));
                            }
                        }
                    }
                }
            }
            
            if !found_tool_call {
                break;
            }
            
            // Get next action from assistant with all accumulated context
            let mut builder = client.raw_builder().model("gpt-4o");
            for msg in &state.messages {
                match msg.role {
                    Role::System => builder = builder.system(&msg.content),
                    Role::User => builder = builder.user(&msg.content),
                    Role::Assistant => builder = builder.assistant(&msg.content),
                }
            }
            
            info!("Sending request to OpenAI with timeout");
            match with_progress("Waiting for AI response...".to_string(), 
                tokio::time::timeout(std::time::Duration::from_secs(30), builder.execute())
            ).await {
                Ok(Ok(response)) => {
                    println!("\n{}", conversation_state::format_chat_message(&Role::Assistant, &response));
                    state.add_assistant_message(&response);
                    current_response = response;
                }
                Ok(Err(e)) => {
                    info!("Error getting response from OpenAI API: {}", e);
                    break;
                }
                Err(_) => {
                    info!("OpenAI API request timed out after 30 seconds");
                    break;
                }
            }
            
            iteration += 1;
        }
        
        if iteration >= MAX_ITERATIONS {
            info!("Warning: Reached maximum number of tool call iterations");
        }
        
        Ok(())
    }

    pub async fn run_cli(&self) -> Result<()> {
        info!("MCP Host CLI - Enter 'help' for commands");

        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let args: Vec<&str> = line.trim().split_whitespace().collect();
            if args.is_empty() {
                continue;
            }

            let command = args[0];
            let server_args = &args[1..];

            match command {
                "load_config" => {
                    if server_args.len() != 1 {
                        println!("{}: load_config <config_file>", style("Usage").cyan().bold());
                        continue;
                    }

                    let config_path = server_args[0];
                    match self.load_config(config_path).await {
                        Ok(()) => println!("{} {}", style("Successfully loaded configuration from").green().bold(), config_path),
                        Err(e) => println!("{}: {}", style("Error loading configuration").red().bold(), e),
                    }
                },
                "chat" => {
                    if server_args.len() != 1 {
                        info!("Usage: chat <server>");
                        continue;
                    }

                    let server_name = server_args[0];
                    match self.enter_chat_mode(server_name).await {
                        Ok(mut state) => {
                            println!("\n{}", style("Entering chat mode. Type 'exit' or 'quit' to leave.").cyan().bold());

                            loop {
                                println!("\n{}", style("\nUser:\n").cyan().bold());
                                let mut input = String::new();
                                std::io::stdin().read_line(&mut input)?;
                                let user_input = input.trim();
                                if user_input.eq_ignore_ascii_case("exit") || user_input.eq_ignore_ascii_case("quit") {
                                    info!("Exiting chat mode.");
                                    break;
                                }

                                state.add_user_message(user_input);

                                // Use OpenAI client if available
                                if let Some(client) = &self.openai_client {
                                    let mut builder = client.raw_builder().model("gpt-4o");
                                
                                    // Add all messages from conversation state
                                    for msg in &state.messages {
                                        match msg.role {
                                            Role::System => builder = builder.system(&msg.content),
                                            Role::User => builder = builder.user(&msg.content),
                                            Role::Assistant => builder = builder.assistant(&msg.content),
                                        }
                                    }

                                    match builder.execute().await {
                                        Ok(response) => {
                                            println!("\n{}: {}", style("Assistant").cyan().bold(), response);
                                            // Handle the multi-step response
                                            if let Err(e) = self.handle_assistant_response(&response, server_name, &mut state, client).await {
                                                info!("Error handling assistant response: {}", e);
                                            }
                                        }
                                        Err(e) => info!("Error getting response: {}", e),
                                    }
                                } else {
                                    info!("Error: OpenAI client not initialized. Set OPENAI_API_KEY environment variable.");
                                    break;
                                }
                            }
                        }
                        Err(e) => info!("Error entering chat mode: {}", e),
                    }
                }
                "help" => {
                    println!("\n{}", style("Available commands:").cyan().bold());
                    println!("  {}  - Load servers from config file", style("load_config <file>").yellow());
                    println!("  {}              - List running servers", style("servers").yellow());
                    println!("  {}    - Start a server", style("start <name> <command> [args]").yellow());
                    println!("  {}                  - Stop a server", style("stop <server>").yellow());
                    println!("  {}               - List tools for a server", style("tools <server>").yellow());
                    println!("  {}             - Call a tool with JSON arguments", style("call <server> <tool>").yellow());
                    println!("  {}               - Enter interactive chat mode with a server", style("chat <server>").yellow());
                    println!("  {}                         - Exit the program", style("quit").yellow());
                }
                "servers" => {
                    let servers = self.servers.lock().await;
                    println!("\n{}", style("Running servers:").cyan().bold());
                    for (name, server) in servers.iter() {
                        println!("  {} - initialized: {}", 
                            style(name).yellow(),
                            if server.initialized { style("yes").green() } else { style("no").red() }
                        );
                    }
                    // info!();
                }
                "start" => {
                    if server_args.len() < 2 {
                        info!("Usage: start <name> <command> [args...]");
                        continue;
                    }

                    let server_name = server_args[0];
                    let server_command = server_args[1];
                    let server_extra_args = server_args[2..].to_vec().into_iter().map(String::from).collect::<Vec<_>>();

                    match self.start_server(server_name, server_command, &server_extra_args).await {
                        Ok(()) => info!("Started server '{}'", server_name),
                        Err(e) => info!("Error starting server: {}", e),
                    }
                }
                "stop" => {
                    if server_args.len() != 1 {
                        info!("Usage: stop <server>");
                        continue;
                    }

                    let server_name = server_args[0];
                    match self.stop_server(server_name).await {
                        Ok(()) => info!("Stopped server '{}'", server_name),
                        Err(e) => info!("Error stopping server: {}", e),
                    }
                }
                "tools" => {
                    if server_args.len() != 1 {
                        info!("Usage: tools <server>");
                        continue;
                    }

                    let server_name = server_args[0];
                    match self.list_server_tools(server_name).await {
                        Ok(tools) => {
                            info!("\nAvailable tools for {}:", server_name);
                            for tool in tools {
                                info!("  {} - {}", tool.name, tool.description.unwrap_or_default());
                                let schema = tool.input_schema;
                                info!("    Arguments schema:");
                                info!("{}", serde_json::to_string_pretty(&schema)?
                                    .split('\n')
                                    .map(|line| format!("      {}", line))
                                    .collect::<Vec<_>>()
                                    .join("\n"));
                            }
                            // info!();
                        }
                        Err(e) => info!("Error: {}", e),
                    }
                }
                "call" => {
                    if server_args.len() != 2 {
                        info!("Usage: call <server> <tool>");
                        continue;
                    }

                    let server_name = server_args[0];
                    let tool_name = server_args[1];

                    info!("Enter arguments (JSON):");
                    let mut json_input = String::new();
                    let stdin = io::stdin(); // Standard input stream
                    stdin.read_line(&mut json_input)?;

                    let args_value: Value = match serde_json::from_str(&json_input) {
                        Ok(v) => v,
                        Err(e) => {
                            info!("Invalid JSON: {}", e);
                            continue;
                        }
                    };

                    match self.call_tool(server_name, tool_name, args_value).await {
                        Ok(result) => {
                            if result.trim().is_empty() {
                                println!("\n{}", style("No results returned").yellow());
                            } else {
                                println!("\n{}", style("Result:").cyan().bold());
                                if result.trim().starts_with('{') || result.trim().starts_with('[') {
                                    // Pretty print JSON
                                    if let Ok(json) = serde_json::from_str::<Value>(&result) {
                                        println!("```json\n{}\n```", serde_json::to_string_pretty(&json)?);
                                    } else {
                                        println!("{}", result);
                                    }
                                } else {
                                    println!("{}", result);
                                }
                            }
                        }
                        Err(e) => println!("{}: {}", style("Error calling tool").red().bold(), e),
                    }
                }
                "quit" => break,
                _ => info!("Unknown command. Type 'help' for available commands."),
            }
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging with env_logger
    env_logger::init();

    let host = MCPHost::new().await?;

    // Get command line arguments
    let args: Vec<String> = std::env::args().collect();
    
    // If arguments are provided, handle them first
    if args.len() > 1 {
        match args[1].as_str() {
            "load_config" if args.len() == 3 => {
                let config_path = &args[2];
                if let Err(e) = host.load_config(config_path).await {
                    info!("Error loading configuration: {}", e);
                    return Ok(());
                }
                info!("Successfully loaded configuration from {}", config_path);
            }
            _ => {
                info!("Invalid command line arguments");
                info!("Usage: {} load_config <config_file>", args[0]);
                return Ok(());
            }
        }
    }

    // Start the interactive CLI loop
    host.run_cli().await?;

    // Stop all servers before exit
    let servers = host.servers.lock().await;
    for name in servers.keys() {
        let _ = host.stop_server(name).await;
    }

    Ok(())
}
