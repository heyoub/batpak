//! BVISOR platform qualification matrix — terminal status per `(backend, kind)` cell.
//!
//! Mirrors [`LINUX_QUALIFICATION_LEDGER`] for linux rows and requires every
//! `(backend, kind)` pair across the four platform backends to carry a terminal
//! qualification status (`proven`, `fail-closed`, `waived`, `fault-injected`).
//! `incomplete` is rejected.

use crate::docs_catalog::file_declares_test_fn;
use crate::repo_surface::{ensure, resolve_repo_or_core_path};
use crate::source_cache::SourceCache;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

#[cfg(test)]
#[path = "platform_qualification_matrix_tests.rs"]
mod platform_qualification_matrix_tests;

pub(crate) const MATRIX_REL: &str = "traceability/platform_qualification_matrix.yaml";

const BACKENDS: &[&str] = &["linux", "macos", "wasm", "windows"];
const KINDS: &[&str] = &[
    "Filesystem",
    "NetworkDenyAll",
    "NetworkAllowList",
    "ChildSpawnDenyNewTasks",
    "ChildSpawnAllowThreads",
    "ChildSpawnAllowDescendants",
    "Environment",
    "InheritedFdsNone",
    "InheritedFdsOnly",
    "LaunchWorkload",
    "CaptureStreams",
    "TempRoot",
    "ExposePath",
    "CommitArtifact",
    "DiscardArtifact",
    "Kill",
    "ListOutputs",
];

