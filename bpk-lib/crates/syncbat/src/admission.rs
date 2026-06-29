//! Pre-handler admission guard seam.
//!
//! A [`crate::Core`] may run a single admission guard BEFORE dispatching to a
//! handler. The guard inspects the resolved descriptor and the input bytes (and
//! may stamp opaque receipt metadata onto the invocation [`crate::Ctx`]) and
//! either admits the call or denies it. A denial short-circuits dispatch: the
//! handler never runs, and the runtime records a
//! [`crate::ReceiptOutcome::Denied`] receipt. This is the ONLY place `Core`
//! dispatch emits `Denied`.
//!
//! The seam is generic: a guard sees only a descriptor name + input bytes and
//! attaches only opaque extension bytes. It carries no downstream policy
//! vocabulary — admission *meaning* lives in the caller's guard implementation.

use crate::core::Ctx;
use crate::operation::OperationDescriptor;

/// Decision returned by an [`AdmissionGuard`].
///
/// `#[non_exhaustive]` so the admission vocabulary can grow (e.g. a future
/// `Defer`) without breaking downstream exhaustive matches.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AdmissionDecision {
    /// Admit the call; dispatch proceeds to the handler.
    Admit,
    /// Deny the call before handler execution. The runtime records a `Denied`
    /// receipt carrying this class/detail and returns a denial error.
    Deny {
        /// Stable denial class.
        code: String,
        /// Human-readable denial detail.
        message: String,
    },
}

impl AdmissionDecision {
    /// Construct a denial decision.
    #[must_use]
    pub fn deny(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Deny {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Return true when the decision admits the call.
    #[must_use]
    pub const fn is_admit(&self) -> bool {
        matches!(self, Self::Admit)
    }
}

/// Pre-handler admission guard run by [`crate::Core`] before handler dispatch.
pub trait AdmissionGuard {
    /// Decide whether to admit the resolved call before the handler runs.
    ///
    /// The guard may attach opaque receipt metadata to `cx` (e.g. a denial
    /// reason, or correlation identity) regardless of the decision; the runtime
    /// drains that metadata into the recorded receipt.
    fn admit(
        &self,
        descriptor: &OperationDescriptor,
        input: &[u8],
        cx: &mut Ctx<'_>,
    ) -> AdmissionDecision;
}

impl<F> AdmissionGuard for F
where
    F: Fn(&OperationDescriptor, &[u8], &mut Ctx<'_>) -> AdmissionDecision,
{
    fn admit(
        &self,
        descriptor: &OperationDescriptor,
        input: &[u8],
        cx: &mut Ctx<'_>,
    ) -> AdmissionDecision {
        self(descriptor, input, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::AdmissionDecision;

    #[test]
    fn deny_carries_class_and_detail() {
        // Pins the constructor: a stub returning `Admit` would silently let a
        // rejected call through.
        let decision = AdmissionDecision::deny("policy", "blocked");
        assert!(!decision.is_admit());
        assert_eq!(
            decision,
            AdmissionDecision::Deny {
                code: "policy".to_owned(),
                message: "blocked".to_owned(),
            }
        );
    }

    #[test]
    fn admit_is_admit() {
        assert!(AdmissionDecision::Admit.is_admit());
    }
}
