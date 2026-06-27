use flume::Receiver;

use batpak::store::Freshness;

use super::config::SubscriptionRuntimeConfig;
use super::error::SubscriptionRuntimeError;
use super::session::{SessionControl, SubscriptionSession, SubscriptionStore};

/// Route binding passed to projection projectors at session open.
#[derive(Clone, Debug)]
pub struct ProjectionRouteBinding {
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Route-declared projection id.
    pub projection_id: String,
    /// Entity coordinate bound to the projection.
    pub entity: String,
    /// Wire `payload_schema_ref` token for stream envelopes.
    pub wire_payload_schema_ref: String,
    /// Optional inner projection schema ref carried inside the envelope.
    pub inner_projection_schema_ref: Option<String>,
    /// Freshness mode for projection materialization.
    pub freshness: Freshness,
    /// Optional route-specific queue clamp.
    pub backpressure_capacity: Option<usize>,
}

/// syncbat-owned projector that opens typed projection subscription sessions.
pub trait ProjectionProjector: Send + Sync {
    /// Open one projection subscription session for a validated route binding.
    ///
    /// # Errors
    /// Cursor, store, or runtime configuration failures.
    fn open(
        &self,
        store: SubscriptionStore,
        route: ProjectionRouteBinding,
        resume_cursor: Option<&[u8]>,
        client_window: u32,
        control_rx: Receiver<SessionControl>,
        config: SubscriptionRuntimeConfig,
    ) -> Result<Box<dyn SubscriptionSession>, SubscriptionRuntimeError>;
}