const TERMINAL_STATUSES: &[&str] = &["proven", "fail-closed", "waived", "fault-injected"];
const RUNNERS: &[&str] = &[
    "linux-native",
    "windows-native",
    "macos-native",
    "wasm-structural",
    "contract-any",
];

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct ProfileFloorYaml {
    landlock_abi_min: Option<i64>,
    requires_cgroup_kill: bool,
    requires_pids_peak: bool,
    requires_unprivileged_userns: bool,
    requires_seccomp_filter: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct MatrixCell {
    backend: String,
    kind: String,
    status: String,
    runner: String,
    profile_floor: ProfileFloorYaml,
    mechanism: String,
    proof_receipts: Vec<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    expiry: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct MatrixFile {
    schema_version: u32,
    cells: Vec<MatrixCell>,
}

struct LinuxLedgerSeed {
    kind: &'static str,
    status: &'static str,
    mechanism: &'static str,
    runner: &'static str,
    landlock_abi_min: Option<i64>,
    requires_cgroup_kill: bool,
    requires_pids_peak: bool,
    requires_unprivileged_userns: bool,
    requires_seccomp_filter: bool,
    proof_receipts: &'static [&'static str],
}

const LINUX_LEDGER: &[LinuxLedgerSeed] = &[
    LinuxLedgerSeed {
        kind: "Filesystem",
        status: "proven",
        mechanism: "linux:landlock:Enforced",
        runner: "linux-native",
        landlock_abi_min: Some(1),
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &["crates/bvisor/tests/grid_linux_fs.rs::g1_landlock_denies_secret_read_outside_declared_root"],
    },
    LinuxLedgerSeed {
        kind: "LaunchWorkload",
        status: "proven",
        mechanism: "linux:process_spawn:Enforced",
        runner: "linux-native",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/tests/launcher_capture_linux.rs::launcher_captures_workload_streams_cleanly_and_deterministically",
            "crates/bvisor/tests/launcher_skeleton_linux.rs::missing_primitive_refuses_before_any_child",
        ],
    },
    LinuxLedgerSeed {
        kind: "CaptureStreams",
        status: "proven",
        mechanism: "linux:pipe_capture:Enforced",
        runner: "linux-native",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &["crates/bvisor/tests/launcher_capture_linux.rs::launcher_captures_workload_streams_cleanly_and_deterministically"],
    },
    LinuxLedgerSeed {
        kind: "Kill",
        status: "proven",
        mechanism: "linux:cgroup_kill:Enforced",
        runner: "linux-native",
        landlock_abi_min: None,
        requires_cgroup_kill: true,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &["crates/bvisor/tests/cgroup_enforcement_linux.rs::pids_max_genuinely_denies_forks_past_the_cap_or_explicit_skip"],
    },
    LinuxLedgerSeed {
        kind: "Environment",
        status: "proven",
        mechanism: "linux:explicit_env:Enforced",
        runner: "linux-native",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/tests/env_exact_linux.rs::child_env_equals_the_admitted_table_with_no_ambient_leak",
            "crates/bvisor/tests/env_exact_linux.rs::an_unresolvable_lease_fails_closed_and_the_target_never_runs",
            "crates/bvisor/tests/env_exact_linux.rs::a_contract_invalid_policy_is_refused_before_execution",
            "crates/bvisor/tests/env_exact_linux.rs::a_secret_lease_resolves_but_the_durable_plan_and_report_carry_only_the_ref",
        ],
    },
    LinuxLedgerSeed {
        kind: "InheritedFdsNone",
        status: "proven",
        mechanism: "linux:fd_scrub:Enforced",
        runner: "linux-native",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/tests/inherited_fds_none_linux.rs::child_inherits_only_the_declared_fds_no_sentinel_leak",
            "crates/bvisor/tests/launcher_inherited_fds_linux.rs::undeclared_inherited_fd_is_scrubbed_before_the_workload",
            "crates/bvisor/tests/inherited_fds_none_linux.rs::an_unrealized_fd_policy_fails_closed_and_the_target_never_runs",
            "crates/bvisor/tests/inherited_fds_none_linux.rs::a_none_policy_spec_runs_through_the_execute_path",
        ],
    },
    LinuxLedgerSeed {
        kind: "InheritedFdsOnly",
        status: "fail-closed",
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        runner: "contract-any",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/src/backend/linux/backend_impl_tests.rs::linux_unimplemented_kinds_refuse_at_plan",
        ],
    },
    LinuxLedgerSeed {
        kind: "NetworkDenyAll",
        status: "proven",
        mechanism: "linux:empty_netns:Enforced",
        runner: "linux-native",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: true,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/tests/network_deny_all_linux.rs::host_sees_only_loopback_in_the_child_netns_no_external_interface_or_skip",
            "crates/bvisor/tests/network_deny_all_linux.rs::workload_cannot_reach_the_network_from_the_empty_netns_or_skip",
            "crates/bvisor/tests/network_deny_all_linux.rs::a_deny_all_spec_runs_through_the_execute_path_or_skip",
            "crates/bvisor/tests/network_deny_all_linux.rs::network_allow_list_fails_closed_at_admission_the_target_never_runs",
        ],
    },
    LinuxLedgerSeed {
        kind: "NetworkAllowList",
        status: "fail-closed",
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        runner: "contract-any",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/tests/network_deny_all_linux.rs::network_allow_list_fails_closed_at_admission_the_target_never_runs",
        ],
    },
    LinuxLedgerSeed {
        kind: "ChildSpawnDenyNewTasks",
        status: "proven",
        mechanism: "linux:seccomp_deny_tasks:Enforced",
        runner: "linux-native",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: true,
        proof_receipts: &[
            "crates/bvisor/tests/child_spawn_linux.rs::deny_new_tasks_fork_is_refused_and_host_sees_seccomp_filter_or_skip",
            "crates/bvisor/tests/child_spawn_linux.rs::a_deny_new_tasks_spec_runs_through_the_execute_path_or_skip",
            "crates/bvisor/tests/child_spawn_linux.rs::allow_threads_fails_closed_at_admission_the_target_never_runs",
        ],
    },
    LinuxLedgerSeed {
        kind: "ChildSpawnAllowThreads",
        status: "fail-closed",
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        runner: "contract-any",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/tests/child_spawn_linux.rs::allow_threads_fails_closed_at_admission_the_target_never_runs",
        ],
    },
    LinuxLedgerSeed {
        kind: "ChildSpawnAllowDescendants",
        status: "proven",
        mechanism: "linux:cgroup_descendant_boundary:Enforced",
        runner: "linux-native",
        landlock_abi_min: None,
        requires_cgroup_kill: true,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/tests/child_spawn_linux.rs::allow_descendants_is_cgroup_confined_and_cgroup_kill_drains_the_tree_or_skip",
            "crates/bvisor/tests/child_spawn_linux.rs::an_allow_descendants_spec_runs_through_the_execute_path_or_skip",
        ],
    },
    LinuxLedgerSeed {
        kind: "TempRoot",
        status: "fail-closed",
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        runner: "contract-any",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/src/backend/linux/backend_impl_tests.rs::linux_unimplemented_kinds_refuse_at_plan",
        ],
    },
    LinuxLedgerSeed {
        kind: "ExposePath",
        status: "fail-closed",
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        runner: "contract-any",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/src/backend/linux/backend_impl_tests.rs::linux_unimplemented_kinds_refuse_at_plan",
        ],
    },
    LinuxLedgerSeed {
        kind: "CommitArtifact",
        status: "fail-closed",
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        runner: "contract-any",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/src/backend/linux/backend_impl_tests.rs::linux_unimplemented_kinds_refuse_at_plan",
        ],
    },
    LinuxLedgerSeed {
        kind: "DiscardArtifact",
        status: "fail-closed",
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        runner: "contract-any",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/src/backend/linux/backend_impl_tests.rs::linux_unimplemented_kinds_refuse_at_plan",
        ],
    },
    LinuxLedgerSeed {
        kind: "ListOutputs",
        status: "fail-closed",
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        runner: "contract-any",
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
        proof_receipts: &[
            "crates/bvisor/src/backend/linux/backend_impl_tests.rs::linux_unimplemented_kinds_refuse_at_plan",
        ],
    },
];

