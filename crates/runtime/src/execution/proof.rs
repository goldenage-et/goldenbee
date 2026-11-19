//! Executable compatibility proof for the pinned native ADK Runner.
//!
//! This is deliberately small: native `Runner`, `LlmAgent`, `InMemorySessionService`,
//! streaming events, canonical history, and no Gateway or Marketplace tier.

use adk_rust::prelude::*;
use adk_rust::runner::Runner;
use adk_rust::session::{Event, GetRequest, InMemorySessionService, SessionService};
use adk_rust::{Agent, Llm, SessionId, UsageMetadata, UserId};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One observed native ADK event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnEvent {
    /// Event identity.
    pub id: String,
    /// Native invocation identity.
    pub invocation_id: String,
    /// Author.
    pub author: String,
    /// Content role.
    pub role: String,
    /// Text content.
    pub text: String,
    /// Whether this is a partial stream chunk.
    pub partial: bool,
    /// Whether the turn is complete.
    pub turn_complete: bool,
    /// Provider-confirmed usage on this event.
    pub usage: Option<UsageSummary>,
}

/// Token usage exposed by the proof.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageSummary {
    /// Prompt tokens.
    pub prompt_tokens: i32,
    /// Candidate tokens.
    pub candidate_tokens: i32,
    /// Total tokens.
    pub total_tokens: i32,
}

/// Evidence from one native turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofTurn {
    /// Streamed output aggregated from model events.
    pub output: String,
    /// Native invocation identity.
    pub invocation_id: String,
    /// Observed events.
    pub events: Vec<TurnEvent>,
    /// Number of canonical events after the turn.
    pub history_len: usize,
    /// Canonical history roles and text.
    pub history: Vec<TurnEvent>,
    /// Aggregated usage.
    pub usage: UsageSummary,
}

/// A proof harness owning a native ADK Runner and session service.
pub struct ProofHarness {
    app_name: String,
    sessions: Arc<InMemorySessionService>,
    runner: Runner,
}

impl ProofHarness {
    /// Build the native Runner with one LlmAgent.
    pub fn new(app_name: impl Into<String>, model: Arc<dyn Llm>) -> adk_rust::Result<Self> {
        let app_name = app_name.into();
        let agent = LlmAgentBuilder::new("proof-agent")
            .description("Native ADK standalone compatibility proof")
            .instruction("Answer the user. Keep the answer short.")
            .model(model)
            .build()?;
        let sessions = Arc::new(InMemorySessionService::new());
        let runner = Runner::builder()
            .app_name(app_name.clone())
            .agent(Arc::new(agent) as Arc<dyn Agent>)
            .session_service(sessions.clone())
            .build()?;
        Ok(Self {
            app_name,
            sessions,
            runner,
        })
    }

    /// Create a native session if it does not exist.
    pub async fn ensure_session(&self, user_id: &str, session_id: &str) -> adk_rust::Result<()> {
        let existing = self
            .sessions
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: user_id.to_string(),
                session_id: session_id.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await;
        if existing.is_ok() {
            return Ok(());
        }
        self.sessions
            .create(adk_rust::session::CreateRequest {
                app_name: self.app_name.clone(),
                user_id: user_id.to_string(),
                session_id: Some(session_id.to_string()),
                state: Default::default(),
            })
            .await?;
        Ok(())
    }

    /// Run one turn through the native Runner and collect streaming evidence.
    pub async fn run_turn(
        &self,
        user_id: &str,
        session_id: &str,
        input: &str,
    ) -> adk_rust::Result<ProofTurn> {
        self.ensure_session(user_id, session_id).await?;
        let user = UserId::new(user_id)?;
        let session = SessionId::new(session_id)?;
        let mut stream = self
            .runner
            .run(user, session, Content::new("user").with_text(input))
            .await?;
        let mut events = Vec::new();
        let mut output = String::new();
        let mut invocation_id = String::new();
        let mut usage = UsageSummary::default();
        while let Some(event) = stream.next().await {
            let event = event?;
            if invocation_id.is_empty() {
                invocation_id = event.invocation_id.clone();
            }
            let content = event.llm_response.content.clone();
            let role = content
                .as_ref()
                .map(|content| content.role.clone())
                .unwrap_or_default();
            let text = content
                .as_ref()
                .map(|content| {
                    content
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            if role == "model" && !event.llm_response.partial {
                output.push_str(&text);
            }
            if let Some(metadata) = &event.llm_response.usage_metadata {
                usage.prompt_tokens += metadata.prompt_token_count;
                usage.candidate_tokens += metadata.candidates_token_count;
                usage.total_tokens += metadata.total_token_count;
            }
            events.push(TurnEvent {
                id: event.id.clone(),
                invocation_id: event.invocation_id.clone(),
                author: event.author.clone(),
                role,
                text,
                partial: event.llm_response.partial,
                turn_complete: event.llm_response.turn_complete,
                usage: event.llm_response.usage_metadata.as_ref().map(usage_of),
            });
        }
        let history = self.history(user_id, session_id).await?;
        Ok(ProofTurn {
            output,
            invocation_id,
            events,
            history_len: history.len(),
            history,
            usage,
        })
    }

    /// Read canonical native session history.
    pub async fn history(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> adk_rust::Result<Vec<TurnEvent>> {
        let session = self
            .sessions
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: user_id.to_string(),
                session_id: session_id.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await?;
        Ok(session
            .events()
            .all()
            .into_iter()
            .filter_map(event_evidence)
            .collect())
    }

    /// Read the total native event count.
    pub async fn history_len(&self, user_id: &str, session_id: &str) -> adk_rust::Result<usize> {
        let session = self
            .sessions
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: user_id.to_string(),
                session_id: session_id.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await?;
        Ok(session.events().len())
    }
}

fn event_evidence(event: Event) -> Option<TurnEvent> {
    let content = event.llm_response.content.clone()?;
    let text = content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    Some(TurnEvent {
        id: event.id,
        invocation_id: event.invocation_id,
        author: event.author,
        role: content.role,
        text,
        partial: event.llm_response.partial,
        turn_complete: event.llm_response.turn_complete,
        usage: event.llm_response.usage_metadata.as_ref().map(usage_of),
    })
}

fn usage_of(metadata: &UsageMetadata) -> UsageSummary {
    UsageSummary {
        prompt_tokens: metadata.prompt_token_count,
        candidate_tokens: metadata.candidates_token_count,
        total_tokens: metadata.total_token_count,
    }
}
