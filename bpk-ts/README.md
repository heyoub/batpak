# BatPAK TypeScript SDK

Typed TypeScript clients for a batpak host: a NETBAT/1 wire client, a
canonical MessagePack codec that is byte-for-byte identical to the Rust
encoder, and manifest-generated event types with Effect 4 runtime validation.

The Rust substrate ([batpak](../README.md)) owns source truth; this workspace
is how TypeScript applications talk to it across a network boundary —
appending events, paging commit order, verifying receipts, and walking
hash-chain ancestry, with the same canonical bytes on both sides.

```sh
npm install @batpak/sdk
```

Ten reference operations live on NETBAT/1 over TCP today — six core substrate
ops plus four domain-neutral `evidence.*` ops:

```
system.heartbeat   — wire-parity liveness probe.
bank.commit        — append a typed event to the underlying batpak store;
                     returns the full AppendReceipt (event_id, sequence,
                     content_hash, key_id, optional signature, extensions).
event.get          — read a stored event by event_id; returns header
                     + canonical payload bytes + coordinate.
event.query        — bounded, domain-neutral commit-order pages over event
                     summaries; resume by sending after_global_sequence =
                     the previous page's next_after_global_sequence, then
                     fetch payload bytes with event.get.
receipt.verify     — verify ack-shaped append receipt fields against the
                     committed store.
event.walk         — bounded hash-chain ancestry from a starting event_id;
                     relation order, not commit-order pagination.
evidence.chain_walk — chain-walk evidence report over bounded hash-chain ancestry.
evidence.store_resource — point-in-time store resource evidence snapshot.
evidence.read_walk — read-walk evidence report with full Region selector axes.
evidence.projection_run — projection-run evidence; reference refbat registers
                     no projections, so unknown projection ids return a
                     handler error unless an embedder registers projections.
```

Authority direction:

```
Rust #[derive(EventPayload)]
  +
refbat::manifest::descriptors()   (substrate-owned registry)
  ↓
cargo xtask export-ts-manifest
  ↓
bpk-ts/batpak.manifest.json
  ↓
@batpak/codegen
  ↓
@batpak/generated   (Effect 4 schemas + TS types + golden hex)
  ↓
@batpak/sdk         (one-install re-export for npm consumers)
```

## npm

For apps talking to a NETBAT/1 host (for example `refbat`), install one package:

```sh
npm install @batpak/sdk
```

That pulls `@batpak/client`, `@batpak/schema`, `@batpak/generated`, and
`@batpak/canonical` transitively. Import everything from `@batpak/sdk`:

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

Lower-level packages remain published separately for consumers that want
a narrower dependency surface.

## Workspace layout

```
packages/
  canonical/  Named-field MessagePack codec matching rmp-serde 1.3.1
              byte-for-byte. 29 direct tests.
  client/     NETBAT/1 frame writer/reader; typed NetbatError union
              covering all 12 codes from netbat::NetbatError::code();
              TCP transport via duck-typed NodeSocketLike. 29 direct tests.
  codegen/    Reads batpak.manifest.json, emits @batpak/generated.
              Refuses unsupported manifestVersion, netbatVersion,
              canonicalEncoding, field-name drift, unknown typeToken.
              28 direct tests.
  generated/  AUTO-GENERATED Effect 4 schemas + TS types + golden hex.
              Fully overwritten by each codegen run.
  schema/     Effect 4 Schema bridge — decodeBytes/encodeBytes wrap
              @batpak/canonical with runtime validation; bank.event() is
              the real Effect Schema authoring API for downstream-only
              TS events. 6 direct tests.
  sdk/        One-install npm entry; re-exports canonical, client,
              schema, and generated for downstream apps.
  test/       End-to-end parity harness across every event and every
              operation in the manifest. 127 parity assertions.
examples/
  heartbeat-spike/  Calibration pulse against refbat:
                    - sends system.heartbeat
                    - sends bank.commit (appends a typed event)
                    - sends event.query (pages metadata by coordinate and
                      global_sequence)
                    - sends event.get (reads it back, decodes through
                      Effect 4 schema; proves byte round-trip)
                    - sends an unknown_operation to validate the typed
                      ERR-frame path.
                    - note: receipt.verify, event.walk, and the four
                      evidence.* ops round out the ten-op host profile and
                      are covered by manifest/parity tests and refbat tests.
  audit-loop/       Living loop against refbat:
                    - commits app-owned events (kind_category=0x01)
                    - rebuilds an ordered audit view from event.query +
                      event.get (not commit acks)
                    - supports --replay-only after refbat restart on the
                      same store directory
```

