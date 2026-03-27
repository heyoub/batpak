/// BypassReason: products implement this to justify skipping gates.
/// [SPEC:src/pipeline/bypass.rs]
pub trait BypassReason: Send + Sync {
    fn name(&self) -> &'static str;
    fn justification(&self) -> &'static str;
}

/// `BypassReceipt<T>`: audit trail shows "bypassed: {reason}".
/// Fields are `pub(crate)` to prevent external forgery — use getters for read access.
pub struct BypassReceipt<T> {
    pub(crate) payload: T,
    pub(crate) reason: &'static str,
    pub(crate) justification: &'static str,
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

    /// Consume the receipt and return the payload.
    pub fn into_payload(self) -> T {
        self.payload
    }
}
