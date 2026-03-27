use serde::{Deserialize, Serialize};
// NOTE: No `use crate::wire::*` needed here. The #[serde(with = "crate::wire::...")]
// annotations are string literal paths — serde resolves them at compile time, not
// through Rust's `use` mechanism. The wire module just needs to exist in the crate.

/// WaitCondition: what an Outcome::Pending is waiting for.
/// [SPEC:src/outcome/wait.rs]

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WaitCondition {
    Timeout {
        resume_at_ms: u64,
    },
    Event {
        #[serde(with = "crate::wire::u128_bytes")]
        event_id: u128,
    },
    All(Vec<WaitCondition>),
    Any(Vec<WaitCondition>),
    Custom {
        tag: u16,
        data: Vec<u8>,
    },
}

/// CompensationAction: what to do when an error needs compensation.
/// The writer persists this as data. Products implement the handler.
/// [SPEC:src/outcome/wait.rs — CompensationAction]

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CompensationAction {
    Rollback {
        #[serde(with = "crate::wire::vec_u128_bytes")]
        event_ids: Vec<u128>,
    },
    Notify {
        #[serde(with = "crate::wire::u128_bytes")]
        target_id: u128,
        message: String,
    },
    Release {
        #[serde(with = "crate::wire::vec_u128_bytes")]
        resource_ids: Vec<u128>,
    },
    Custom {
        action_type: String,
        data: Vec<u8>,
    },
}
