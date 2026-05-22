# Events

An event records a domain fact at a coordinate.

Events are source truth. Projections, indexes, caches, and reports may be rebuilt or rederived from accepted events and their artifacts.

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

## Ordering

batpak records ordering information required by the substrate. Application-level ordering beyond that must be modeled by the application or by explicit higher-level batteries.

## Corrections

Accepted events are immutable. Corrective work is represented by later events, not by editing old ones.

