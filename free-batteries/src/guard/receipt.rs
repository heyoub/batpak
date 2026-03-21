/// Receipt<T>: proof that all gates passed. Consumed exactly once.
/// The seal module prevents external construction. Only GateSet::evaluate() creates these.
/// [SPEC:src/guard/receipt.rs — TOCTOU fix]
pub struct Receipt<T> {
    _seal: seal::Token,
    gates_passed: Vec<&'static str>,
    payload: T,
}

mod seal {
    /// Private module. Token cannot be constructed outside guard/.
    pub(crate) struct Token;
}

/// Receipt is NOT Clone, NOT Copy, NOT Serialize.
/// It wraps the payload INSIDE so it can't be mutated after gate evaluation.
/// Consumed via into_parts().
impl<T> Receipt<T> {
    /// Only callable from within the crate (seal::Token is pub(crate)).
    /// [FILE:src/guard/mod.rs — GateSet::evaluate() is the only caller]
    pub(crate) fn new(payload: T, gates_passed: Vec<&'static str>) -> Self {
        Self {
            _seal: seal::Token,
            gates_passed,
            payload,
        }
    }

    pub fn payload(&self) -> &T {
        &self.payload
    }
    pub fn gates_passed(&self) -> &[&'static str] {
        &self.gates_passed
    }

    /// Consuming extraction. After this, the receipt is gone.
    pub fn into_parts(self) -> (T, Vec<&'static str>) {
        (self.payload, self.gates_passed)
    }
}
