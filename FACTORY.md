# Factory

Free Battery Factory builds small sync-first batteries for software boundaries.

batpak stands for battery pack. The name is a model, not decoration: a battery owns one boundary, stores local truth, exposes named terminals, and can be wired into a larger host without becoming the whole machine.

The core sentence:

> batpak is a battery pack for application truth: small, sync-first, append-only cells that can be replayed, verified, and wired into larger systems.

The factory sentence:

> The Free Battery Factory makes batteries for software boundaries.

The rule:

> A battery does not own the machine. It powers one boundary.

Use factory words for mental model and composition. Use engineering words for exact API contracts.

## Product Pressure Boundary

The downstream product direction is to describe a pain, shape a living work
artifact, and project it wherever the work needs to happen. That sentence is
pressure on BatPAK's naming and boundaries, not a BatPAK feature list.

BatPAK supplies durable append/event truth, receipts, replay, projections as
mechanisms, evidence, and opaque extension bytes. Higher layers own the product,
agent-framework, Moonwalker, and PCP-Core semantics that interpret those bytes.

## Factory Contract

Every shipped battery should preserve the same shape:

- explicit terminals
- bounded state
- receipt-bearing operations
- replayable source truth where applicable
- no hidden runtime ownership
- conformance checks that can be run from the root command surface

## Command Counter

Humans and agents enter through `just` at the repository root:

```sh
just list
just inspect
just host-dev
just host-loop
just ledger-list
just context
just verify
just perf-gates
just loom
just seal
just ship dry
```

`just` is the command counter. `xtask` is the factory machinery. `ast-grep` is the semantic inspection camera. Tests inspect behavior. Receipts preserve evidence.

The opt-in factory ledger (`just ledger-run -- …`, `just ledger-list`, `just ledger-run-gate …`) records command proof events into `bpk-lib/target/factory-ledger/store/`. Normal builds do not depend on it; use it when you want a queryable local proof trail for wrapped commands such as `just ledger-run -- just host-loop`.

Command events (`factory.command.*`) record that a wrapped command ran. Gate events (`factory.gate.completed`) record which named proof that command represented when using `just ledger-run-gate …`.

The opt-in context packet (`just context`) writes a PCP-aligned handoff artifact to `bpk-lib/target/context/latest.json` and `latest.md`. It captures git state, stacked-PR hints, factory-ledger tail, and boundary reminders for agent/operator handoff. It is local tooling only — not PCP-Core, not a CI gate.

The manual proof commands (`just perf-gates`, `just loom`) run the existing
xtask proof surfaces from the root counter. They are release-confidence tools:
perf gates are hardware-dependent and should run alone; loom explores bounded
concurrency schedules. They do not change the public Rust API.

## Current Batteries

The shipped family is described in [BATTERIES.md](BATTERIES.md). Do not treat reserved names as shipped products.
