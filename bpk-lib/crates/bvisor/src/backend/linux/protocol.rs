//! FROZEN wire protocol for the single-threaded Linux confinement **launcher**
//! (kernel plan §10.8). PURE library types ONLY: serde, canonical encode/decode,
//! and a state-machine transition checker. NO `unsafe`, NO syscalls, NO memfd, NO
//! `[[bin]]` — those live in the launcher basement (a later step). This module is
//! the shared, frozen contract BOTH sides agree on: the multithreaded host
//! *produces* a [`LinuxLaunchPlanV1`], seals it into a memfd (later), and hands the
//! launcher pre-opened authority fds plus a control socket; the launcher
//! *validates* everything here, installs confinement, and execs the workload.
//!
//! ## Identity binding (fail-closed)
//! The body CARRIES the planning-time plan-identity digests — `plan_id`
//! ([`BoundaryPlanHash`]), `h_a` ([`AdmissionProgramHash`]), `h_p`
//! ([`BackendProfileHash`]), and `h_l` (the canonical [`LoweringSchedule`] digest,
//! a [`Digest32`]) — as provenance. TODAY the launcher VERIFIES exactly ONE of them:
//! it recomputes `blake3(canonical(body.lowering))` and refuses (`IdentityMismatch`,
//! nothing executes) unless it equals `h_l`. The independent schedule reconstruction
//! through the admission membrane and the `h_a`/`h_p` profile-drift checks are a LATER
//! step (#75) — NOT claimed here. The launcher may deny more, never report less danger.
//!
//! ## Why a wire view of the schedule, not the schedule type itself
//! The real [`LoweringSchedule`]/[`ScheduleEntry`] are *proof-carrying*: possessing
//! one means it was produced by `compile_schedule` (validated, canonical, acyclic).
//! Their fields are private and they derive no `serde`, deliberately — a
//! deserialized schedule would NOT carry that proof. So the body embeds
//! [`LoweringWireV1`], the serializable PROJECTION of an already-compiled
//! schedule's observable fields (built host-side from the real accessors via
//! [`LoweringWireV1::from_schedule`]). It is NOT a parallel schedule *compiler* or
//! ordering abstraction — it has no validation logic — and it is bound back to the
//! authoritative identity by `h_l`. The launcher reconstructs + re-validates the
//! real schedule through the admission membrane (a later step) and checks its
//! `H_L` equals this body's `h_l`, failing closed on drift.

use crate::contract::ids::{
    AdmissionProgramHash, AttemptId, BackendProfileHash, BoundaryPlanHash, Digest32,
};
use crate::contract::lowering::LoweringSchedule;
use crate::contract::report::Outcome;
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// 1. Envelope
// ─────────────────────────────────────────────────────────────────────────────

/// Frozen byte tag identifying a BatPak Linux-launcher frame. NEVER change — a
/// reader rejects any frame that does not start with these exact 8 bytes.
pub const MAGIC: [u8; 8] = *b"BVZLNCH1";

/// Frozen protocol version. A reader rejects any other value (no silent forward
/// compatibility — a launcher fails closed on a version it does not implement).
pub const PROTO_VERSION: u16 = 1;

/// Fixed envelope header size: `magic(8) · proto_version(2) · body_len(4) ·
/// body_blake3(32)`, all little-endian. The body follows immediately.
pub const HEADER_LEN: usize = 8 + 2 + 4 + 32;

/// Why a framed buffer failed envelope validation. Fail-closed: the body is
/// returned ONLY when every check passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnvelopeReject {
    /// The buffer is shorter than the fixed header, or shorter than the header
    /// plus the declared body length.
    Truncated,
    /// The leading 8 bytes are not [`MAGIC`].
    BadMagic,
    /// The proto-version field is not [`PROTO_VERSION`].
    UnsupportedVersion {
        /// The version actually found in the header.
        found: u16,
    },
    /// The total buffer length does not equal `HEADER_LEN + body_len` exactly
    /// (trailing garbage, or a declared body length that overruns the buffer).
    LengthMismatch,
    /// The recomputed BLAKE3 of the body does not match the header's digest.
    DigestMismatch,
}

