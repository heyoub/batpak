//! The monster: SimBackend + the harness-owned GroundTruth shadow + the
//! G1–G13 proof grid + the startup-reconciliation matrix.
//!
//! Boundary: compiled out entirely unless `dangerous-test-hooks` is on, exactly
//! like batpak's `store/sim` (`crates/core/src/store/sim/mod.rs`).
//!
//! [`backend`] is a [`crate::Backend`] that LIES deterministically: a
//! [`backend::LieMode`] (sibling of batpak's `FaultMode`) + a
//! [`backend::LieInjector`] (sibling of `FaultInjector`), one seeded PRNG
//! advanced once per consultation, so the same seed yields the same lie
//! sequence. [`ground_truth`] records what ACTUALLY happened INDEPENDENTLY of
//! the backend's self-report; the [`grid`] and [`reconciliation_matrix`] oracles
//! diff GroundTruth vs the sealed [`crate::BoundaryReport`].
//!
//! THE MONSTER NEVER GRADES ITSELF — the oracle is harness-owned, the same
//! separation as recovery_matrix reopening a real store and classifying
//! independently.

pub(crate) mod backend;
pub(crate) mod grid;
pub(crate) mod ground_truth;
pub(crate) mod reconciliation_matrix;

/// FNV-1a 64-bit offset basis, matching batpak's recovery-matrix digest
/// (`crates/core/src/store/sim/recovery.rs`).
pub(crate) const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Fold one `u64` token into a running FNV-1a digest. Byte-for-byte the same
/// mixing batpak's recovery matrix uses, so determinism digests read alike.
pub(crate) fn fold(digest: u64, token: u64) -> u64 {
    let mut d = digest;
    for byte in token.to_le_bytes() {
        d ^= u64::from(byte);
        d = d.wrapping_mul(FNV_PRIME);
    }
    d
}

/// Read the replay seed from `BVISOR_SEED` (falling back to `BATPAK_SEED`, then
/// the supplied `default`). The single entry point the oracles use so that
/// `BVISOR_SEED=N cargo test ...` deterministically replays a sweep.
pub(crate) fn seed_from_env(default: u64) -> u64 {
    for key in ["BVISOR_SEED", "BATPAK_SEED"] {
        if let Ok(raw) = std::env::var(key) {
            if let Ok(parsed) = raw.trim().parse::<u64>() {
                return parsed;
            }
        }
    }
    default
}

/// A tiny deterministic splitmix64 PRNG. Self-contained (no `fastrand` dep) so
/// the monster's lie sequence is reproducible from a seed with zero external
/// state — the same seed advances to the same sequence of lies.
#[derive(Clone, Debug)]
pub(crate) struct Prng {
    state: u64,
}

impl Prng {
    /// Seed the PRNG.
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Advance once and return the next pseudo-random `u64` (splitmix64).
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_matches_batpak_mixing_shape() {
        // Folding distinct tokens diverges; folding the same token from the same
        // base converges (determinism), mirroring the recovery-matrix digest.
        let a = fold(FNV_OFFSET, 1);
        let b = fold(FNV_OFFSET, 2);
        assert_ne!(a, b, "distinct tokens must fold to distinct digests");
        assert_eq!(a, fold(FNV_OFFSET, 1), "folding is deterministic");
    }

    #[test]
    fn prng_is_deterministic_per_seed() {
        let mut a = Prng::new(0xDEAD_BEEF);
        let mut b = Prng::new(0xDEAD_BEEF);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64(), "same seed, same sequence");
        }
        let mut c = Prng::new(0xDEAD_BEF0);
        assert_ne!(
            Prng::new(0xDEAD_BEEF).next_u64(),
            c.next_u64(),
            "distinct seeds (almost surely) diverge"
        );
    }
}
