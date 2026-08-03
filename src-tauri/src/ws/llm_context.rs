use std::sync::Arc;

use kxen_app::agent::agent_loop::AgentEvent;
use kxen_app::core::event::{Event, EventBus};
use kxen_app::core::session as ses;
use kxen_app::llm::ModelRef;
use serde_json::json;

pub(super) fn event_handler(
    transcript: Arc<std::sync::Mutex<Vec<ses::Part>>>,
    session_id: String,
    stream_id: String,
    model: ModelRef,
    bus: EventBus,
) -> Arc<dyn Fn(AgentEvent) + Send + Sync> {
    Arc::new(move |event| {
        if matches!(&event, AgentEvent::Done { .. } | AgentEvent::Aborted | AgentEvent::Error { .. }) {
            return;
        }
        match &event {
            AgentEvent::Reasoning { text } => {
                let mut parts = kxen_app::core::shared::lock(&transcript);
                match parts.last_mut() {
                    Some(ses::Part::Reasoning { text: existing }) => existing.push_str(text),
                    _ => parts.push(ses::Part::Reasoning { text: text.clone() }),
                }
            }
            AgentEvent::ToolCall { name, summary, arguments } => {
                let args = serde_json::from_str(arguments).unwrap_or_else(|_| json!(arguments));
                kxen_app::core::shared::lock(&transcript).push(ses::Part::ToolCall {
                    name: name.clone(),
                    input: json!(summary),
                    output: String::new(),
                    args: Some(args),
                });
            }
            AgentEvent::ToolResult { name, output, .. } => {
                let mut parts = kxen_app::core::shared::lock(&transcript);
                if let Some(ses::Part::ToolCall { output: slot, .. }) = parts.iter_mut().rev().find(
                    |part| matches!(part, ses::Part::ToolCall { name: candidate, output, .. } if candidate == name && output.is_empty()),
                ) {
                    *slot = super::run_finalize::cap_output(output, 10_000);
                }
            }
            _ => {}
        }
        let Ok(mut payload) = serde_json::to_value(&event) else { return };
        if let Some(object) = payload.as_object_mut() {
            object.insert("session_id".into(), json!(session_id));
            object.insert("stream_id".into(), json!(stream_id));
            object.insert("model".into(), json!({ "provider": model.provider, "model": model.model }));
        }
        bus.publish(Event::LlmDelta(payload));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_reduces_transcript_and_publishes_only_non_terminal_deltas() {
        let transcript = Arc::new(std::sync::Mutex::new(Vec::new()));
        let bus = EventBus::new(16);
        let mut events = bus.subscribe();
        let handler = event_handler(transcript.clone(), "ses_one".into(), "stream_one".into(), ModelRef::new("xai", "grok"), bus);

        handler(AgentEvent::Reasoning { text: "first ".into() });
        handler(AgentEvent::Reasoning { text: "second".into() });
        handler(AgentEvent::ToolCall { name: "read".into(), summary: "read file".into(), arguments: r#"{"path":"README.md"}"#.into() });
        handler(AgentEvent::ToolResult { name: "other".into(), summary: "ignored".into(), output: "unused".into() });
        handler(AgentEvent::ToolResult { name: "read".into(), summary: "done".into(), output: "contents".into() });
        handler(AgentEvent::ToolCall { name: "shell".into(), summary: "run".into(), arguments: "not-json".into() });
        handler(AgentEvent::Text { text: "answer".into() });

        for terminal in [AgentEvent::Done { turns: 1, stats: None }, AgentEvent::Aborted, AgentEvent::Error { message: "failed".into() }] {
            handler(terminal);
        }

        let parts = kxen_app::core::shared::lock(&transcript);
        assert!(matches!(&parts[0], ses::Part::Reasoning { text } if text == "first second"));
        assert!(matches!(&parts[1], ses::Part::ToolCall { name, output, args: Some(args), .. }
            if name == "read" && output == "contents" && args["path"] == "README.md"));
        assert!(matches!(&parts[2], ses::Part::ToolCall { name, args: Some(args), .. }
            if name == "shell" && args == "not-json"));
        drop(parts);

        let mut published = Vec::new();
        while let Ok(Event::LlmDelta(payload)) = events.try_recv() {
            published.push(payload);
        }
        assert_eq!(published.len(), 7, "terminal events must not be published as deltas");
        assert!(published.iter().all(|payload| {
            payload["session_id"] == "ses_one"
                && payload["stream_id"] == "stream_one"
                && payload["model"] == json!({ "provider": "xai", "model": "grok" })
        }));
    }
}
