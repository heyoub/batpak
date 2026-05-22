# Integration

batpak owns a local truth boundary. It does not own the whole application.

## Negative Space

batpak does not:

- start an async runtime
- choose your task queue
- become your database server
- become your ORM
- become your workflow engine
- open network connections from the core substrate
- grant authority from `authority_required`
- implement PCP-Core wire validation without explicit codecs, tests, and traceability

## Async Hosts

Async hosts may integrate with batpak by moving blocking work to their own runtime boundary. The substrate remains sync-first.

## Platform Contact

Filesystem, clock, lock, sync, and mmap contact should route through the platform boundary where the store owns that behavior.

## Larger Systems

Use circuits and terminals to connect batteries. Do not hide ownership by letting one battery mutate another battery's state through an unmodeled route.

