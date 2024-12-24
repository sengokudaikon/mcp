use axum::{
    extract::{Form, Path, State},
    response::{Html, IntoResponse, Sse, sse::Event},
    http::StatusCode,
    Json, Router,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    convert::Infallible,
};
use uuid::Uuid;
use anyhow::Result;
use futures::{Stream, StreamExt};
use serde::Deserialize;
use crate::{
    ai_client::{AIClient, StreamResult},
    conversation_state::ConversationState,
    MCPHost,
};

use crate::shared_protocol_objects::Role;

#[derive(Clone)]
pub struct WebAppState {
    pub sessions: Arc<Mutex<HashMap<Uuid, ConversationState>>>,
    pub host: Arc<MCPHost>,
}

impl WebAppState {
    pub fn new(host: Arc<MCPHost>) -> Self {
        WebAppState {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            host,
        }
    }
}

#[derive(Deserialize)]
pub struct UserQuery {
    user_input: String,
    session_id: String,
}

pub async fn root() -> impl IntoResponse {
    let html = r#"
<!DOCTYPE html>
<html>
<head>
  <meta charset="UTF-8">
  <title>HTMX + AI Streaming Demo</title>
  <script src="https://cdn.jsdelivr.net/npm/htmx.org@1.9.2"></script>
</head>
<body>
  <h1>HTMX + AI Streaming Demo</h1>
  <div>
    <label>Session ID:
      <input type="text" id="sessionId" value="" placeholder="(auto-generated on first submit)">
    </label>
  </div>

  <form id="askForm"
        hx-post="/ask"
        hx-trigger="submit"
        hx-swap="none"
        style="margin-top: 1em;">
    <input type="text" name="user_input" placeholder="Ask me something..." />
    <input type="hidden" name="session_id" />
    <button type="submit">Send</button>
  </form>

  <div id="streamArea" style="border: 1px solid #ccc; padding: 1em; margin-top: 1em;">
  </div>

<script>
document.getElementById('askForm').addEventListener('submit', function(evt) {
  evt.preventDefault();

  let form = evt.target;
  let user_input = form.user_input.value;
  if (!user_input.trim()) return;

  let sessionElem = document.getElementById('sessionId');
  if (!sessionElem.value) {
    sessionElem.value = crypto.randomUUID();
  }

  form.session_id.value = sessionElem.value;

  fetch('/ask', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams(new FormData(form))
  }).then(response => {
    if (!response.ok) {
      alert("Error from server: " + response.status);
      return;
    }
    return response.json();
  }).then(data => {
    if (!data || !data.ok) {
      alert("No valid SSE path returned");
      return;
    }

    let eventSource = new EventSource(data.sse_url);
    let streamArea = document.getElementById('streamArea');
    streamArea.innerHTML = "";

    eventSource.onmessage = function(e) {
      if (e.data === "[DONE]") {
        eventSource.close();
        return;
      }
      streamArea.innerHTML += e.data;
    };

    eventSource.onerror = function(e) {
      streamArea.innerHTML += "<br><strong style='color:red;'>[Stream error occurred]</strong>";
      eventSource.close();
    };
  }).catch(err => {
    alert("Request error: " + err);
  });
});
</script>
</body>
</html>
"#;

    Html(html)
}

pub async fn ask(
    State(app_state): State<WebAppState>,
    Form(query): Form<UserQuery>,
) -> impl IntoResponse {
    let user_input = query.user_input.trim().to_string();
    let session_id_str = query.session_id.clone();

    let session_id = if session_id_str.is_empty() {
        Uuid::new_v4()
    } else {
        match Uuid::parse_str(&session_id_str) {
            Ok(id) => id,
            Err(_) => Uuid::new_v4(),
        }
    };

    {
        let mut sessions = app_state.sessions.lock().unwrap();
        let entry = sessions.entry(session_id).or_insert_with(|| {
            ConversationState::new("Welcome to the HTMX + AI Demo!".to_string(), vec![])
        });
        entry.add_user_message(&user_input);
    }

    let sse_url = format!("/sse/{}", session_id);
    let result = serde_json::json!({
        "ok": true,
        "sse_url": sse_url,
    });

    Json(result)
}

#[axum::debug_handler]
pub async fn sse_handler(
    State(app_state): State<WebAppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    let mut sessions = app_state.sessions.lock().unwrap();
    let state = match sessions.get_mut(&session_id) {
        Some(conv) => conv,
        None => return Err((StatusCode::BAD_REQUEST, "Session not found".to_string())),
    };

    // Get the last user message from the conversation
    let last_user_msg = state.messages.iter()
        .rev()
        .find(|msg| matches!(msg.role, Role::User))
        .map(|msg| msg.content.clone())
        .unwrap_or_default();

    // Process the message using the host's unified logic
    // Get the AI client
    let Some(client) = &app_state.host.ai_client else {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "No AI client configured".to_string()));
    };

    // Build the request with streaming enabled
    let mut builder = client.raw_builder();
    
    // Add system messages
    for msg in state.messages.iter().filter(|m| matches!(m.role, Role::System)) {
        builder = builder.system(msg.content.clone());
    }
    
    // Add conversation messages
    for msg in state.messages.iter().filter(|m| !matches!(m.role, Role::System)) {
        match msg.role {
            Role::User => builder = builder.user(msg.content.clone()),
            Role::Assistant => builder = builder.assistant(msg.content.clone()),
            _ => {}
        }
    }

    // Enable streaming
    builder = builder.streaming(true);

    // Execute streaming request
    match builder.execute_streaming().await {
        Ok(stream_result) => {
            let event_stream = stream_result_to_sse(stream_result);
            Ok(Sse::new(event_stream))
        }
        Err(e) => {
            Err((StatusCode::INTERNAL_SERVER_ERROR, format!("AI error: {}", e)))
        }
    }
}

fn stream_result_to_sse(
    mut stream_result: StreamResult
) -> impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>> {
    StreamExt::map(
        stream_result,
        |chunk_result| {
            match chunk_result {
                Ok(event) => {
                    use crate::ai_client::StreamEvent;
                    match event {
                        StreamEvent::MessageStart { .. } => {
                            Ok(axum::response::sse::Event::default().data(""))
                        },
                        StreamEvent::ContentBlockStart { .. } => {
                            Ok(axum::response::sse::Event::default().data(""))
                        },
                        StreamEvent::ContentDelta { text, .. } => {
                            Ok(axum::response::sse::Event::default().data(text))
                        },
                        StreamEvent::ContentBlockStop { .. } => {
                            Ok(axum::response::sse::Event::default().data(""))
                        },
                        StreamEvent::MessageDelta { stop_reason, .. } => {
                            if let Some(reason) = stop_reason {
                                if reason.to_uppercase().contains("STOP") {
                                    return Ok(axum::response::sse::Event::default().data("[DONE]"));
                                }
                            }
                            Ok(axum::response::sse::Event::default().data(""))
                        },
                        StreamEvent::MessageStop => {
                            Ok(axum::response::sse::Event::default().data("[DONE]"))
                        },
                        StreamEvent::Error { message, .. } => {
                            let msg = format!("[ERROR] {}", message);
                            Ok(axum::response::sse::Event::default().data(msg))
                        },
                    }
                }
                Err(e) => {
                    let msg = format!("[ERROR] {}", e);
                    Ok(axum::response::sse::Event::default().data(msg))
                }
            }
        }
    )
}
