use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;
mod openai;
use openai::OpenAIClient;
use shared_protocol_objects::Role;
mod conversation_state;
use conversation_state::ConversationState;
use std::io;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::time::timeout;
use uuid::Uuid;

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

        // Convert our tool list to a JSON structure similar to the Python code
        let tools_json: Vec<serde_json::Value> = tool_info_list.iter().map(|t| {
            json!({
                "name": t.name,
                "description": t.description.as_ref().unwrap_or(&"".to_string()),
                "inputSchema": t.input_schema
            })
        }).collect();

        let system_prompt = self.generate_system_prompt(&tools_json);

        // Create the conversation state
        let state = ConversationState::new(system_prompt, tool_info_list);

        Ok(state)
    }

    fn generate_system_prompt(&self, tools: &[serde_json::Value]) -> String {
        // Emulating the Python script's system prompt logic
        let tools_section = serde_json::to_string_pretty(&json!({ "tools": tools })).unwrap_or("".to_string());

        let mut prompt = String::new();
        prompt.push_str("You are an assistant with access to a set of tools you can use to answer the user's question.\n");
        prompt.push_str("String and scalar parameters should be specified as is, while lists and objects should use JSON format.\n");
        prompt.push_str("Here are the functions available in JSONSchema format:\n");
        prompt.push_str(&tools_section);
        prompt.push_str("\n\n**GENERAL GUIDELINES:**\n\n");
        prompt.push_str("1. Step-by-step reasoning:\n");
        prompt.push_str("   - Analyze tasks systematically.\n");
        prompt.push_str("   - Break down complex problems into smaller, manageable parts.\n");
        prompt.push_str("   - Verify assumptions at each step to avoid errors.\n");
        prompt.push_str("   - Reflect on results to improve subsequent actions.\n\n");
        prompt.push_str("2. Effective tool usage:\n");
        prompt.push_str("   - Explore:\n");
        prompt.push_str("     - Identify available information and verify its structure.\n");
        prompt.push_str("     - Check assumptions and understand data relationships.\n");
        prompt.push_str("   - Iterate:\n");
        prompt.push_str("     - Start with simple queries or actions.\n");
        prompt.push_str("     - Build upon successes, adjusting based on observations.\n");
        prompt.push_str("   - Handle errors:\n");
        prompt.push_str("     - Carefully analyze error messages.\n");
        prompt.push_str("     - Use errors as a guide to refine your approach.\n");
        prompt.push_str("     - Document what went wrong and suggest fixes.\n\n");
        prompt.push_str("3. Clear communication:\n");
        prompt.push_str("   - Explain your reasoning and decisions at each step.\n");
        prompt.push_str("   - Share discoveries transparently with the user.\n");
        prompt.push_str("   - Outline next steps or ask clarifying questions as needed.\n\n");

        prompt.push_str("EXAMPLES OF BEST PRACTICES:\n\n");
        prompt.push_str("- Working with databases:\n");
        prompt.push_str("  - Check schema before writing queries.\n");
        prompt.push_str("  - Verify the existence of columns or tables.\n");
        prompt.push_str("  - Start with basic queries and refine based on results.\n\n");
        prompt.push_str("- Processing data:\n");
        prompt.push_str("  - Validate data formats and handle edge cases.\n");
        prompt.push_str("  - Ensure integrity and correctness of results.\n\n");
        prompt.push_str("- Accessing resources:\n");
        prompt.push_str("  - Confirm resource availability and permissions.\n");
        prompt.push_str("  - Handle missing or incomplete data gracefully.\n\n");

        prompt.push_str("REMEMBER:\n");
        prompt.push_str("- Be thorough and systematic.\n");
        prompt.push_str("- Each tool call should have a clear and well-explained purpose.\n");
        prompt.push_str("- Make reasonable assumptions if ambiguous.\n");
        prompt.push_str("- Minimize unnecessary user interactions by providing actionable insights.\n\n");

        prompt.push_str("EXAMPLES OF ASSUMPTIONS:\n");
        prompt.push_str("- Default sorting (e.g., descending order) if not specified.\n");
        prompt.push_str("- Assume basic user intentions, such as fetching top results by a common metric.\n");

        prompt
    }

    pub async fn new() -> Result<Self> {
        let openai_client = match std::env::var("OPENAI_API_KEY") {
            Ok(api_key) => Some(OpenAIClient::new(api_key)),
            Err(_) => None,
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

    pub async fn start_server(&self, name: &str, command: &str, args: &[String]) -> Result<()> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

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

    async fn initialize_server(&self, name: &str) -> Result<()> {
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
        let request_str = serde_json::to_string(&request)? + "\n";
        println!("DEBUG: Sending request: {}", request_str.trim());
        
        // Create channels for stdin/stdout communication
        let (tx, mut rx) = mpsc::channel(1);
        
        // Get the server's I/O handles
        let (stdin, stdout) = {
            let servers = self.servers.lock().await;
            let server = servers.get(server_name)
                .ok_or_else(|| anyhow::anyhow!("Server not found: {}", server_name))?;
            
            (Arc::clone(&server.stdin), Arc::clone(&server.stdout))
        };

        // Write request and read response in a separate task
        tokio::spawn(async move {
            // Write request
            {
                let request_bytes = request_str.as_bytes().to_vec(); // Clone the data
                let mut stdin_guard = stdin.lock().await;
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
            let mut response_line = String::new();
            {
                let mut stdout_guard = stdout.lock().await;
                let mut reader = BufReader::new(&mut *stdout_guard);
                
                match reader.read_line(&mut response_line).await {
                    Ok(0) => {
                        let _ = tx.send(Err(anyhow::anyhow!("Server closed connection"))).await;
                    }
                    Ok(_) => {
                        println!("DEBUG: Received response: {}", response_line.trim());
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

        let response = self.send_request(server_name, request).await?;
        let tools: ListToolsResult = serde_json::from_value(response.result.unwrap_or_default())?;
        Ok(tools.tools)
    }

    pub async fn call_tool(&self, server_name: &str, tool_name: &str, args: Value) -> Result<String> {
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

    pub async fn run_cli(&self) -> Result<()> {
        println!("MCP Host CLI - Enter 'help' for commands");

        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let args: Vec<&str> = line.trim().split_whitespace().collect();
            if args.is_empty() {
                continue;
            }

            let command = args[0];
            let server_args = &args[1..];

            match command {
                "chat" => {
                    if server_args.len() != 1 {
                        println!("Usage: chat <server>");
                        continue;
                    }

                    let server_name = server_args[0];
                    match self.enter_chat_mode(server_name).await {
                        Ok(mut state) => {
                            println!("Entering chat mode. Type 'exit' or 'quit' to leave.");

                            loop {
                                let mut input = String::new();
                                std::io::stdin().read_line(&mut input)?;
                                let user_input = input.trim();
                                if user_input.eq_ignore_ascii_case("exit") || user_input.eq_ignore_ascii_case("quit") {
                                    println!("Exiting chat mode.");
                                    break;
                                }

                                state.add_user_message(user_input);

                                // Use OpenAI client if available
                                if let Some(client) = &self.openai_client {
                                    let mut builder = client.raw_builder().model("gpt-4");
                                
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
                                            state.add_assistant_message(&response);
                                            println!("Assistant: {}", response);
                                        }
                                        Err(e) => println!("Error getting response: {}", e),
                                    }
                                } else {
                                    println!("Error: OpenAI client not initialized. Set OPENAI_API_KEY environment variable.");
                                    break;
                                }
                            }
                        }
                        Err(e) => println!("Error entering chat mode: {}", e),
                    }
                }
                "help" => {
                    println!("Available commands:");
                    println!("  servers                         - List running servers");
                    println!("  start <name> <command> [args]   - Start a server");
                    println!("  stop <server>                   - Stop a server");
                    println!("  tools <server>                  - List tools for a server");
                    println!("  call <server> <tool>            - Call a tool with JSON arguments");
                    println!("  chat <server>                   - Enter interactive chat mode with a server");
                    println!("  quit                            - Exit the program");
                }
                "servers" => {
                    let servers = self.servers.lock().await;
                    println!("\nRunning servers:");
                    for (name, server) in servers.iter() {
                        println!("  {} - initialized: {}", name, server.initialized);
                    }
                    println!();
                }
                "start" => {
                    if server_args.len() < 2 {
                        println!("Usage: start <name> <command> [args...]");
                        continue;
                    }

                    let server_name = server_args[0];
                    let server_command = server_args[1];
                    let server_extra_args = server_args[2..].to_vec().into_iter().map(String::from).collect::<Vec<_>>();

                    match self.start_server(server_name, server_command, &server_extra_args).await {
                        Ok(()) => println!("Started server '{}'", server_name),
                        Err(e) => println!("Error starting server: {}", e),
                    }
                }
                "stop" => {
                    if server_args.len() != 1 {
                        println!("Usage: stop <server>");
                        continue;
                    }

                    let server_name = server_args[0];
                    match self.stop_server(server_name).await {
                        Ok(()) => println!("Stopped server '{}'", server_name),
                        Err(e) => println!("Error stopping server: {}", e),
                    }
                }
                "tools" => {
                    if server_args.len() != 1 {
                        println!("Usage: tools <server>");
                        continue;
                    }

                    let server_name = server_args[0];
                    match self.list_server_tools(server_name).await {
                        Ok(tools) => {
                            println!("\nAvailable tools for {}:", server_name);
                            for tool in tools {
                                println!("  {} - {}", tool.name, tool.description.unwrap_or_default());
                                let schema = tool.input_schema;
                                println!("    Arguments schema:");
                                println!("{}", serde_json::to_string_pretty(&schema)?
                                    .split('\n')
                                    .map(|line| format!("      {}", line))
                                    .collect::<Vec<_>>()
                                    .join("\n"));
                            }
                            println!();
                        }
                        Err(e) => println!("Error: {}", e),
                    }
                }
                "call" => {
                    if server_args.len() != 2 {
                        println!("Usage: call <server> <tool>");
                        continue;
                    }

                    let server_name = server_args[0];
                    let tool_name = server_args[1];

                    println!("Enter arguments (JSON):");
                    let mut json_input = String::new();
                    let stdin = io::stdin(); // Standard input stream
                    stdin.read_line(&mut json_input)?;

                    let args_value: Value = match serde_json::from_str(&json_input) {
                        Ok(v) => v,
                        Err(e) => {
                            println!("Invalid JSON: {}", e);
                            continue;
                        }
                    };

                    match self.call_tool(server_name, tool_name, args_value).await {
                        Ok(result) => {
                            if result.trim().is_empty() {
                                println!("\nNo results returned");
                            } else {
                                println!("\nResult:\n{}\n", result);
                            }
                        }
                        Err(e) => println!("Error calling tool: {}", e),
                    }
                }
                "quit" => break,
                _ => println!("Unknown command. Type 'help' for available commands."),
            }
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let host = MCPHost::new().await?;

    // Start the CLI loop
    host.run_cli().await?;

    // Stop all servers before exit
    let servers = host.servers.lock().await;
    for name in servers.keys() {
        let _ = host.stop_server(name).await;
    }

    Ok(())
}
