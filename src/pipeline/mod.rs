use crate::guard::{Denial, GateSet, Receipt};
use crate::store::StoreError;

/// Bypass types for skipping gate evaluation with an auditable reason.
pub mod bypass;
pub use bypass::{BypassReason, BypassReceipt};

/// `Proposal<T>`: wraps a value for gate evaluation.
pub struct Proposal<T>(
    /// The payload to be evaluated and committed.
    pub(crate) T,
);

/// `Committed<T>`: proof that an event was persisted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Committed<T> {
    /// The committed event payload.
    payload: T,
    /// Proof metadata attached to the committed payload.
    metadata: CommitMetadata,
}

/// Narrow commit proof metadata returned by persistence closures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitMetadata {
    /// Unique identifier assigned to this event by the store.
    event_id: u128,
    /// Monotonically increasing sequence number within the stream.
    sequence: u64,
    /// Content hash of the committed payload (blake3, or all-zeros if feature is off).
    hash: [u8; 32],
    /// True when this metadata describes the genesis marker. Genesis metadata
    /// is an explicit marker; sequence `0` is also a legitimate store-assigned
    /// global sequence for the first real event.
    is_genesis: bool,
}

/// `Pipeline<Ctx>`: evaluate gates then commit.
pub struct Pipeline<Ctx> {
    /// The set of gates applied during proposal evaluation.
    gates: GateSet<Ctx>,
}

impl<T> Proposal<T> {
    /// Wraps `payload` in a new `Proposal` ready for gate evaluation.
    pub fn new(payload: T) -> Self {
        Self(payload)
    }

    /// Returns a reference to the wrapped payload without consuming the proposal.
    pub fn payload(&self) -> &T {
        &self.0
    }

    /// Transforms the wrapped payload, producing a `Proposal` of a different type.
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Proposal<U> {
        Proposal(f(self.0))
    }
}

impl<T> Committed<T> {
    pub(crate) fn new(payload: T, metadata: CommitMetadata) -> Self {
        Self { payload, metadata }
    }

    /// Returns the committed payload.
    pub fn payload(&self) -> &T {
        &self.payload
    }

    /// Returns the committed event identifier.
    pub fn event_id(&self) -> u128 {
        self.metadata.event_id
    }

    /// Returns the committed sequence number.
    pub fn sequence(&self) -> u64 {
        self.metadata.sequence
    }

    /// Returns the committed payload hash.
    pub fn hash(&self) -> &[u8; 32] {
        &self.metadata.hash
    }

    /// Consumes the proof and returns the payload.
    pub fn into_payload(self) -> T {
        self.payload
    }

    /// Consumes the proof and returns the payload plus narrow metadata.
    pub fn into_parts(self) -> (T, CommitMetadata) {
        (self.payload, self.metadata)
    }
}

impl CommitMetadata {
    /// Creates explicit commit proof metadata for a persisted payload.
    ///
    /// # Errors
    /// Returns [`StoreError::InvalidCommitMetadata`] if the supplied
    /// metadata fails [`CommitMetadata::validate`].
    pub fn new(event_id: u128, sequence: u64, hash: [u8; 32]) -> Result<Self, StoreError> {
        let built = Self {
            event_id,
            sequence,
            hash,
            is_genesis: false,
        };
        built.validate()?;
        Ok(built)
    }

    /// Constructs explicit genesis metadata. Genesis is a semantic marker, not
    /// shorthand for "sequence zero"; ordinary store appends can also carry
    /// sequence `0` for the first committed event.
    pub const fn genesis(event_id: u128, hash: [u8; 32]) -> Self {
        Self {
            event_id,
            sequence: 0,
            hash,
            is_genesis: true,
        }
    }

    /// Creates metadata from a store append receipt when no content hash is available.
    ///
    /// Append receipts always describe a non-genesis commit, so this
    /// constructor runs the same validation as [`CommitMetadata::new`].
    ///
    /// # Errors
    /// Returns [`StoreError::InvalidCommitMetadata`] if the append receipt
    /// would produce invalid commit metadata.
    pub fn from_append_receipt(receipt: crate::store::AppendReceipt) -> Result<Self, StoreError> {
        Self::new(receipt.event_id, receipt.sequence, [0u8; 32])
    }

    /// Validate that this metadata represents a legal commit.
    ///
    /// # Errors
    /// Returns [`StoreError::InvalidCommitMetadata`] if the metadata claims to
    /// be genesis while carrying a non-zero sequence.
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.is_genesis && self.sequence != 0 {
            return Err(StoreError::InvalidCommitMetadata {
                reason: "genesis metadata must carry sequence=0".into(),
            });
        }
        Ok(())
    }

    /// Returns the committed event identifier.
    pub const fn event_id(self) -> u128 {
        self.event_id
    }

    /// Returns the committed sequence number.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Returns the committed payload hash.
    pub const fn hash(self) -> [u8; 32] {
        self.hash
    }

    /// Returns `true` when this metadata represents the genesis marker.
    pub const fn is_genesis(self) -> bool {
        self.is_genesis
    }
}

impl<Ctx> Pipeline<Ctx> {
    /// Creates a new `Pipeline` backed by the given gate set.
    pub fn new(gates: GateSet<Ctx>) -> Self {
        Self { gates }
    }

    /// Runs all gates against `ctx`; returns a `Receipt` on success or the first `Denial`.
    ///
    /// # Errors
    /// Returns the first `Denial` produced by any gate in the pipeline's gate set.
    pub fn evaluate<T>(&self, ctx: &Ctx, proposal: Proposal<T>) -> Result<Receipt<T>, Denial> {
        self.gates.evaluate(ctx, proposal)
    }

    /// commit: generic over error type E. Pipeline doesn't know about StoreError.
    /// Products pass a closure that inspects the payload and returns commit metadata.
    ///
    /// The metadata returned by the closure is revalidated before the
    /// `Committed<T>` is assembled; a failed revalidation is surfaced as
    /// `StoreError::InvalidCommitMetadata` (converted into `E`).
    ///
    /// # Errors
    /// Returns `Err(E)` if the caller-supplied `commit_fn` closure fails,
    /// or if the metadata it returns fails revalidation.
    pub fn commit<T, E>(
        &self,
        receipt: Receipt<T>,
        commit_fn: impl FnOnce(&T) -> Result<CommitMetadata, E>,
    ) -> Result<Committed<T>, E>
    where
        E: From<StoreError>,
    {
        let (payload, _gate_names) = receipt.into_parts();
        let metadata = commit_fn(&payload)?;
        metadata.validate().map_err(E::from)?;
        Ok(Committed::new(payload, metadata))
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
    ///
    /// The metadata returned by the closure is revalidated before the
    /// `Committed<T>` is assembled; a failed revalidation is surfaced as
    /// `StoreError::InvalidCommitMetadata` (converted into `E`).
    ///
    /// # Errors
    /// Returns `Err(E)` if the caller-supplied `commit_fn` closure fails,
    /// or if the metadata it returns fails revalidation.
    pub fn commit_bypass<T, E>(
        receipt: BypassReceipt<T>,
        commit_fn: impl FnOnce(&T) -> Result<CommitMetadata, E>,
    ) -> Result<Committed<T>, E>
    where
        E: From<StoreError>,
    {
        let payload = receipt.into_payload();
        let metadata = commit_fn(&payload)?;
        metadata.validate().map_err(E::from)?;
        Ok(Committed::new(payload, metadata))
    }
}
