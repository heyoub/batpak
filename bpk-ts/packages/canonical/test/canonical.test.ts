import { describe, expect, it } from "vitest";

import { decode, decodeHex, encode, encodeHex } from "../src/index.js";

// Golden bytes are produced by the Rust side via
// `cargo xtask export-ts-manifest` and embedded here as a literal copy
// so this package's tests do not require the manifest file to exist.
// The full parity test in @batpak/test reads the manifest directly.
const FIXTURE_NONCE = "heartbeat-fixture-0001";
const FIXTURE_SERVER_TS_MS = 1_700_000_000_000;

const GOLDEN_REQUEST_HEX =
  "81a56e6f6e6365b66865617274626561742d666978747572652d30303031";
const GOLDEN_ACK_HEX =
  "82a56e6f6e6365b66865617274626561742d666978747572652d30303031ac7365727665725f74735f6d73cf0000018bcfe56800";

describe("canonical encoder vs Rust golden bytes", () => {
  it("encodes SystemHeartbeatRequest exactly", () => {
    const bytes = encode({ nonce: FIXTURE_NONCE });
    expect(encodeHex(bytes)).toBe(GOLDEN_REQUEST_HEX);
  });

  it("encodes SystemHeartbeatAck exactly (insertion order = Rust field order)", () => {
    const bytes = encode({
      nonce: FIXTURE_NONCE,
      server_ts_ms: FIXTURE_SERVER_TS_MS,
    });
    expect(encodeHex(bytes)).toBe(GOLDEN_ACK_HEX);
  });
});

describe("canonical decoder vs Rust golden bytes", () => {
  it("decodes SystemHeartbeatRequest into the fixture object", () => {
    const value = decode(decodeHex(GOLDEN_REQUEST_HEX));
    expect(value).toEqual({ nonce: FIXTURE_NONCE });
  });

  it("decodes SystemHeartbeatAck into the fixture object", () => {
    const value = decode(decodeHex(GOLDEN_ACK_HEX));
    expect(value).toEqual({
      nonce: FIXTURE_NONCE,
      server_ts_ms: FIXTURE_SERVER_TS_MS,
    });
  });

  it("preserves Rust insertion order when round-tripping", () => {
    const value = decode(decodeHex(GOLDEN_ACK_HEX)) as Record<string, unknown>;
    expect(Object.keys(value)).toEqual(["nonce", "server_ts_ms"]);
  });
});

describe("encoder rejects out-of-subset values", () => {
  it("rejects floats", () => {
    expect(() => encode({ x: 1.5 })).toThrow(/non-integer/);
  });

  it("rejects negative integers", () => {
    expect(() => encode({ x: -1 })).toThrow(/negative integer/);
  });

  it("rejects above-safe integers", () => {
    expect(() => encode({ x: Number.MAX_SAFE_INTEGER + 1 })).toThrow(
      /above Number\.MAX_SAFE_INTEGER/,
    );
  });

  it("rejects undefined", () => {
    expect(() => encode({ x: undefined } as unknown)).toThrow(
      /rejects value of type undefined/,
    );
  });
});

describe("integer width rules match rmp-serde shortest-encoding", () => {
  const cases: Array<[number, string]> = [
    [0, "00"],
    [127, "7f"],
    [128, "cc80"],
    [255, "ccff"],
    [256, "cd0100"],
    [65535, "cdffff"],
    [65536, "ce00010000"],
    [4294967295, "ceffffffff"],
    [4294967296, "cf0000000100000000"],
    [Number.MAX_SAFE_INTEGER, "cf001fffffffffffff"],
  ];

  for (const [value, hex] of cases) {
    it(`encodes ${value} as 0x${hex}`, () => {
      expect(encodeHex(encode(value))).toBe(hex);
    });
  }
});

describe("hex helpers", () => {
  it("encodeHex matches the netbat lowercase convention", () => {
    expect(encodeHex(new Uint8Array([0x4e, 0x45, 0x54, 0x42, 0x41, 0x54]))).toBe(
      "4e455442415"+"4",
    );
  });

  it("decodeHex accepts uppercase too", () => {
    expect(Array.from(decodeHex("aB0F"))).toEqual([0xab, 0x0f]);
  });

  it("decodeHex rejects odd length", () => {
    expect(() => decodeHex("abc")).toThrow(/not byte-aligned/);
  });
});
