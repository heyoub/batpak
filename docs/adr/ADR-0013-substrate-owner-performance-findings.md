# ADR-0013: Substrate-owner performance findings

## Status
Accepted

## Context

This ADR records the current substrate-owner reading of the runtime and
benchmark surfaces after the `0.6.x` measurement pass. It is not a new runtime
design decision by itself. It is a measured memo that:

- captures what the current tree actually does,
- separates bad benchmark environment from proven runtime regression,
- records the strongest performance findings worth carrying forward into
  planning, and
- fixes the order of operations for the next runtime-performance work.

The immediate trigger was a sync-heavy benchmark session that looked alarming
at first pass. The question was whether the tree had regressed generally, or
whether the benchmark environment was the dominant problem. The follow-on work
added dedicated batching/cadence benchmark surfaces and re-read the relevant
runtime code paths around sync cadence, segment scan/recovery, and writer
orchestration.

This ADR exists so the performance story is source-anchored and substrate-owner
honest instead of being reconstructed later from chat logs.

## Measurement setup

The benchmark and gate surfaces used in this memo are:

- `cargo xtask doctor`
- `cargo xtask bench --surface neutral`
- `cargo xtask perf-gates`
- targeted native-only benchmark surfaces in `benches/unified_bench.rs` and
  `benches/batch_throughput.rs`

The key environment reading came from `cargo xtask doctor`, whose fsync probe
reported approximately:

- `4.07 ms/fsync`
- `246 fsyncs/sec`

Per the repo's own hint ladder in `tools/integrity/src/main.rs`, this is
squarely in the "slow fsync" environment class and is consistent with
virtualized, overlay, devcontainer, or otherwise non-canonical storage. This
matters because the sync-heavy write surfaces in this tree are sensitive enough
that environment quality can dominate the result.

The benchmark session also included the fresh benchmark-harness fix in
`benches/projection_latency.rs`, which made reopen/close timing reflect the
current exclusive store-lock contract instead of relying on a stale harness
assumption.

## Findings

### 1. Bad sync-heavy environment does not equal proven global regression

The environment is bad for sync-heavy workloads, but the current tree does not
show a proven broad runtime regression.

Signals that point at environment first:

- `cargo xtask doctor` measured `246 fsyncs/sec`
- fully durable single append sits almost exactly at that ceiling
- batch/cadence throughput scales the way an fsync-bound model predicts until
  CPU cost takes over
- `cargo xtask perf-gates` still passed on this tree, which rules out only the
  repo's coarse catastrophic-regression thresholds on this host

This is the right interpretation:

- **environment finding**: the current machine is a poor sync-heavy benchmark
  environment
- **non-finding**: no broad global runtime regression is proven from these
  measurements alone

### 2. The fsync ceiling explains the durable single-append floor

The durable single-append path is effectively pinned to the fsync ceiling on
this hardware:

- fsync probe: `246 fsyncs/sec`
- durable single append: roughly `247-253 elem/s`

That is the expected shape for a per-append durable path. It means the runtime
is not mysteriously leaving large sync-heavy throughput on the floor in the
single-durable case on this machine; the floor is the machine.

### 3. The single-writer CPU ceiling on this hardware is about 18-21 Kelem/s

Once sync cadence is relaxed enough that fsync is no longer the dominant cost,
the writer loop flattens into a CPU-bound regime at roughly:

- `17.81 Kelem/s` for `batch_1` at cadence `1000`
- `21.42 Kelem/s` for `batch_32` at cadence `1000`

Nearby steady-flow write surfaces on this host also landed around
`20-29 Kelem/s` depending on event count and harness shape, but those are
adjacent observations rather than the specific cadence-sweep plateau itself.

The practical substrate-owner reading is:

**on hardware with this class of CPU and this current writer loop shape, the
single-writer steady-state CPU ceiling is about `18-21 Kelem/s` once fsync is
amortized away.**

That ceiling is where encode + CRC + hash-chain + index insert + publish +
receipt/signature overhead becomes the limiting factor.

### 4. Sync cadence dominates more than drain budget on this machine

The new batching/cadence surfaces show that sync cadence is the main lever on
this environment.

Representative results:

- `batch_1`, cadence `1`: `248 elem/s`
- `batch_1`, cadence `8`: `1.76 Kelem/s`
- `batch_1`, cadence `64`: `9.88 Kelem/s`
- `batch_1`, cadence `256`: `16.15 Kelem/s`
- `batch_1`, cadence `1000`: `17.81 Kelem/s`

Against that, uncontended group-commit drain width helps far less:

- batch sweep `1`: `248 elem/s`
- `8`: `266 elem/s`
- `16`: `266 elem/s`
- `32`: `264 elem/s`
- `64`: `320 elem/s`
- unbounded: `325 elem/s`

The runtime truth that matches those numbers is in
`src/store/write/writer/runtime.rs`: the writer drains already-queued ordinary
appends opportunistically, but the cadence loop is what controls when periodic
sync is attempted.

The recommendation that follows from this result is straightforward:
**treat cadence first as the major tuning lever on sync-heavy hardware; treat
drain width as secondary.**

### 5. Drain budget is contention-sensitive, not a general steady-flow lever

`group_commit_max_batch` matters a great deal under contention and much less
under a single producer that cannot keep the mailbox saturated.

Representative contrast:

- uncontended `batch_32`: `264 elem/s`
- contended callers `batch_32`: `5.59 Kelem/s`

