# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- Initial implementation of batpak v0.1.0
- Coordinate-addressed append-only causal log
- Typestate-aware transitions with EventSourced projection replay
- Outcome<T> algebraic type with monad laws (verified by proptest)
- Gate/Pipeline system with sealed Receipt for TOCTOU prevention
- Store with background writer thread, flume-based bisync channels
- Blake3 hash chains (optional), CRC32 frame verification
- ProjectionCache trait with NoCache, RedbCache, LmdbCache backends
- Region-based query, subscription (push), and cursor (pull) APIs
