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
    kind: WatermarkKind,
    /// Maximum time to wait for the watermark.
    ///
    /// `WaitTimeout` means the append committed but did not cross the
    /// requested watermark within `timeout`. The event is still in the log;
    /// query reflects it. Re-call `wait_for_<kind>` with a longer timeout if
    /// you need to re-acquire the guarantee.
    timeout: Duration,
}

impl DurabilityGate {
    /// Construct a gate waiting up to `timeout` for `kind` to cross the append.
    #[must_use]
    pub const fn new(kind: WatermarkKind, timeout: Duration) -> Self {
        Self { kind, timeout }
    }

    /// Watermark that must cross the appended event's HLC.
    pub fn kind(&self) -> WatermarkKind {
        self.kind
    }

    /// Maximum time to wait for the watermark.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl Store<Open> {
    fn receipt_point(&self, receipt: &AppendReceipt) -> Result<HlcPoint, StoreError> {
        use crate::id::EntityIdType;
        let raw = receipt.event_id.as_u128();
        self.index
            .get_by_id(raw)
            .map(|entry| HlcPoint {
                wall_ms: entry.wall_ms,
                global_sequence: entry.global_sequence,
            })
            .ok_or_else(|| StoreError::InvariantViolation {
                kind: StoreInvariant::GateReceiptNotIndexed { event_id: raw },
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
                WatermarkKind::Accepted => self.wait_for_accepted(target, gate.timeout),
                WatermarkKind::Written => self.wait_for_written(target, gate.timeout),
                WatermarkKind::Durable => self.wait_for_durable(target, gate.timeout),
                WatermarkKind::Applied => self.wait_for_applied(target, gate.timeout),
                WatermarkKind::Visible => self.wait_for_visible(target, gate.timeout),
                WatermarkKind::Emitted => self.wait_for_emitted(target, gate.timeout),
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
    fn timeout_accessor_returns_the_configured_wait_not_default() {
        // `DurabilityGate::timeout -> Default::default()` would always report a
        // zero Duration; the accessor must echo the configured wait.
        let gate = DurabilityGate::new(WatermarkKind::Durable, Duration::from_millis(250));
        assert_eq!(
            gate.timeout(),
            Duration::from_millis(250),
            "DurabilityGate::timeout must return the configured wait, not Duration::default()"
        );
        assert_ne!(
            gate.timeout(),
            Duration::default(),
            "a non-zero configured timeout must never read back as the default zero"
        );
    }

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