impl std::fmt::Display for EnvelopeReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "frame shorter than the declared envelope/body"),
            Self::BadMagic => write!(f, "frame magic does not match the launcher protocol tag"),
            Self::UnsupportedVersion { found } => {
                write!(f, "unsupported launcher protocol version {found}")
            }
            Self::LengthMismatch => {
                write!(f, "frame length does not match the declared body length")
            }
            Self::DigestMismatch => write!(f, "frame body digest does not match the header digest"),
        }
    }
}

impl std::error::Error for EnvelopeReject {}

/// Produce `[envelope][body]` for `body` — the HOST side. Pure: actual memfd
/// sealing (`F_SEAL_*`) is a later launcher-basement step, NOT here. The header is
/// `magic · proto_version(LE) · body_len(LE u32) · blake3(body)`. A body longer
/// than `u32::MAX` (impossible for bounded memfd transport) saturates the length
/// field, which [`parse_and_verify`] then rejects as a length mismatch.
#[must_use]
pub fn frame(body: &[u8]) -> Vec<u8> {
    let body_len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    let digest = batpak::event::hash::compute_hash(body);
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&PROTO_VERSION.to_le_bytes());
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&digest);
    out.extend_from_slice(body);
    out
}

/// Validate a framed buffer and return the verified body slice — the LAUNCHER
/// side. Checks, in order: total length ≥ header, magic, proto-version, declared
/// body length fits exactly, recomputed BLAKE3 matches. Fail-closed: any failure
/// returns the matching [`EnvelopeReject`] and NO body.
///
/// # Errors
/// An [`EnvelopeReject`] for the first failing check.
pub fn parse_and_verify(bytes: &[u8]) -> Result<&[u8], EnvelopeReject> {
    if bytes.len() < HEADER_LEN {
        return Err(EnvelopeReject::Truncated);
    }
    // Magic.
    if bytes[0..8] != MAGIC {
        return Err(EnvelopeReject::BadMagic);
    }
    // Proto version.
    let mut ver = [0u8; 2];
    ver.copy_from_slice(&bytes[8..10]);
    let found = u16::from_le_bytes(ver);
    if found != PROTO_VERSION {
        return Err(EnvelopeReject::UnsupportedVersion { found });
    }
    // Declared body length.
    let mut len = [0u8; 4];
    len.copy_from_slice(&bytes[10..14]);
    let body_len = u32::from_le_bytes(len) as usize;
    // Header digest.
    let mut header_digest = [0u8; 32];
    header_digest.copy_from_slice(&bytes[14..HEADER_LEN]);
    // Body must fit EXACTLY (no trailing bytes, no overrun).
    let Some(total) = HEADER_LEN.checked_add(body_len) else {
        return Err(EnvelopeReject::LengthMismatch);
    };
    if bytes.len() != total {
        return Err(EnvelopeReject::LengthMismatch);
    }
    let body = &bytes[HEADER_LEN..total];
    // Integrity.
    if batpak::event::hash::compute_hash(body) != header_digest {
        return Err(EnvelopeReject::DigestMismatch);
    }
    Ok(body)
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Descriptor table
// ─────────────────────────────────────────────────────────────────────────────

/// The role a pre-opened authority descriptor plays. Authority rides handles,
/// never reopened paths (CVE-2019-5736 / Leaky-Vessels class). Singleton roles
/// (every variant EXCEPT the roots) may appear at most once in a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DescriptorRole {
    /// A directory the workload may read under.
    ReadRoot,
    /// A directory the workload may write under.
    WriteRoot,
    /// The workload executable (exec rides this fd, never a path).
    TargetExe,
    /// The prepared cgroup leaf directory the child enters.
    CgroupDir,
    /// The child's standard input.
    Stdin,
    /// The child's standard output.
    Stdout,
    /// The child's standard error.
    Stderr,
    /// The private control channel carrying the launcher status state machine.
    ControlChannel,
}

impl DescriptorRole {
    /// Whether at most one descriptor of this role may appear in a table. The
    /// read/write ROOTS may repeat (multiple roots); everything else is singleton.
    #[must_use]
    pub fn is_singleton(self) -> bool {
        !matches!(self, Self::ReadRoot | Self::WriteRoot)
    }
}

/// The `fstat`-able kind a descriptor is DECLARED to be. The launcher later
/// `fstat`-checks each handle against its declared shape; this module only defines
/// and structurally validates the declarations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DescriptorKind {
    /// A directory (`S_IFDIR`).
    Directory,
    /// A regular file (`S_IFREG`).
    Regular,
    /// A socket (`S_IFSOCK`).
    Socket,
    /// A pipe / FIFO (`S_IFIFO`).
    Pipe,
}

