# Region Read

Agent surface task: `read_region`.

Problem: read store entries through a substrate boundary that callers can audit.

Correct API: `Region::scope`, `Region::entity`, `Store::query`, typed query
helpers when the payload type owns the kind.

Minimal code is mirrored by `bpk-lib/crates/core/examples/read_only.rs`.

Wrong tempting move: add a public unbounded scan because it is shorter in the
first caller.

Test command: `cargo test -p batpak --test store_query_behavior --all-features`.

Invariant protected: public reads remain bounded by coordinate/region discipline.
