//! Sidecar for the `reflink_impl` mutation test, split out of fs.rs's inline
//! `mod tests` island to keep that island under the non-overridable
//! 200-nonblank cap. Linked from fs.rs via `#[path]` and gated to the platforms
//! where `reflink_impl` exists.

use super::reflink_impl;
use std::error::Error;

#[test]
fn reflink_impl_ok_status_iff_destination_is_an_exact_copy() -> Result<(), Box<dyn Error>> {
    // FS-independent invariant that pins BOTH reflink_impl mutants:
    //   * `result == 0` -> `!=` inverts success detection
    //   * whole body -> `Ok(())` skips the clone entirely (dest never written)
    // The clone syscall (FICLONE / clonefile) succeeds only on reflink-capable
    // filesystems, so we do NOT assert a fixed Ok/Err. Instead we assert the
    // load-bearing invariant that holds on EVERY filesystem: reflink_impl
    // returns Ok exactly when it produced a byte-exact destination copy.
    let dir = tempfile::tempdir()?;
    let from = dir.path().join("reflink-src.bin");
    let to = dir.path().join("reflink-dst.bin");
    let payload = b"reflink-invariant-payload";
    std::fs::write(&from, payload)?;

    let result = reflink_impl(&from, &to);
    let dest_is_exact_copy = std::fs::read(&to)
        .map(|bytes| bytes == payload)
        .unwrap_or(false);
    assert_eq!(
        result.is_ok(),
        dest_is_exact_copy,
        "INV-REFLINK: reflink_impl must return Ok exactly when it produced a \
         byte-exact destination copy. The `== -> !=` mutant reports the inverse \
         status (Err after a real clone, or Ok after a failed one) and the \
         `-> Ok(())` mutant returns Ok without ever writing the destination \
         (is_ok={}, dest_is_exact_copy={dest_is_exact_copy})",
        result.is_ok()
    );
    Ok(())
}