That gap is expected from the implementation. `group_commit_max_batch` is a
bound on how many already-queued ordinary appends the writer may drain in one
turn; it does not manufacture queue depth. Under single-producer steady flow,
the writer often finds the mailbox empty before the drain budget is exhausted.

This is an important non-commit for downstream planning:
**drain budget is not a guaranteed throughput lever for single-producer
workloads. It is primarily a contended-producer lever.**

### 6.5. Current topology signal on this workload favored tiled over aos over
scan

The native unified-bench ordering on the measured query surface favored:

- `tiled`
- `aos`
- `scan`

This is not a new public-surface commitment and it is not enough, by itself,
to close any future topology-shape discussion. It is still worth recording as
the current measured ordering to beat if topology tuning or Decision 28-adjacent
work is revisited later.

### 7. Batch append already wins by amortizing the sync boundary

`append_batch` is doing real work for this tree on this hardware:

- batch size `1`: about `231 elem/s`
- batch size `10`: about `2.19 Kelem/s`
- batch size `50`: about `8.71 Kelem/s`
- batch size `100`: about `12.61-13.99 Kelem/s`
- batch size `256`: about `20.78-21.78 Kelem/s`

This is consistent with the current runtime contract: batch append reaches one
durable boundary for the whole batch and only publishes after that final sync.
The extra `batch_cadence_interaction` surface also shows that background cadence
adds only modest overhead on top of batch's own sync-before-visible behavior:

- cadence `1`: `13.39 Kelem/s`
- cadence `1000`: `14.84 Kelem/s`
- cadence `10000`: `13.73 Kelem/s`

That result reinforces the current substrate truth: the biggest write-path win
already in-tree is still durable-boundary amortization.

### 8. Visible-EOF preallocation is blocked on a logical end-of-data contract

The scan/recovery read closed the question of whether visible segment
preallocation is a cheap next spike in the current tree.

It is not.

Current blockers:

- segment scan has skip-and-continue behavior for unreadable payload decode in
  `src/store/segment/scan/full_scan.rs`
- `frame_decode` accepts a zero-length frame shape, so a zero-filled tail is
  not a clean terminal condition by default
- SIDX boundary discovery in `src/store/segment/mod.rs` is anchored to physical
  EOF, not logical end-of-data

The consequence is that visible-EOF preallocation is not a one-line storage
optimization here. In this tree it is a format/scan contract problem first.

The minimal prerequisite is a **logical end-of-data boundary** that both scan
and footer discovery can trust.

### 9. If pipeline work is needed later, the first design to evaluate is an
intra-segment fsync thread

The writer still owns a large synchronous critical section: append/batch commit
prep, sync trigger, index insert, publish sequencing, fanout, and segment
rotation. Given that critical-section shape, the first pipeline design worth
evaluating is not epoch handoff. It is an **intra-segment fsync-thread**
approach that preserves publish ordering and batch sync-before-visible
semantics while offloading the wait on `sync_active_segment()`.

This remains contingent work, not a committed change. The findings above say
there is still cheaper work to do first.

## Recommendation

The recommended order of work is:

1. **Batching and cadence tuning first.**
   Measure and tune the existing cadence and group-commit knobs before changing
   runtime architecture. On the measured hardware, cadence is the dominant
   lever.

2. **Logical EOD contract before any visible preallocation work.**
   Do not pursue visible-EOF segment preallocation until scan/recovery/footer
   logic has an explicit logical end-of-data contract.

3. **Intra-segment fsync-thread evaluation only if tuning still leaves
   meaningful headroom on the table.**
   If batching/cadence tuning does not get the runtime where it needs to be,
   evaluate an intra-segment fsync-thread design before considering larger
   pipeline restructures such as epoch handoff.

This order intentionally avoids architectural churn before the cheaper and more
constrained tuning work is exhausted.

## Scope boundary / non-commits

This ADR does **not** commit the tree to any immediate runtime change. It
records current findings and the substrate-owner recommendation for next steps.

Specifically, this ADR does not commit to:

- changing the default `sync.every_n_events`
- changing the default `group_commit_max_batch`
- changing the default `SyncMode`
- introducing visible-EOF preallocation
- introducing a logical EOD marker or footer redesign yet
- shipping an intra-segment fsync-thread design
- claiming a global regression has been proven

Operationally relevant non-commits reinforced by this memo:

- cadence sync remains a best-effort background policy for ordinary single
  append, not a per-event durability guarantee on return
- `group_commit_max_batch` does not guarantee throughput gains under
  single-producer steady flow
- lock release has an OS-visibility window after drop; retry-with-timeout is
  the established repo pattern rather than assuming immediate reopen success
- the measured `18-21 Kelem/s` CPU ceiling is a measurement on this class of
  hardware and runtime shape, not a timeless store-wide invariant

## Cross-reference

- Environment hint ladder: `tools/integrity/src/main.rs`
- Writer cadence drain loop: `src/store/write/writer/runtime.rs`
- Single append path: `src/store/write/writer/append.rs`
- Batch sync-before-visible path: `src/store/write/writer/batch.rs`
- Segment frame decode and SIDX boundary discovery: `src/store/segment/mod.rs`
- Segment scan behavior: `src/store/segment/scan/full_scan.rs`
- New batching/cadence benchmark surfaces:
  - `benches/unified_bench.rs`
  - `benches/batch_throughput.rs`
- Technical reference tuning surface: `REFERENCE.md`
