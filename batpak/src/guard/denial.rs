use serde::Serialize;
use std::fmt;

/// Denial: returned by a Gate when it rejects a proposal.
/// Separate from OutcomeError. Library does NOT auto-store denials.
/// Products decide whether to persist denials as events.
/// [SPEC:src/guard/denial.rs]

#[derive(Clone, Debug, PartialEq, Serialize)]
// NOTE: Denial does NOT derive Deserialize. The gate field is &'static str which
// cannot be deserialized from owned data (no 'static lifetime at deser time).
// The library never persists Denials — it returns them to callers.
// Products that want to persist denials serialize them into event payloads.
pub struct Denial {
    pub gate: &'static str,
    pub code: String,
    pub message: String,
    pub context: Vec<(String, String)>,
}

impl Denial {
    pub fn new(gate: &'static str, message: impl Into<String>) -> Self {
        Self {
            gate,
            code: String::new(),
            message: message.into(),
            context: vec![],
        }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = code.into();
        self
    }

    pub fn with_context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.push((key.into(), value.into()));
        self
    }
}

impl fmt::Display for Denial {
    /// "\[gate\] message"
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.gate, self.message)
    }
}
impl std::error::Error for Denial {}
