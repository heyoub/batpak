//! Seeded workload + op-trace digest.
//!
//! [`run`] drives a small, seeded sequence of operations over the three
//! simulation backends ([`SimClock`], [`SimScheduler`], [`SimFs`]) and the
//! model state, checking the [`invariants`] after every step and folding each
//! op into a FNV-1a digest. The digest is the determinism witness: two runs
//! from the same seed visit the same ops in the same order and therefore return
//! the same digest. `BATPAK_SEED=N` selects the seed.
//!
//! The workload PRNG is seeded independently of the [`SimFs`] fault PRNG so the
//! two streams never cross-contaminate (changing op selection must not perturb
//! which faults fire, and vice versa).

use super::invariants::{ModelState, SimEvent};
use super::Sim;
use crate::store::fault::InjectionPoint;
use std::path::Path;

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Fold one `u64` token into a running FNV-1a digest.
fn fold(digest: u64, token: u64) -> u64 {
    let mut d = digest;
    for byte in token.to_le_bytes() {
        d ^= u64::from(byte);
        d = d.wrapping_mul(FNV_PRIME);
    }
    d
}

/// Chain-hash combiner: deterministic fold of the previous head, the sequence,
/// and a payload token into the next chain head.
fn chain_hash(prev: u64, seq: u64, payload: u64) -> u64 {
    fold(fold(fold(FNV_OFFSET, prev), seq), payload)
}

