//! Compile-fail: Attempting to construct a Receipt<T> outside the crate.
//! The seal::Token is pub(crate), so Receipt::new() is unreachable.
//! This test verifies the TOCTOU seal is enforced.

fn main() {
    // This should fail: Receipt::new() requires seal::Token which is pub(crate)
    let _receipt = batpak::guard::Receipt::<i32>::new(42, vec!["fake_gate"]);
}
