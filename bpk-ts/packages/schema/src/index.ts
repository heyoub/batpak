/**
 * `@batpak/schema` — Effect 4 Schema bridge for the BatPAK canonical wire.
 *
 * What this package actually does (production, not stub):
 *
 *   1. Wraps Effect 4 Schema decode/encode around `@batpak/canonical`
 *      MessagePack bytes. Consumers parse incoming wire bytes through
 *      a generated Schema and get back a validated typed value; encode
 *      a typed value and get back wire-canonical bytes.
 *   2. Exposes the Effect `Schema` namespace so consumers can compose
 *      additional refinements on top of generated types.
 *   3. Exposes `bank.event()` — the long-term TS authoring entry point.
 *      In 0.7.6 this is a typed alias over `Schema.Struct({...})` so
 *      consumers can begin defining downstream-only event shapes today;
 *      Phase 2+ will round-trip TS declarations back to Rust generation,
 *      but the symbol stays stable across that transition.
 *
 * Authority direction in 0.7.6:
 *   - Rust `#[derive(EventPayload)]` + the manifest is still the source
 *     of truth for events that need to ride the canonical wire.
 *   - `bank.event()` is for downstream-only TS event shapes (e.g. agent
 *     internal state) that never leave the TS world.
 */

import * as Schema from "effect/Schema";

import { decode as canonicalDecode, encode as canonicalEncode } from "@batpak/canonical";

/** Re-export the Effect Schema namespace so downstreams can pin against it. */
export { Schema };

/**
 * Decode canonical MessagePack bytes into a typed value validated by the
 * given Effect Schema.
 *
 * Throws if either MessagePack decoding fails or the decoded shape does
 * not match the schema.
 */
export function decodeBytes<T, E>(
  schema: Schema.Codec<T, E>,
  bytes: Uint8Array,
): T {
  const raw = canonicalDecode(bytes);
  return Schema.decodeUnknownSync(schema)(raw);
}

/**
 * Encode a typed value into canonical MessagePack bytes, validating the
 * value against the given schema first.
 */
export function encodeBytes<T, E>(
  schema: Schema.Codec<T, E>,
  value: T,
): Uint8Array {
  const validated = Schema.encodeUnknownSync(schema)(value);
  return canonicalEncode(validated);
}

/**
 * `bank.event()` — declare a TS-side event by specifying its field
 * shape.
 *
 * Phase 0/0.7.6 semantics:
 *   - Returns an `effect/Schema.Struct(...)` directly. No magic.
 *   - The schema is REAL — `Schema.decodeUnknownSync(myEvent)` validates
 *     incoming objects and `Schema.encodeUnknownSync(myEvent)` encodes
 *     them.
 *   - This entry point is intended for downstream-only TS events. Events
 *     that need to ride the canonical wire MUST currently be authored
 *     in Rust via `#[derive(EventPayload)]` so their numeric kind and
 *     canonical bytes stay byte-exact between languages.
 *
 * Phase 2+: this symbol will also drive Rust generation, so the
 * downstream pinning stays stable across the authority flip.
 */
export const bank = {
  /**
   * Build an Effect Schema struct for a TS-side event.
   *
   * @example
   *   const Move = bank.event({
   *     x: Schema.Number,
   *     y: Schema.Number,
   *     reason: Schema.String,
   *   });
   *   type Move = typeof Move.Type;
   */
  event<Fields extends Schema.Struct.Fields>(fields: Fields): Schema.Struct<Fields> {
    return Schema.Struct(fields);
  },
} as const;
