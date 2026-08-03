use crate::llm::types::Delta;

const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVENTS: usize = 65_536;

pub(crate) struct StreamBudget {
    bytes: usize,
    events: usize,
    byte_limit: usize,
    event_limit: usize,
}

impl Default for StreamBudget {
    fn default() -> Self {
        Self { bytes: 0, events: 0, byte_limit: MAX_OUTPUT_BYTES, event_limit: MAX_EVENTS }
    }
}

impl StreamBudget {
    pub(crate) fn observe(&mut self, delta: &Delta) -> Result<(), String> {
        self.events = self.events.saturating_add(1);
        if self.events > self.event_limit {
            return Err(format!("provider stream exceeded {} event limit", self.event_limit));
        }
        let added = match delta {
            Delta::Text(text) | Delta::Reasoning(text) | Delta::Error(text) => text.len(),
            Delta::ToolFragments(chunks) => chunks.iter().fold(0usize, |total, chunk| {
                let function = chunk.function.as_ref();
                total
                    .saturating_add(chunk.id.as_ref().map_or(0, String::len))
                    .saturating_add(function.and_then(|item| item.name.as_ref()).map_or(0, String::len))
                    .saturating_add(function.and_then(|item| item.arguments.as_ref()).map_or(0, String::len))
            }),
            Delta::ToolCall { name, input } => {
                name.len().saturating_add(serde_json::to_vec(input).map(|value| value.len()).unwrap_or(self.byte_limit))
            }
            Delta::Usage { .. } | Delta::Done => 0,
        };
        self.bytes = self.bytes.saturating_add(added);
        if self.bytes > self.byte_limit {
            return Err(format!("provider stream exceeded {} byte output limit", self.byte_limit));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulative_small_deltas_hit_byte_and_event_limits() {
        let mut bytes = StreamBudget { bytes: 0, events: 0, byte_limit: 5, event_limit: 10 };
        bytes.observe(&Delta::Text("abc".into())).unwrap();
        assert!(bytes.observe(&Delta::Text("def".into())).unwrap_err().contains("byte output"));

        let mut events = StreamBudget { bytes: 0, events: 0, byte_limit: 100, event_limit: 2 };
        events.observe(&Delta::Usage { input: 1, output: 1 }).unwrap();
        events.observe(&Delta::Done).unwrap();
        assert!(events.observe(&Delta::Done).unwrap_err().contains("event limit"));
    }
}
