# ADR-0005: Test Support Feature Boundary

## Status
Accepted

## Context
Some tests need privileged hooks such as intentional writer panic injection, but those hooks should not appear in default production builds.

## Decision
Privileged test hooks live behind a non-default `test-support` feature and are exercised by targeted integration tests and CI feature matrices.

## Consequences
- Default consumers do not see test-only APIs.
- Full-feature CI still validates restart and fault-injection behavior.
- The repo can keep adversarial tests without widening the production surface.
