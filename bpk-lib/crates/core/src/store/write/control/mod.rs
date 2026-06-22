#[cfg(feature = "dangerous-test-hooks")]
use super::writer::CooperativePump;
use super::writer::{AppendGuards, WriterCommand, WriterHandle};
use crate::store::{AppendReceipt, StoreError};

mod fence;
mod outbox;
mod store_bridge;
mod submission;
mod ticket;

pub use self::fence::VisibilityFence;
pub use self::outbox::Outbox;
pub(crate) use self::submission::AppendSubmission;
pub use self::ticket::{AppendTicket, BatchAppendTicket};

pub(crate) type AppendReply = Result<AppendReceipt, StoreError>;
pub(crate) type BatchAppendReply = Result<Vec<AppendReceipt>, StoreError>;

#[cfg(test)]
mod tests {
    use super::submission::AppendSubmission;

    #[test]
    fn reaction_with_zero_causation_yields_none() {
        let clock = crate::store::SystemClock::new();
        let submission = AppendSubmission::reaction(&clock, 42, 0);
        assert_eq!(
            submission.options.causation_id, None,
            "causation_id=0 is the wire sentinel — reaction() must not produce Some(0)"
        );
    }

    #[test]
    fn reaction_with_nonzero_causation_is_preserved() {
        let clock = crate::store::SystemClock::new();
        let submission = AppendSubmission::reaction(&clock, 42, 99);
        assert_eq!(
            submission.options.causation_id,
            Some(crate::id::CausationId::from(99u128))
        );
    }
}