const WINDOWS_FAIL_CLOSED_WITNESS: &str =
    "crates/bvisor/tests/platform_matrix_refusal.rs::windows_scaffolding_refuses_representative_enforced_kinds";
const MACOS_FAIL_CLOSED_WITNESS: &str =
    "crates/bvisor/tests/platform_matrix_refusal.rs::macos_scaffolding_refuses_representative_enforced_kinds";
const WASM_FAIL_CLOSED_WITNESS: &str =
    "crates/bvisor/tests/platform_matrix_refusal.rs::wasm_scaffolding_refuses_representative_enforced_kinds";

fn scaffolding_fail_closed_witness(backend: &str) -> Vec<String> {
    let witness = match backend {
        "windows" => WINDOWS_FAIL_CLOSED_WITNESS,
        "macos" => MACOS_FAIL_CLOSED_WITNESS,
        "wasm" => WASM_FAIL_CLOSED_WITNESS,
        _ => return Vec::new(),
    };
    vec![witness.to_string()]
}

fn structural_floor() -> ProfileFloorYaml {
    ProfileFloorYaml {
        landlock_abi_min: None,
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
    }
}

fn default_runner(_backend: &str) -> &'static str {
    // Scaffolding fail-closed witnesses (`platform_matrix_refusal.rs`) are
    // contract/refusal proofs runnable on any host — not native OS execution.
    "contract-any"
}

fn linux_seed(kind: &str) -> Option<&'static LinuxLedgerSeed> {
    LINUX_LEDGER.iter().find(|row| row.kind == kind)
}

fn derive_matrix() -> MatrixFile {
    let mut cells = Vec::with_capacity(BACKENDS.len() * KINDS.len());
    for backend in BACKENDS {
        for kind in KINDS {
            if *backend == "linux" {
                if let Some(seed) = linux_seed(kind) {
                    cells.push(MatrixCell {
                        backend: backend.to_string(),
                        kind: kind.to_string(),
                        status: seed.status.to_string(),
                        runner: seed.runner.to_string(),
                        profile_floor: ProfileFloorYaml {
                            landlock_abi_min: seed.landlock_abi_min,
                            requires_cgroup_kill: seed.requires_cgroup_kill,
                            requires_pids_peak: seed.requires_pids_peak,
                            requires_unprivileged_userns: seed.requires_unprivileged_userns,
                            requires_seccomp_filter: seed.requires_seccomp_filter,
                        },
                        mechanism: seed.mechanism.to_string(),
                        proof_receipts: seed
                            .proof_receipts
                            .iter()
                            .map(|s| (*s).to_string())
                            .collect(),
                        owner: None,
                        reason: None,
                        expiry: None,
                    });
                    continue;
                }
            }
            cells.push(MatrixCell {
                backend: backend.to_string(),
                kind: kind.to_string(),
                status: "fail-closed".to_string(),
                runner: default_runner(backend).to_string(),
                profile_floor: structural_floor(),
                mechanism: format!("{backend}:none/unimplemented-this-chunk:Unsupported"),
                proof_receipts: scaffolding_fail_closed_witness(backend),
                owner: None,
                reason: None,
                expiry: None,
            });
        }
    }
    cells.sort();
    MatrixFile {
        schema_version: 1,
        cells,
    }
}

