# Projections

The log is the source of truth. A projection is a cached interpretation.

Factory prose may call projections gauges. The Rust API should keep the engineering name `Projection`.

## Projection Rules

- A projection is derived from events.
- A projection may be rebuilt.
- A projection cache is optional acceleration unless a surface explicitly says otherwise.
- A stale projection must be classified, not silently treated as fresh.
- A projection failure should produce typed outcome or evidence when user-visible freshness depends on it.

## Outside The Model

If a value cannot be rebuilt from the log, it is application state outside batpak's projection model.

That may be valid application design. It is not a batpak projection.

