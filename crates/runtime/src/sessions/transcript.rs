//! Redacted transcript projection over canonical native session events.
//!
//! The transcript is a view. It never becomes a second conversation store: the
//! native ADK event sequence in [`super::FileSessionService`] remains the only
//! canonical history.

use adk_rust::session::Event;
use adk_rust::Part;
use common::SessionTurn;
use serde::Serialize;

/// Build the ordered, redacted conversation turns for a canonical event list.
pub fn turns(events: &[Event]) -> Vec<SessionTurn> {
    let mut turns = Vec::new();
    for event in events {
        // A streaming partial is not canonical history. The native store keeps
        // complete turns, but a foreign writer could still append one.
        if event.llm_response.partial {
            continue;
        }
        let at = event.timestamp.timestamp_millis();
        let role = if event.author.is_empty() {
            "system".to_string()
        } else {
            event.author.clone()
        };
        let mut text = String::new();
        let mut append = |part: &str| {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(part);
        };
        if let Some(content) = event.llm_response.content.as_ref() {
            for part in &content.parts {
                match part {
                    Part::Text { text: value } => append(value),
                    Part::Thinking { thinking, .. } => append(thinking),
                    Part::FunctionCall { name, args, .. } => append(&format!(
                        "tool:{name} args={}",
                        common::canonical_digest(args)
                    )),
                    Part::FunctionResponse {
                        function_response, ..
                    } => append(&format!(
                        "tool:{} result={}",
                        function_response.name,
                        common::canonical_digest(&function_response.response)
                    )),
                    Part::InlineData { mime_type, .. } => {
                        append(&format!("attachment:{mime_type}"))
                    }
                    Part::FileData { mime_type, .. } => append(&format!("attachment:{mime_type}")),
                    _ => {}
                }
            }
        }
        if let Some(compaction) = &event.actions.compaction {
            for part in &compaction.compacted_content.parts {
                if let Part::Text { text: value } = part {
                    append(&format!("compaction:{value}"));
                }
            }
        }
        if text.trim().is_empty() {
            continue;
        }
        turns.push(SessionTurn {
            role,
            text: common::redact(&text),
            at,
        });
    }
    turns
}

/// Digest of an exported turn list.
pub fn digest(turns: &[SessionTurn]) -> String {
    common::canonical_digest(&TranscriptDigest { turns })
}

#[derive(Serialize)]
struct TranscriptDigest<'a> {
    turns: &'a [SessionTurn],
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_rust::Content;

    fn event(author: &str, text: &str) -> Event {
        let mut event = Event::new("inv-1");
        event.author = author.into();
        let mut content = Content::new("model");
        content.parts.push(Part::Text {
            text: text.to_string(),
        });
        event.llm_response.content = Some(content);
        event
    }

    #[test]
    fn turns_preserve_order_and_redact_secret_markers() {
        let events = vec![
            event("user", "hello"),
            event("agent", "token=sk-secret-value"),
        ];
        let turns = turns(&events);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].text, "hello");
        assert!(!turns[1].text.contains("sk-secret-value"));
    }

    #[test]
    fn digest_is_stable_for_the_same_turns() {
        let first = turns(&[event("user", "hello")]);
        assert_eq!(digest(&first), digest(&first.clone()));
        assert_ne!(digest(&first), digest(&turns(&[event("user", "other")])));
    }
}
