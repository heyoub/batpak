/// BypassReason: products implement this to justify skipping gates.
/// [SPEC:src/pipeline/bypass.rs]
pub trait BypassReason: Send + Sync {
    fn name(&self) -> &'static str;
    fn justification(&self) -> &'static str;
}

/// `BypassReceipt<T>`: audit trail shows "bypassed: {reason}".
pub struct BypassReceipt<T> {
    pub payload: T,
    pub reason: &'static str,
    pub justification: &'static str,
}