/// The expected shape of a pre-opened descriptor: its kind plus, where meaningful
/// (directories, regular files), whether it must be writable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptorShape {
    /// The expected `fstat` kind.
    pub kind: DescriptorKind,
    /// Whether the handle must be writable. Meaningful for [`DescriptorKind::Directory`]
    /// and [`DescriptorKind::Regular`]; for sockets/pipes the launcher ignores it.
    pub writable: bool,
}

/// One declared slot of the descriptor table — a single pre-opened authority
/// handle the launcher will `fstat`-validate before use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptorSlotV1 {
    /// The fd's index in the inherited descriptor table (dense, host-assigned).
    pub slot_index: u32,
    /// What this descriptor is FOR.
    pub role: DescriptorRole,
    /// What this descriptor must look like (`fstat` declaration).
    pub expected: DescriptorShape,
}

/// Why a descriptor table is structurally invalid. The launcher fails closed
/// before touching any handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableReject {
    /// Two slots declare the same `slot_index`.
    DuplicateSlotIndex {
        /// The repeated index.
        slot_index: u32,
    },
    /// A singleton role (anything but a read/write root) appears more than once.
    DuplicateSingletonRole {
        /// The repeated role.
        role: DescriptorRole,
    },
}

impl std::fmt::Display for TableReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateSlotIndex { slot_index } => {
                write!(f, "descriptor table has two slots at index {slot_index}")
            }
            Self::DuplicateSingletonRole { role } => {
                write!(f, "descriptor table has two {role:?} descriptors")
            }
        }
    }
}

impl std::error::Error for TableReject {}

