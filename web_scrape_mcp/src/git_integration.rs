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

/// Return the ToolInfo definition for the git tool.
pub fn graph_tool_info() -> shared_protocol_objects::ToolInfo {
    shared_protocol_objects::ToolInfo {
        name: "graph_tool".into(),
        description: Some(
            "PROACTIVE USAGE INSTRUCTIONS:\n\
            1. CONTINUOUSLY RECORD USER INFORMATION during conversations without explicit commands\n\
            2. CREATE NODES for any revealed:\n\
               - Personal details\n\
               - Preferences\n\
               - Experiences\n\
               - Opinions\n\
               - Goals\n\
               - Problems\n\
               - Relationships\n\
            3. UPDATE EXISTING NODES when new information emerges\n\
            4. CONNECT RELATED INFORMATION as it's discovered\n\n\
            ALWAYS RUN get_top_tags, get_tags_by_date, and get_most_connected NEAR THE START OF YOUR CHATS WITH THE USER AND KEEP THE GRAPH UP TO DATE AS BEST AS YOU CAN. THIS IS VERY IMPORTANT.\n\
            BEFORE ASKING THE USER QUESTIONS, SEARCH THE GRAPH USING search_nodes AND get_nodes_by_tag TO DISCOVER WHAT YOU ALREADY KNOW ABOUT THEM!\n\n\
            Core Functions:\n\
            - Track relationships and connections\n\
            - Record preferences and interests\n\
            - Log life events and milestones\n\
            - Document work and projects\n\
            - Monitor goals and progress\n\
            - Build interaction history\n\
            - Map skill development\n\
            - Note behavioral patterns\n\
            - Store decision history\n\
            - Record communication preferences\n\
            - Track problem-solving approaches\n\
            - Map professional networks\n\
            - Document tools and workflows\n\
            - Store scheduling patterns\n\
            - Track information sources\n\
            - Log important dates\n\
            - Monitor routines\n\n\
            USAGE PATTERN:\n\
            1. START CONVERSATIONS by checking existing knowledge\n\
            2. LISTEN ACTIVELY for new information\n\
            3. STORE INFORMATION IMMEDIATELY as it's shared\n\
            4. CONNECT new information to existing knowledge\n\
            5. USE stored information to personalize responses\n\n\
            SEARCH STRATEGY:\n\
            1. Use search_nodes with relevant keywords\n\
            2. Use get_nodes_by_tag for categorized info\n\
            3. Use get_children to explore connections\n\
            4. Use get_most_connected and get_top_tags for patterns\n\n\
            REMEMBER: Don't wait for commands - actively maintain the user's knowledge graph during natural conversation."
        .into()),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create_root", "create_node", "update_node", "delete_node", "move_node", "connect_nodes", "get_node", "get_children", "get_nodes_by_tag", "search_nodes", "get_most_connected", "get_top_tags", "get_recent_nodes", "get_tags_by_date"],
                    "description": "The action to perform on the graph"
                },
                "params": {
                    "type": "object",
                    "description": "Parameters for the action.",
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "description": {"type": "string"},
                                "content": {"type": "string"},
                                "parent_name": {"type": "string"},
                                "relation": {"type": "string"},
                                "tags": {"type": "array", "items": {"type": "string"}},
                                "metadata": {"type": "object", "additionalProperties": {"type": "string"}}
                            },
                            "required": ["name", "description", "content"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "node_name": {"type": "string"},
                                "new_name": {"type": "string"},
                                "new_description": {"type": "string"},
                                "new_content": {"type": "string"},
                                "new_tags": {"type": "array", "items": {"type": "string"}},
                                "new_metadata": {"type": "object", "additionalProperties": {"type": "string"}}
                            },
                            "required": ["node_name"]
                        },
                        {
                            "type": "object",
                            "title": "MoveNodeParams",
                            "properties": {
                                "node_name": { "type": "string", "description": "Name of the node to move" },
                                "new_parent_name": { "type": "string", "description": "Name of the new parent node" },
                                "new_relation": { "type": "string", "description": "New relationship type to the parent" }
                            },
                            "required": ["node_name", "new_parent_name", "new_relation"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "node_name": {"type": "string"}
                            },
                            "required": ["node_name"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "from_node_name": {"type": "string"},
                                "to_node_name": {"type": "string"},
                                "relation": {"type": "string"}
                            },
                            "required": ["from_node_name", "to_node_name", "relation"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "node_name": {"type": "string"}
                            },
                            "required": ["node_name"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "parent_node_name": {"type": "string"}
                            },
                            "required": ["parent_node_name"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "tag": {"type": "string"}
                            },
                            "required": ["tag"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "query": {"type": "string"}
                            },
                            "required": ["query"]
                        }
                    ]
                }
            },
            "required": ["action", "params"]
        }),
    }
}

pub fn git_tool_info() -> shared_protocol_objects::ToolInfo {
    shared_protocol_objects::ToolInfo {
        name: "git".to_string(),
        description: Some("Interact with a Git repository. Supports init_repo, add_files, commit_changes, undo_last_commit, get_status, get_log, push_changes.".to_string()),
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
