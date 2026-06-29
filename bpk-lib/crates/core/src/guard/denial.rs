use crate::event::EventKind;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Denial: returned by a Gate when it rejects a proposal.
/// Separate from OutcomeError. Library does NOT auto-store denials.
/// Products decide whether to persist denials as events.

#[derive(Clone, Debug, PartialEq, Serialize)]
// NOTE: Denial does NOT derive Deserialize. The gate field is &'static str which
// cannot be deserialized from owned data (no 'static lifetime at deser time).
// The library never persists Denials — it returns them to callers.
// Products that want to persist denials serialize them into event payloads.
pub struct Denial {
    /// Name of the gate that issued this denial.
    pub gate: &'static str,
    /// Machine-readable error code for this denial.
    pub code: String,
    /// Human-readable description of why the proposal was denied.
    pub message: String,
    /// Key-value pairs providing additional context about the denial.
    pub context: Vec<(String, String)>,
}

impl Denial {
    /// Creates a new `Denial` from the gate name and a human-readable message.
    pub fn new(gate: &'static str, message: impl Into<String>) -> Self {
        Self {
            gate,
            code: String::new(),
            message: message.into(),
            context: vec![],
        }
    }

    /// Attaches a machine-readable error code to this denial.
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = code.into();
        self
    }

    /// Appends a key-value pair to the denial's context metadata.
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

/// Persistable gate identifier for denial tracing.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GateId(String);

/// Validation failure for [`GateId`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GateIdError {
    /// Gate identifiers must not be empty.
    Empty,
    /// Gate identifiers must be ASCII.
    NonAscii,
}

impl GateId {
    /// Construct a validated gate identifier.
    ///
    /// # Errors
    /// Returns [`GateIdError`] when the identifier is empty or contains
    /// non-ASCII bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, GateIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(GateIdError::Empty);
        }
        if !value.is_ascii() {
            return Err(GateIdError::NonAscii);
        }
        Ok(Self(value))
    }

    /// Construct a validated gate identifier from a gate `name()`.
    pub(crate) fn from_name(value: &str) -> Self {
        if let Ok(gate_id) = Self::new(value.to_owned()) {
            return gate_id;
        }
        debug_assert!(
            false,
            "gate names used for denial tracing must be non-empty ASCII: {value:?}"
        );
        let mut fallback = String::from("invalid:");
        for byte in value.as_bytes() {
            use std::fmt::Write as _;
            let _ = write!(&mut fallback, "{byte:02x}");
        }
        Self(fallback)
    }

    /// Borrow the gate identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Persisted verdict for one gate in a denial trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Verdict {
    /// Gate ran and permitted the proposal.
    Permit,
    /// Gate ran and denied the proposal.
    Deny {
        /// Machine-readable denial code.
        code: String,
        /// Human-readable denial message.
        message: String,
        /// Structured context key/value pairs attached to the denial.
        context: Vec<(String, String)>,
    },
    /// Gate was not evaluated because an earlier denial stopped the pipeline.
    Skipped,
}

/// Persisted evaluation result for one gate in a denial trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateEvaluation {
    gate_id: GateId,
    verdict: Verdict,
    evidence_hash: Option<[u8; 32]>,
}

impl GateEvaluation {
    /// Build a persisted gate evaluation.
    #[must_use]
    pub fn new(gate_id: GateId, verdict: Verdict, evidence_hash: Option<[u8; 32]>) -> Self {
        Self {
            gate_id,
            verdict,
            evidence_hash,
        }
    }

    /// Borrow the gate identifier for this evaluation.
    #[must_use]
    pub fn gate_id(&self) -> &GateId {
        &self.gate_id
    }

    /// Borrow the persisted verdict for this evaluation.
    #[must_use]
    pub fn verdict(&self) -> &Verdict {
        &self.verdict
    }

    /// Return the optional evidence hash attached to this evaluation.
    #[must_use]
    pub fn evidence_hash(&self) -> Option<[u8; 32]> {
        self.evidence_hash
    }
}

/// Persistable denial payload appended as `SYSTEM_DENIAL`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenialPayload {
    evaluations: Vec<GateEvaluation>,
    pipeline_id: Option<String>,
    proposed_kind: EventKind,
    proposed_content_hash: Option<[u8; 32]>,
}

impl DenialPayload {
    pub(crate) fn new(
        evaluations: Vec<GateEvaluation>,
        pipeline_id: Option<String>,
        proposed_kind: EventKind,
        proposed_content_hash: Option<[u8; 32]>,
    ) -> Self {
        Self {
            evaluations,
            pipeline_id,
            proposed_kind,
            proposed_content_hash,
        }
    }

    /// Borrow the ordered gate evaluations captured in this denial trace.
    #[must_use]
    pub fn evaluations(&self) -> &[GateEvaluation] {
        &self.evaluations
    }

    /// Return the optional pipeline identity attached to this trace.
    #[must_use]
    pub fn pipeline_id(&self) -> Option<&str> {
        self.pipeline_id.as_deref()
    }

    /// Return the event kind that was proposed before the denial.
    #[must_use]
    pub fn proposed_kind(&self) -> EventKind {
        self.proposed_kind
    }

    /// Return the optional proposed payload hash that was evaluated.
    #[must_use]
    pub fn proposed_content_hash(&self) -> Option<[u8; 32]> {
        self.proposed_content_hash
    }
}

impl fmt::Display for GateIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "gate id cannot be empty"),
            Self::NonAscii => write!(f, "gate id must be ASCII"),
        }
    }
}

impl std::error::Error for GateIdError {}