/// Structurally validate a descriptor table: reject duplicate `slot_index` and
/// duplicate singleton roles (e.g. two [`DescriptorRole::Stdout`]). A well-formed
/// table is accepted. This is a PURE data check; the `fstat` checks come later.
///
/// # Errors
/// The first [`TableReject`] encountered (slot-index duplicates checked before
/// role duplicates, both in slice order).
pub fn validate_table(table: &[DescriptorSlotV1]) -> Result<(), TableReject> {
    use std::collections::BTreeSet;
    let mut seen_index: BTreeSet<u32> = BTreeSet::new();
    for slot in table {
        if !seen_index.insert(slot.slot_index) {
            return Err(TableReject::DuplicateSlotIndex {
                slot_index: slot.slot_index,
            });
        }
    }
    let mut seen_singleton: BTreeSet<DescriptorRole> = BTreeSet::new();
    for slot in table {
        if slot.role.is_singleton() && !seen_singleton.insert(slot.role) {
            return Err(TableReject::DuplicateSingletonRole { role: slot.role });
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. Target spec
// ─────────────────────────────────────────────────────────────────────────────

/// An OPT-IN request that the launcher birth the workload child in a NEW, unprivileged
/// user namespace and map it to uid/gid 0 inside that namespace (the prerequisite for
/// unprivileged netns creation, S9). The recipe is fixed: the child enters the new
/// userns and BLOCKS on a sync pipe; the parent writes `uid_map` (`0 <euid> 1`),
/// `setgroups=deny`, then `gid_map` (`0 <egid> 1`), then releases the child — which is
/// then uid 0 inside the namespace. This is INFRASTRUCTURE: it mints no confinement
/// claim on its own.
///
/// It is a struct (not a bare bool) so the `0.9.0` namespace work can add fields
/// (e.g. a non-default uid/gid target, extra map ranges) without a wire break. With NO
/// fields today it canonical-encodes to an empty map (`0x80`) when present.
///
/// CRITICAL OFF-PATH INVARIANT: [`TargetSpecV1::user_namespace`] is
/// `#[serde(default, skip_serializing_if = "Option::is_none")]`, so a plan that does
/// NOT request a userns omits the field ENTIRELY from the canonical bytes — the wire
/// form (and every PROVEN oracle that runs through the no-userns path) is byte-for-byte
/// unchanged. `CLONE_NEWUSER` is added to the `clone3` flags ONLY when this is `Some`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct UserNsRequest {}

impl UserNsRequest {
    /// The default request: map child uid/gid 0 → the parent's effective uid/gid.
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }
}

/// An OPT-IN request that the launcher birth the workload child in a NEW, EMPTY network
/// namespace (`CLONE_NEWNET`) — the structural realization of `NetworkDenyAll` (proof-spine
/// S9 / D3). An empty netns has NO external interface: only a loopback `lo` (which the kernel
/// reports `IFF_UP`, like every loopback, but with NO address assigned and NO routes, so it is
/// unreachable — `127.0.0.1` included — unless separately admitted), so the workload is
/// STRUCTURALLY unable to reach any IP/packet network — no externally-routable socket op can succeed and
/// no inherited routable socket exists (the S5 fd-scrub already closed any inherited socket).
///
/// REQUIRES the userns rendezvous: an UNPRIVILEGED process may create a new netns ONLY when
/// it is also root inside a new user namespace, so a plan that requests this MUST also
/// request [`TargetSpecV1::user_namespace`] (the S8 rendezvous makes the child uid 0 in its
/// userns). The launcher ORs `CLONE_NEWNET` into the `clone3` flags ALONGSIDE `CLONE_NEWUSER`
/// (the child is born into the empty netns at clone3 time, before the rendezvous releases it);
/// `CLONE_NEWNET` adds NO new syscall — only a flag bit.
///
/// HOSTCONTROL CARVE-OUT (D3): netns isolation does NOT affect already-OPEN fd-passed
/// sockets. The launcher's own declared private control channels (the protocol Unix-socket /
/// error-pipe / sync-pipe fds it inherits to the child) are HostControl, NOT workload network
/// authority — they keep working through the empty netns, so the launcher protocol still runs
/// the workload to a verdict. "Deny network" is therefore about UNDECLARED network authority,
/// never the launcher's own declared control plumbing.
///
/// It is a struct (not a bare bool) so future work can add fields (e.g. admit `lo` UP, a veth
/// pair to a broker) without a wire break. With NO fields today it canonical-encodes to an
/// empty map (`0x80`) when present.
///
/// CRITICAL OFF-PATH INVARIANT: [`TargetSpecV1::network_namespace`] is
/// `#[serde(default, skip_serializing_if = "Option::is_none")]`, so a plan that does NOT
/// deny network omits the field ENTIRELY from the canonical bytes — the no-netns wire form
/// (and every PROVEN oracle that runs through it) is byte-for-byte unchanged. `CLONE_NEWNET`
/// is added to the `clone3` flags ONLY when this is `Some`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NetworkNsRequest {}

impl NetworkNsRequest {
    /// The default request: birth the child in a new, empty network namespace (only `lo`,
    /// with no address + no routes ⇒ unreachable; no external interface, no inherited routable socket).
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }
}

/// The exact process image to launch. Environment is EXPLICIT — nothing is
/// inherited — and the executable rides a descriptor (`exe_slot`), never a path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSpecV1 {
    /// The argument vector (`argv[0]` is the conventional program name).
    pub argv: Vec<String>,
    /// The complete environment as explicit `(name, value)` pairs. No inherited env.
    pub envp: Vec<(String, String)>,
    /// Index into the descriptor table of the [`DescriptorRole::TargetExe`] handle.
    pub exe_slot: u32,
    /// OPT-IN unprivileged user-namespace rendezvous (S8). `None` (the default) ⇒ the
    /// child shares the launcher's userns and `CLONE_NEWUSER` is NOT set — the EXISTING
    /// no-userns path is byte-for-byte unchanged (the field is omitted from the
    /// canonical encoding, see [`UserNsRequest`]). `Some` ⇒ the child is born in a new
    /// userns and uid/gid-mapped to 0 via the parent rendezvous before it execs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_namespace: Option<UserNsRequest>,
    /// OPT-IN empty network namespace = `NetworkDenyAll` (S9 / D3). `None` (the default) ⇒
    /// the child shares the launcher's netns and `CLONE_NEWNET` is NOT set — the EXISTING
    /// no-netns path is byte-for-byte unchanged (the field is omitted from the canonical
    /// encoding, see [`NetworkNsRequest`]). `Some` ⇒ the child is born in a NEW, EMPTY
    /// netns (only `lo`, no address + no routes ⇒ unreachable; no external interface) so it is
    /// structurally unable to reach any network. REQUIRES [`Self::user_namespace`] to also be `Some` (unprivileged
    /// `CLONE_NEWNET` needs the child to be root-in-userns); the launcher refuses a netns
    /// request without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_namespace: Option<NetworkNsRequest>,
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Lowering wire view (projection of the real, validated schedule)
// ─────────────────────────────────────────────────────────────────────────────

/// One projected schedule entry: the observable, frozen fields of a real
/// [`crate::contract::lowering::ScheduleEntry`]. `phase_code` is the entry's frozen
/// wire code ([`crate::contract::primitive::LoweringPhase::code`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoweringWireEntryV1 {
    /// The primitive's stable id (`linux.<mechanism>.v<n>`).
    pub id: String,
    /// The declaration revision.
    pub version: u32,
    /// The frozen setup-phase wire code.
    pub phase_code: u8,
    /// Digest of the primitive instance's canonical parameters.
    pub param_digest: Digest32,
    /// Digest of the EXACT declaration this entry was compiled from.
    pub decl_digest: Digest32,
}

