/// BypassReason: products implement this to justify skipping gates.
pub trait BypassReason: Send + Sync {
    /// Returns the short name identifying this bypass reason.
    fn name(&self) -> &'static str;
    /// Returns the full justification text explaining why gates were skipped.
    fn justification(&self) -> &'static str;
}

/// Audit trail carried by a [`crate::pipeline::Committed`] when the commit
/// came from a gate bypass rather than a gate-evaluated [`crate::guard::Receipt`].
///
/// Per G8, reason + justification (+ approver, when products choose to track
/// one) must survive past `commit_bypass` so downstream observers can tell a
/// bypass-committed event from one that passed gate evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BypassAudit {
    /// Short reason name (`BypassReason::name()`) — stable identifier for
    /// the bypass class.
    pub reason: String,
    /// Full justification (`BypassReason::justification()`) — human-readable
    /// explanation for the audit log.
    pub justification: String,
    /// Optional approver identity. `None` when the bypass is
    /// caller-asserted without an explicit approver; products can populate
    /// this with an operator id, ticket reference, or similar.
    pub approved_by: Option<String>,
}

impl BypassAudit {
    /// Build a `BypassAudit` from a reason/justification pair with no
    /// explicit approver recorded.
    pub fn new(reason: impl Into<String>, justification: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            justification: justification.into(),
            approved_by: None,
        }
    }

    /// Attach an approver identity to this audit entry.
    pub fn with_approved_by(mut self, approver: impl Into<String>) -> Self {
        self.approved_by = Some(approver.into());
        self
    }
}

/// `BypassReceipt<T>`: audit trail shows "bypassed: {reason}".
/// Fields are `pub(crate)` to prevent external forgery — use getters for read access.
pub struct BypassReceipt<T> {
    /// The proposal payload that bypassed gate evaluation.
    pub(crate) payload: T,
    /// Short name identifying the bypass reason.
    pub(crate) reason: &'static str,
    /// Full justification text explaining why gates were skipped.
    pub(crate) justification: &'static str,
    /// Optional approver identity recorded alongside the bypass reason.
    pub(crate) approved_by: Option<String>,
}

impl<T> BypassReceipt<T> {
    /// The original proposal payload.
    pub fn payload(&self) -> &T {
        &self.payload
    }

    /// The bypass reason name.
    pub fn reason(&self) -> &'static str {
        self.reason
    }

    /// The bypass justification text.
    pub fn justification(&self) -> &'static str {
        self.justification
    }

    /// The recorded approver identity, if any.
    pub fn approved_by(&self) -> Option<&str> {
        self.approved_by.as_deref()
    }

    /// Attach an approver identity to this receipt. Defaults to `None`
    /// when `Pipeline::bypass` created the receipt without an approver.
    pub fn with_approved_by(mut self, approver: impl Into<String>) -> Self {
        self.approved_by = Some(approver.into());
        self
    }

    /// Consume the receipt and return the payload.
    pub fn into_payload(self) -> T {
        self.payload
    }

    /// Build a structured [`BypassAudit`] snapshot of this receipt for
    /// attachment to the resulting [`crate::pipeline::Committed`] value.
    pub(crate) fn to_audit(&self) -> BypassAudit {
        BypassAudit {
            reason: self.reason.to_string(),
            justification: self.justification.to_string(),
            approved_by: self.approved_by.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BypassReceipt;

    #[test]
    fn approved_by_reports_the_recorded_approver_not_a_constant() {
        // `approved_by -> Some("xyzzy")` would forge an approver onto every
        // receipt. A receipt built without one must report None, and one built
        // with `with_approved_by` must echo exactly that approver.
        let bare = BypassReceipt {
            payload: 7u32,
            reason: "test-reason",
            justification: "test-justification",
            approved_by: None,
        };
        assert_eq!(
            bare.approved_by(),
            None,
            "a receipt with no approver must report None, not a hardcoded Some(_)"
        );

        let signed = bare.with_approved_by("operator:alice");
        assert_eq!(
            signed.approved_by(),
            Some("operator:alice"),
            "approved_by must return the recorded approver, not a constant"
        );
    }
}
