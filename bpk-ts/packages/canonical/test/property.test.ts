/**
 * Property-based round-trip tests for `@batpak/canonical` using
 * fast-check.
 *
 * The fixture-driven `canonical.test.ts` proves the encoder produces
 * the exact bytes rmp-serde does for two specific shapes. This file
 * proves the codec is round-trip-safe across an arbitrary stream of
 * inputs: every `encode(decode(x)) === x` and `decode(encode(x))`
 * matches the original deep-equal.
 *
 * Property tests catch encoder/decoder pairs that disagree on edge
 * shapes (empty strings, empty maps, 0/MAX_SAFE_INTEGER, deeply
 * nested objects, mixed key types) where a hand-written fixture set
 * is too thin to surface drift.
 */

import { describe, expect, it } from "vitest";
import fc from "fast-check";

import { decode, encode } from "../src/index.js";

// Encoder supports JSON-shaped values: null, boolean, finite number,
// string, array, object-with-string-keys. Build a recursive arbitrary
// over those shapes.
const jsonValue: fc.Arbitrary<unknown> = fc.letrec((tie) => ({
  json: fc.oneof(
    { withCrossShrink: true },
    fc.constant(null),
    fc.boolean(),
    // Use Number.MAX_SAFE_INTEGER for the int domain; the encoder
    // emits canonical msgpack int family (positive fixint, uint8,
    // uint16, uint32, uint64-safe). Bounds match what hbat exposes
    // on the wire today.
    fc.integer({ min: 0, max: Number.MAX_SAFE_INTEGER }),
    fc.string({ maxLength: 32 }),
    fc.array(tie("json"), { maxLength: 6 }),
    fc.dictionary(fc.string({ maxLength: 8 }), tie("json"), {
      maxKeys: 6,
    }),
  ),
})).json;

describe("canonical codec round-trip (property)", () => {
  it("encode then decode returns a deep-equal value (anything JSON-shaped)", () => {
    fc.assert(
      fc.property(jsonValue, (value) => {
        const bytes = encode(value);
        const decoded = decode(bytes);
        expect(decoded).toEqual(value);
      }),
      { numRuns: 200 },
    );
  });

  it("encoded bytes are deterministic across two runs", () => {
    fc.assert(
      fc.property(jsonValue, (value) => {
        const a = encode(value);
        const b = encode(value);
        expect(Array.from(a)).toEqual(Array.from(b));
      }),
      { numRuns: 200 },
    );
  });

  it("integers across the safe-int domain survive round-trip", () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 0, max: Number.MAX_SAFE_INTEGER }),
        (n) => {
          const bytes = encode(n);
          const decoded = decode(bytes);
          expect(decoded).toBe(n);
        },
      ),
      { numRuns: 500 },
    );
  });

  it("empty containers (string, array, object) survive round-trip", () => {
    for (const v of ["", [], {}]) {
      expect(decode(encode(v))).toEqual(v);
    }
  });

  it("object key insertion order is preserved through encode/decode", () => {
    // CRITICAL: Rust struct field order = declaration order = wire
    // order. The encoder MUST emit keys in JS-object insertion order
    // (which matches what fixture literals carry from the codegen).
    //
    // Caveat: ECMA-262 hoists "integer-indexed" string keys to the
    // front of any JS object, regardless of insertion order. (e.g.
    // `{"a": 1, "0": 2}` is enumerated `["0", "a"]`.) The codegen
    // never emits an integer-indexed key (all wire field names start
    // with a letter), so the audit-relevant property is "non-integer
    // keys preserve insertion order" — filter the arbitrary to match.
    const nonIntegerKey = fc
      .string({ minLength: 1, maxLength: 6 })
      .filter((key) => !/^(0|[1-9][0-9]*)$/u.test(key));
    fc.assert(
      fc.property(
        fc
          .array(nonIntegerKey, { minLength: 2, maxLength: 6 })
          .filter((keys) => new Set(keys).size === keys.length),
        (keys) => {
          const original: Record<string, number> = {};
          keys.forEach((key, idx) => {
            original[key] = idx;
          });
          const decoded = decode(encode(original)) as Record<string, number>;
          expect(Object.keys(decoded)).toEqual(keys);
        },
      ),
      { numRuns: 100 },
    );
  });
});