pub(crate) fn render(matrix: &MatrixFile) -> String {
    let mut out = String::from(
        "# GENERATED — mirror of platform qualification ledger; regenerate via cargo xtask platform-qualification-matrix\nschema_version: 1\ncells:\n",
    );
    for cell in &matrix.cells {
        out.push_str(&format!(
            "  - {{ backend: {}, kind: {}, status: {}, runner: {}, profile_floor: {{ landlock_abi_min: {}, requires_cgroup_kill: {}, requires_pids_peak: {}, requires_unprivileged_userns: {}, requires_seccomp_filter: {} }}, mechanism: \"{}\", proof_receipts: [{}",
            cell.backend,
            cell.kind,
            cell.status,
            cell.runner,
            match cell.profile_floor.landlock_abi_min {
                Some(v) => v.to_string(),
                None => "null".to_string(),
            },
            cell.profile_floor.requires_cgroup_kill,
            cell.profile_floor.requires_pids_peak,
            cell.profile_floor.requires_unprivileged_userns,
            cell.profile_floor.requires_seccomp_filter,
            cell.mechanism,
            cell.proof_receipts
                .iter()
                .map(|r| format!("\"{r}\""))
                .collect::<Vec<_>>()
                .join(", "),
        ));
        out.push_str("] }");
        if let (Some(owner), Some(reason), Some(expiry)) = (&cell.owner, &cell.reason, &cell.expiry)
        {
            out.pop(); // remove trailing '}' to append waiver fields inside the map
            out.push_str(&format!(
                ", owner: \"{owner}\", reason: \"{reason}\", expiry: \"{expiry}\" }}"
            ));
        }
        out.push('\n');
    }
    out
}

fn parse(text: &str) -> Result<MatrixFile> {
    yaml_serde::from_str(text).context("parse platform_qualification_matrix.yaml")
}

pub(crate) fn assert_mirror(committed: &str, derived: &MatrixFile) -> Result<()> {
    let parsed = parse(committed)?;
    ensure(
        parsed == *derived,
        "platform-qualification-matrix: STALE — committed file does not mirror source-derived matrix; regenerate via `cargo xtask platform-qualification-matrix`".to_owned(),
    )
}

