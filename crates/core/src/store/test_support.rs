use crate::store::write::writer::WriterCommand;
use crate::store::{Store, StoreError};

/// Test helper: trigger a panic in the writer thread to exercise restart_policy.
/// Returns Ok(()) if the panic command was sent and acknowledged by the writer.
/// After calling this, the writer will panic and (if restart_policy allows) restart.
/// Wait briefly after calling to let the restart complete before sending more commands.
#[doc(hidden)]
impl Store {
    /// Test-only: trigger a panic in the writer thread to exercise restart_policy.
    pub fn panic_writer_for_test(&self) -> Result<(), StoreError> {
        let (tx, rx) = flume::bounded(1);
        self.writer
            .as_ref()
            .ok_or(StoreError::WriterCrashed)?
            .tx
            .send(WriterCommand::PanicForTest { respond: tx })
            .map_err(|_| StoreError::WriterCrashed)?;
        let _ = rx.recv_timeout(std::time::Duration::from_millis(500));
        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(())
    }
}
