#![no_main]
//! GAUNT-FUZZ-1 — `checkpoint_snapshot_v6` target.
//!
//! Drives `batpak::__fuzz::__fuzz_checkpoint_snapshot_v6(body)`, which forwards
//! into `decode_checkpoint_snapshot_v6` — the v6 checkpoint snapshot decoder. Like
//! the generic checkpoint path, a corrupt snapshot must be IGNORED (typed `None`),
//! never panic.
//!
//! This decoder takes only a body (no version scalar — the version is fixed at v6),
//! so the whole input is the msgpack body.
//!
//! S1 contract: (a) never panic; (b) no unbounded allocation; (c) a valid v6 body
//! returns `true`; (d) a corrupt body returns `false`, never a panic.

use libfuzzer_sys::fuzz_target;

use batpak::__fuzz::__fuzz_checkpoint_snapshot_v6;

fuzz_target!(|data: &[u8]| {
    // (a)+(c)+(d): never panic; the bool discriminates Some (true) / None (false).
    let decoded = __fuzz_checkpoint_snapshot_v6(data);
    let _ = decoded;
});
