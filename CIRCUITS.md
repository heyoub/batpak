# Circuits

A circuit is a bounded route through which work, state, or evidence moves.

Circuits connect terminals without hiding ownership.

## Circuit Rules

## Circuits Are Bounded

A circuit has known inputs, outputs, and failure modes.

## Circuits Do Not Erase Ownership

Crossing a circuit does not make one battery own another battery's state.

## Hidden Circuits Are Bugs

If durable state changes through an unmodeled route, the route must be exposed or removed.

## Journal Composition

batpak composes multiple store roots through explicit circuits and
observations — not through a hidden cross-directory `Store` API.

| Term | Meaning |
| --- | --- |
| **Journal** | One `Store` on one `data_dir`; one writer owner; lifetime-held directory lock. |
| **Stream** | One `Coordinate` chain inside a journal — a logical context stream. |
| **Observation** | A foreign fact recorded locally: journal B may reference journal A's event or receipt without A's writer executing inside B. |
| **Multi-journal** | Composition and scaling layer — not an extra mutation layer inside one `data_dir`. |

Facts that govern scale-out:

- One writer per `data_dir`. Two live owners of the same mutable directory are
  not supported under today's lock contract.
- No single `global_sequence` spans separate store roots.
- Coordinate sharding gives logical order inside one journal; journal sharding
  gives physical relief when a domain outgrows one writer.
- Bridges and tail consumers read exported or copied snapshots from another
  journal; they must not open the same live `data_dir` beside the owner.

## Circuit Examples

- append path from host input to event receipt
- replay path from event history to projection
- cursor delivery from durable checkpoint to ordered observation
- NETBAT route: `@batpak/sdk` → `netbat` → `syncbat` → `refbat` → `Store`
- multi-journal observation: journal B records a witness of journal A's event or receipt
- netbat route from network frame to operation handling
- release path from `just seal` to manifest evidence

