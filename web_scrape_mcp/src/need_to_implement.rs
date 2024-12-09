use std::fs;
use std::collections::HashMap;
use serde::{Serialize, Deserialize}; 

pub const DEFAULT_GRAPH_DIR: &str = "/tmp/knowledge_graphs";
use petgraph::Graph;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use serde_json::{json, Value};
use anyhow::{Result, anyhow};
use shared_protocol_objects::{
    JsonRpcResponse,
    CallToolParams, CallToolResult,
    ToolResponseContent,
    success_response, error_response, INVALID_PARAMS, INTERNAL_ERROR
};

#[derive(Serialize, Deserialize, Clone)]
struct DataNode {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    metadata: HashMap<String, String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    quotes: Vec<String>,
    #[serde(default)]
    child_nodes: Vec<String>,
    #[serde(default = "chrono::Utc::now")]
    date_created: chrono::DateTime<chrono::Utc>,
    #[serde(default = "chrono::Utc::now")]
    date_modified: chrono::DateTime<chrono::Utc>,
}

impl DataNode {
    fn new(name: String, description: String, content: String) -> Self {
        let now = chrono::Utc::now();
        DataNode {
            name,
            description,
            content,
            metadata: HashMap::new(),
            tags: Vec::new(),
            quotes: Vec::new(),
            child_nodes: Vec::new(),
            date_created: now,
            date_modified: now,
        }
    }
}

// Custom serialization for Graph
// We'll use petgraph's built-in serde support instead of custom impls

#[derive(Serialize, Deserialize)]
struct SerializableGraph {
    nodes: Vec<(NodeIndex, DataNode)>,
    edges: Vec<(NodeIndex, NodeIndex, String)>
}

#[derive(Clone)]
pub struct GraphManager {
    graph: Graph<DataNode, String>, 
    root: Option<NodeIndex>,
    path: std::path::PathBuf,
}

impl GraphManager {
    fn node_name_exists(&self, name: &str) -> bool {
        self.graph.node_indices().any(|idx| {
            self.graph.node_weight(idx)
                .map(|node| node.name == name)
                .unwrap_or(false)
        })
    }

    pub fn new(filename: String) -> Self {
        // Get graph directory from env var or use default
        let graph_dir = std::env::var("KNOWLEDGE_GRAPH_DIR")
            .unwrap_or_else(|_| DEFAULT_GRAPH_DIR.to_string());
        
        // Create directory if it doesn't exist
        std::fs::create_dir_all(&graph_dir)
            .expect("Failed to create knowledge graph directory");
        
        // Build absolute path for graph file
        let path = std::path::PathBuf::from(graph_dir).join(filename);
        
        // Try loading existing graph first
        let graph = if path.exists() {
            //debug!("Found existing graph file at {}", path.display());
            let serializable = match fs::read_to_string(&path) {
                Ok(data) => {
                    //debug!("Successfully loaded graph data");
                    match serde_json::from_str(&data) {
                        Ok(s) => {
                            //debug!("Successfully parsed graph data");
                            s
                        }
                        Err(_e) => {
                            //debug!("Failed to parse graph data: {}", e);
                            SerializableGraph { 
                                nodes: vec![], 
                                edges: vec![] 
                            }
                        }
                    }
                }
                Err(_e) => {
                    //debug!("Failed to read graph file: {}", e);
                    SerializableGraph { 
                        nodes: vec![], 
                        edges: vec![] 
                    }
                }
            };
            
            let mut graph = Graph::new();
            // Restore nodes
            for (idx, node) in serializable.nodes {
                while graph.node_count() <= idx.index() {
                    graph.add_node(DataNode::new(
                        String::new(),
                        String::new(),
                        String::new()
                    ));
                }
                graph[idx] = node;
            }
            // Restore edges
            for (from, to, weight) in serializable.edges {
                graph.add_edge(from, to, weight);
            }
            graph
        } else {
            Graph::new()
        };
        
        let root = graph.node_indices().find(|&i| {
            graph.edges_directed(i, petgraph::Direction::Incoming).count() == 0
        });
        Self { graph, root, path: path.to_owned() }
    }
    

