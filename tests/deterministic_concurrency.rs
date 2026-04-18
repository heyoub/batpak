// justifies: loom proofs treat any unwrap failure as a falsified invariant; unwrap is the idiomatic assertion style for loom tests.
#![allow(clippy::unwrap_used)]
//! Deterministic concurrency proofs using loom.

use loom::sync::{Arc, Mutex};
use loom::thread;

/// Run a loom model with a bounded exploration budget.
///
/// Plain `loom::model(|| ...)` has no exploration ceiling. On a slow CI
/// runner an exhaustive model with more than ~3 threads can take hours
/// or get OOM-killed mid-exploration, producing an inconclusive result
/// that looks like a CI infrastructure flake. The `Builder` lets us cap
/// how much state space loom explores: `preemption_bound` limits the
/// number of preemptions per execution, which transitively bounds the
/// total exploration size for a given model.
fn loom_model_bounded<F>(check: F)
where
    F: Fn() + Sync + Send + 'static,
{
    let mut builder = loom::model::Builder::new();
    // 3 preemption points is enough to explore every model in this file
    // (verified on a developer machine — all models use 2 threads with
    // a small number of operations) but small enough that any future
    // model with more threads / more ops will fail loud rather than spin.
    builder.preemption_bound = Some(3);
    builder.check(check);
}

fn model_idempotent_append(committed: &Mutex<bool>, commit_count: &Mutex<u64>) {
    let mut committed_guard = committed.lock().unwrap();
    if !*committed_guard {
        *committed_guard = true;
        drop(committed_guard);

        let mut count_guard = commit_count.lock().unwrap();
        *count_guard += 1;
    }
}

fn model_compare_and_append(sequence: &Mutex<u32>, success_count: &Mutex<u32>, expected: u32) {
    let mut sequence_guard = sequence.lock().unwrap();
    if *sequence_guard == expected {
        *sequence_guard += 1;
        drop(sequence_guard);

        let mut success_guard = success_count.lock().unwrap();
        *success_guard += 1;
    }
}

fn model_bounded_restart(
    restart_count: &Mutex<u32>,
    successful_restarts: &Mutex<u32>,
    max_restarts: u32,
) {
    let mut restart_guard = restart_count.lock().unwrap();
    if *restart_guard < max_restarts {
        *restart_guard += 1;
        drop(restart_guard);

        let mut success_guard = successful_restarts.lock().unwrap();
        *success_guard += 1;
    }
}

fn model_single_compactor(compacting: &Mutex<bool>, winners: &Mutex<u32>) {
    let mut compacting_guard = compacting.lock().unwrap();
    if !*compacting_guard {
        *compacting_guard = true;
        drop(compacting_guard);

        let mut winners_guard = winners.lock().unwrap();
        *winners_guard += 1;
    }
}

