# ADR-0010: EventPayload Macro Surface — Payload Binding Layer

## Status
Proposed

## Context

batpak's public write surface takes `(coord, kind, payload)` on every append
path. `EventKind` is a raw `(category: u8, type_id: u8)` pair — callers must
construct and pass it correctly at every callsite. There is no mechanism that
binds a Rust payload type to its EventKind at the type level, so nothing
prevents a caller from appending a `UserCreated` struct under the wrong kind,
or querying by a mistyped constant.

The missing layer is a **payload-binding seam**: a trait that asserts "this
type is the canonical representation of this EventKind," a derive macro that
generates the binding from an attribute, and a set of typed API surfaces that
consume that binding so callers never hand-write `EventKind::custom(...)` in
normal use.

This ADR defines that layer. It does not design projection synthesis
(`#[derive(EventSourced)]`) or reactive workflow macros (`react_loop_typed`).
Those are adjacent canal locks; they are deferred explicitly, not forgotten.

---

## Decision

### The Trait

```rust
pub trait EventPayload: Serialize + DeserializeOwned {
    const KIND: EventKind;
}
```

- `Serialize + DeserializeOwned` — not the HRTB `for<'de> Deserialize<'de>` form;
  `DeserializeOwned` is correct for owned deserialization contexts.
- `const KIND: EventKind` — statically available; no virtual dispatch, no
  registration call, no runtime lookup.
- The trait lives in `batpak` (the main crate). No separate `batpak-macros-support`
  crate unless the proc macro implementation requires it. The support crate is
  an implementation detail, not a public dependency surface.

### The Macro

`#[derive(EventPayload)]` generates the `EventPayload` impl and the required
support items for a named-field struct.

**What it generates:**

1. **One semantic binding**: `impl EventPayload for T { const KIND: EventKind = ...; }`
2. **Minimal support items**: doc attributes that stamp the kind constants onto
   the generated impl, and (behind `#[cfg(test)]`) a static registration item
   and a collision-detection test.

**Attribute contract — `#[batpak(...)]`:**

- `#[batpak(category = <u8 literal>, type_id = <u8 literal>)]` is required
  exactly once per derive.
- `category` and `type_id` are both required. Either missing is a compile error
  with a span-pointed message.
- Unknown keys are errors. Duplicate keys are errors. Multiple `#[batpak(...)]`
  attributes on one item are an error.
- Values are validated at attribute-parse time in the proc macro: out-of-range
  values (category > 0xF, type_id = 0) emit span-pointed errors before any code
  generation runs.

**Named-field structs only in v1.** Tuple structs, enums, and unions are
rejected at macro-expansion time with a clear error. This constraint exists
because schema evolution rules (below) depend on named fields.

**Generic structs** are allowed if the type's existing generic parameters and
where-clauses are preserved unchanged, and the macro adds only the bounds
minimally required for the generated impl (`Serialize + DeserializeOwned`).
The derive must not over-bind.

**Span hygiene**: the generated `impl` block uses `$crate`-qualified paths
throughout. The macro must work correctly both inside the batpak workspace and
in a downstream crate that depends on `batpak` as a library. Path hygiene is
verified by:
- one expansion test inside the batpak workspace
- one fixture crate that depends on `batpak` as a downstream user would, used
  in a `trybuild` or integration test

### Schema Evolution Rules

Each `EventPayload` type carries a schema version. The generated impl exposes:

```rust
const SCHEMA_VERSION: u64 = N;
```

where `N` defaults to 1 and is overridable via
`#[batpak(category = ..., type_id = ..., schema_version = N)]`.

Rules:
- Adding optional fields (with `#[serde(default)]`) requires no version bump.
- Removing fields or changing field types requires a version bump.
- Renaming a field without `#[serde(rename)]` requires a version bump.

These rules are documentation-level constraints. Enforcement is the caller's
responsibility. The version value is stamped into the generated impl and
visible in `rustdoc`.

### Collision Detection

The derive emits a static registration item (using the `inventory` crate) under
`#[cfg(test)]`. A generated `#[test]` function in the same module scans all
registered items and panics if two types share the same `(category, type_id)`
pair.

**Scope**: detection is **per binary**, not per store and not per organization.
Two separate binaries can each register the same `(category, type_id)` with no
warning. Two independent libraries that happen to be composed into the same
binary will produce a collision panic at test time if their kinds overlap.

