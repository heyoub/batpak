# Terminals

A terminal is a named boundary where a battery accepts, denies, emits, or observes work.

Terminals are authority boundaries, not just function names. If work crosses from a host into a battery, or from one battery into another, the route must be visible.

## Terminal Rules

- Terminals name what may cross the boundary.
- Terminals apply policy before durable state changes.
- Terminals emit receipts or typed outcomes for durable operations.
- Terminals do not hide runtime ownership.
- Hidden wires are bugs.

## Today In batpak

Current terminal-shaped surfaces include:

- `Store` public methods
- append and batch append paths
- gate evaluation and denial paths
- cursor and subscription delivery surfaces
- projection and replay entry points
- netbat routes and operation handling surfaces

## Terminal Versus Function

A function is an implementation unit. A terminal is a boundary promise.

Many functions are not terminals. A terminal is where authority, input, evidence, or durable state crosses a meaningful boundary.

## Breakers

Gates and policy checks act like breakers. They do not make work disappear; they accept, deny, or classify it with evidence.