/// A serializable PROJECTION of an already-compiled [`LoweringSchedule`] — the
/// launcher's instruction DAG on the wire. NOT a parallel schedule compiler: it
/// carries no validation/ordering logic and is bound to the authoritative identity
/// by the body's `h_l`. Built host-side via [`LoweringWireV1::from_schedule`]; the
/// launcher reconstructs + re-validates the real schedule and checks its `H_L`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoweringWireV1 {
    /// The projected entries, in the schedule's canonical lowering order.
    pub entries: Vec<LoweringWireEntryV1>,
}

impl LoweringWireV1 {
    /// Project an already-validated [`LoweringSchedule`] onto its wire view, reusing
    /// the real entry accessors. The order is preserved exactly.
    #[must_use]
    pub fn from_schedule(schedule: &LoweringSchedule) -> Self {
        let entries = schedule
            .entries()
            .iter()
            .map(|e| LoweringWireEntryV1 {
                id: e.id().as_str().to_owned(),
                version: e.version().get(),
                phase_code: e.phase().code(),
                param_digest: *e.param_digest(),
                decl_digest: *e.decl_digest(),
            })
            .collect();
        Self { entries }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Body + plan
// ─────────────────────────────────────────────────────────────────────────────

/// The canonical launcher plan body (v1). Carries the attempt identity, the bound
/// plan-identity digests, the lowering DAG projection, the descriptor table, and
/// the target image. Canonical-encoded (msgpack, named fields) and digest-bound by
/// the envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxLaunchBodyV1 {
    /// The attempt this launch belongs to.
    pub attempt_id: AttemptId,
    /// Content-addressed plan identity.
    pub plan_id: BoundaryPlanHash,
    /// Admission-program identity (`H_A`).
    pub h_a: AdmissionProgramHash,
    /// Profile-snapshot identity (`H_P`) bound at planning time.
    pub h_p: BackendProfileHash,
    /// Canonical lowering-schedule digest (`H_L`) — the integrity binding for
    /// [`Self::lowering`]. The launcher re-derives and compares; mismatch fails closed.
    pub h_l: Digest32,
    /// The lowering DAG projection (`linux.*.v1` action entries, canonical order).
    pub lowering: LoweringWireV1,
    /// The declared pre-opened authority descriptors.
    pub descriptor_table: Vec<DescriptorSlotV1>,
    /// The process image to launch.
    pub target: TargetSpecV1,
}

/// Why a body could not be canonically encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncodeError {
    /// The canonical (msgpack) encoder failed (rendered, so the error stays
    /// `Clone + PartialEq`). Effectively unreachable for the frozen wire shape.
    Canonical(String),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Canonical(e) => write!(f, "could not canonically encode the launch body: {e}"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Why a framed buffer could not be decoded into a [`LinuxLaunchPlanV1`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The envelope failed validation.
    Envelope(EnvelopeReject),
    /// The envelope passed but the body did not canonically decode (rendered).
    Canonical(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Envelope(r) => write!(f, "launch frame envelope rejected: {r}"),
            Self::Canonical(e) => write!(f, "launch body did not canonically decode: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<EnvelopeReject> for DecodeError {
    fn from(r: EnvelopeReject) -> Self {
        Self::Envelope(r)
    }
}

/// A complete launcher plan: the canonical body, framed by the integrity envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinuxLaunchPlanV1 {
    /// The plan body.
    pub body: LinuxLaunchBodyV1,
}

impl LinuxLaunchPlanV1 {
    /// Canonically encode the body and frame it: `[envelope][canonical(body)]`.
    ///
    /// # Errors
    /// [`EncodeError::Canonical`] if the canonical encoder fails.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let body_bytes = batpak::canonical::to_bytes(&self.body)
            .map_err(|e| EncodeError::Canonical(e.to_string()))?;
        Ok(frame(&body_bytes))
    }

    /// Verify the envelope, then canonically decode the body.
    ///
    /// # Errors
    /// [`DecodeError::Envelope`] if the envelope is rejected; [`DecodeError::Canonical`]
    /// if the verified body does not decode.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let body_bytes = parse_and_verify(bytes)?;
        let body: LinuxLaunchBodyV1 = batpak::canonical::from_bytes(body_bytes)
            .map_err(|e| DecodeError::Canonical(e.to_string()))?;
        Ok(Self { body })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. Launcher status state machine
// ─────────────────────────────────────────────────────────────────────────────

/// How a launcher setup PHASE resolved. A phase is a lifecycle checkpoint, NOT a
/// claim that a mechanism was installed: a phase with no scheduled actions resolves
/// [`Self::NotRequired`], and the launcher MUST NOT report an installation it did
/// not perform. Each value is the explicit, honest result a phase carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PhaseResult {
    /// The phase had scheduled actions and the launcher applied them, observing
    /// exactly the scheduled set (count, identity, order, digests all matching).
    Applied,
    /// The phase had NO scheduled actions, so nothing was applied. NEVER emitted
    /// for a nonempty phase (that would under-report).
    NotRequired,
    /// The launcher declined to proceed (fail-closed deny): a scheduled action it
    /// does not implement, or a verification mismatch.
    Refused,
    /// The launcher itself faulted while resolving the phase.
    Faulted,
}

/// One of the four resolvable launcher setup phases. A phase is a checkpoint that
/// carries a [`PhaseResult`]; it does not assert a meaning, only that the
/// lifecycle position was reached and resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SetupPhase {
    /// Identity mapping phase.
    Identity,
    /// Visibility construction phase.
    Visibility,
    /// Ambient-authority scrub phase.
    AmbientAuthority,
    /// Confinement-policy phase.
    Confinement,
}

impl SetupPhase {
    /// The progression state that marks this phase as RESOLVED (its result
    /// recorded). Resolution is a lifecycle position, not an installation claim.
    #[must_use]
    pub fn resolved_state(self) -> LauncherState {
        match self {
            Self::Identity => LauncherState::IdentityPhaseResolved,
            Self::Visibility => LauncherState::VisibilityPhaseResolved,
            Self::AmbientAuthority => LauncherState::AmbientAuthorityPhaseResolved,
            Self::Confinement => LauncherState::ConfinementPhaseResolved,
        }
    }
}

/// Why a launcher [`PhaseResult::Refused`] a phase (fail-closed deny). Extensible:
/// these are the refusals nameable now; more may be added without breaking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RefusalReason {
    /// A scheduled action names a primitive the launcher does not implement.
    MissingPrimitive,
    /// A bound plan-identity digest did not match the live profile / reconstruction.
    IdentityMismatch,
    /// The plan body failed structural validation.
    PlanInvalid,
    /// A pre-opened handle did not match its declared descriptor shape.
    HandleMismatch,
}