pub(crate) fn validate_matrix(repo_root: &Path, matrix: &MatrixFile) -> Result<()> {
    ensure(
        matrix.schema_version == 1,
        "platform-qualification-matrix: schema_version must be 1".to_owned(),
    )?;
    ensure(
        matrix.cells.len() == BACKENDS.len() * KINDS.len(),
        format!(
            "platform-qualification-matrix: expected {} cells, got {}",
            BACKENDS.len() * KINDS.len(),
            matrix.cells.len()
        ),
    )?;
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut cache = SourceCache::new(repo_root);
    for cell in &matrix.cells {
        ensure(
            BACKENDS.contains(&cell.backend.as_str()),
            format!(
                "platform-qualification-matrix: unknown backend `{}`",
                cell.backend
            ),
        )?;
        ensure(
            KINDS.contains(&cell.kind.as_str()),
            format!(
                "platform-qualification-matrix: unknown kind `{}`",
                cell.kind
            ),
        )?;
        ensure(
            seen.insert((cell.backend.clone(), cell.kind.clone())),
            format!(
                "platform-qualification-matrix: duplicate cell `{}:{}`",
                cell.backend, cell.kind
            ),
        )?;
        ensure(
            cell.status != "incomplete",
            format!(
                "platform-qualification-matrix: cell `{}:{}` status `incomplete` is forbidden",
                cell.backend, cell.kind
            ),
        )?;
        ensure(
            TERMINAL_STATUSES.contains(&cell.status.as_str()),
            format!(
                "platform-qualification-matrix: cell `{}:{}` has unknown status `{}`",
                cell.backend, cell.kind, cell.status
            ),
        )?;
        ensure(
            RUNNERS.contains(&cell.runner.as_str()),
            format!(
                "platform-qualification-matrix: cell `{}:{}` has unknown runner `{}`",
                cell.backend, cell.kind, cell.runner
            ),
        )?;
        ensure(
            !cell.mechanism.trim().is_empty(),
            format!(
                "platform-qualification-matrix: cell `{}:{}` has blank mechanism",
                cell.backend, cell.kind
            ),
        )?;
        if cell.status == "proven" {
            ensure(
                !cell.proof_receipts.is_empty(),
                format!(
                    "platform-qualification-matrix: proven cell `{}:{}` requires proof_receipts",
                    cell.backend, cell.kind
                ),
            )?;
            if cell.backend == "linux" && cell.runner != "linux-native" {
                bail!(
                    "platform-qualification-matrix: linux proven cell `{}:{}` must use runner linux-native",
                    cell.backend,
                    cell.kind
                );
            }
            for receipt in &cell.proof_receipts {
                validate_proof_receipt(repo_root, &mut cache, receipt)?;
            }
        } else if cell.status == "fail-closed" {
            ensure(
                cell.runner == "contract-any",
                format!(
                    "platform-qualification-matrix: fail-closed cell `{}:{}` must use runner contract-any (got `{}`); native runners are for proven cells only",
                    cell.backend, cell.kind, cell.runner
                ),
            )?;
            ensure(
                !cell.proof_receipts.is_empty(),
                format!(
                    "platform-qualification-matrix: fail-closed cell `{}:{}` requires proof_receipts",
                    cell.backend, cell.kind
                ),
            )?;
            for receipt in &cell.proof_receipts {
                validate_proof_receipt(repo_root, &mut cache, receipt)?;
            }
        } else {
            ensure(
                cell.proof_receipts.is_empty(),
                format!(
                    "platform-qualification-matrix: non-proven cell `{}:{}` must have empty proof_receipts",
                    cell.backend, cell.kind
                ),
            )?;
        }
        if cell.status == "waived" {
            for (field, value) in [
                ("owner", cell.owner.as_deref()),
                ("reason", cell.reason.as_deref()),
                ("expiry", cell.expiry.as_deref()),
            ] {
                ensure(
                    value.is_some_and(|v| !v.trim().is_empty()),
                    format!(
                        "platform-qualification-matrix: waived cell `{}:{}` requires `{field}`",
                        cell.backend, cell.kind
                    ),
                )?;
            }
        }
    }
    Ok(())
}

fn validate_proof_receipt(repo_root: &Path, cache: &mut SourceCache, witness: &str) -> Result<()> {
    let (rel_path, fn_name) = witness.rsplit_once("::").with_context(|| {
        format!(
            "platform-qualification-matrix: witness `{witness}` must be repo-relative `path::fn`"
        )
    })?;
    let full = resolve_repo_or_core_path(repo_root, rel_path);
    ensure(
        full.is_file(),
        format!("platform-qualification-matrix: witness `{witness}` points at missing file `{rel_path}`"),
    )?;
    ensure(
        file_declares_test_fn(cache, &full, fn_name)?,
        format!("platform-qualification-matrix: witness `{witness}` names no `#[test]`/`fn {fn_name}` in `{rel_path}`"),
    )
}

fn check_gate(repo_root: &Path, derived: &MatrixFile) -> Result<()> {
    let committed = std::fs::read_to_string(repo_root.join(MATRIX_REL))
        .with_context(|| format!("read {MATRIX_REL}"))?;
    assert_mirror(&committed, derived)?;
    validate_matrix(repo_root, derived)?;
    outln!(
        "platform-qualification-matrix: ok ({} cell(s), mirror current)",
        derived.cells.len()
    );
    Ok(())
}

pub(crate) fn run(repo_root: &Path, check: bool) -> Result<()> {
    let derived = derive_matrix();
    let rendered = render(&derived);
    let path = repo_root.join(MATRIX_REL);
    if check {
        return check_gate(repo_root, &derived);
    }
    if std::fs::read_to_string(&path).ok().as_deref() != Some(rendered.as_str()) {
        std::fs::write(&path, &rendered).with_context(|| format!("write {MATRIX_REL}"))?;
        outln!(
            "platform-qualification-matrix: regenerated {MATRIX_REL} ({} cell(s))",
            derived.cells.len()
        );
    } else {
        outln!(
            "platform-qualification-matrix: {MATRIX_REL} already current ({} cell(s))",
            derived.cells.len()
        );
    }
    Ok(())
}

pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let derived = derive_matrix();
    check_gate(repo_root, &derived)
}

#[cfg(test)]
pub(crate) fn derive_for_test() -> MatrixFile {
    derive_matrix()
}
