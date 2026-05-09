use crate::store::{AppendReceipt, HlcPoint, Open, Store, StoreError, WatermarkKind};
use std::time::Duration;

/// Inline append-time durability wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurabilityGate {
    /// Watermark that must cross the appended event's HLC.
    pub kind: WatermarkKind,
    /// Maximum time to wait for the watermark.
    ///
    /// `WaitTimeout` means the append committed but did not cross the
    /// requested watermark within `timeout`. The event is still in the log;
    /// query reflects it. Re-call `wait_for_<kind>` with a longer timeout if
    /// you need to re-acquire the guarantee.
    pub timeout: Duration,
}

impl Store<Open> {
    fn receipt_point(&self, receipt: &AppendReceipt) -> Result<HlcPoint, StoreError> {
        self.index
            .get_by_id(receipt.event_id)
            .map(|entry| HlcPoint {
                wall_ms: entry.wall_ms,
                global_sequence: entry.global_sequence,
            })
            .ok_or_else(|| StoreError::InvariantViolation {
                reason: format!(
                    "append receipt {:032x} was not visible for durability gate lookup",
                    receipt.event_id
                ),
            })
    }

    pub(crate) fn wait_for_gate(
        &self,
        receipt: &AppendReceipt,
        gate: DurabilityGate,
    ) -> Result<(), StoreError> {
        let target = self.receipt_point(receipt)?;
        match gate.kind {
            WatermarkKind::Durable => self.wait_for_durable(target, gate.timeout),
            WatermarkKind::Applied => self.wait_for_applied(target, gate.timeout),
            WatermarkKind::Visible => self.wait_for_visible(target, gate.timeout),
        }
    }
}
