//! [`CanonicalPolicy`] — the normalize-then-key keystone (proof-spine §2).
//!
//! THE S2 LAW: `K(p₁) = K(p₂) ⟺ CanonicalPolicy(p₁) == CanonicalPolicy(p₂)`.
//! Two SEMANTICALLY DISTINCT capability policies must derive DISTINCT requirement
//! keys, even if a given backend currently lowers them identically (a future
//! backend could differentiate them). The ONLY policies that may share a key are
//! SYNTACTIC ALIASES of the same policy — e.g. an fd list `[3, 1]` and `[1, 3]`,
//! or a net-dest list in a different order. There is NO general
//! behavioral-equivalence escape hatch (`equivalent_keys!` is REJECTED for v1).
//!
//! This module turns each capability policy into a deterministic CANONICAL NORMAL
//! FORM — a variant discriminant plus a normalized, length-prefixed byte payload
//! (lists sorted + deduplicated). Keying derives from that normal form. The normal
//! form is what makes the alias-collapse SAFE and the distinct-semantics split
//! PROVABLE: the byte encoding is INJECTIVE over policy meaning (distinct meaning ⇒
//! distinct bytes) yet INSENSITIVE to syntactic ordering (aliases ⇒ same bytes).
//!
//! WHY HAND-ROLLED BYTES (not serde). The encoding here is a pure `Vec<u8>`
//! construction with explicit family/variant tags and length prefixes — NOT a
//! MessagePack/serde-format surface — so it neither needs nor uses `crate::encoding`
//! (ADR-0019 governs serde-format wire bytes, which this is not). Building the bytes
//! by hand is exactly what lets us PIN the injectivity: every field boundary is an
//! explicit length prefix, so no two distinct policies can alias by field-run
//! ambiguity (`["a","bc"]` ≠ `["ab","c"]`).

use crate::contract::capability::{
    EnvEntry, EnvPolicy, EnvSource, FdPolicy, NetDest, NetPolicy, SpawnPolicy,
};

/// The CAPABILITY FAMILY tag — the first byte of every canonical encoding. Two
/// policies from different families can NEVER share canonical bytes (the family
/// byte differs), so e.g. an empty fd `Only([])` and an empty env `Exact([])`
/// stay distinct even though their payloads are both "an empty list".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Family {
    Fd = 0x01,
    Spawn = 0x02,
    Env = 0x03,
    Net = 0x04,
}

/// The canonical normal form of one capability POLICY: a family tag, a variant
/// discriminant, and the normalized (sorted + deduplicated) payload. Two values
/// are `==` iff the policies they were normalized from are SEMANTICALLY IDENTICAL
/// (modulo syntactic list ordering). The wrapped bytes ARE the canonical form;
/// equality is byte equality, so the §2 law reduces to a byte comparison.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalPolicy {
    /// The canonical byte encoding (family tag · variant discriminant · payload).
    bytes: Vec<u8>,
}

impl CanonicalPolicy {
    /// The canonical bytes — the load-bearing normal form. Deterministic for a
    /// given policy meaning, identical across syntactic aliases, distinct across
    /// distinct meanings.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Normalize an fd-inheritance policy ([`FdPolicy`]) to its canonical form. The
    /// `Only(..)` fd list is sorted + deduplicated, so `Only([3, 1, 3])` and
    /// `Only([1, 3])` canonicalize identically, while `Only([])` (an explicit
    /// empty grant) stays DISTINCT from `None` (no grant) via the variant byte.
    #[must_use]
    pub fn of_fd(policy: &FdPolicy) -> Self {
        let mut enc = Encoder::new(Family::Fd);
        match policy {
            FdPolicy::None => enc.variant(0),
            FdPolicy::Only(fds) => {
                enc.variant(1);
                let mut sorted = fds.clone();
                sorted.sort_unstable();
                sorted.dedup();
                enc.len_prefixed_u32_list(&sorted);
            }
        }
        enc.finish()
    }

    /// Normalize a child-spawn policy ([`SpawnPolicy`]) to its canonical form. A pure
    /// two-variant discriminant (no payload), so `Deny` and `Allow` are the only two
    /// canonical forms and they never collide.
    #[must_use]
    pub fn of_spawn(policy: &SpawnPolicy) -> Self {
        let mut enc = Encoder::new(Family::Spawn);
        match policy {
            SpawnPolicy::Deny => enc.variant(0),
            SpawnPolicy::Allow => enc.variant(1),
        }
        enc.finish()
    }

