use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Serialize, Deserialize)]
pub struct BashParams {
    pub command: String,
}

pub struct BashExecutor;

impl BashExecutor {
    pub fn new() -> Self {
        BashExecutor
    }

    pub fn execute(&self, params: BashParams) -> Result<String> {
        let output = Command::new("sh")
            .arg("-c")
            .arg(&params.command)
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout)
        } else {
            Err(anyhow::anyhow!("Command failed: {}", stderr))
        }
    }
}
