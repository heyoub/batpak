//! `NetworkDenyAll` lowering for the Linux backend (proof-spine S9 / D3), split out of
//! `backend_impl.rs` to hold it under the non-overridable file-size cap.
//!
//! THE LOWERING (mirrors the S4/S5 seam): the admitted [`NetPolicy`] DRIVES whether the
//! launch plan engages an EMPTY network namespace. For [`NetPolicy::DenyAll`] the launcher
//! births the workload child in a NEW, EMPTY netns (`CLONE_NEWNET`, alongside the S8
//! `CLONE_NEWUSER` rendezvous it requires): the netns has NO external interface — only a
//! loopback `lo` (which the kernel reports `IFF_UP`, like every loopback, but with NO address
//! assigned and NO routes, so neither `127.0.0.1` nor any external destination is reachable) —
//! so the workload is STRUCTURALLY unable to reach any IP/packet network. Combined with the S5 fd-scrub (which already closes every
//! undeclared inherited fd, including any inherited routable socket), this realizes the D3
//! "network" definition: no inherited IP/packet/netlink/undeclared-Unix sockets, no
//! externally-routable socket op succeeds, isolated netns, loopback unavailable unless
//! separately admitted, no iface-config / no joining another netns.
//!
//! HOSTCONTROL CARVE-OUT (D3): the launcher's OWN declared private control channels (the
//! protocol Unix-socket / error-pipe / userns-sync-pipe fds it fd-PASSES to the child) are
//! HostControl, NOT workload network authority. netns isolation does not affect already-open
//! fd-passed sockets, so the launcher protocol still runs the workload to a verdict. "Deny
//! network" is therefore about UNDECLARED network authority, never the launcher's own
//! declared control plumbing.
//!
//! `NetPolicy::AllowList(..)` is OUT OF SCOPE (S9 — no broker in v1). It is absent from the
//! ceiling so it never admits, but defense-in-depth this gate ALSO refuses it: were it ever
//! to reach `execute()` (a future ceiling change without a broker lowering), the workload
//! must NOT run with the empty-netns silently realizing the wrong policy (deny-everything
//! instead of the scoped allow-list — an unrealized guarantee). SAFE std; the OS work
//! (`CLONE_NEWNET` + the userns rendezvous) is the launcher's.

use super::LinuxBackend;
use crate::contract::capability::{Capability, NetPolicy};
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement};
use crate::contract::report::ObservedFact;

/// The outcome of lowering the admitted network policy: whether the launch plan must
/// engage the empty network namespace (`NetworkDenyAll`), carried with the observed facts.
pub(super) struct NetLowering {
    /// `true` ⇒ the launch plan must request the empty netns (+ the userns rendezvous it
    /// requires); `false` ⇒ no netns engagement (the no-netns path, byte-for-byte unchanged).
    pub(super) deny_network: bool,
    /// The observed facts threaded through (the lowering fact appended).
    pub(super) observed: Vec<ObservedFact>,
}

/// LOWER the plan's admitted [`Capability::Network`] policy onto the launcher's empty-netns
/// engagement. On a policy the lowering does not realize (`AllowList` — no broker in v1) it
/// returns `Err(observed)` with a `network_lowering_failed` fact appended, so the caller
/// FAILS CLOSED — the workload never runs under an unrealized network guarantee. With NO
/// `Network` capability admitted (the spec declared none) `deny_network` is `false` — the
/// no-netns path is unchanged (the default is NOT to confine a guarantee the spec did not
/// request; admission already refused any spec needing a network guarantee this backend
/// cannot back).
///
/// `_backend` is unused today (the netns needs no host resolver) but kept in the signature
/// so the seam matches `lower_environment` / `lower_inherited_fds`.
pub(super) fn lower_network(
    _backend: &LinuxBackend,
    plan: &BoundaryPlan,
    mut observed: Vec<ObservedFact>,
) -> Result<NetLowering, Vec<ObservedFact>> {
    match admitted_network_policy(plan) {
        // No Network capability admitted: the spec did not request a network guarantee, so
        // there is nothing to lower — the no-netns path runs unchanged.
        None => Ok(NetLowering {
            deny_network: false,
            observed,
        }),
        // DenyAll: engage the empty netns. The launcher births the child in a NEW, EMPTY
        // netns (CLONE_NEWNET) alongside the S8 userns rendezvous — no external interface,
        // so the workload is structurally unable to reach any network.
        Some(NetPolicy::DenyAll) => {
            observed.push(ObservedFact {
                kind: "network_lowered".to_string(),
                detail: "NetworkDenyAll: the launcher births the workload in a NEW, EMPTY \
                         network namespace (CLONE_NEWNET, alongside the userns rendezvous) — \
                         only `lo` (no address, no routes ⇒ unreachable), no external \
                         interface, no inherited routable \
                         socket (fd-scrub); the workload is structurally unable to reach any \
                         network. The launcher's own declared control channels (HostControl) \
                         keep working (fd-passed sockets are unaffected by netns)."
                    .to_string(),
            });
            Ok(NetLowering {
                deny_network: true,
                observed,
            })
        }
        // AllowList(..) is out of scope (S9 — no broker in v1): the empty netns does not
        // realize a scoped allow-list. It is absent from the ceiling (never admits), but
        // fail CLOSED here too — the workload must never run under an unrealized guarantee.
        Some(NetPolicy::AllowList(dests)) => {
            observed.push(ObservedFact {
                kind: "network_lowering_failed".to_string(),
                detail: format!(
                    "refusing to launch: NetworkAllowList({} dest(s)) is not realized by this \
                     backend (no broker in v1; only NetworkDenyAll is lowered, via an empty \
                     netns); the target never runs",
                    dests.len()
                ),
            });
            Err(observed)
        }
    }
}

/// The admitted [`NetPolicy`] to realize: the admitted `Network` capability's policy, or
/// `None` when the spec declared no `Network` capability. The plan was admitted against our
/// ceiling, so any admitted `Network` capability whose key is `NetworkDenyAll` is `DenyAll`.
fn admitted_network_policy(plan: &BoundaryPlan) -> Option<NetPolicy> {
    plan.admitted.iter().find_map(|a| match &a.requirement {
        BoundaryRequirement::Capability(Capability::Network { policy }) => Some(policy.clone()),
        BoundaryRequirement::Capability(_) | BoundaryRequirement::HostControl(_) => None,
    })
}
