use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::process::Command;
use serde_json::json;

use shared_protocol_objects::ToolInfo;

#[derive(Debug, Serialize, Deserialize)]
pub struct BashParams {
    pub command: String,
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
            description: Some("Execute bash shell commands".to_string()),
            input_schema: json!({})
        }
    }

    pub async fn execute(&self, params: BashParams) -> Result<BashResult> {
        let output = Command::new("sh")
            .arg("-c")
            .arg(&params.command)
            .output()?;

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
                }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
    }
}

