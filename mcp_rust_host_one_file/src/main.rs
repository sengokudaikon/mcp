use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::mpsc;
use uuid::Uuid;
use serde_json::json;


use shared_protocol_objects::{
    ClientCapabilities, JsonRpcRequest, JsonRpcResponse, ServerCapabilities, Implementation, Tool, ToolContent,
    LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS, ToolInfo, CallToolParams, CallToolResult,RequestId, ListToolsResult, ToolResponseContent,
    success_response, error_response,
};

// Server Management Types
#[derive(Debug)]
struct ManagedServer {
    name: String,
    process: Child,
    stdin: ChildStdin,
    stdout: tokio::process::ChildStdout, // Added `stdout` field
    capabilities: Option<ServerCapabilities>,
    initialized: bool,
}

pub struct MCPHost {
    servers: Arc<Mutex<HashMap<String, ManagedServer>>>,
    client_info: Implementation,
}

impl MCPHost {
    pub async fn new() -> Result<Self> {
        Ok(Self {
            servers: Arc::new(Mutex::new(HashMap::new())),
            client_info: Implementation {
                name: "mcp-host".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        })
    }

    pub async fn start_server(&self, name: &str, command: &str, args: &[String]) -> Result<()> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = tokio::process::ChildStdin::from_std(stdin)?;

        let stdout = child.stdout.take().expect("Failed to get stdout");


        let server = ManagedServer {
            name: name.to_string(),
            process: child,
            stdin: tokio::process::ChildStdin::from_std(stdin)?,
            stdout: tokio::process::ChildStdout::from_std(stdout)?,
            capabilities: None,
            initialized: false,
        };

        let mut servers = self.servers.lock().unwrap();
        servers.insert(name.to_string(), server);

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
            let mut servers = self.servers.lock().unwrap();
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
        let server = {
            let mut servers = self.servers.lock().unwrap();
            servers.get_mut(server_name).context("Server not found")?
        };
    
        // Send request via stdin
        let request_str = serde_json::to_string(&request)? + "\n";
        server.stdin.write_all(request_str.as_bytes()).await?;
    
        // Read response from stdout
        let mut reader = BufReader::new(&mut server.stdout);
        let mut response_line = String::new();
        reader.read_line(&mut response_line).await?;
    
        let response: JsonRpcResponse = serde_json::from_str(&response_line)?;
        Ok(response)
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
                ToolResponseContent::Text { text } => {
                    output.push_str(&text);
                    output.push('\n');
                }
                ToolResponseContent::Resource { resource } => {
                    output.push_str(&format!("Resource: {}\n", resource.uri));
                    if let Some(text) = resource.text {
                        output.push_str(&text);
                        output.push('\n');
                    }
                }
            }
        }

        Ok(output)
    }

    pub async fn stop_server(&self, name: &str) -> Result<()> {
        let mut servers = self.servers.lock().unwrap();
        if let Some(mut server) = servers.remove(name) {
            server.process.kill()?;
        }
        Ok(())
    }

    pub async fn run_cli(&self) -> Result<()> {
        println!("MCP Host CLI - Enter 'help' for commands");
        
        let mut input = String::new();
        loop {
            print!("> ");
            io::stdout().flush()?;
            
            input.clear();
            io::stdin().read_line(&mut input)?;
            
            let args: Vec<&str> = input.trim().split_whitespace().collect();
            if args.is_empty() {
                continue;
            }

            match args[0] {
                "help" => {
                    println!("Available commands:");
                    println!("  servers                         - List running servers");
                    println!("  start <name> <command> [args]   - Start a server");
                    println!("  stop <server>                   - Stop a server");
                    println!("  tools <server>                  - List tools for a server");
                    println!("  call <server> <tool>            - Call a tool with JSON arguments");
                    println!("  quit                            - Exit the program");
                }
                "servers" => {
                    let servers = self.servers.lock().unwrap();
                    println!("\nRunning servers:");
                    for (name, server) in servers.iter() {
                        println!("  {} - initialized: {}", name, server.initialized);
                    }
                    println!();
                }
                "start" => {
                    if args.len() < 3 {
                        println!("Usage: start <name> <command> [args...]");
                        continue;
                    }
                    
                    let server_args = args[3..].to_vec().into_iter().map(String::from).collect::<Vec<_>>();
                    match self.start_server(args[1], args[2], &server_args).await {
                        Ok(()) => println!("Started server '{}'", args[1]),
                        Err(e) => println!("Error starting server: {}", e),
                    }
                }
                "stop" => {
                    if args.len() != 2 {
                        println!("Usage: stop <server>");
                        continue;
                    }
                    
                    match self.stop_server(args[1]).await {
                        Ok(()) => println!("Stopped server '{}'", args[1]),
                        Err(e) => println!("Error stopping server: {}", e),
                    }
                }
                "tools" => {
                    if args.len() != 2 {
                        println!("Usage: tools <server>");
                        continue;
                    }
                    
                    match self.list_server_tools(args[1]).await {
                        Ok(tools) => {
                            println!("\nAvailable tools for {}:", args[1]);
                            for tool in tools {
                                println!("  {} - {}", tool.name, tool.description.unwrap_or_default());
                                if let Some(schema) = tool.inputSchema {
                                    println!("    Arguments: {}", serde_json::to_string_pretty(&schema)?);
                                }
                            }
                            println!();
                        }
                        Err(e) => println!("Error: {}", e),
                    }
                }
                "call" => {
                    if args.len() != 3 {
                        println!("Usage: call <server> <tool>");
                        continue;
                    }

                    print!("Enter arguments (JSON): ");
                    io::stdout().flush()?;
                    
                    let mut json_input = String::new();
                    io::stdin().read_line(&mut json_input)?;
                    
                    let args_value: Value = match serde_json::from_str(&json_input) {
                        Ok(v) => v,
                        Err(e) => {
                            println!("Invalid JSON: {}", e);
                            continue;
                        }
                    };

                    match self.call_tool(args[1], args[2], args_value).await {
                        Ok(result) => println!("\nResult:\n{}\n", result),
                        Err(e) => println!("Error: {}", e),
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
    let servers = host.servers.lock().unwrap();
    for name in servers.keys() {
        let _ = host.stop_server(name).await;
    }

    Ok(())
}