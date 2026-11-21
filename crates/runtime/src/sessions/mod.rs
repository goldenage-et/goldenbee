//! Durable native session storage and management.
//!
//! [`FileSessionService`] owns the canonical native ADK event history. This
//! module adds the management surface over that same owner: list, continue,
//! resume, fork, rename, and durable transcript export. Identity is always the
//! authenticated `(namespace, principal)` pair plus the session id, so a
//! caller cannot address another principal's or namespace's history.

pub mod store;
pub mod transcript;

pub use store::{FileSessionService, SessionSummary, SESSION_STORE_FILE, SESSION_STORE_SCHEMA};

use crate::Runtime;
use common::{scopes, Principal, SessionCompaction, SessionTranscript, SessionView};
use std::sync::Arc;

/// Native ADK application identity used for sessions owned by this Worker.
fn session_app(runtime: &Runtime) -> String {
    let package = runtime.config().package.trim();
    if package.is_empty() {
        "goldenbee".to_string()
    } else {
        package.to_string()
    }
}

fn native_user(principal: &Principal) -> crate::Result<String> {
    crate::execution::runner::native_user_id(&principal.namespace, &principal.id)
}

fn view(
    summary: SessionSummary,
    events: &[adk_rust::session::Event],
    namespace: &str,
    principal: &str,
) -> SessionView {
    SessionView {
        namespace: namespace.to_string(),
        principal: principal.to_string(),
        session_id: summary.session_id,
        title: summary.title,
        forked_from: summary.forked_from,
        event_count: summary.event_count,
        turns: transcript::turns(events),
        created_at: summary.created_at,
        updated_at: summary.updated_at,
    }
}

impl Runtime {
    async fn session_owner(&self) -> crate::Result<Arc<FileSessionService>> {
        self.sessions().await.ok_or_else(|| {
            crate::Error::new(
                "session.store_not_composed",
                "this runtime has no canonical native session store",
            )
        })
    }

    /// List the authenticated caller's sessions, newest first.
    pub async fn session_views(&self, principal: &Principal) -> crate::Result<Vec<SessionView>> {
        principal.require(scopes::SESSION_READ)?;
        let sessions = self.session_owner().await?;
        let app = session_app(self);
        let user = native_user(principal)?;
        let mut views = Vec::new();
        for summary in sessions.summaries(&app, &user).await? {
            let details = sessions.details(&app, &user, &summary.session_id).await?;
            views.push(view(
                details.summary,
                &details.events,
                &principal.namespace,
                &principal.id,
            ));
        }
        Ok(views)
    }

    /// The session a caller would continue: the most recently updated one.
    pub async fn latest_session(&self, principal: &Principal) -> crate::Result<SessionView> {
        principal.require(scopes::SESSION_READ)?;
        let sessions = self.session_owner().await?;
        let app = session_app(self);
        let user = native_user(principal)?;
        let summary = sessions.latest(&app, &user).await?;
        let details = sessions.details(&app, &user, &summary.session_id).await?;
        Ok(view(
            details.summary,
            &details.events,
            &principal.namespace,
            &principal.id,
        ))
    }

    /// Resume one canonical session by identity.
    pub async fn session_view(
        &self,
        principal: &Principal,
        session_id: &str,
    ) -> crate::Result<SessionView> {
        principal.require(scopes::SESSION_READ)?;
        let sessions = self.session_owner().await?;
        let app = session_app(self);
        let user = native_user(principal)?;
        let details = sessions.details(&app, &user, session_id).await?;
        Ok(view(
            details.summary,
            &details.events,
            &principal.namespace,
            &principal.id,
        ))
    }

    /// Fork a session into an independent branch without changing the source.
    pub async fn fork_session(
        &self,
        principal: &Principal,
        session_id: &str,
        request: &common::SessionForkRequest,
    ) -> crate::Result<SessionView> {
        principal.require(scopes::SESSION_WRITE)?;
        let sessions = self.session_owner().await?;
        let app = session_app(self);
        let user = native_user(principal)?;
        sessions.fork(&app, &user, session_id, request).await?;
        let details = sessions.details(&app, &user, &request.session_id).await?;
        let view = view(
            details.summary,
            &details.events,
            &principal.namespace,
            &principal.id,
        );
        self.audit(
            &principal.id,
            "session.fork",
            &request.session_id,
            "created",
            &format!("source={session_id}"),
        )
        .await?;
        Ok(view)
    }

    /// Rename a session.
    pub async fn rename_session(
        &self,
        principal: &Principal,
        session_id: &str,
        request: &common::SessionRenameRequest,
    ) -> crate::Result<SessionView> {
        principal.require(scopes::SESSION_WRITE)?;
        let sessions = self.session_owner().await?;
        let app = session_app(self);
        let user = native_user(principal)?;
        sessions
            .rename(&app, &user, session_id, &request.title)
            .await?;
        let details = sessions.details(&app, &user, session_id).await?;
        let view = view(
            details.summary,
            &details.events,
            &principal.namespace,
            &principal.id,
        );
        self.audit(
            &principal.id,
            "session.rename",
            session_id,
            "renamed",
            "title updated",
        )
        .await?;
        Ok(view)
    }

    /// Schedule a manual compaction of one canonical session.
    ///
    /// The pinned ADK Runner has no public "compact now" method. This records
    /// a durable scheduling intent in the session state; the next model turn
    /// applies native truncation before its request and leaves canonical
    /// session events untouched. The response states that behavior instead of
    /// claiming an immediate rewrite.
    pub async fn request_session_compaction(
        &self,
        principal: &Principal,
        session_id: &str,
    ) -> crate::Result<SessionCompaction> {
        principal.require(scopes::SESSION_WRITE)?;
        let sessions = self.session_owner().await?;
        let app = session_app(self);
        let user = native_user(principal)?;
        sessions.request_compaction(&app, &user, session_id).await?;
        self.audit(
            &principal.id,
            "session.compaction",
            session_id,
            "scheduled",
            "next_model_turn",
        )
        .await?;
        Ok(SessionCompaction {
            session_id: session_id.to_string(),
            namespace: principal.namespace.clone(),
            principal: principal.id.clone(),
            state: "scheduled".into(),
            applies_on: "next_model_turn".into(),
            manual_entry_point: false,
            reason: "the pinned ADK Runner applies native compaction proactively on the next model turn; canonical session history is retained".into(),
        })
    }

    /// Export the canonical history to a durable transcript.
    pub async fn export_session(
        &self,
        principal: &Principal,
        session_id: &str,
    ) -> crate::Result<SessionTranscript> {
        principal.require(scopes::SESSION_WRITE)?;
        let sessions = self.session_owner().await?;
        let app = session_app(self);
        let user = native_user(principal)?;
        let transcript = sessions
            .export(&app, &user, session_id, &principal.namespace, &principal.id)
            .await?;
        self.audit(
            &principal.id,
            "session.export",
            session_id,
            "exported",
            &format!("digest={}", transcript.digest),
        )
        .await?;
        Ok(transcript)
    }

    /// Read the newest durable transcript, creating one when necessary.
    pub async fn session_transcript(
        &self,
        principal: &Principal,
        session_id: &str,
    ) -> crate::Result<SessionTranscript> {
        principal.require(scopes::SESSION_READ)?;
        let sessions = self.session_owner().await?;
        let app = session_app(self);
        let user = native_user(principal)?;
        sessions
            .transcript(&app, &user, session_id, &principal.namespace, &principal.id)
            .await
    }
}
