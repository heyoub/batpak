/// Delivery canal used by typed reactor runners.
///
/// This is intentionally a selector over existing primitives, not a new owner
/// of delivery semantics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReactorCanal {
    /// Ordered pull replay through [`Cursor`](crate::store::Cursor).
    ///
    /// This is the default typed-reactor canal. It is at-least-once within the
    /// process and can become durable at-least-once when the reactor carries a
    /// checkpoint id.
    #[default]
    CursorGuaranteed,
    /// Lossy push observation through [`Subscription`](crate::store::Subscription).
    ///
    /// This keeps writer isolation and does not checkpoint, restart, or provide
    /// an [`AtLeastOnce`](crate::store::AtLeastOnce) witness. Use it only for
    /// live views that may skip work under backpressure.
    LossySubscription,
}
