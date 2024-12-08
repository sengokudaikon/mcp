use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command as AsyncCommand};
use tokio::sync::mpsc;
use tokio::time::timeout;
use uuid::Uuid;

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
    stdout: ChildStdout,
    capabilities: Option<ServerCapabilities>,
    initialized: bool,
}

pub struct MCPHost {
    servers: Arc<Mutex<HashMap<String, ManagedServer>>>,
    client_info: Implementation,
    request_timeout: std::time::Duration,
}

impl MCPHost {
    pub async fn new() -> Result<Self> {
        Ok(Self {
            servers: Arc::new(Mutex::new(HashMap::new())),
            client_info: Implementation {
                name: "mcp-host".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            request_timeout: std::time::Duration::from_secs(30), // Default timeout
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
        let stdin = ChildStdin::from_std(child_stdin)?;

        let stdout = child.stdout.take().expect("Failed to get stdout");
        let stdout = ChildStdout::from_std(stdout)?;

        let server = ManagedServer {
            name: name.to_string(),
            process: child,
            stdin,
            stdout,
            capabilities: None,
            initialized: false,
        };

        {
            let mut servers = self.servers.lock().unwrap();
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
        let request_str = serde_json::to_string(&request)? + "\n";

        // Create channels for stdin/stdout communication
        let (tx, mut rx) = mpsc::channel(1);
        
        // Clone Arc for the spawn
        let servers = Arc::clone(&self.servers);
        let server_name = server_name.to_string();

        tokio::spawn(async move {
            // Scope the mutex guard to release it before the await
            let result = {
                let mut servers = servers.lock().unwrap();
                if let Some(server) = servers.get_mut(&server_name) {
                    // Write request
                    if let Err(e) = server.stdin.write_all(request_str.as_bytes()).await {
                        tx.send(Err(anyhow::anyhow!("Failed to write to stdin: {}", e))).await
                    } else if let Err(e) = server.stdin.flush().await {
                        tx.send(Err(anyhow::anyhow!("Failed to flush stdin: {}", e))).await
                    } else {
                        // Read response
                        let mut reader = BufReader::new(&mut server.stdout);
                        let mut response_line = String::new();
                        
                        match reader.read_line(&mut response_line).await {
                            Ok(0) => tx.send(Err(anyhow::anyhow!("Server closed connection"))).await,
                            Ok(_) => {
                                match serde_json::from_str(&response_line) {
                                    Ok(response) => tx.send(Ok(response)).await,
                                    Err(e) => tx.send(Err(anyhow::anyhow!("Failed to parse response: {}", e))).await,
                                }
                            }
                            Err(e) => tx.send(Err(anyhow::anyhow!("Failed to read response: {}", e))).await,
                        }
                    }
                } else {
                    tx.send(Err(anyhow::anyhow!("Server not found: {}", server_name))).await
                }
            };

            if let Err(e) = result {
                eprintln!("Failed to send response through channel: {}", e);
            }
            Ok(()) as Result<(), tokio::sync::mpsc::error::SendError<_>>
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
                    match content.ctype.as_str() {
                        "text" => {
                            output.push_str(&content.text);
                            output.push('\n');
                        }
                        _ => {
                            output.push_str(&format!("Unknown content type: {}\n", content.ctype));
                        }
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

        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let args: Vec<&str> = line.trim().split_whitespace().collect();
            if args.is_empty() {
                continue;
            }

            let command = args[0];
            let server_args = &args[1..];

            match command {
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
                                if let Some(schema) = serde_json::to_string_pretty(&tool.input_schema).ok() {
                                    println!("    Arguments: {}", serde_json::to_string_pretty(&schema)?);
                                }
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
