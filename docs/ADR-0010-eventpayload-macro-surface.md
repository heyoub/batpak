# ADR-0010: EventPayload Macro Surface — Payload Binding Layer

## Status
Accepted (shipped in 0.6.0).

**Shipped subset note.** The decisions below shipped as four derives plus
their runtime seams: `#[derive(EventPayload)]`, the `DecodeTyped` decode
seam, `#[derive(EventSourced)]`, and `#[derive(MultiEventReactor)]`.
Adjacent concerns — durability-truth-up of typed-append / typed-reactor /
cursor-checkpoint behavior — are covered by the Integrity Closeout chapter,
not by this ADR.

## Context

batpak's public write surface takes `(coord, kind, payload)` on every append
path. `EventKind` is a sealed packed `u16` carrying a 4-bit category and a
12-bit type id: `(category: 4 bits) << 12 | (type_id: 12 bits)`. The
construction signature is `EventKind::custom(category: u8, type_id: u16)`
(see `crates/core/src/event/kind.rs`). Callers had to construct and pass that value at
every callsite. There was no mechanism binding a Rust payload type to its
`EventKind` at the type level, so nothing prevented a caller from appending a
payload under the wrong kind, or querying by a mistyped constant.

The missing layer is a **payload-binding seam**: a trait that asserts "this
type is the canonical representation of this EventKind," a derive macro that
generates the binding from an attribute, and a set of typed API surfaces that
consume that binding so callers never hand-write `EventKind::custom(...)` in
normal use.

This ADR covers the payload-binding seam; subsequent chapters cover adjacent
seams (dispatch, materialization) without implying any ordering between
them.

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
  registration call, no runtime lookup. `P::KIND` is the sole event identity
  mechanism across every downstream consumer.

### The Macro

`#[derive(EventPayload)]` generates the `EventPayload` impl and minimal
support items for a named-field struct.

**What it generates:**

1. `impl ::batpak::event::EventPayload for T { const KIND: ::batpak::event::EventKind = ::batpak::event::EventKind::custom(category, type_id); }`
2. An unconditional static registration item for binary-wide collision
   detection (see Collision Detection below).
3. A `#[cfg(test)]`-gated `#[test]` function that scans the collision registry.

**Attribute contract — `#[batpak(...)]`:**

- `#[batpak(category = <integer literal>, type_id = <integer literal>)]` is
  required exactly once per derive.
- `category` is a 4-bit value (0x1–0xF, excluding 0x0 and 0xD which are
  reserved for system and effect kinds respectively).
- `type_id` is a 12-bit value (0x000–0xFFF).
- Both keys are required. Missing either is a compile error with a
  span-pointed message at the struct site.
- Unknown keys are errors. Duplicate keys are errors. Multiple `#[batpak(...)]`
  attributes on one item are an error, spanned at the second occurrence.
- Values are validated at attribute-parse time via
  `batpak_macros_support::{validate_category, validate_type_id}` before any
  code generation runs. Out-of-range values produce span-pointed errors on the
  literal token.

**Named-field structs only.** Tuple structs, enums, and unions are rejected
at macro-expansion time with a clear span-pointed error.

**Generics.** The derive preserves the input type's generic parameters and
where-clauses (`impl_generics` / `ty_generics` / `where_clause` from
`syn::Generics::split_for_impl`). It adds no bounds beyond what
`EventPayload` already requires. The collision registry keys a
single textual `type_name` per derive site, so practical use is non-generic
named-field structs; collision behavior across multiple generic
instantiations of the same derive site is undefined.

**Path hygiene.** The generated `impl` block uses absolute `::batpak::...`
paths throughout (`::batpak::event::EventPayload`,
`::batpak::event::EventKind`, `::batpak::event::EventKind::custom`,
`::batpak::__private::inventory`,
`::batpak::__private::EventPayloadRegistration`,
`::batpak::__private::assert_no_kind_collisions`). The `::batpak::` prefix
resolves correctly in two contexts:

- **Downstream crates** — via the root re-export
  `pub use batpak_macros::EventPayload;` at `crates/core/src/lib.rs`, which makes
  `batpak::EventPayload` name the derive macro while the trait lives at
  `batpak::event::EventPayload` (mirroring the `serde` pattern: trait and
  derive share a name across different namespaces). The fixture at
  `crates/core/fixtures/downstream/` proves this.
