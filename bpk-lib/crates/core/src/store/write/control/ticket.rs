#[cfg(feature = "dangerous-test-hooks")]
use super::CooperativePump;
use super::{AppendReceipt, AppendReply, BatchAppendReply, StoreError};

struct Ticket<T> {
    rx: flume::Receiver<Result<T, StoreError>>,
    /// Cooperative-mode pump, or `None` on the threaded path. When present,
    /// `wait` drains the writer queue inline before blocking on the receive, so
    /// the awaited reply is already produced (there is no writer thread). Only
    /// exists under `dangerous-test-hooks`, where cooperative mode is available.
    #[cfg(feature = "dangerous-test-hooks")]
    pump: Option<CooperativePump>,
}

impl<T> Ticket<T> {
    #[cfg(feature = "dangerous-test-hooks")]
    fn new(rx: flume::Receiver<Result<T, StoreError>>, pump: Option<CooperativePump>) -> Self {
        Self { rx, pump }
    }

    #[cfg(not(feature = "dangerous-test-hooks"))]
    fn new(rx: flume::Receiver<Result<T, StoreError>>) -> Self {
        Self { rx }
    }

    fn wait(self) -> Result<T, StoreError> {
        #[cfg(feature = "dangerous-test-hooks")]
        if let Some(pump) = &self.pump {
            pump.pump();
        }
        crate::store::recv_writer_reply(&self.rx)
    }

    fn try_check(&self) -> Option<Result<T, StoreError>> {
        match self.rx.try_recv() {
            Ok(value) => Some(value),
            Err(flume::TryRecvError::Disconnected) => Some(Err(StoreError::WriterCrashed)),
            Err(flume::TryRecvError::Empty) => None,
        }
    }

    fn receiver(&self) -> &flume::Receiver<Result<T, StoreError>> {
        &self.rx
    }
}

/// Nonblocking handle for a single append result.
#[must_use = "an AppendTicket must be awaited (`.wait()`) or polled; dropping it discards the append outcome and any writer error"]
pub struct AppendTicket {
    inner: Ticket<AppendReceipt>,
}

impl AppendTicket {
    #[cfg(feature = "dangerous-test-hooks")]
    pub(crate) fn new(rx: flume::Receiver<AppendReply>, pump: Option<CooperativePump>) -> Self {
        Self {
            inner: Ticket::new(rx, pump),
        }
    }

    #[cfg(not(feature = "dangerous-test-hooks"))]
    pub(crate) fn new(rx: flume::Receiver<AppendReply>) -> Self {
        Self {
            inner: Ticket::new(rx),
        }
    }

    /// Wait for the writer to finish this append.
    ///
    /// # Errors
    /// Returns [`StoreError::WriterCrashed`] if the writer exits before sending
    /// a reply, or any append error returned by the writer.
    pub fn wait(self) -> AppendReply {
        self.inner.wait()
    }

    /// Check whether the append result is ready without blocking.
    pub fn try_check(&self) -> Option<AppendReply> {
        self.inner.try_check()
    }

    /// Expose the underlying receiver for optional async interop.
    pub fn receiver(&self) -> &flume::Receiver<AppendReply> {
        self.inner.receiver()
    }
}

/// Nonblocking handle for a batch append result.
#[must_use = "a BatchAppendTicket must be awaited (`.wait()`) or polled; dropping it discards the batch outcome and any writer error"]
pub struct BatchAppendTicket {
    inner: Ticket<Vec<AppendReceipt>>,
}

impl BatchAppendTicket {
    #[cfg(feature = "dangerous-test-hooks")]
    pub(crate) fn new(
        rx: flume::Receiver<BatchAppendReply>,
        pump: Option<CooperativePump>,
    ) -> Self {
        Self {
            inner: Ticket::new(rx, pump),
        }
    }

    #[cfg(not(feature = "dangerous-test-hooks"))]
    pub(crate) fn new(rx: flume::Receiver<BatchAppendReply>) -> Self {
        Self {
            inner: Ticket::new(rx),
        }
    }

    /// Wait for the writer to finish this batch.
    ///
    /// # Errors
    /// Returns [`StoreError::WriterCrashed`] if the writer exits before sending
    /// a reply, or any batch-append error returned by the writer.
    pub fn wait(self) -> BatchAppendReply {
        self.inner.wait()
    }

    /// Check whether the batch result is ready without blocking.
    pub fn try_check(&self) -> Option<BatchAppendReply> {
        self.inner.try_check()
    }

    /// Expose the underlying receiver for optional async interop.
    pub fn receiver(&self) -> &flume::Receiver<BatchAppendReply> {
        self.inner.receiver()
    }
}