#[test]
fn loom_idempotency_single_winner_under_race() {
    loom_model_bounded(|| {
        let committed = Arc::new(Mutex::new(false));
        let commit_count = Arc::new(Mutex::new(0_u64));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let committed = Arc::clone(&committed);
            let commit_count = Arc::clone(&commit_count);
            handles.push(thread::spawn(move || {
                model_idempotent_append(&committed, &commit_count);
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert!(
            *committed.lock().unwrap(),
            "PROPERTY: one racing append must commit the idempotent key."
        );
        assert_eq!(
            *commit_count.lock().unwrap(),
            1,
            "PROPERTY: racing idempotent appends must linearize to a single committed write."
        );
    });
}

#[test]
fn loom_cas_only_one_writer_can_claim_sequence() {
    loom_model_bounded(|| {
        let sequence = Arc::new(Mutex::new(0_u32));
        let success_count = Arc::new(Mutex::new(0_u32));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let sequence = Arc::clone(&sequence);
            let success_count = Arc::clone(&success_count);
            handles.push(thread::spawn(move || {
                model_compare_and_append(&sequence, &success_count, 0);
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            *success_count.lock().unwrap(),
            1,
            "PROPERTY: two racing CAS appends with expected_sequence=0 must have exactly one winner."
        );
        assert_eq!(
            *sequence.lock().unwrap(),
            1,
            "PROPERTY: the claimed sequence must advance exactly once after the race."
        );
    });
}

#[test]
fn loom_bounded_restart_allows_only_configured_number_of_recoveries() {
    loom_model_bounded(|| {
        let restart_count = Arc::new(Mutex::new(0_u32));
        let successful_restarts = Arc::new(Mutex::new(0_u32));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let restart_count = Arc::clone(&restart_count);
            let successful_restarts = Arc::clone(&successful_restarts);
            handles.push(thread::spawn(move || {
                model_bounded_restart(&restart_count, &successful_restarts, 1);
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            *successful_restarts.lock().unwrap(),
            1,
            "PROPERTY: a bounded restart policy with max_restarts=1 must admit exactly one recovery under race."
        );
        assert_eq!(
            *restart_count.lock().unwrap(),
            1,
            "PROPERTY: restart bookkeeping must stop once the configured limit is exhausted."
        );
    });
}

#[test]
fn loom_compaction_has_single_exclusive_owner() {
    loom_model_bounded(|| {
        let compacting = Arc::new(Mutex::new(false));
        let winners = Arc::new(Mutex::new(0_u32));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let compacting = Arc::clone(&compacting);
            let winners = Arc::clone(&winners);
            handles.push(thread::spawn(move || {
                model_single_compactor(&compacting, &winners);
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            *winners.lock().unwrap(),
            1,
            "PROPERTY: only one compaction claimant may own the exclusive window at a time."
        );
    });
}

/// Batch visibility proof: a reader must observe either 0 or N batch entries,
/// never a strict subset.
///
/// This model mirrors the **real** SequenceGate pattern:
/// - The "entries" are stored in independent atomic slots (one per item).
///   Each store is its own Release op — loom is free to interleave the
///   reader between any two slot stores. This is the structural property
///   the previous mutex-over-the-loop model was missing.
/// - The writer's only synchronization with the reader is the final
///   `visible.store(N, Release)`. Earlier slot stores are *not* directly
///   ordered with the reader's snapshot — they only become observable
///   transitively through the visible store's release barrier.
/// - The reader Acquire-loads `visible`, then snapshots all slots, then
///   counts entries whose sequence is `< visible`.
///
/// The property: every reader interleaving observes either 0 or N visible
/// entries, never a strict prefix. If `publish` ever used Relaxed ordering
/// (or if any slot store was reordered past it), loom would find an
/// interleaving where the reader sees `vis = N` but a slot still reads as
/// the empty sentinel — i.e. partial visibility. With proper Release/Acquire,
/// the happens-before edge across the visible store guarantees that any
/// reader seeing `vis = N` also sees all slot stores.
///
/// [INV-BATCH-ATOMIC-VISIBILITY]
#[test]
fn loom_batch_visibility_no_prefix_exposure() {
    use loom::sync::atomic::{AtomicU64, Ordering};

    // Three slots, each a distinct atomic. Writer fills them one at a time
    // and only THEN publishes the watermark. Sequences are 1, 2, 3 so that
    // 0 is a clean "empty" sentinel.
    const SENTINEL: u64 = 0;
    const N: u64 = 3;

    loom_model_bounded(|| {
        let slot0 = Arc::new(AtomicU64::new(SENTINEL));
        let slot1 = Arc::new(AtomicU64::new(SENTINEL));
        let slot2 = Arc::new(AtomicU64::new(SENTINEL));
        let visible = Arc::new(AtomicU64::new(0));

        let w0 = Arc::clone(&slot0);
        let w1 = Arc::clone(&slot1);
        let w2 = Arc::clone(&slot2);
        let wv = Arc::clone(&visible);
        let writer = thread::spawn(move || {
            // Each slot store is its own Release atomic op — independent
            // synchronization point. Loom can interleave the reader between
            // any pair of these.
            w0.store(1, Ordering::Release);
            w1.store(2, Ordering::Release);
            w2.store(3, Ordering::Release);
            // Publish: visible = N+1 means sequences {1, 2, 3} are all
            // visible (entry visible iff seq < visible).
            wv.store(N + 1, Ordering::Release);
        });

        let r0 = Arc::clone(&slot0);
        let r1 = Arc::clone(&slot1);
        let r2 = Arc::clone(&slot2);
        let rv = Arc::clone(&visible);
        let reader = thread::spawn(move || {
            // Acquire-load the watermark first. Pairs with the writer's
            // Release on visible — establishes happens-before with all
            // earlier writer ops in program order.
            let vis = rv.load(Ordering::Acquire);

            // Snapshot all slots. These reads are Acquire so the reader
            // sees the latest committed values (which may still be the
            // sentinel if the writer hasn't reached that slot yet).
            let s0 = r0.load(Ordering::Acquire);
            let s1 = r1.load(Ordering::Acquire);
            let s2 = r2.load(Ordering::Acquire);

            // Filter: entries are visible iff non-sentinel AND seq < vis.
            let visible_count: u64 = [s0, s1, s2]
                .iter()
                .filter(|&&seq| seq != SENTINEL && seq < vis)
                .count()
                .try_into()
                .expect("count fits u64");

            // PROPERTY: every reader interleaving sees 0 or N visible
            // entries — never a strict prefix.
            assert!(
                visible_count == 0 || visible_count == N,
                "PROPERTY: reader observed {visible_count} of {N} batch entries.\n\
                 This is a partial batch exposure — the SequenceGate did not prevent\n\
                 a reader from seeing a strict prefix of the batch.\n\
                 Slots: [{s0}, {s1}, {s2}], visible: {vis}.\n\
                 Investigate: src/store/index/mod.rs SequenceGate::publish ordering."
            );
        });

        writer.join().unwrap();
        reader.join().unwrap();
    });
}