- **Inside the `batpak` crate itself** — `pub extern crate self as batpak;`
  at the crate root makes `::batpak::...` resolve to `self::...` from within
  the crate, so `#[derive(EventPayload)]` inside `crates/core/src/`-style modules works
  identically to the downstream case.

### Schema evolution

Schema evolution is the caller's responsibility. Additive optional fields
with `Option<T>` or `#[serde(default)]` are wire-safe; removing, renaming,
or retyping a field requires bumping `type_id`. **No `SCHEMA_VERSION`
constant exists** and no `schema_version` attribute is accepted by the
derive; any additional key (e.g. `schema_version`) is rejected as unknown.

**Projection cache invalidation is a separate concept**, governed by
`cache_version` on projection types via `#[derive(EventSourced)]` — never
mixed with payload wire identity. No attribute, doc, or error message in
this layer references payload schema versioning, and no projection-cache
attribute touches payload wire identity.

### Collision Detection

The derive emits a static registration item unconditionally via the
`inventory` crate, re-exported through `batpak::__private::inventory`. A
generated `#[cfg(test)] #[test]` function per derive site calls
`::batpak::__private::assert_no_kind_collisions()`, which iterates the
registry and panics with a clear message if two types share the same
`(category, type_id)` pair. Applications and integration tests can call
`batpak::event::validate_event_payload_registry()` directly for a structured
error instead of a panic.

**Scope.** Detection is **per binary**, not per store and not per
organization. Two separate binaries can each register the same
`(category, type_id)` with no warning. Two independent libraries that happen
to be composed into the same test binary will produce a collision panic at
test time if their kinds overlap.

**Dependency surface.** Both `inventory::collect!` in `batpak-macros-support`
and derive-generated `inventory::submit!` registrations are unconditional.
A library's `#[cfg(test)]` items are not compiled into the `cfg(test)` build
of a dependent crate, so test-gating `submit!` would hide dependency-crate
payloads from the composing binary. **The invariant we defend**:
`inventory` never participates in runtime dispatch or production event
routing. `P::KIND` is the sole dispatch identity. The registry is an
observability and validation surface: `Store::open` warns once per process by
default when collisions are linked, and callers can set
`EventPayloadValidation::FailFast` to reject opens or call
`validate_event_payload_registry()` explicitly at startup.

### Typed API Surface (shipped)

**Principle.** Every public write method that currently takes
`kind: EventKind` explicitly gets a typed sibling. Every public read method
whose only type selector is `EventKind` gets a typed sibling where the type
constraint is meaningful.

**Authoritative list (verified against `crates/core/src/store/mod.rs`):**

Write surface:
- `Store::append_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T) -> Result<AppendReceipt, StoreError>`
- `Store::append_typed_with_options<T: EventPayload>(&self, coord: &Coordinate, payload: &T, opts: AppendOptions) -> Result<AppendReceipt, StoreError>`
- `Store::submit_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T) -> Result<AppendTicket, StoreError>`
- `Store::try_submit_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T) -> Result<Outcome<AppendTicket>, StoreError>`
- `Store::append_reaction_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T, correlation_id: u128, causation_id: u128) -> Result<AppendReceipt, StoreError>`
- `Store::submit_reaction_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T, correlation_id: u128, causation_id: u128) -> Result<AppendTicket, StoreError>`
- `Store::try_submit_reaction_typed<T: EventPayload>(&self, coord: &Coordinate, payload: &T, correlation_id: u128, causation_id: u128) -> Result<Outcome<AppendTicket>, StoreError>`
- `BatchAppendItem::typed<T: EventPayload>(coord: Coordinate, payload: &T, options: AppendOptions, causation: CausationRef) -> Result<Self, StoreError>`

Read surface:
- `Store::by_fact_typed<T: EventPayload>(&self) -> Vec<IndexEntry>`

Typestate integration:
- `impl<From: StateMarker, To: StateMarker, P: EventPayload> Transition<From, To, P> { pub fn from_payload(payload: P) -> Self }`

These are the authoritative shipped boundaries and match the code signatures
verbatim. `try_submit_typed` and `try_submit_reaction_typed` return
`Result<Outcome<AppendTicket>, StoreError>` — not `Option<AppendTicket>` —
because the outer `Result` distinguishes store errors (writer failures,
serialization) from admission-control outcomes (pressure signals the caller
can retry).

