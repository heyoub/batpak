# Events

An event records a domain fact at a coordinate.

Events are the source of truth. Projections, indexes, caches, and reports may be rebuilt or rederived from accepted events and their artifacts.

## Valid Append Shape

The exact Rust constructor may change, but a valid append supplies:

- a coordinate
- a payload
- enough metadata to canonicalize and link the event
- policy input for gates where required

## Coordinates

A coordinate names where an event belongs. Keep the engineering name `Coordinate`; factory prose may describe it as where a cell lands in the pack.

## Payloads

Payload bytes are part of the event contract. When structured payloads are hashed or linked, canonical encoding matters.

## Envelope Boundary

The substrate event kind is not the same as a domain receipt taxonomy. batpak may
store one registered envelope kind, while the payload inside that event carries
domain-owned strings such as `receipt_kind`.

That split is intentional:

- batpak owns numeric `EventKind`, canonical bytes, hashes, receipts, indexes,
  and replay traversal.
- application layers own envelope schemas, domain receipt families, and payload
  dispatch.
- `event.query` returns metadata summaries only; it does not return payload
  bytes, extension maps, or decoded domain fields.

Use `event.get` to fetch payload bytes after traversal, then decode the envelope
above batpak.

## Ordering

batpak records ordering information required by the substrate. Application-level ordering beyond that must be modeled by the application or by explicit higher-level batteries.

## Corrections

Accepted events are immutable. Corrective work is represented by later events, not by editing old ones.
