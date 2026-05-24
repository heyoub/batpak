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

