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
just verify
just seal
just ship dry
```

`just` is the command counter. `xtask` is the factory machinery. `ast-grep` is the semantic inspection camera. Tests inspect behavior. Receipts preserve evidence.

## Current Batteries

The shipped family is described in [BATTERIES.md](BATTERIES.md). Do not treat reserved names as shipped products.