    async fn save(&self) -> Result<()> {
        //debug!("Starting save operation to {}", self.path.display());
        // Convert to serializable format with better error handling
        let serializable = SerializableGraph {
            nodes: self.graph.node_indices()
                .map(|idx| (idx, self.graph[idx].clone()))
                .collect(),
            edges: self.graph.edge_indices()
                .map(|idx| {
                    let (a, b) = self.graph.edge_endpoints(idx)
                        .ok_or_else(|| anyhow!("Invalid edge index {}", idx.index()))?;
                    Ok((a, b, self.graph[idx].clone()))
                })
                .collect::<Result<Vec<_>>>()?,
        };
        let json = serde_json::to_string(&serializable)
            .map_err(|e| anyhow!("Failed to serialize graph: {}", e))?;
        // //debug!("Writing graph data...");
        tokio::fs::write(&self.path, json).await
            .map_err(|e| anyhow!("Failed to write graph file {}: {}", self.path.display(), e))?;
        // //debug!("Graph save completed successfully");
        Ok(())
    }

    async fn create_root(&mut self, node: DataNode) -> Result<NodeIndex> {
        if self.root.is_some() {
            let connected_nodes = self.get_most_connected_nodes(10)
                .iter()
                .map(|(_, node, count)| format!("- {} ({} connections)", node.name, count))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(anyhow!("Root already exists.\n\nMost connected nodes for reference:\n{}", connected_nodes));
        }
        if self.node_name_exists(&node.name) {
            let connected_nodes = self.get_most_connected_nodes(10)
                .iter()
                .map(|(_, node, count)| format!("- {} ({} connections)", node.name, count))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(anyhow!("A node with this name already exists.\n\nMost connected nodes for reference:\n{}", connected_nodes));
        }
        let idx = self.graph.add_node(node);
        self.root = Some(idx);
        self.save().await?;
        Ok(idx)
    }

    async fn create_connected_node(&mut self, node: DataNode, parent: NodeIndex, rel: String) -> Result<NodeIndex> {
        if !self.graph.node_weight(parent).is_some() {
            return Err(anyhow!("Parent node index {} not found in graph", parent.index()));
        }
        if self.node_name_exists(&node.name) {
            let connected_nodes = self.get_most_connected_nodes(10)
                .iter()
                .map(|(_, node, count)| format!("- {} ({} connections)", node.name, count))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(anyhow!("Node with name '{}' already exists.\n\nMost connected nodes for reference:\n{}", node.name, connected_nodes));
        }
        
        // Add node to graph
        let idx = self.graph.add_node(node.clone());
        self.graph.add_edge(parent, idx, rel.clone());
        
        // Update parent's child list
        self.update_parent_child_list(parent);
        
        self.save().await.map_err(|e| anyhow!(
            "Failed to save graph after adding node '{}' with relation '{}' to parent {}: {}", 
            node.name, rel, parent.index(), e
        ))?;
        Ok(idx)
    }

    async fn update_node(&mut self, idx: NodeIndex, node: DataNode) -> Result<()> {
        // Only check for name uniqueness if the name is actually changing
        if let Some(current) = self.graph.node_weight(idx) {
            if current.name != node.name && self.node_name_exists(&node.name) {
                return Err(anyhow!("A node with this name already exists"));
            }
        }
        if let Some(n) = self.graph.node_weight_mut(idx) {
            let date_created = n.date_created; // Preserve original creation date
            *n = node;
            n.date_created = date_created; // Restore creation date
            n.date_modified = chrono::Utc::now();
            self.save().await?;
        }
        Ok(())
    }

    async fn move_node(&mut self, node_idx: NodeIndex, new_parent_idx: NodeIndex, new_relation: String) -> Result<()> {
        // Get current parent
        let incoming: Vec<_> = self.graph.neighbors_directed(node_idx, petgraph::Direction::Incoming).collect();
        
        // Remove edge from old parent
        if let Some(old_parent_idx) = incoming.first() {
            // Collect edge IDs first to avoid borrowing conflict
            let edge_ids: Vec<_> = self.graph
                .edges_connecting(*old_parent_idx, node_idx)
                .map(|e| e.id())
                .collect();
        
            // Then remove edges using collected IDs
            for edge_id in edge_ids {
                self.graph.remove_edge(edge_id);
            }
            
            // Update old parent's child list
            self.update_parent_child_list(*old_parent_idx);
        }

        // Add edge to new parent
        self.graph.add_edge(new_parent_idx, node_idx, new_relation);
        
        // Update new parent's child list
        self.update_parent_child_list(new_parent_idx);

        self.save().await?;
        Ok(())
    }