## refbat — the reference host

`refbat` (in `bpk-lib/crates/refbat/`) registers all ten reference operations against
a real BatPAK store. `publish = false`. Loopback-only by default; bind
to a non-loopback interface only with `--allow-non-loopback`.

Boot:

```sh
cargo run -p refbat -- serve \
  --store $(mktemp -d) \
  --tcp 127.0.0.1:0 \
  --print-port
```

The first stdout line is a machine-readable rendezvous:

```
REFBAT_READY {"addr":"127.0.0.1:54321","port":54321,"protocol":"NETBAT/1"}
```

Parse the JSON, take `port`, connect.

## Running locally

```sh
# Regenerate the manifest from the substrate.
cd bpk-lib
cargo run -p xtask -- export-ts-manifest --out ../bpk-ts/batpak.manifest.json

# Build + test the TS workspace.
cd ../bpk-ts
pnpm install
pnpm -w build
pnpm -w test          # 220 tests across all packages

# Live integration:
cd ../bpk-lib
cargo run -p refbat -- serve --store "$(mktemp -d)" --tcp 127.0.0.1:0 --print-port \
  > /tmp/refbat-ready.txt 2>&1 &
sleep 0.5
PORT=$(node -e 'const j=require("fs").readFileSync("/tmp/refbat-ready.txt","utf-8").trim();process.stdout.write(String(JSON.parse(j.replace(/^REFBAT_READY /,"")).port))')

cd ../bpk-ts
node examples/heartbeat-spike/dist/index.js --port "$PORT"
# spike: ok  ← calibration pulse proves heartbeat + commit/query/get + ERR
#              path on the wire; receipt.verify/event.walk remain manifest-
#              and parity-tested reference operations.
```

## Wire encoding contract

Canonical bytes are named-field MessagePack via `rmp-serde 1.3.1` on the
Rust side and a minimal byte-equivalent encoder on the TS side. Both
respect Rust struct **declaration order** for fields (not alphabetical).

Token vocabulary supported by the codegen:

```
string              → string
u8 / u16 / u32      → number with isInt + isBetween bounds
u64-safe            → number bounded to Number.MAX_SAFE_INTEGER
u64-millis          → same as u64-safe; semantically milliseconds since epoch
i64-microseconds    → number bounded to ±Number.MAX_SAFE_INTEGER
option<string>      → string | null
option<u128-hex>    → 32-char lowercase hex string | null
option<u8>          → number | null, bounded to 0..255
option<u16>         → number | null, bounded to 0..65535
option<u64-safe>    → number | null, bounded to Number.MAX_SAFE_INTEGER
bool                → boolean
array<EventSummary> → EventSummary[]
map<string,string>  → Record<string, string>
u128 values         → emitted as 32-char lowercase hex string (overflows safe-int)
```

NETBAT/1 frame format (verbatim from `netbat::transport`):

```
NETBAT/1 CALL <operation-name> <hex-input>\n
OK <hex-output>\n
ERR <code> <hex-message>\n
```

ERR `<code>` is one of the 12 stable ASCII tokens from
`netbat::NetbatError::code()`. The message is hex-encoded **UTF-8 text**
— never hex-encoded MessagePack.

## Determinism

`@batpak/generated/src` is **fully overwritten** on every `pnpm generate`.
A CI step verifies `rm -rf packages/generated/src && pnpm -w build`
produces a byte-identical tree.

## Out of scope for 0.8.x

- NETBAT/2 STREAM (reserved per ADR-0030).
- TS-authored events generating Rust kinds (Phase 2; `bank.event()`
  exists for downstream-only TS schemas in the meantime).
- Browser / WebSocket / NAPI / WASM transports.
- A `Bank` Rust type (composition vocabulary stays in TS until proven
  necessary).