/// The launcher's setup progress. MONOTONE forward only: setup proceeds through
/// the non-terminal states in order, then resolves to exactly one terminal. The
/// child execs ONLY from [`Self::ReadyToExec`]; any setup fault means the workload
/// never runs (fail-closed). Terminals are absorbing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LauncherState {
    /// The launcher process started; nothing verified yet.
    LauncherStarted,
    /// The bound plan-identity digests were verified against the live profile.
    IdentityVerified,
    /// The plan body (descriptor table, target, lowering) was validated.
    PlanVerified,
    /// The pre-opened handles were `fstat`-checked against the descriptor table.
    HandlesVerified,
    /// The workload child process was created (pre-confinement).
    ChildCreated,
    /// The identity phase resolved (its [`PhaseResult`] was recorded — applied,
    /// not-required, refused, or faulted). NOT a claim that a mapping was installed.
    IdentityPhaseResolved,
    /// The visibility phase resolved (its [`PhaseResult`] was recorded).
    VisibilityPhaseResolved,
    /// The ambient-authority phase resolved (its [`PhaseResult`] was recorded).
    AmbientAuthorityPhaseResolved,
    /// The confinement phase resolved (its [`PhaseResult`] was recorded). NOT a
    /// claim that an enforcement policy was installed — see [`confinement_installed`].
    ConfinementPhaseResolved,
    /// Every required setup action completed; the child may exec.
    ReadyToExec,
    /// Terminal: the workload was exec'd successfully.
    ExecSucceeded,
    /// Terminal: setup was refused (fail-closed deny — identity/plan/handle/profile
    /// mismatch or a missing capability); the workload never ran.
    SetupRefused,
    /// Terminal: the launcher itself faulted during setup; the workload never ran.
    SetupFaulted,
}

