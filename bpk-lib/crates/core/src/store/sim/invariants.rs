//! Per-step safety invariants for the simulation.
//!
//! These mirror the store's durability contract on the simulator's own model
//! state, so a determinism or fault-injection regression trips a clear,
//! seed-reproducible assertion rather than corrupting silently. The three
//! checked properties:
//!
//!   * **hash-chain continuity** — each recorded event's `prev` hash equals the
//!     running chain head, so the log is an unbroken Merkle-style chain.
//!   * **monotonic visible frontier** — the visible frontier never moves
//!     backward across steps.
//!   * **no-loss-after-crash-recover** — after a simulated crash, every event
//!     that was acknowledged as durable is still present on recovery.
//!
//! The engine checks model-only state for fast determinism tests. Real-Store
//! safety over sim backends is proven via the fork/import/recovery DST corpus
//! (`recovery.rs`, `fork_recovery.rs`, `import_recovery.rs`).

/// A single logged event in the simulation model.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SimEvent {
    /// Monotonic sequence number assigned at append.
    pub(crate) seq: u64,
    /// Chain head this event extended (the previous event's `hash`).
    pub(crate) prev: u64,
    /// This event's chain hash (`fold(prev, seq, payload)`).
    pub(crate) hash: u64,
    /// Whether the writer acknowledged this event as durable (fsynced).
    pub(crate) durable: bool,
}

/// Running model state checked by the invariants after every workload step.
#[derive(Default)]
pub(crate) struct ModelState {
    /// The append-ordered event log.
    pub(crate) log: Vec<SimEvent>,
    /// Highest visible frontier observed so far (monotonic).
    pub(crate) visible_frontier: u64,
}

/// A safety violation discovered by an invariant check. Carries enough context
/// to reproduce under `BATPAK_SEED`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Violation {
    /// `event.prev` did not match the running chain head.
    ChainBreak {
        /// Sequence number of the offending event.
        seq: u64,
        /// Chain head expected at that point.
        expected_prev: u64,
        /// `prev` the event actually recorded.
        found_prev: u64,
    },
    /// The visible frontier regressed.
    FrontierRegression {
        /// Highest frontier seen before the regression.
        previous: u64,
        /// The smaller value observed.
        observed: u64,
    },
    /// A durably-acked event went missing after crash/recover.
    DurableLoss {
        /// Sequence number that should have survived.
        seq: u64,
    },
}

impl ModelState {
    /// Append `event`, advancing the frontier when it is durable. The caller
    /// links the event's `prev` to [`Self::chain_head`] (the last *durable*
    /// hash). Only durable events enter the hash chain — a torn or unsynced
    /// event is recorded for the no-loss/frontier view but does not advance the
    /// chain, mirroring the real store where only committed events are chained.
    pub(crate) fn append(&mut self, event: SimEvent) {
        if event.durable {
            self.visible_frontier = self.visible_frontier.max(event.seq);
        }
        self.log.push(event);
    }

    /// The current hash-chain head: the hash of the last *durable* event, or 0
    /// for an empty (or all-non-durable) chain. Callers link the next event's
    /// `prev` to this so the durable chain stays continuous across torn writes.
    pub(crate) fn chain_head(&self) -> u64 {
        self.log
            .iter()
            .rev()
            .find(|e| e.durable)
            .map_or(0, |e| e.hash)
    }

    /// Verify hash-chain continuity (over durable events) and frontier
    /// monotonicity. `prev_frontier` is the frontier observed at the previous
    /// step; the current frontier must be `>=` it.
    ///
    /// # Errors
    /// Returns [`Violation::FrontierRegression`] if the frontier moved backward,
    /// or [`Violation::ChainBreak`] if any durable event's `prev` link is
    /// discontinuous with the prior durable event's hash.
    pub(crate) fn check(&self, prev_frontier: u64) -> Result<(), Violation> {
        if self.visible_frontier < prev_frontier {
            return Err(Violation::FrontierRegression {
                previous: prev_frontier,
                observed: self.visible_frontier,
            });
        }
        let mut head = 0u64;
        for event in self.log.iter().filter(|e| e.durable) {
            if event.prev != head {
                return Err(Violation::ChainBreak {
                    seq: event.seq,
                    expected_prev: head,
                    found_prev: event.prev,
                });
            }
            head = event.hash;
        }
        Ok(())
    }

    /// After a simulated crash that truncated the log to its durable prefix,
    /// confirm every event that was acked durable is still present.
    ///
    /// # Errors
    /// Returns [`Violation::DurableLoss`] for the first durably-acked event in
    /// `pre_crash` that did not survive into the recovered log.
    pub(crate) fn check_no_loss(&self, pre_crash: &[SimEvent]) -> Result<(), Violation> {
        for event in pre_crash.iter().filter(|e| e.durable) {
            let survived = self.log.iter().any(|e| e.seq == event.seq);
            if !survived {
                return Err(Violation::DurableLoss { seq: event.seq });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, prev: u64, hash: u64, durable: bool) -> SimEvent {
        SimEvent {
            seq,
            prev,
            hash,
            durable,
        }
    }

    #[test]
    fn continuous_chain_passes() {
        let mut m = ModelState::default();
        m.append(ev(0, 0, 11, true));
        m.append(ev(1, 11, 22, true));
        assert!(m.check(0).is_ok(), "PROPERTY: an unbroken chain passes");
        assert_eq!(m.visible_frontier, 1);
    }

    #[test]
    fn chain_break_is_detected() {
        let mut m = ModelState::default();
        m.append(ev(0, 0, 11, true));
        m.append(ev(1, 999, 22, true)); // wrong prev
        assert_eq!(
            m.check(0),
            Err(Violation::ChainBreak {
                seq: 1,
                expected_prev: 11,
                found_prev: 999
            }),
            "PROPERTY: a broken prev-link is caught with full context"
        );
    }

    #[test]
    fn frontier_regression_is_detected() {
        let m = ModelState::default();
        assert_eq!(
            m.check(5),
            Err(Violation::FrontierRegression {
                previous: 5,
                observed: 0
            }),
            "PROPERTY: a visible-frontier regression is caught"
        );
    }

    #[test]
    fn durable_loss_after_crash_is_detected() {
        let pre = vec![ev(0, 0, 11, true), ev(1, 11, 22, true)];
        let mut recovered = ModelState::default();
        recovered.append(ev(0, 0, 11, true)); // seq 1 lost
        assert_eq!(
            recovered.check_no_loss(&pre),
            Err(Violation::DurableLoss { seq: 1 }),
            "PROPERTY: a durably-acked event missing after recovery is a no-loss violation"
        );
    }
}
