# Protocol registry semantic field classes

**Owner layer:** above batpak (protocol-registry / Moonwalker planning spec).

**Disposition:** do not implement semantic field classes, drop policies,
normalization profiles, adapter mapping tables, upstream watcher merge rules, or
non-auto-merge policy vocabulary inside batpak core.

## What batpak already owns

- `batpak::registry` attested row mechanics (`crates/core/src/registry.rs`):
  canonical row bodies, `row_hash`, drift and verification reports with sorted
  findings, supersession graph hygiene, and composition with
  `CanonicalArtifactEnvelope`.

## What stays above batpak

Protocol registry owns domain semantics:

- `semantic_field_classes` and field-class evolution
- `drop_policy` / retention semantics for protocol-shaped rows
- `normalization_profile` and adapter mapping to batpak coordinates
- upstream watcher identity and explicit non-auto-merge rules

Those surfaces compose batpak registry mechanics; they do not belong in the
generic substrate crate.