impl LauncherState {
    /// The strictly-ordered non-terminal progression: each state's index is its
    /// forward rank. A legal step advances rank by exactly one; from the last entry
    /// ([`Self::ReadyToExec`]) only a terminal may follow. Exposed so a launcher
    /// driver (and tests) can walk the canonical setup order.
    #[must_use]
    pub fn non_terminal_progression() -> &'static [Self] {
        &Self::PROGRESSION
    }

    /// The strictly-ordered non-terminal progression (see
    /// [`Self::non_terminal_progression`]).
    const PROGRESSION: [Self; 10] = [
        Self::LauncherStarted,
        Self::IdentityVerified,
        Self::PlanVerified,
        Self::HandlesVerified,
        Self::ChildCreated,
        Self::IdentityPhaseResolved,
        Self::VisibilityPhaseResolved,
        Self::AmbientAuthorityPhaseResolved,
        Self::ConfinementPhaseResolved,
        Self::ReadyToExec,
    ];

    /// Whether this is an absorbing terminal state.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::ExecSucceeded | Self::SetupRefused | Self::SetupFaulted
        )
    }

    /// This state's rank in [`Self::PROGRESSION`], or `None` for a terminal.
    fn rank(self) -> Option<usize> {
        Self::PROGRESSION.iter().position(|s| *s == self)
    }
}

/// Whether `from → to` is a legal launcher transition. MONOTONE forward progress:
/// every non-terminal advances by EXACTLY one progression step (no skipping, no
/// going back); from [`LauncherState::ReadyToExec`] ONLY the three terminals are
/// reachable; terminals are absorbing (no transition out of, or self-loop on, a
/// terminal). A setup refusal/fault may be entered from ANY non-terminal state
/// (fail-closed can abort at any point) — but `ReadyToExec`'s successful path is
/// `ExecSucceeded`, and `ReadyToExec` may still refuse/fault before exec.
#[must_use]
pub fn is_valid_transition(from: LauncherState, to: LauncherState) -> bool {
    // Terminals are absorbing: nothing leaves a terminal.
    if from.is_terminal() {
        return false;
    }
    // A fail-closed refusal/fault may be entered from any non-terminal state.
    if matches!(
        to,
        LauncherState::SetupRefused | LauncherState::SetupFaulted
    ) {
        return true;
    }
    // ExecSucceeded is reachable ONLY from ReadyToExec.
    if to == LauncherState::ExecSucceeded {
        return from == LauncherState::ReadyToExec;
    }
    // Otherwise both are non-terminal: advance by exactly one progression step.
    match (from.rank(), to.rank()) {
        (Some(f), Some(t)) => t == f + 1,
        _ => false,
    }
}

/// Whether the child may exec in `state` — TRUE only in [`LauncherState::ReadyToExec`].
#[must_use]
pub fn can_exec(state: LauncherState) -> bool {
    state == LauncherState::ReadyToExec
}

