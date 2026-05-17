use crate::store::config::duration_micros;
use crate::store::{
    AppendReceipt, HlcPoint, Open, Store, StoreError, StoreInvariant, WatermarkKind,
};
use std::time::{Duration, Instant};

struct GateWaitMeasurement {
    result: Result<(), StoreError>,
    waited_us: u64,
}

fn measure_gate_wait<ResolveTarget, WaitForTarget>(
    resolve_target: ResolveTarget,
    wait_for_target: WaitForTarget,
) -> Result<GateWaitMeasurement, StoreError>
where
    ResolveTarget: FnOnce() -> Result<HlcPoint, StoreError>,
    WaitForTarget: FnOnce(HlcPoint) -> Result<(), StoreError>,
{
    let target = resolve_target()?;
    let started = Instant::now();
    let result = wait_for_target(target);
    Ok(GateWaitMeasurement {
        waited_us: duration_micros(started.elapsed()),
        result,
    })
}

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
                kind: StoreInvariant::GateReceiptNotIndexed {
                    event_id: receipt.event_id,
                },
            })
    }

    pub(crate) fn wait_for_gate(
        &self,
        receipt: &AppendReceipt,
        gate: DurabilityGate,
    ) -> Result<(), StoreError> {
        let measurement = measure_gate_wait(
            || self.receipt_point(receipt),
            |target| match gate.kind {
                WatermarkKind::Durable => self.wait_for_durable(target, gate.timeout),
                WatermarkKind::Applied => self.wait_for_applied(target, gate.timeout),
                WatermarkKind::Visible => self.wait_for_visible(target, gate.timeout),
            },
        )?;
        tracing::trace!(
            target: "batpak::durability_gate",
            kind = ?gate.kind,
            waited_us = measurement.waited_us,
            ok = measurement.result.is_ok(),
            "append durability gate wait completed",
        );
        measurement.result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[test]
    fn gate_wait_measurement_excludes_receipt_lookup_work() {
        let lookup_complete = Arc::new(AtomicBool::new(false));
        let wait_saw_lookup_complete = Arc::clone(&lookup_complete);
        let before_lookup = Instant::now();

        let measurement = measure_gate_wait(
            || {
                std::thread::sleep(Duration::from_millis(40));
                lookup_complete.store(true, Ordering::SeqCst);
                Ok(HlcPoint {
                    wall_ms: 1,
                    global_sequence: 1,
                })
            },
            |_| {
                assert!(
                    wait_saw_lookup_complete.load(Ordering::SeqCst),
                    "gate wait must start only after receipt_point lookup completes"
                );
                Ok(())
            },
        )
        .expect("measurement succeeds");

        let lookup_inclusive_us = duration_micros(before_lookup.elapsed());
        assert!(
            measurement.waited_us < lookup_inclusive_us / 2,
            "waited_us must measure only the watermark wait window, not receipt lookup work; waited_us={} lookup_inclusive_us={lookup_inclusive_us}",
            measurement.waited_us
        );
        assert!(measurement.result.is_ok());
    }
}
