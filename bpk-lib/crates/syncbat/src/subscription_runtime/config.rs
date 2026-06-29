use super::error::SubscriptionRuntimeError;

/// Runtime limits for subscription delivery sessions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubscriptionRuntimeConfig {
    /// Server-side maximum client window clamp.
    pub server_max_window: usize,
    /// Page size for `query_entries_after` replay/live passes.
    pub query_page_size: usize,
}

impl Default for SubscriptionRuntimeConfig {
    fn default() -> Self {
        Self {
            server_max_window: 256,
            query_page_size: 64,
        }
    }
}

impl SubscriptionRuntimeConfig {
    /// Construct with explicit limits.
    #[must_use]
    pub const fn new(server_max_window: usize, query_page_size: usize) -> Self {
        Self {
            server_max_window,
            query_page_size,
        }
    }

    /// Validate that the configured runtime can make progress.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::InvalidConfig`] when a limit is zero.
    pub fn validate(&self) -> Result<(), SubscriptionRuntimeError> {
        if self.server_max_window == 0 {
            return Err(SubscriptionRuntimeError::InvalidConfig {
                reason: "server max window is zero",
            });
        }
        if self.query_page_size == 0 {
            return Err(SubscriptionRuntimeError::InvalidConfig {
                reason: "query page size is zero",
            });
        }
        Ok(())
    }
}