/// Widen a `usize` length into a digest token without a lossy `as` cast.
/// Saturates on the impossible >64-bit platform rather than truncating.
fn usize_token(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Run a `steps`-long seeded workload, returning the op-trace digest on success
/// or a seed-tagged violation description if a safety invariant tripped.
///
/// Each step selects an op from the workload PRNG: append, fsync, advance-clock,
/// spawn-background-work, or crash/recover. After every step the invariants are
/// checked against the model; a violation short-circuits with `Err` rather than
/// panicking, so the integration test can assert on it cleanly.
///
/// # Errors
/// Returns a seed-tagged description string if a hash-chain, frontier, or
/// no-loss invariant is violated at any step.
pub(crate) fn run(sim: &Sim, steps: usize) -> Result<u64, String> {
    let mut wl = fastrand::Rng::with_seed(sim.seed);
    let mut model = ModelState::default();
    let mut digest = FNV_OFFSET;
    let mut next_seq = 0u64;
    let seg = Path::new("sim-seg.fbat");

    digest = fold(digest, sim.seed);

    for step in 0..steps {
        let prev_frontier = model.visible_frontier;
        // Advance logical time every step so timestamps stay strictly forward.
        let dt = i64::from(wl.u16(..)) + 1;
        let now = sim.clock.advance_us(dt);
        digest = fold(digest, now.cast_unsigned());

        match wl.u32(..) % 6 {
            // Append a single event, write its bytes (maybe torn), and record.
            // Appends are weighted 3-in-6 so the log grows faster than it crashes.
            0..=2 => {
                let payload = wl.u64(..);
                let prev = model.chain_head();
                let hash = chain_hash(prev, next_seq, payload);
                let point = InjectionPoint::SingleAppendStart {
                    entity: "sim".to_string(),
                };
                let landed = sim.fs.write_bytes(seg, &point, &payload.to_le_bytes());
                // Durable only if the full payload landed AND a later fsync
                // honors it; for the model we mark durable on a full landing
                // plus an honored sync below.
                let full = landed == 8;
                let synced = full
                    && sim.fs.fsync(
                        seg,
                        &InjectionPoint::SingleAppendWritten {
                            entity: "sim".to_string(),
                        },
                    );
                model.append(SimEvent {
                    seq: next_seq,
                    prev,
                    hash,
                    durable: synced,
                });
                digest = fold(fold(digest, next_seq), hash);
                next_seq += 1;
            }
            // Explicit fsync of the segment.
            3 => {
                let honored = sim
                    .fs
                    .fsync(seg, &InjectionPoint::BatchFsync { batch_id: next_seq });
                digest = fold(digest, u64::from(honored));
            }
            // Spawn cooperative background work; drain deterministically.
            4 => {
                let token = wl.u64(..);
                let handle = sim.scheduler.spawn_owned(Box::new(move || {
                    // Pure body: its only effect is having run; the digest
                    // folds the token so the spawn is observable.
                    std::hint::black_box(token);
                }));
                let joined = handle.join().is_ok();
                digest = fold(fold(digest, token), u64::from(joined));
            }
            // Simulated crash + recover: snapshot durable view, crash, rebuild
            // the model log from what survived, and assert no durable loss.
            _ => {
                let pre: Vec<SimEvent> = model.log.clone();
                let durable_bytes = sim.fs.durable_len(seg);
                let live_bytes = sim.fs.len(seg);
                // Read back the durable prefix; under short-read faults this may
                // deliver fewer bytes, which folds into the digest deterministically.
                let recovered_bytes = sim
                    .fs
                    .read_bytes(
                        seg,
                        &InjectionPoint::ColdStartScanFrame {
                            segment_id: 0,
                            frame_index: step,
                        },
                        0,
                        durable_bytes,
                    )
                    .len();
                digest = fold(
                    fold(digest, usize_token(live_bytes)),
                    usize_token(recovered_bytes),
                );
                sim.fs.crash();
                // Rebuild the recovered model: keep exactly the durably-acked
                // prefix (the contract the real store guarantees on recover).
                let recovered: Vec<SimEvent> = pre.iter().copied().filter(|e| e.durable).collect();
                let mut rebuilt = ModelState::default();
                for ev in &recovered {
                    rebuilt.append(*ev);
                }
                rebuilt
                    .check_no_loss(&pre)
                    .map_err(|v| format!("no-loss violation (seed={}): {v:?}", sim.seed))?;
                model = rebuilt;
                digest = fold(digest, usize_token(durable_bytes));
            }
        }

        // Per-step safety check; reproduce with BATPAK_SEED on failure.
        model.check(prev_frontier).map_err(|v| {
            format!(
                "invariant violation at step {step} (seed={}): {v:?}",
                sim.seed
            )
        })?;
        digest = fold(digest, model.visible_frontier);
    }

    // Drain any background work spawned but not explicitly joined, so the run
    // ends with a quiescent scheduler (deterministic: nothing is left pending).
    sim.scheduler.run_all();

    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::super::Sim;
    use super::usize_token;

    #[test]
    fn usize_token_widens_value_losslessly() {
        // A body-stubbing mutant collapses every input to `0`; a non-zero input
        // pins the lossless widening so that mutant cannot survive.
        assert_eq!(
            usize_token(5),
            5,
            "PROPERTY: usize_token widens the length value, it does not zero it"
        );
        assert_eq!(
            usize_token(0),
            0,
            "PROPERTY: zero widens to zero (boundary)"
        );
        assert_eq!(
            usize_token(usize::MAX),
            u64::try_from(usize::MAX).unwrap_or(u64::MAX),
            "PROPERTY: a 64-bit-or-narrower platform widens usize::MAX without truncation"
        );
    }

    #[test]
    fn workload_digest_is_stable_across_runs() {
        let a = Sim::new(0xABCD).run_workload(128).expect("invariants hold");
        let b = Sim::new(0xABCD).run_workload(128).expect("invariants hold");
        assert_eq!(
            a, b,
            "PROPERTY: a seeded workload yields a byte-stable op-trace digest"
        );
    }

    #[test]
    fn step_count_affects_digest() {
        let short = Sim::new(5).run_workload(8).expect("invariants hold");
        let long = Sim::new(5).run_workload(64).expect("invariants hold");
        assert_ne!(
            short, long,
            "PROPERTY: more steps fold more ops into the digest"
        );
    }
}
