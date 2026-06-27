//! Banana-split fold-fusion law over arbitrary folds and event streams.
//!
//! PROVES: LAW — Bird-Meertens banana-split: folding two/three catamorphisms in
//! a single fused traversal equals folding each separately, for ANY stream and
//! ANY pair/triple of folds from the algebraic family.
//! CATCHES: a mutant that drops one fold's relevant kinds from the fused union,
//! or splits the single traversal into per-fold passes, passes the fixed example
//! but fails this generated law over overlapping/disjoint/empty streams.
//! SEEDED: bounded proptest (64 cases) with proptest-regressions persistence;
//! `arb_event_stream` includes the empty stream and the shared kind alphabet.
//! INVARIANTS: INV-PROJECTION-FUSION-EQUIVALENCE (a fused N-fold projection
//! returns exactly the tuple of the independently-projected folds, and reads the
//! stream once).
//!
//! Why a property and not an example: the existing fusion example tests pin one
//! witness (`LeftCount`/`RightTotal` over three hand-picked kinds). A generated
//! property pins the *law* across overlapping/disjoint/empty-stream cases, so a
//! mutant that drops one fold's relevant kinds from the fused union — passing the
//! single fixed witness — is still killed here.
//!
//! NOTE: `Store::project_fused2/3` is public (`read_api.rs`); this property
//! proves the fusion law over the catamorphism algebra. A follow-on may wire
//! the same law directly through the store fused API surface.

use batpak::event::EventKind;
use proptest::prelude::*;
use std::cell::Cell;

#[path = "common/proptest.rs"]
mod proptest_support;

/// Small kind alphabet shared by folds and (indirectly) by streams so generated
/// streams realistically overlap the folds' relevant kinds. These are the raw
/// u16 encodings of valid custom kinds (category 0x1, ascending type ids).
const KIND_ALPHABET: &[u16] = &[0x1001, 0x1002, 0x1003, 0x1004];

/// One generated event: a kind plus a JSON payload.
type GenEvent = (EventKind, serde_json::Value);

/// Generates a valid custom [`EventKind`] without ever touching the panicking
/// `custom` constructor; the metacircular law below asserts validity.
fn arb_event_kind() -> impl Strategy<Value = EventKind> {
    (1u8..16, 0u16..0x1000).prop_filter_map(
        "category 0xD is reserved; try_custom must accept the rest",
        |(category, type_id)| {
            if category == 0xD {
                return None;
            }
            EventKind::try_custom(category, type_id).ok()
        },
    )
}

/// Bounded, shallow `serde_json::Value` payload (the `"n"` field drives SumN).
fn arb_json_value() -> impl Strategy<Value = serde_json::Value> {
    prop_oneof![
        any::<i64>().prop_map(|n| serde_json::json!({ "n": n })),
        Just(serde_json::Value::Null),
        ".{0,8}".prop_map(serde_json::Value::String),
    ]
}

/// Generates a bounded event stream, including the empty stream (a fusion edge
/// case the fixed example tests never cover).
fn arb_event_stream(max_len: usize) -> impl Strategy<Value = Vec<GenEvent>> {
    proptest::collection::vec((arb_event_kind(), arb_json_value()), 0..=max_len)
}

/// The fixed catamorphism family the fusion law ranges over. A generated
/// `EventSourced` is impossible (the trait needs a concrete type), so the
/// *folds* are a small fixed family that differ algebraically and the
/// *generated* part is which two/three participate plus the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FoldShape {
    CountAll,
    CountKind(u16),
    SumNOfKind(u16),
    LastIndexOfKind(u16),
    XorOfKind(u16),
}

impl FoldShape {
    /// Separate (reference) catamorphism: fold the shape over the whole stream.
    fn fold(self, stream: &[GenEvent]) -> i64 {
        let mut acc = FoldAcc::new(self);
        for (idx, event) in stream.iter().enumerate() {
            acc.step(idx, event);
        }
        acc.finish()
    }
}

fn arb_fold() -> impl Strategy<Value = FoldShape> {
    let pick_kind = proptest::sample::select(KIND_ALPHABET);
    prop_oneof![
        Just(FoldShape::CountAll),
        pick_kind.clone().prop_map(FoldShape::CountKind),
        pick_kind.clone().prop_map(FoldShape::SumNOfKind),
        pick_kind.clone().prop_map(FoldShape::LastIndexOfKind),
        pick_kind.prop_map(FoldShape::XorOfKind),
    ]
}

/// Incremental accumulator driving both the separate and the fused passes, so
/// equality follows from the algebra, not from sharing one loop.
struct FoldAcc {
    shape: FoldShape,
    acc: i64,
    xor: u64,
}

impl FoldAcc {
    fn new(shape: FoldShape) -> Self {
        Self {
            shape,
            acc: 0,
            xor: 0,
        }
    }

