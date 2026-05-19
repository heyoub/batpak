import { describe, expect, it } from "vitest";

import { encodeHex } from "@batpak/canonical";

import { bank, decodeBytes, encodeBytes, Schema } from "../src/index.js";

describe("decodeBytes / encodeBytes round-trip", () => {
  const Heartbeat = Schema.Struct({
    nonce: Schema.String,
  });

  it("encodes a typed value to canonical bytes that match the manual encoder", () => {
    const bytes = encodeBytes(Heartbeat, { nonce: "heartbeat-fixture-0001" });
    expect(encodeHex(bytes)).toBe(
      "81a56e6f6e6365b66865617274626561742d666978747572652d30303031",
    );
  });

  it("decodes canonical bytes back into a typed value", () => {
    const bytes = encodeBytes(Heartbeat, { nonce: "heartbeat-fixture-0001" });
    const value = decodeBytes(Heartbeat, bytes);
    expect(value).toEqual({ nonce: "heartbeat-fixture-0001" });
  });

  it("rejects bytes whose decoded shape does not match the schema", () => {
    const Ack = Schema.Struct({
      nonce: Schema.String,
      server_ts_ms: Schema.Number,
    });
    // Encode { nonce } against the Heartbeat schema (missing server_ts_ms).
    const bytes = encodeBytes(Heartbeat, { nonce: "x" });
    expect(() => decodeBytes(Ack, bytes)).toThrow();
  });

  it("rejects values that fail schema validation on encode", () => {
    const PositiveInt = Schema.Struct({
      n: Schema.Number.pipe(
        Schema.check(Schema.isInt(), Schema.isGreaterThanOrEqualTo(0)),
      ),
    });
    expect(() => encodeBytes(PositiveInt, { n: -1 })).toThrow();
  });
});

describe("bank.event() — authoring API", () => {
  const Move = bank.event({
    x: Schema.Number,
    y: Schema.Number,
    reason: Schema.String,
  });

  it("returns a real Effect Schema usable for decode/encode", () => {
    const value: typeof Move.Type = { x: 1, y: 2, reason: "fixture" };
    const bytes = encodeBytes(Move, value);
    const decoded = decodeBytes(Move, bytes);
    expect(decoded).toEqual(value);
  });

  it("validates field shape on decode", () => {
    const wrongShape = encodeBytes(
      Schema.Struct({ x: Schema.Number, y: Schema.String, reason: Schema.String }),
      { x: 1, y: "not-a-number", reason: "oops" },
    );
    expect(() => decodeBytes(Move, wrongShape)).toThrow();
  });
});