/// Map a TERMINAL launcher state to the canonical run-time [`Outcome`]. A
/// non-terminal state has no outcome yet and returns `None`.
///
/// Mapping: `ExecSucceeded → Completed`; `SetupRefused → Unsupported` (a
/// fail-closed refusal — the backend could not honor the plan); `SetupFaulted →
/// SupervisorFault` (the launcher itself faulted).
#[must_use]
pub fn outcome_class(terminal: LauncherState) -> Option<Outcome> {
    match terminal {
        LauncherState::ExecSucceeded => Some(Outcome::Completed),
        LauncherState::SetupRefused => Some(Outcome::Unsupported),
        LauncherState::SetupFaulted => Some(Outcome::SupervisorFault),
        LauncherState::LauncherStarted
        | LauncherState::IdentityVerified
        | LauncherState::PlanVerified
        | LauncherState::HandlesVerified
        | LauncherState::ChildCreated
        | LauncherState::IdentityPhaseResolved
        | LauncherState::VisibilityPhaseResolved
        | LauncherState::AmbientAuthorityPhaseResolved
        | LauncherState::ConfinementPhaseResolved
        | LauncherState::ReadyToExec => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. Phase-honesty predicates (pure)
// ─────────────────────────────────────────────────────────────────────────────

/// Whether a phase's [`PhaseResult`] is consistent with what the launcher
/// scheduled vs. observed — the anti-over-claim / anti-under-claim oracle. PURE and
/// total. The launcher (a later step) calls this with its OBSERVED action entries.
///
/// Rules:
/// - [`PhaseResult::NotRequired`] ⟺ `scheduled.is_empty()` AND `observed.is_empty()`.
///   A launcher may never report a nonempty phase as not-required (under-claim), nor
///   claim it observed actions while not-required.
/// - [`PhaseResult::Applied`] ⟺ `!scheduled.is_empty()` AND `observed == scheduled`
///   EXACTLY — same length and each entry equal field-for-field (id, version,
///   phase_code, param_digest, decl_digest) IN ORDER. A launcher may never report an
///   empty phase as applied (over-claim), nor with any divergence in count, identity,
///   parameters, declaration, or order.
/// - [`PhaseResult::Refused`] / [`PhaseResult::Faulted`] are failure resolutions that
///   short-circuit the launch; they do NOT assert action parity, so they are
///   structurally consistent regardless of counts. Their legality (that they forbid
///   exec) is enforced by [`ready_to_exec`], not here.
#[must_use]
pub fn phase_resolution_consistent(
    scheduled: &[LoweringWireEntryV1],
    observed: &[LoweringWireEntryV1],
    result: PhaseResult,
) -> bool {
    match result {
        PhaseResult::NotRequired => scheduled.is_empty() && observed.is_empty(),
        PhaseResult::Applied => !scheduled.is_empty() && observed == scheduled,
        PhaseResult::Refused | PhaseResult::Faulted => true,
    }
}

/// Whether confinement was ACTUALLY installed — DERIVED evidence, NOT a state. True
/// only when the confinement phase had scheduled actions AND they were applied. An
/// empty confinement schedule (e.g. an exec-only plan) can never yield `true`, so a
/// [`LauncherState::ConfinementPhaseResolved`] checkpoint never over-claims an
/// install.
#[must_use]
pub fn confinement_installed(
    scheduled_confinement_action_count: usize,
    confinement_result: PhaseResult,
) -> bool {
    scheduled_confinement_action_count > 0 && confinement_result == PhaseResult::Applied
}

/// Whether the launch may proceed to exec, given the four resolved phase results and
/// the schedule binding. PURE and total. True ⟺ the child was created, every phase
/// resolved to {Applied, NotRequired} (no Refused, no Faulted), the AMBIENT-AUTHORITY
/// phase resolved [`PhaseResult::Applied`] (the scrub is mandatory — `NotRequired`
/// is a violation), and the observed schedule digest equals the bound `h_l`. Any
/// failure resolution, a missing scrub, an unbound schedule, or an uncreated child
/// ⇒ false (fail-closed).
#[must_use]
pub fn ready_to_exec(
    child_created: bool,
    phases: [(SetupPhase, PhaseResult); 4],
    observed_schedule_digest: Digest32,
    h_l: Digest32,
) -> bool {
    if !child_created {
        return false;
    }
    if observed_schedule_digest != h_l {
        return false;
    }
    let mut ambient_applied = false;
    for (phase, result) in phases {
        match result {
            PhaseResult::Applied | PhaseResult::NotRequired => {}
            PhaseResult::Refused | PhaseResult::Faulted => return false,
        }
        if matches!(phase, SetupPhase::AmbientAuthority) && result == PhaseResult::Applied {
            ambient_applied = true;
        }
    }
    ambient_applied
}

// The full protocol conformance suite — canonical round-trip, the frozen golden
// vector, every envelope reject (incl. `parse_and_verify`'s accept + tamper
// paths), the descriptor table, the launcher state machine, and the phase-honesty
// predicates — lives in the integration test
// `crates/bvisor/tests/launcher_protocol.rs` (a member of the over-claim witness
// corpus). Kept out-of-source so this module stays types-only.
