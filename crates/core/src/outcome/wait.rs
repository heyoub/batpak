use serde::{Deserialize, Serialize};
// NOTE: No `use crate::wire::*` needed here. The #[serde(with = "crate::wire::...")]
// annotations are string literal paths — serde resolves them at compile time, not
// through Rust's `use` mechanism. The wire module just needs to exist in the crate.

/// WaitCondition: what an Outcome::Pending is waiting for.

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WaitCondition {
    /// Wait until the given Unix epoch millisecond timestamp is reached.
    Timeout {
        /// Timestamp in milliseconds at which the pending outcome may resume.
        resume_at_ms: u64,
    },
    /// Wait until a specific event is observed.
    Event {
        /// ID of the event that must arrive to resume this outcome.
        #[serde(with = "crate::wire::u128_bytes")]
        event_id: u128,
    },
    /// Wait until every contained condition is satisfied.
    All(Vec<WaitCondition>),
    /// Wait until at least one contained condition is satisfied.
    Any(Vec<WaitCondition>),
    /// A product-defined wait condition identified by a tag and opaque data.
    Custom {
        /// Numeric tag identifying the custom condition type.
        tag: u16,
        /// Opaque bytes carrying condition-specific parameters.
        data: Vec<u8>,
    },
}

/// CompensationAction: what to do when an error needs compensation.
/// The writer persists this as data. Products implement the handler.

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CompensationAction {
    /// Undo the effects of the listed events.
    Rollback {
        /// IDs of the events whose effects should be reversed.
        #[serde(with = "crate::wire::vec_u128_bytes")]
        event_ids: Vec<u128>,
    },
    /// Send a notification to a target aggregate or service.
    Notify {
        /// ID of the aggregate or endpoint to notify.
        #[serde(with = "crate::wire::u128_bytes")]
        target_id: u128,
        /// Message payload to deliver.
        message: String,
    },
    /// Release held resources back to the pool.
    Release {
        /// IDs of the resources to release.
        #[serde(with = "crate::wire::vec_u128_bytes")]
        resource_ids: Vec<u128>,
    },
    /// A product-defined compensation action identified by a string type tag.
    Custom {
        /// String identifier for the custom action type.
        action_type: String,
        /// Opaque bytes carrying action-specific parameters.
        data: Vec<u8>,
    },
}
