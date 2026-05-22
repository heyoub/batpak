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

## Circuit Examples

- append path from host input to event receipt
- replay path from event history to projection
- cursor delivery from durable checkpoint to ordered observation
- netbat route from network frame to operation handling
- release path from `just seal` to manifest evidence

