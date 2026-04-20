use super::{AppendReceipt, AppendReply, BatchAppendReply, StoreError};

struct Ticket<T> {
    rx: flume::Receiver<Result<T, StoreError>>,
}

impl<T> Ticket<T> {
    fn new(rx: flume::Receiver<Result<T, StoreError>>) -> Self {
        Self { rx }
    }

    fn wait(self) -> Result<T, StoreError> {
        self.rx.recv().map_err(|_| StoreError::WriterCrashed)?
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
pub struct AppendTicket {
    inner: Ticket<AppendReceipt>,
}

impl AppendTicket {
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
pub struct BatchAppendTicket {
    inner: Ticket<Vec<AppendReceipt>>,
}

impl BatchAppendTicket {
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