    /// Normalize an environment policy ([`EnvPolicy::Exact`]) to its canonical form.
    /// The entry list is sorted by NAME (the only ordering that is a syntactic alias —
    /// names are unique by the contract gate, so reordering carries no meaning), and
    /// each entry encodes `name · source-tag · payload`, every field LENGTH-PREFIXED.
    ///
    /// DISTINCT-SEMANTICS ⇒ DISTINCT-BYTES, load-bearing here: a `Literal("x")` and a
    /// `SecretLease(SecretRef("x"))` for the SAME name carry the SAME string payload
    /// but DIFFERENT meaning (an inline value vs a lease ref), so they get DISTINCT
    /// SOURCE TAGS (`0` literal, `1` lease) and therefore distinct canonical bytes.
    /// Length prefixes keep `name`/`value` boundaries unambiguous (`["AB","C"]` vs
    /// `["A","BC"]` cannot alias). NO de-duplication: a valid table has unique names,
    /// and the canonical form is over the table AS-IS (an invalid duplicate is refused
    /// at admission, never keyed).
    #[must_use]
    pub fn of_env(policy: &EnvPolicy) -> Self {
        let mut enc = Encoder::new(Family::Env);
        match policy {
            EnvPolicy::Exact(entries) => {
                enc.variant(0);
                let mut sorted: Vec<&EnvEntry> = entries.iter().collect();
                sorted.sort_by(|a, b| a.name.cmp(&b.name));
                enc.len_prefixed_env_list(&sorted);
            }
        }
        enc.finish()
    }

    /// Normalize a network policy ([`NetPolicy`]) to its canonical form. `DenyAll`
    /// is a payloadless variant; `AllowList(..)` sorts + deduplicates its
    /// destinations by `(host, port)` and length-prefixes each host, so reordered
    /// allow-lists alias but a different destination set diverges. `DenyAll` and an
    /// empty `AllowList([])` stay DISTINCT via the variant byte (deny-everything is
    /// not the same policy as allow-nothing-explicitly).
    #[must_use]
    pub fn of_net(policy: &NetPolicy) -> Self {
        let mut enc = Encoder::new(Family::Net);
        match policy {
            NetPolicy::DenyAll => enc.variant(0),
            NetPolicy::AllowList(dests) => {
                enc.variant(1);
                let mut sorted = dests.clone();
                sorted.sort_unstable_by(|a, b| (&a.host, a.port).cmp(&(&b.host, b.port)));
                sorted.dedup();
                enc.len_prefixed_dest_list(&sorted);
            }
        }
        enc.finish()
    }
}

/// A deterministic canonical-byte builder. Every multi-byte field is fixed-width
/// big-endian or length-prefixed, so the byte stream is unambiguously parseable in
/// principle — which is exactly the property that makes the encoding INJECTIVE over
/// policy meaning.
struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    /// Start an encoding with its family tag as the first byte.
    fn new(family: Family) -> Self {
        Self {
            bytes: vec![family as u8],
        }
    }

    /// Append the variant discriminant byte (the second byte of every encoding).
    fn variant(&mut self, discriminant: u8) {
        self.bytes.push(discriminant);
    }

    /// Append a count as a fixed-width big-endian `u64` (so a list's length is part
    /// of the bytes — an empty list and a one-element list never alias).
    fn count(&mut self, n: usize) {
        // `usize` ≤ `u64` on every target this crate builds for; widen explicitly.
        self.bytes.extend_from_slice(&(n as u64).to_be_bytes());
    }

    /// Encode a sorted+deduplicated `u32` list as `count · [u32 big-endian]*`.
    fn len_prefixed_u32_list(&mut self, items: &[u32]) {
        self.count(items.len());
        for &item in items {
            self.bytes.extend_from_slice(&item.to_be_bytes());
        }
    }

    /// Encode a name-sorted env-entry list as
    /// `count · [name-len · name-utf8 · source-tag · value-len · value-utf8]*`.
    /// The SOURCE TAG (`0` literal, `1` lease) is what makes a `Literal("x")` and a
    /// `SecretLease(SecretRef("x"))` of the same name DISTINCT canonical bytes despite
    /// the identical payload string; every field is length-prefixed so name/value
    /// boundaries are unambiguous.
    fn len_prefixed_env_list(&mut self, items: &[&EnvEntry]) {
        self.count(items.len());
        for entry in items {
            let name = entry.name.as_bytes();
            self.count(name.len());
            self.bytes.extend_from_slice(name);
            let (tag, payload): (u8, &str) = match &entry.source {
                EnvSource::Literal(value) => (0, value.as_str()),
                EnvSource::SecretLease(reference) => (1, reference.id()),
            };
            self.bytes.push(tag);
            let payload = payload.as_bytes();
            self.count(payload.len());
            self.bytes.extend_from_slice(payload);
        }
    }

    /// Encode a sorted+deduplicated destination list as
    /// `count · [host-len · host-utf8 · port-be-u16]*`.
    fn len_prefixed_dest_list(&mut self, items: &[NetDest]) {
        self.count(items.len());
        for dest in items {
            let raw = dest.host.as_bytes();
            self.count(raw.len());
            self.bytes.extend_from_slice(raw);
            self.bytes.extend_from_slice(&dest.port.to_be_bytes());
        }
    }

    /// Finish, yielding the immutable canonical form.
    fn finish(self) -> CanonicalPolicy {
        CanonicalPolicy { bytes: self.bytes }
    }
}

#[cfg(test)]
#[path = "canonical_policy_tests.rs"]
mod tests;
