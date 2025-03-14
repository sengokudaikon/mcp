use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::process::Command;
use serde_json::json;

use shared_protocol_objects::ToolInfo;

#[derive(Debug, Serialize, Deserialize)]
pub struct BashParams {
    pub command: String,
    #[serde(default = "default_cwd")]
    pub cwd: String,
}

fn default_cwd() -> String {
    std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("/"))
        .to_string_lossy()
        .to_string()
}

#[derive(Debug)]
pub struct BashResult {
    pub success: bool,
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub struct BashExecutor;

impl BashExecutor {
    pub fn new() -> Self {
        BashExecutor
    }

    pub fn tool_info(&self) -> ToolInfo {
        ToolInfo {
            name: "bash".to_string(),
            description: Some(
                "Executes bash shell commands on the host system. Use this tool to:
                
                1. Run system commands and utilities
                2. Check file/directory status
                3. Process text/data with command line tools
                4. Manage files and directories
                
                Important notes:
                - Always provide the full command including any required flags
                - Use absolute paths or specify working directory (cwd)
                - Commands run with the same permissions as the host process
                - Output is limited to stdout/stderr (no interactive prompts)
                - Commands run in a non-interactive shell (sh)".to_string()
            ),
            input_schema: json!({})
        }
    }

    pub async fn execute(&self, params: BashParams) -> Result<BashResult> {
        // Create working directory if it doesn't exist
        let cwd = std::path::PathBuf::from(&params.cwd);
        if !cwd.exists() {
            std::fs::create_dir_all(&cwd)?;
        }

        let output = Command::new("sh")
            .arg("-c")
            .arg(&params.command)
            .current_dir(&cwd)
            .output()?;

        // Check if there were permission issues
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("permission denied") {
                return Err(anyhow::anyhow!("Permission denied. Try running with appropriate permissions or in a different directory."));
            }
        }

        Ok(BashResult {
            success: output.status.success(),
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

pub fn bash_tool_info() -> ToolInfo {
    ToolInfo {
        name: BashExecutor::new().tool_info().name,
        description: BashExecutor::new().tool_info().description,
        input_schema: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                },
                "cwd": {
                    "type": "string",
                    "description": "The working directory for the command"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QuickBashParams {
    pub cmd: String,
}

pub fn quick_bash_tool_info() -> ToolInfo {
    ToolInfo {
        name: "quick_bash".to_string(),
        description: Some(
            "Fast shell command tool for simple one-liners. Use this to:
            
            1. Run quick system checks (ls, ps, grep, find, etc.)
            2. View file contents (cat, head, tail, less)
            3. Create, move, or delete files and directories
            4. Process text with utilities like grep, sed, awk
            
            Advantages over regular bash tool:
            - Streamlined interface for common commands
            - Optimized for one-line operations
            - Focuses on readable command output
            - Perfect for file system operations and text processing
            
            Example commands:
            - `ls -la /path/to/dir`
            - `grep -r \"pattern\" /search/path`
            - `find . -name \"*.txt\" -mtime -7`
            - `cat file.txt | grep pattern | wc -l`
            - `du -sh /path/to/dir`
            
            Note: Commands run with your current user permissions.".to_string()
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "cmd": {
                    "type": "string",
                    "description": "The shell command to execute"
                }
            },
            "required": ["cmd"],
            "additionalProperties": false
        }),
    }
}

pub async fn handle_quick_bash(params: QuickBashParams) -> Result<BashResult> {
    let executor = BashExecutor::new();
    
    // Convert the quick bash params to regular bash params
    let bash_params = BashParams {
        command: params.cmd,
        cwd: default_cwd(),  // Always use the current working directory
    };
    
    // Execute the command using the existing executor
    executor.execute(bash_params).await
}
