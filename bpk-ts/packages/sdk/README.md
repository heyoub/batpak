# @batpak/sdk

One-install TypeScript SDK for [batpak](https://github.com/heyoub/batpak), the
embedded, tamper-evident event store. This package re-exports everything a
client application needs to talk to a batpak host over the NETBAT/1 wire
protocol:

- **@batpak/client** — NETBAT/1 frame writer/reader with typed error codes,
  TCP transport included.
- **@batpak/canonical** — named-field MessagePack codec, byte-for-byte
  identical to the Rust encoder (parity-tested in CI).
- **@batpak/generated** — event types and Effect 4 schemas generated from the
  substrate-owned manifest.
- **@batpak/schema** — runtime-validated `decodeBytes`/`encodeBytes`, plus
  `bank.event()` for authoring downstream-only TypeScript events.

## Install

```sh
npm install @batpak/sdk
```

## Use

```typescript
import {
  call,
  decodeBytes,
  encodeBytes,
  encodeHex,
  BankCommitRequest,
  BANK_COMMIT,
} from "@batpak/sdk";
```

The reference host (`refbat`, in the main repository) exposes six core
operations — `system.heartbeat`, `bank.commit`, `event.get`, `event.query`,
`receipt.verify`, `event.walk` — plus four `evidence.*` report operations.
Every payload round-trips through the same canonical bytes the Rust store
hashes and signs, so receipts verified on one side hold on the other.

## Why

batpak gives applications a tamper-evident, replayable record of what
happened: every event is Blake3 hash-bound to its ancestor, every accepted
write returns a verifiable (optionally Ed25519-signed) receipt, and derived
views are rebuilt from the log by construction. This SDK brings that contract
to TypeScript without weakening it: types and schemas are generated from the
substrate manifest, never hand-maintained.

Full documentation, wire contract, and examples:
[github.com/heyoub/batpak](https://github.com/heyoub/batpak/tree/main/bpk-ts).

## License

MIT OR Apache-2.0.
