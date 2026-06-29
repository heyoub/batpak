#![cfg(feature = "dangerous-test-hooks")]

use std::sync::Arc;

use batpak::prelude::StoreError;
use batpak::store::{CountdownAction, FaultInjector, InjectionPoint, ProbabilisticInjector};

#[test]
fn probabilistic_injector_and_maybe_inject_surface_faults() {
    let point = InjectionPoint::BatchItemWritten {
        batch_id: 7,
        item_index: 0,
        total_items: 1,
    };
    let injector: Option<Arc<dyn FaultInjector>> = Some(Arc::new(ProbabilisticInjector::new(
        1.0,
        CountdownAction::Fail("boom"),
    )));

    let err = batpak::store::fault::maybe_inject(point, &injector)
        .expect_err("PROPERTY: probability=1.0 must force fault injection");

    assert!(
        matches!(err, StoreError::FaultInjected(ref message) if message.contains("boom")),
        "PROPERTY: maybe_inject must propagate the ProbabilisticInjector error, got {err:?}"
    );
}
