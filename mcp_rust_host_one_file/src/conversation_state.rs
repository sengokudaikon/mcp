use shared_protocol_objects::{Role, ToolInfo};

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ConversationState {
    pub messages: Vec<Message>,
    pub system_prompt: String,
    pub tools: Vec<ToolInfo>,
}

impl ConversationState {
    pub fn new(system_prompt: String, tools: Vec<ToolInfo>) -> Self {
        let mut state = Self {
            messages: Vec::new(),
            system_prompt: system_prompt.clone(),
            tools,
        };

        // Add the system prompt as the first system message
        state.add_system_message(&system_prompt);
        state
    }

    pub fn add_system_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::System,
            content: content.to_string(),
        });
    }

    pub fn add_user_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::User,
            content: content.to_string(),
        });
    }

    pub fn add_assistant_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::Assistant,
            content: content.to_string(),
        });
    }
}
