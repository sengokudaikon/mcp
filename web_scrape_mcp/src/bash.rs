use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::process::Command;

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
            description: "Execute bash shell commands".to_string(),
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

#[derive(Debug)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
}
