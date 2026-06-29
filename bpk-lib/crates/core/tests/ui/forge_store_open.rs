//! Compile-fail: attempting to construct an open `Store<Open>` via a struct
//! literal outside the crate.
//!
//! `Store`'s fields and the `Open` typestate's writer-handle field are all
//! `pub(crate)`, so a downstream forge of an "open" store (one that claims to
//! own a writer handle it never created) is unreachable. This pins the
//! typestate invariant INV-TYPESTATE-OPEN-HAS-WRITER: an `Open` store can only
//! be produced by `Store::open`, which actually spawns the writer.

use batpak::store::{Open, Store};

fn main() {
    // This should fail: every field of `Store` and the writer handle inside
    // `Open` are `pub(crate)`, so the struct literal cannot be named here.
    let _store: Store<Open> = Store::<Open> {
        index: unimplemented!(),
        reader: unimplemented!(),
        cache: unimplemented!(),
        watermark_handle: unimplemented!(),
        projection_registry: unimplemented!(),
        lifecycle_gate: unimplemented!(),
        config: unimplemented!(),
        runtime: unimplemented!(),
        should_shutdown_on_drop: false,
        open_report: None,
        cumulative_reserved_kind_fallbacks: unimplemented!(),
        state: unimplemented!(),
    };
}