    async fn delete_node(&mut self, idx: NodeIndex) -> Result<()> {
        if Some(idx) == self.root {
            return Err(anyhow!("Cannot delete root node"));
        }

        let node_name = self.graph.node_weight(idx)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| format!("index {}", idx.index()));

        let neighbors: Vec<_> = self.graph.neighbors(idx).collect();
        let incoming: Vec<_> = self.graph.neighbors_directed(idx, petgraph::Direction::Incoming).collect();
        
        if neighbors.len() > 1 || incoming.len() > 1 {
            return Err(anyhow!(
                "Cannot delete node '{}' with multiple connections (has {} outgoing and {} incoming)", 
                node_name, neighbors.len(), incoming.len()
            ));
        }

        // Update parent's child_nodes list
        if let Some(parent_idx) = incoming.first() {
            // First remove the node
            self.graph.remove_node(idx)
                .ok_or_else(|| anyhow!("Failed to remove node '{}' from graph", node_name))?;
            
            // Then update parent's child list
            self.update_parent_child_list(*parent_idx);
        } else {
            self.graph.remove_node(idx)
                .ok_or_else(|| anyhow!("Failed to remove node '{}' from graph", node_name))?;
        }

        self.graph.remove_node(idx)
            .ok_or_else(|| anyhow!("Failed to remove node '{}' from graph", node_name))?;
            