**Dependency surface**: `inventory` appears only in `#[cfg(test)]` generated
code. It does not enter batpak's normal (non-test) dependency surface.

### v1 Typed API Surface

**Principle**: every public write method that currently takes `kind: EventKind`
explicitly gets a typed sibling. Every public read method whose only type
selector is `EventKind` gets a typed sibling where the type constraint is
meaningful.

**Authoritative v1 list:**

Write surface:
- `Store::append_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T) -> Result<AppendReceipt, StoreError>`
- `Store::append_typed_with_options<T: EventPayload>(&self, coord: &Coordinate, payload: &T, opts: AppendOptions) -> Result<AppendReceipt, StoreError>`
- `Store::submit_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T) -> Result<AppendTicket, StoreError>`
- `Store::try_submit_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T) -> Option<AppendTicket>`
- `Store::append_reaction_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T, correlation_id: u128, causation_id: u128) -> Result<AppendReceipt, StoreError>`
- `Store::submit_reaction_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T, correlation_id: u128, causation_id: u128) -> Result<AppendTicket, StoreError>`
- `Store::try_submit_reaction_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T, correlation_id: u128, causation_id: u128) -> Option<AppendTicket>`
- `BatchAppendItem::typed<T: EventPayload>(coord: Coordinate, payload: &T, options: AppendOptions, causation: Option<CausationRef>) -> Result<Self, StoreError>`

Read surface:
- `Store::by_fact_typed<T: EventPayload>(&self) -> Vec<IndexEntry>`

Typestate integration:
- `Transition::from_payload<P: EventPayload>(payload: P) -> Transition<From, To, P>`

These are the authoritative v1 boundaries. The principle is the rationale;
this list is the authority. Surfaces that are "kind-adjacent" (Outbox,
VisibilityFence) are not in scope for v1.

### Acceptance Criterion

A downstream user can perform normal typed append, submit, and basic fact-query
operations without writing `EventKind::custom(...)` anywhere in their code.
Specifically:
- derive `EventPayload` on a payload struct
- use `append_typed`, `submit_typed`, and `by_fact_typed` end-to-end
- see a compile error with a span-pointed message if `(category, type_id)` is
  missing or out of range
- see a test-time panic if another type in the same binary claims the same kind

---

## Non-Goals

**This ADR closes the payload-binding layer.** It deliberately does not design:

- **`#[derive(EventSourced)]` (Layer 2)**: projection synthesis, dispatch table
  generation, `relevant_event_kinds()` derivation, `from_events()` generation.
  This is the next canal lock. It will use `EventPayload::KIND` as its hook
  into the kind system. Designing it correctly requires this layer to exist and
  prove out first.

- **`react_loop_typed` and typed reactor dispatch (Layer 3)**: reactive workflow
  macros, multi-event routing ergonomics. The design is less constrained and
  involves more surface area than the write/read typed surfaces above. Deferred
  intentionally.

- **`AppendReceiptTyped<T>`**: caller already holds the payload; returning it in
  the receipt means cloning or changing ownership shape. Does not close a
  structural loop the way typed append/query do. Rejected for v1.

- **No-std or WASM targets**: the `inventory` crate's static registration
  mechanism requires a linker that supports `link_section`. WASM and no-std
  environments are out of scope for the collision registry.

---

## Rationale

The three-crate structure (`batpak` / `batpak-macros` / optionally
`batpak-macros-support`) follows the standard Rust proc macro pattern. The
`batpak-macros` crate is a proc macro crate (cannot export non-macro items).
Any types that must be shared between the macro and the runtime (e.g., the
registration item type consumed by the collision scan) go in
`batpak-macros-support` or directly in `batpak::__private`. The latter is
preferred unless the support surface is substantial.

`DeserializeOwned` over the HRTB form: `for<'de> Deserialize<'de>` is
technically more general, but `DeserializeOwned: for<'de> Deserialize<'de>`
covers all owned types and produces cleaner compiler output. All EventPayload
impls are expected to own their data.

Typed siblings use a suffix convention (`_typed`) rather than overloaded
generics on the existing methods to avoid introducing new type-inference
ambiguity at existing callsites. The untyped surface is not removed; the typed
surface is additive.

`Transition::from_payload` is a library change, not a macro feature. It is
included because it makes the typestate and payload-binding layers coherent:
once a payload type carries `::KIND`, any `Transition` can derive its kind
automatically from the payload type.
