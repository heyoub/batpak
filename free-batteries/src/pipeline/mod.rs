use crate::guard::{Denial, GateSet, Receipt};
use serde::{Deserialize, Serialize};

pub mod bypass;
pub use bypass::{BypassReason, BypassReceipt};

/// Proposal<T>: wraps a value for gate evaluation.
/// [SPEC:src/pipeline/mod.rs]
pub struct Proposal<T>(pub T);

/// Committed<T>: proof that an event was persisted.
/// [SPEC:src/pipeline/mod.rs]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Committed<T> {
    pub payload: T,
    #[serde(with = "crate::wire::u128_bytes")]
    pub event_id: u128,
    pub sequence: u64,
    pub hash: [u8; 32], // blake3, or [0u8;32] if feature off
}

/// Pipeline<Ctx>: evaluate gates then commit.
/// [SPEC:src/pipeline/mod.rs]
pub struct Pipeline<Ctx> {
    gates: GateSet<Ctx>,
}

impl<T> Proposal<T> {
    pub fn new(payload: T) -> Self {
        Self(payload)
    }

    pub fn payload(&self) -> &T {
        &self.0
    }

    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Proposal<U> {
        Proposal(f(self.0))
    }
}

impl<Ctx> Pipeline<Ctx> {
    pub fn new(gates: GateSet<Ctx>) -> Self {
        Self { gates }
    }

    pub fn evaluate<T>(&self, ctx: &Ctx, proposal: Proposal<T>) -> Result<Receipt<T>, Denial> {
        self.gates.evaluate(ctx, proposal)
    }

    /// commit: generic over error type E. Pipeline doesn't know about StoreError.
    /// Products pass a closure that calls store.append() and wraps the result.
    /// [SPEC:IMPLEMENTATION NOTES item 9 — Pipeline::commit() E is generic]
    pub fn commit<T, E>(
        &self,
        receipt: Receipt<T>,
        commit_fn: impl FnOnce(T) -> Result<Committed<T>, E>,
    ) -> Result<Committed<T>, E> {
        let (payload, _gate_names) = receipt.into_parts();
        commit_fn(payload)
    }

    /// bypass: skip gates with an auditable reason.
    /// [FILE:src/pipeline/bypass.rs]
    pub fn bypass<T>(proposal: Proposal<T>, reason: &'static dyn BypassReason) -> BypassReceipt<T> {
        BypassReceipt {
            payload: proposal.0,
            reason: reason.name(),
            justification: reason.justification(),
        }
    }

    /// commit_bypass: persist a bypassed proposal through the same commit path.
    /// Mirrors commit() but takes a BypassReceipt instead of a Receipt.
    /// [SPEC:src/pipeline/mod.rs — Pipeline::commit_bypass]
    pub fn commit_bypass<T, E>(
        receipt: BypassReceipt<T>,
        commit_fn: impl FnOnce(T) -> Result<Committed<T>, E>,
    ) -> Result<Committed<T>, E> {
        commit_fn(receipt.payload)
    }
}