        self.save().await.map_err(|e| anyhow!(
            "Failed to save graph after deleting node '{}': {}", 
            node_name, e
        ))?;
        Ok(())
    }
    

    fn get_node(&self, idx: NodeIndex) -> Option<&DataNode> {
        self.graph.node_weight(idx)
    }

    async fn connect(&mut self, from: NodeIndex, to: NodeIndex, rel: String) -> Result<()> {
        self.graph.add_edge(from, to, rel);
        Ok(self.save().await?)
    }

    // Method to get a node by its name
    fn get_node_by_name(&self, name: &str) -> Option<(NodeIndex, &DataNode)> {
        self.graph.node_indices()
            .find_map(|idx| {
                self.graph.node_weight(idx).and_then(|node| {
                    if node.name == name {
                        Some((idx, node))
                    } else {
                        None
                    }
                })
            })
    }

    // Method to get all immediate children of a node
    fn get_children(&self, parent: NodeIndex) -> Vec<(NodeIndex, &DataNode, String)> {
        let mut children = Vec::new();
        // Get actual children from graph edges
        for edge in self.graph.edges(parent) {
            let child_idx = edge.target();
            if let Some(child_node) = self.graph.node_weight(child_idx) {
                children.push((child_idx, child_node, edge.weight().clone()));
            }
        }
        children
    }

    fn update_parent_child_list(&mut self, parent: NodeIndex) {
        let children = self.get_children(parent);
        let actual_child_names: Vec<String> = children.iter()
            .map(|(_, node, _)| node.name.clone())
            .collect();
            
        if let Some(parent_node) = self.graph.node_weight_mut(parent) {
            if parent_node.child_nodes != actual_child_names {
                parent_node.child_nodes = actual_child_names;
                parent_node.date_modified = chrono::Utc::now();
                // Schedule save for next event loop iteration
                let graph_manager = (*self).clone();
                tokio::spawn(async move {
                    if let Err(e) = graph_manager.save().await {
                        eprintln!("Failed to save graph after updating child_nodes: {}", e);
                    }
                });
            }
        }
    }

    // Method to get all nodes matching a tag
    fn get_nodes_by_tag(&self, tag: &str) -> Vec<(NodeIndex, &DataNode)> {
        self.graph.node_indices()
            .filter_map(|idx| {
                self.graph.node_weight(idx).and_then(|node| {
                    if node.tags.contains(&tag.to_string()) {
                        Some((idx, node))
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    // Method to get all nodes with names or descriptions matching a query string
    fn search_nodes(&self, query: &str) -> Vec<(NodeIndex, &DataNode)> {
        self.graph.node_indices()
            .filter_map(|idx| {
                self.graph.node_weight(idx).and_then(|node| {
                    if node.name.contains(query) || node.description.contains(query) {
                        Some((idx, node))
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    fn get_most_connected_nodes(&self, limit: usize) -> Vec<(NodeIndex, &DataNode, usize)> {
        let mut nodes: Vec<_> = self.graph.node_indices()
            .filter_map(|idx| {
                self.graph.node_weight(idx).map(|node| {
                    // Count both incoming and outgoing edges
                    let edge_count = self.graph.edges_directed(idx, petgraph::Direction::Incoming).count() +
                                   self.graph.edges_directed(idx, petgraph::Direction::Outgoing).count();
                    (idx, node, edge_count)
                })
            })
            .collect();
        
        // Sort by edge count in descending order
        nodes.sort_by(|a, b| b.2.cmp(&a.2));
        nodes.truncate(limit);
        nodes
    }

    fn get_recent_nodes(&self, limit: usize) -> Vec<(NodeIndex, &DataNode)> {
        let mut nodes: Vec<_> = self.graph.node_indices()
            .filter_map(|idx| {
                self.graph.node_weight(idx).map(|node| (idx, node))
            })
            .collect();
        
        // Sort by modified date in descending order
        nodes.sort_by(|a, b| b.1.date_modified.cmp(&a.1.date_modified));
        nodes.truncate(limit);
        nodes
    }

    fn get_top_tags(&self, limit: usize) -> Vec<(String, usize)> {
        // Create a HashMap to count tag occurrences
        let mut tag_counts: HashMap<String, usize> = HashMap::new();
        
        // Count occurrences of each tag
        for node in self.graph.node_weights() {
            for tag in &node.tags {
                *tag_counts.entry(tag.clone()).or_insert(0) += 1;
            }
        }
        
        // Convert to vector and sort by count
        let mut tag_vec: Vec<_> = tag_counts.into_iter().collect();
        tag_vec.sort_by(|a, b| b.1.cmp(&a.1));
        tag_vec.truncate(limit);
        tag_vec
    }

    fn get_tags_by_date(&self, limit: usize) -> (Vec<(String, chrono::DateTime<chrono::Utc>)>, Vec<(String, chrono::DateTime<chrono::Utc>)>) {
        let mut tag_dates: HashMap<String, Vec<chrono::DateTime<chrono::Utc>>> = HashMap::new();
        
        // Collect all dates for each tag
        for node in self.graph.node_weights() {
            for tag in &node.tags {
                tag_dates.entry(tag.clone())
                    .or_default()
                    .push(node.date_created);
            }
        }

        // Convert to vectors with earliest/latest date per tag
        let mut tag_vec: Vec<_> = tag_dates.into_iter()
            .map(|(tag, mut dates)| {
                dates.sort();
                (tag, dates[0], *dates.last().unwrap())
            })
            .collect();

        // Get recent tags (sorted by newest date)
        let mut recent_tags = tag_vec.clone();
        recent_tags.sort_by(|a, b| b.2.cmp(&a.2));
        let recent = recent_tags.into_iter()
            .take(limit)
            .map(|(tag, _, date)| (tag, date))
            .collect();

        // Get oldest tags (sorted by oldest date)
        tag_vec.sort_by(|a, b| a.1.cmp(&b.1));
        let oldest = tag_vec.into_iter()
            .take(limit)
            .map(|(tag, date, _)| (tag, date))
            .collect();

        (recent, oldest)
    }
}

// Parameters for creating a new node
#[derive(Deserialize)]
struct CreateNodeParams {
    name: String,
    description: String,
    content: String,
    parent_name: Option<String>, // Use name instead of index for a more user-friendly API
    relation: Option<String>,
    tags: Option<Vec<String>>,
    metadata: Option<HashMap<String, String>>,
    quotes: Option<Vec<String>>
}

// Parameters for updating a node
#[derive(Deserialize)]
struct UpdateNodeParams {
    node_name: String, // Use name to identify the node
    new_name: Option<String>,
    new_description: Option<String>,
    new_content: Option<String>,
    new_tags: Option<Vec<String>>,
    new_metadata: Option<HashMap<String, String>>,
    new_quotes: Option<Vec<String>>
}

// Parameters for deleting a node
#[derive(Deserialize)]
struct DeleteNodeParams {
    node_name: String, // Use name to identify the node
}

#[derive(Deserialize)]
struct MoveNodeParams {
    node_name: String,
    new_parent_name: String, 
    new_relation: String,
}

// Parameters for connecting two nodes
#[derive(Deserialize)]
struct ConnectNodesParams {
    from_node_name: String,
    to_node_name: String,
    relation: String,
}

#[derive(Deserialize)]
struct GetNodeParams {
    node_name: String
}

#[derive(Deserialize)]
struct GetChildrenParams {
    parent_node_name: String
}

#[derive(Deserialize)]
struct GetNodesByTagParams {
    tag: String
}

#[derive(Deserialize)]
struct SearchNodesParams {
    query: String
}

#[derive(Deserialize)]
struct GetMostConnectedParams {
    limit: Option<usize>
}

#[derive(Deserialize)]
struct GetTopTagsParams {
    limit: Option<usize>
}

#[derive(Deserialize)]
struct GetTagsByDateParams {
    limit: Option<usize>
}

#[derive(Deserialize)]
struct GetRecentNodesParams {
    limit: Option<usize>
}

// Define a struct to hold the tool information
struct GraphToolInfo {
    name: String,
    description: String,
    input_schema: Value,
}

// Create a function to build the tool information
fn graph_tool_info() -> GraphToolInfo {
    GraphToolInfo {
        name: "graph_tool".to_string(),
        description: "A tool for managing and interacting with a knowledge graph.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The action to perform.",
                    "enum": ["create_root", "create_node", "update_node", "delete_node", "connect_nodes", "get_node", "get_children", "get_nodes_by_tag", "search_nodes", "get_most_connected", "get_top_tags"]
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

// Function to handle 'tools/call' for the graph tool
pub async fn handle_graph_tool_call(
    params: CallToolParams,
    graph_manager: &mut GraphManager,
    id: Option<Value>,
) -> Result<JsonRpcResponse> {
    
    let tool_info = graph_tool_info();

    if params.name != tool_info.name {
        return Ok(error_response(id, INVALID_PARAMS, 
            &format!("Expected tool name '{}', got '{}'", tool_info.name, params.name)));
    }

    let action = params.arguments.get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Missing or invalid 'action' field"))?;
    
    let action_params = params.arguments.get("params")
        .ok_or_else(|| anyhow!("Missing 'params' field"))?;

    match action {
        "create_root" => {
            let create_params: CreateNodeParams = serde_json::from_value(action_params.clone())?;
            let mut node = DataNode::new(
                create_params.name,
                create_params.description,
                create_params.content
            );
            
            // Set optional fields if provided
            if let Some(tags) = create_params.tags {
                node.tags = tags;
            }
            if let Some(metadata) = create_params.metadata {
                node.metadata = metadata;
            }
            if let Some(quotes) = create_params.quotes {
                node.quotes = quotes;
            }

            match graph_manager.create_root(node).await {
                Ok(idx) => {
                    let result = json!({
                        "message": "Root node created successfully",
                        "node_index": idx.index(),
                        "timestamp": chrono::Utc::now()
                    });
                    Ok(success_response(id.clone(), json!(CallToolResult {
                        content: vec![ToolResponseContent {
                            type_: "text".into(),
                            text: result.to_string(),
                            annotations: None,
                        }],
                        is_error: Some(false),
                        _meta: None,
                        progress: None,
                        total: None
                    })))
                }
                Err(e) => {
                    Ok(error_response(id.clone(), INTERNAL_ERROR, &e.to_string()))
                }
            }
        }
        "create_node" => {
            let create_params: CreateNodeParams = serde_json::from_value(action_params.clone())?;
            if let Some(parent_name) = create_params.parent_name {
                if let Some((parent_idx, _)) = graph_manager.get_node_by_name(&parent_name) {
                    let mut node = DataNode::new(create_params.name, create_params.description, create_params.content);
                    
                    // Set optional fields
                    if let Some(tags) = create_params.tags {
                        node.tags = tags;
                    }
                    if let Some(metadata) = create_params.metadata {
                        node.metadata = metadata;
                    }

                    let relation = create_params.relation.ok_or_else(|| anyhow!("Missing relation for creating connected node"))?;
                    match graph_manager.create_connected_node(node, parent_idx, relation).await {
                        Ok(idx) => {
                            let result = json!({
                                "message": "Node created successfully",
                                "node_index": idx.index(),
                                "timestamp": chrono::Utc::now()
                            });
                            Ok(success_response(id.clone(), json!(CallToolResult {
                                content: vec![ToolResponseContent {
                                    type_: "text".into(),
                                    text: result.to_string(),
                                    annotations: None,
                                }],
                                is_error: Some(false),
                                _meta: None,
                                progress: None,
                                total: None
                            })))
                        }
                        Err(e) => {
                            Ok(error_response(id.clone(), INTERNAL_ERROR, &e.to_string()))
                        }
                    }
                } else {
                    Ok(error_response(id.clone(), INVALID_PARAMS, "Parent node not found"))
                }
            } else {
                Ok(error_response(id.clone(), INVALID_PARAMS, "Parent name is required to create a connected node"))
            }
        }
        "update_node" => {
            let update_params: UpdateNodeParams = serde_json::from_value(action_params.clone())?;
            if let Some((idx, node)) = graph_manager.get_node_by_name(&update_params.node_name) {
                let mut updated_node = DataNode::new(
                    update_params.new_name.unwrap_or_else(|| node.name.clone()),
                    update_params.new_description.unwrap_or_else(|| node.description.clone()),
                    update_params.new_content.unwrap_or_else(|| node.content.clone()),
                );

                // Update optional fields
                if let Some(tags) = update_params.new_tags {
                    updated_node.tags = tags;
                }
                if let Some(metadata) = update_params.new_metadata {
                    updated_node.metadata = metadata;
                }
                if let Some(quotes) = update_params.new_quotes {
                    updated_node.quotes = quotes;
                }

                match graph_manager.update_node(idx, updated_node).await {
                    Ok(_) => {
                        let result = json!({
                            "message": "Node updated successfully",
                            "timestamp": chrono::Utc::now()
                        });
                        Ok(success_response(id.clone(), json!(CallToolResult {
                            content: vec![ToolResponseContent {
                                type_: "text".into(),
                                text: result.to_string(),
                                annotations: None,
                            }],
                            is_error: Some(false),
                            _meta: None,
                            progress: None,
                            total: None
                        })))
                    }
                    Err(e) => {
                        Ok(error_response(id.clone(), INTERNAL_ERROR, &e.to_string()))
                    }
                }
            } else {
                Ok(error_response(id.clone(), INVALID_PARAMS, "Node not found"))
            }
        }
        "delete_node" => {
            let delete_params: DeleteNodeParams = serde_json::from_value(action_params.clone())?;
            if let Some((idx, _)) = graph_manager.get_node_by_name(&delete_params.node_name) {
                match graph_manager.delete_node(idx).await {
                    Ok(_) => {
                        let result = json!({
                            "message": "Node deleted successfully",
                            "timestamp": chrono::Utc::now()
                        });
                        Ok(success_response(id.clone(), json!(CallToolResult {
                            content: vec![ToolResponseContent {
                                type_: "text".into(),
                                text: result.to_string(),
                                annotations: None,
                            }],
                            is_error: Some(false),
                            _meta: None,
                            progress: None,
                            total: None
                        })))
                    }
                    Err(e) => {
                        Ok(error_response(id.clone(), INTERNAL_ERROR, &e.to_string()))
                    }
                }
            } else {
                Ok(error_response(id.clone(), INVALID_PARAMS, "Node not found"))
            }
        }
        "connect_nodes" => {
            let connect_params: ConnectNodesParams = serde_json::from_value(action_params.clone())?;
            if let (Some((from_idx, _)), Some((to_idx, _))) = (graph_manager.get_node_by_name(&connect_params.from_node_name), graph_manager.get_node_by_name(&connect_params.to_node_name)) {
                match graph_manager.connect(from_idx, to_idx, connect_params.relation).await {
                    
                    Ok(_) => {
                        let result = json!({
                            "message": "Nodes connected successfully",
                            "timestamp": chrono::Utc::now()
                        });
                        Ok(success_response(id.clone(), json!(CallToolResult {
                            content: vec![ToolResponseContent {
                                type_: "text".into(),
                                text: result.to_string(),
                                annotations: None,
                            }],
                            is_error: Some(false),
                            _meta: None,
                            progress: None,
                            total: None
                        })))
                    }
                    Err(e) => {
                        Ok(error_response(id.clone(), INTERNAL_ERROR, &e.to_string()))
                    }
                }
            } else {
                Ok(error_response(id.clone(), INVALID_PARAMS, "One or both nodes not found"))
            }
        }
        "get_node" => {
            let get_params: GetNodeParams = serde_json::from_value(action_params.clone())?;
            if let Some((_, node)) = graph_manager.get_node_by_name(&get_params.node_name) {
                let node_info = json!({
                    "name": node.name,
                    "description": node.description,
                    "content": node.content,
                    "tags": node.tags,
                    "metadata": node.metadata,
                    "timestamp": chrono::Utc::now()
                });
                Ok(success_response(id.clone(), json!(CallToolResult {
                    content: vec![ToolResponseContent {
                        type_: "text".into(),
                        text: node_info.to_string(),
                        annotations: None,
                    }],
                    is_error: Some(false),
                    _meta: None,
                    progress: None,
                    total: None
                })))
            } else {
                Ok(error_response(id.clone(), INVALID_PARAMS, "Node not found"))
            }
        }
        "get_children" => {
            let get_children_params: GetChildrenParams = serde_json::from_value(action_params.clone())?;
            if let Some((parent_idx, _)) = graph_manager.get_node_by_name(&get_children_params.parent_node_name) {
                let children = graph_manager.get_children(parent_idx);
                let children_info: Vec<_> = children.into_iter().map(|(_, child, relation)| {
                    json!({
                        "name": child.name,
                        "description": child.description,
                        "content": child.content,
                        "relation": relation,
                        "tags": child.tags,
                        "metadata": child.metadata,
                        "timestamp": chrono::Utc::now()
                    })
                }).collect();
                Ok(success_response(id.clone(), json!(CallToolResult {
                    content: vec![ToolResponseContent {
                        type_: "text".into(),
                        text: json!(children_info).to_string(),
                        annotations: None,
                    }],
                    is_error: Some(false),
                    _meta: None,
                    progress: None,
                    total: None
                })))
            } else {
                Ok(error_response(id.clone(), INVALID_PARAMS, "Parent node not found"))
            }
        }
        "get_nodes_by_tag" => {
            let get_by_tag_params: GetNodesByTagParams = serde_json::from_value(action_params.clone())?;
            let nodes = graph_manager.get_nodes_by_tag(&get_by_tag_params.tag);
            let nodes_info: Vec<_> = nodes.into_iter().map(|(_, node)| {
                json!({
                    "name": node.name,
                    "description": node.description,
                    "content": node.content,
                    "tags": node.tags,
                    "metadata": node.metadata,
                    "timestamp": chrono::Utc::now()
                })
            }).collect();
            Ok(success_response(id.clone(), json!(CallToolResult {
                content: vec![ToolResponseContent {
                    type_: "text".into(),
                    text: json!(nodes_info).to_string(),
                    annotations: None,
                }],
                is_error: Some(false),
                _meta: None,
                progress: None,
                total: None
            })))
        }
        "search_nodes" => {
            let search_params: SearchNodesParams = serde_json::from_value(action_params.clone())?;
            let nodes = graph_manager.search_nodes(&search_params.query);
            let nodes_info: Vec<_> = nodes.into_iter().map(|(_, node)| {
                json!({
                    "name": node.name,
                    "description": node.description,
                    "content": node.content,
                    "tags": node.tags,
                    "metadata": node.metadata
                })
            }).collect();
            Ok(success_response(id.clone(), json!(CallToolResult {
                content: vec![ToolResponseContent {
                    type_: "text".into(),
                    text: json!(nodes_info).to_string(),
                    annotations: None,
                }],
                is_error: Some(false),
                _meta: None,
                progress: None,
                total: None
            })))
        }
        "get_most_connected" => {
            let most_connected_params: GetMostConnectedParams = serde_json::from_value(action_params.clone())?;
            let limit = most_connected_params.limit.unwrap_or(10);
            let nodes = graph_manager.get_most_connected_nodes(limit);
            let nodes_info: Vec<_> = nodes.into_iter().map(|(_, node, edge_count)| {
                json!({
                    "name": node.name,
                    "description": node.description,
                    "content": node.content,
                    "tags": node.tags,
                    "metadata": node.metadata,
                    "connection_count": edge_count,
                    "timestamp": chrono::Utc::now()
                })
            }).collect();
            Ok(success_response(id.clone(), json!(CallToolResult {
                content: vec![ToolResponseContent {
                    type_: "text".into(),
                    text: json!(nodes_info).to_string(),
                    annotations: None,
                }],
                is_error: Some(false),
                _meta: None,
                progress: None,
                total: None
            })))
        }
        "get_top_tags" => {
            let top_tags_params: GetTopTagsParams = serde_json::from_value(action_params.clone())?;
            let limit = top_tags_params.limit.unwrap_or(10);
            let tags = graph_manager.get_top_tags(limit);
            let tags_info: Vec<_> = tags.into_iter().map(|(tag, count)| {
                json!({
                    "tag": tag,
                    "count": count,
                    "timestamp": chrono::Utc::now()
                })
            }).collect();
            Ok(success_response(id.clone(), json!(CallToolResult {
                content: vec![ToolResponseContent {
                    type_: "text".into(),
                    text: json!(tags_info).to_string(),
                    annotations: None,
                }],
                is_error: Some(false),
                _meta: None,
                progress: None,
                total: None
            })))
        }
        "get_tags_by_date" => {
            let params: GetTagsByDateParams = serde_json::from_value(action_params.clone())?;
            let limit = params.limit.unwrap_or(30);
            let (recent_tags, oldest_tags) = graph_manager.get_tags_by_date(limit);
            
            let result = json!({
                "recent_tags": recent_tags.into_iter().map(|(tag, date)| {
                    json!({
                        "tag": tag,
                        "date": date,
                    })
                }).collect::<Vec<_>>(),
                "oldest_tags": oldest_tags.into_iter().map(|(tag, date)| {
                    json!({
                        "tag": tag,
                        "date": date,
                    })
                }).collect::<Vec<_>>(),
                "timestamp": chrono::Utc::now()
            });
            
            Ok(success_response(id.clone(), json!(CallToolResult {
                content: vec![ToolResponseContent {
                    type_: "text".into(),
                    text: result.to_string(),
                    annotations: None,
                }],
                is_error: Some(false),
                _meta: None,
                progress: None,
                total: None
            })))
        }
        "get_recent_nodes" => {
            let recent_params: GetRecentNodesParams = serde_json::from_value(action_params.clone())?;
            let limit = recent_params.limit.unwrap_or(10);
            let nodes = graph_manager.get_recent_nodes(limit);
            let nodes_info: Vec<_> = nodes.into_iter().map(|(_, node)| {
                json!({
                    "name": node.name,
                    "description": node.description,
                    "content": node.content,
                    "tags": node.tags,
                    "metadata": node.metadata,
                    "date_created": node.date_created,
                    "date_modified": node.date_modified
                })
            }).collect();
            Ok(success_response(id.clone(), json!(CallToolResult {
                content: vec![ToolResponseContent {
                    type_: "text".into(),
                    text: json!(nodes_info).to_string(),
                    annotations: None,
                }],
                is_error: Some(false),
                _meta: None,
                progress: None,
                total: None
            })))
        }
        _ => Ok(error_response(id, INVALID_PARAMS, "Invalid action")),
    }
}
