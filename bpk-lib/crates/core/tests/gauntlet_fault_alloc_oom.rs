#![cfg(feature = "fault-alloc")]
//! GAUNT-FAULT-ALLOC: deterministic allocation-failure (OOM) injection.
//!
//! PROVES: [`FailingAlloc`] arms/disarms deterministically — once armed to fail
//! at the Kth allocation it returns null for that allocation (modeling OOM),
//! and after `disarm()` allocations succeed again. This is the harness used to
//! exercise OOM-handling paths; the contract here is the allocator shim itself.
//!
//! DEDICATED single-test binary because a `#[global_allocator]` is process-wide.
//! Run with `--features fault-alloc`.
//!
//! Slug: GAUNT-FAULT-ALLOC / gauntlet_fault_alloc_oom

use batpak::store::alloc::FailingAlloc;

#[global_allocator]
static ALLOC: FailingAlloc = FailingAlloc::new();

#[test]
fn failing_alloc_arms_and_disarms_deterministically() {
    // Unarmed: allocation succeeds.
    let v: Vec<u8> = Vec::with_capacity(64);
    assert_eq!(v.capacity(), 64, "unarmed allocator must succeed");
    drop(v);

    // Arm to fail on the very next allocation, then attempt one in a way that
    // observes the null without UB: `try_reserve` surfaces allocation failure
    // as a typed error instead of aborting.
    FailingAlloc::fail_after(1);
    let mut probe: Vec<u8> = Vec::new();
    let armed_result = probe.try_reserve(4096);
    // Disarm IMMEDIATELY so the assertion machinery below can allocate freely.
    FailingAlloc::disarm();

    assert!(
        armed_result.is_err(),
        "PROPERTY: armed FailingAlloc must surface allocation failure (got Ok)"
    );

    // Disarmed: allocation succeeds again.
    let mut after: Vec<u8> = Vec::new();
    after
        .try_reserve(4096)
        .expect("disarmed allocator must succeed");
    assert!(after.capacity() >= 4096);
}
