# Receipts

Receipts are structured evidence of outcomes.

They are not logs, recommendations, or explanations. A receipt answers:

- what was attempted
- what inputs mattered
- what outcome occurred
- what evidence binds that outcome to the log or artifact

## Receipt Classes

Implemented or active receipt-shaped outcomes include:

- append receipts
- denial outcomes
- replay and read evidence reports
- projection evidence reports
- snapshot and chain-walk reports
- receipt verification outcomes
- release and inspection reports emitted by tooling

Only canonize a receipt category as public when code, tests, and traceability make it real.

## Verification

Verification should expose typed outcomes where callers need to distinguish failure reasons. Boolean projections may remain as ergonomic helpers over typed truth.

On the reference host, `receipt.verify` accepts ack-shaped append receipt fields
(the same shape as a `bank.commit` ack) and returns `{ valid, outcome, reason_code }`.
Wire `outcome` is `"signed"`, `"unsigned_accepted"`, or `"invalid"`. When invalid,
`reason_code` is a stable snake-case string mapped from substrate verification
errors — never debug formatting.

## Reports

Evidence reports are receipt-shaped artifacts for inspections and derived views. They should name inputs, output hashes, versions, and the reason for any refusal or fallback.

The reference host surfaces the substrate evidence-report family over `NETBAT/1`
through the domain-neutral `evidence.*` ops, with domain-neutral receipt kinds:

- `receipt.evidence.chain_walk.v1` — `evidence.chain_walk`
- `receipt.evidence.store_resource.v1` — `evidence.store_resource`
- `receipt.evidence.read_walk.v1` — `evidence.read_walk`
- `receipt.evidence.projection_run.v1` — `evidence.projection_run`

Each ack ships the report **body** as a canonical-encoding blob (`report_hex`)
alongside its `body_hash`. Evidence-report identity is the content hash over the
canonical body bytes, so the wire ships the exact bytes the hash covers: a
consumer re-hashes `report_hex` and confirms it equals `body_hash`. A typed
field-by-field mirror would re-encode and break that identity, so the blob form
is the identity-preserving shape.

