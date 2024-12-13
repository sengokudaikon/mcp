use std::process::{Command, Stdio};
use std::collections::HashMap;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shared_protocol_objects::{CallToolParams, CallToolResult, ToolInfo, ToolResponseContent, JsonRpcResponse, success_response, error_response, INTERNAL_ERROR};

#[derive(Debug, Deserialize)]
struct GitParams {
    action: String,
    #[serde(default)]
    repo_path: Option<String>,
    #[serde(default)]
    files: Option<Vec<String>>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    max_count: Option<usize>,
}

/// Execute a command in `repo_path` directory, capturing stdout and stderr.
fn run_git_command(repo_path: &str, args: &[&str]) -> Result<(String, String)> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !output.status.success() {
        return Err(anyhow!(format!("Git command failed: {}\nSTDERR: {}", args.join(" "), stderr)));
    }

    Ok((stdout.trim().to_string(), stderr.trim().to_string()))
}

/// Ensure the repo at `repo_path` is initialized.
fn ensure_repo_initialized(repo_path: &str) -> Result<()> {
    if !std::path::Path::new(&format!("{}/.git", repo_path)).exists() {
        run_git_command(repo_path, &["init"])?;
    }
    Ok(())
}

/// Handle calls to the `git` tool.
pub async fn handle_git_tool_call(params: CallToolParams, id: Option<Value>) -> Result<JsonRpcResponse> {
    let git_params: GitParams = serde_json::from_value(params.arguments).map_err(|e| anyhow!(e))?;
    let action = git_params.action.as_str();

    // Default repo path if none provided
    let repo_path = git_params.repo_path.as_deref().unwrap_or("./repo");

    // Ensure repo is initialized for any action except init_repo
    if action != "init_repo" {
        ensure_repo_initialized(repo_path)?;
    }

    let result = match action {
        "init_repo" => {
            ensure_repo_initialized(repo_path)?;
            "Repository initialized.".to_string()
        }
        "add_files" => {
            let files = git_params.files.ok_or_else(|| anyhow!("Missing 'files' parameter"))?;
            let mut args = vec!["add"];
            for f in &files {
                args.push(f);
            }
            run_git_command(repo_path, &args)?;
            "Files added.".to_string()
        }
        "commit_changes" => {
            let msg = git_params.message.as_deref().ok_or_else(|| anyhow!("Missing 'message' parameter"))?;
            run_git_command(repo_path, &["commit", "-m", msg])?;
            "Changes committed.".to_string()
        }
        "undo_last_commit" => {
            run_git_command(repo_path, &["reset", "HEAD~1"])?;
            "Last commit undone (changes are still in working directory).".to_string()
        }
        "get_status" => {
            let (stdout, _) = run_git_command(repo_path, &["status", "--short"])?;
            if stdout.is_empty() {
                "No changes.".to_string()
            } else {
                stdout
            }
        }
        "get_log" => {
            let count = git_params.max_count.unwrap_or(5);
            let (stdout, _) = run_git_command(repo_path, &["log", &format!("--max-count={}", count), "--pretty=format:%H %s"])?;
            if stdout.is_empty() {
                "No commits yet.".to_string()
            } else {
                stdout
            }
        }
        "push_changes" => {
            let remote = git_params.remote.as_deref().unwrap_or("origin");
            let branch = git_params.branch.as_deref().unwrap_or("main");
            run_git_command(repo_path, &["push", remote, branch])?;
            "Changes pushed successfully.".to_string()
        }
        _ => {
            return Ok(error_response(id, INTERNAL_ERROR, &format!("Unknown action '{}'", action)));
        }
    };

    let tool_res = CallToolResult {
        content: vec![ToolResponseContent {
            type_: "text".into(),
            text: result,
            annotations: None,
        }],
        is_error: None,
        _meta: None,
        progress: None,
        total: None,
    };

    Ok(success_response(id, serde_json::to_value(tool_res)?))
}


pub fn git_tool_info() -> shared_protocol_objects::ToolInfo {
    shared_protocol_objects::ToolInfo {
        name: "git".to_string(),
        description: Some(
            "Interacts with a Git repository. Supports essential Git operations.

             **When to Use:**
            - When you need to manage the state of a project or file using Git.
            - When you need to stage, commit, push changes to a repository.
            - To view the current status of files, commits or branches.

             **Actions:**
            - `init_repo`: Initializes a Git repository in the specified directory.
                -  params: `repo_path` (the location for the new repo), optional, default is './repo'
            - `add_files`: Stages specified files to be included in the next commit.
                - params: `files` (an array of file paths).
            - `commit_changes`: Commits the staged changes with a provided message.
                - params: `message` (the commit message).
            - `undo_last_commit`: Undoes the most recent commit, but preserves working directory changes.
               - params: None.
            - `get_status`: Retrieves the current status of the repository.
               - params: None.
            - `get_log`: Retrieves a history of the commits in the repository.
               - params: `max_count` (optional, the number of recent commits, default is 5).
            - `push_changes`: Pushes all committed changes to the specified remote and branch.
               -  params: `remote` (optional, the target remote, default is 'origin') and `branch` (optional, the target branch, default is 'main').

            **Output:**
            - The output includes messages that describe the execution of the provided action.
            - It will show error messages, if the action was not successful.
            -  It includes the git log, or the current status of tracked or untracked files.

            **Usage Constraints:**
            - All paths are relative to the running environment.
            - Always make sure the repo_path is correct.
            - Always check the status and log after running any git action.
            - Do not push if the code is not fully tested and reviewed.
            - Do not use if you are unsure about the git command that will be performed.
           ".to_string()
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["init_repo","add_files","commit_changes","undo_last_commit","get_status","get_log","push_changes"],
                    "description": "The git action to perform."
                },
                "repo_path": {
                    "type": "string",
                    "description": "Path to the git repository directory (default './repo')."
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Files to add (for 'add_files' action)."
                },
                "message": {
                    "type": "string",
                    "description": "Commit message (for 'commit_changes')."
                },
                "remote": {
                    "type": "string",
                    "description": "Remote name for push (default 'origin')."
                },
                "branch": {
                    "type": "string",
                    "description": "Branch name for push (default 'main')."
                },
                "max_count": {
                    "type": "integer",
                    "description": "Number of commits to retrieve for 'get_log' (default: 5)."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }
}