### Acceptance Criterion

A downstream user can perform normal typed append, submit, and basic
fact-query operations without writing `EventKind::custom(...)` anywhere in
their code. Specifically:

- derive `EventPayload` on a payload struct with `#[batpak(category = N, type_id = N)]`
- use `append_typed`, `submit_typed`, and `by_fact_typed` end-to-end
- see a compile error with a span-pointed message if `(category, type_id)` is
  missing, unknown-keyed, duplicated, or out of range
- see a test-time panic, open-time warning, explicit validator error, or
  fail-fast open error if another type in the same binary claims the same
  `(category, type_id)` pair

All four have shipped. See `crates/core/tests/event_payload_surface.rs` for the positive
surface, `crates/core/tests/derive_eventpayload_errors.rs` + `crates/core/tests/ui/ep_*.{rs,stderr}`
for span-pointed compile errors, `crates/core/fixtures/downstream/` for the
path-hygiene fixture, and `crates/core/fixtures/kind-collision-composer/` for cross-crate
collision detection.

---

## Non-Goals

This ADR defines the payload-binding seam. The following are separate seams
with their own ADRs or chapters; nothing here orders or sequences them:

- **Dispatch-layer macros** (`#[derive(EventSourced)]`,
  `#[derive(MultiEventReactor)]`, `react_loop_typed`). Their design
  question is how to dispatch events by `P::KIND` across both replay lanes
  without each consumer inventing its own dispatch model. Covered by the
  Dispatch Chapter.

- **Typed reactor canal semantics**. The choice between cursor-guaranteed
  and fanout-lossy delivery, error propagation, restart policy, and
  checkpoint interaction. Covered by ADR-0011.

- **Query-result materialization** (`Vec<Arc<IndexEntry>>` vs lease-guarded
  borrow vs id-return). The gather-cost seam is distinct from the
  payload-binding seam. Covered by its own ADR when the work opens.

- **`AppendReceiptTyped<T>`**. The caller already holds the payload;
  returning it in the receipt means cloning or changing ownership shape.
  Does not close a structural loop the way typed append/query do. Rejected.

- **No-std or WASM targets**. The `inventory` crate's static registration
  mechanism requires a linker that supports `link_section`. WASM and
  no-std environments are out of scope for the collision registry.

---

## Rationale

**Three-crate structure.** `batpak` / `batpak-macros` / `batpak-macros-support`
follows the standard Rust proc-macro pattern. `batpak-macros` is a proc-macro
crate and cannot export non-macro items; the shared registration type
consumed by the collision scan lives in `batpak-macros-support` and is
re-exported through `batpak::__private`. Users depend only on `batpak`.

**`DeserializeOwned` over the HRTB form.** `for<'de> Deserialize<'de>` is
technically more general, but `DeserializeOwned: for<'de> Deserialize<'de>`
covers all owned types and produces cleaner compiler output. All
`EventPayload` impls own their data.

**Typed siblings use a `_typed` suffix** rather than overloaded generics on
the existing methods to avoid introducing type-inference ambiguity at
existing callsites. The untyped surface is unchanged; the typed surface is
additive.

**`Transition::from_payload` is a library change, not a macro feature.** It
is included here because it makes the typestate and payload-binding layers
coherent: once a payload type carries `::KIND`, any `Transition` can derive
its kind automatically from the payload type. Signature:
`Transition::from_payload(payload: P) -> Self` on
`impl<From: StateMarker, To: StateMarker, P: EventPayload> Transition<From, To, P>`.

**Absolute `::batpak::...` paths in generated code.** The alternative
(`$crate`) expands differently depending on whether the derive fires inside
or outside the defining crate. Absolute paths combined with the
`pub extern crate self as batpak;` alias at the crate root resolve
identically everywhere, which is verified by three tests: the in-workspace
smoke test (`crates/core/tests/event_payload_surface.rs`), the in-crate derive resolver
test, and the downstream fixture at `crates/core/fixtures/downstream/`.

**Kind-adjacent surfaces** — `Outbox::stage`, `VisibilityFence::submit`,
`VisibilityFence::submit_reaction` — are closed by the Integrity Closeout
chapter's typed write-surface work.