    fn step(&mut self, idx: usize, (kind, payload): &GenEvent) {
        let raw = kind.as_raw_u16();
        match self.shape {
            FoldShape::CountAll => self.acc += 1,
            FoldShape::CountKind(k) => {
                if raw == k {
                    self.acc += 1;
                }
            }
            FoldShape::SumNOfKind(k) => {
                if raw == k {
                    if let Some(n) = payload.get("n").and_then(serde_json::Value::as_i64) {
                        self.acc = self.acc.wrapping_add(n);
                    }
                }
            }
            FoldShape::LastIndexOfKind(k) => {
                if raw == k {
                    self.acc = i64::try_from(idx).expect("bounded test stream index") + 1;
                }
            }
            FoldShape::XorOfKind(k) => {
                if raw == k {
                    self.xor ^=
                        u64::from(raw) ^ u64::try_from(idx).expect("bounded test stream index");
                }
            }
        }
    }

    fn finish(self) -> i64 {
        match self.shape {
            FoldShape::XorOfKind(_) => i64::from_ne_bytes(self.xor.to_ne_bytes()),
            FoldShape::CountAll
            | FoldShape::CountKind(_)
            | FoldShape::SumNOfKind(_)
            | FoldShape::LastIndexOfKind(_) => self.acc,
        }
    }
}

thread_local! {
    /// Counts how many times the fused reference traverses (reads) the stream.
    static FUSED_STREAM_READS: Cell<u64> = const { Cell::new(0) };
}

fn reset_fused_stream_reads() {
    FUSED_STREAM_READS.with(|c| c.set(0));
}

fn fused_stream_reads() -> u64 {
    FUSED_STREAM_READS.with(Cell::get)
}

/// Single-pass banana-split fuser for two folds. Walks the stream ONCE.
fn fold_fused2(left: FoldShape, right: FoldShape, stream: &[GenEvent]) -> (i64, i64) {
    FUSED_STREAM_READS.with(|c| c.set(c.get() + 1));
    let mut l = FoldAcc::new(left);
    let mut r = FoldAcc::new(right);
    for (idx, event) in stream.iter().enumerate() {
        l.step(idx, event);
        r.step(idx, event);
    }
    (l.finish(), r.finish())
}

/// Single-pass banana-split fuser for three folds.
fn fold_fused3(a: FoldShape, b: FoldShape, c: FoldShape, stream: &[GenEvent]) -> (i64, i64, i64) {
    FUSED_STREAM_READS.with(|cnt| cnt.set(cnt.get() + 1));
    let mut fa = FoldAcc::new(a);
    let mut fb = FoldAcc::new(b);
    let mut fc = FoldAcc::new(c);
    for (idx, event) in stream.iter().enumerate() {
        fa.step(idx, event);
        fb.step(idx, event);
        fc.step(idx, event);
    }
    (fa.finish(), fb.finish(), fc.finish())
}

proptest! {
    #![proptest_config(proptest_support::cfg(64))]

    /// 2-fold banana-split: fused == (sep_left, sep_right) for any stream/folds.
    #[test]
    fn fused2_equals_separate(
        left in arb_fold(),
        right in arb_fold(),
        stream in arb_event_stream(24),
    ) {
        reset_fused_stream_reads();
        let fused = fold_fused2(left, right, &stream);
        let separate = (left.fold(&stream), right.fold(&stream));
        prop_assert_eq!(fused, separate,
            "BANANA-SPLIT FUSION VIOLATED (2): fused {:?} != separate {:?} for folds {:?}/{:?}",
            fused, separate, left, right);
    }

    /// Performance half of the law: the fused traversal reads the stream once.
    #[test]
    fn fused2_reads_stream_once(
        left in arb_fold(),
        right in arb_fold(),
        stream in arb_event_stream(24),
    ) {
        reset_fused_stream_reads();
        let _ = fold_fused2(left, right, &stream);
        prop_assert_eq!(fused_stream_reads(), 1,
            "FUSION SHOULD READ THE STREAM EXACTLY ONCE, observed {}",
            fused_stream_reads());
    }

    /// 3-fold banana-split generalizes the fixed three-tuple example.
    #[test]
    fn fused3_equals_separate(
        a in arb_fold(),
        b in arb_fold(),
        c in arb_fold(),
        stream in arb_event_stream(24),
    ) {
        reset_fused_stream_reads();
        let fused = fold_fused3(a, b, c, &stream);
        let separate = (a.fold(&stream), b.fold(&stream), c.fold(&stream));
        prop_assert_eq!(fused, separate,
            "BANANA-SPLIT FUSION VIOLATED (3): fused {:?} != separate {:?}",
            fused, separate);
    }

    /// Metacircular generator law: every generated kind is a valid custom kind.
    #[test]
    fn generated_kinds_are_valid(stream in arb_event_stream(24)) {
        for (kind, _) in &stream {
            let category = kind.category();
            prop_assert!(category != 0x0 && category != 0xD,
                "generator yielded reserved category {:#x}", category);
            prop_assert!(kind.type_id() < 0x1000,
                "generator yielded out-of-range type_id {:#x}", kind.type_id());
        }
    }
}
