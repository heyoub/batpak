/**
 * Direct unit tests for @batpak/client.
 *
 * Exercises frame parse/emit edge cases plus full coverage of the
 * typed NetbatError union and the operation-name grammar checks.
 * Complements the manifest-driven parity tests in @batpak/test.
 */

import { describe, expect, it } from "vitest";

import { encodeHex } from "@batpak/canonical";

import {
  encodeRequest,
  FrameValidationError,
  parseRequestFrame,
  parseResponseFrame,
  validateOperationName,
  NETBAT_ERROR_CODES,
  DEFAULT_MAX_INPUT_BYTES,
  MAX_OPERATION_NAME_BYTES,
  type NetbatErrorCode,
} from "../src/index.js";

const utf8 = (s: string) => new TextEncoder().encode(s);
const hex = encodeHex;

describe("encodeRequest", () => {
  it("emits the literal NETBAT/1 CALL prefix + lowercase hex + \\n", () => {
    const out = encodeRequest("system.heartbeat", new Uint8Array([0xde, 0xad]));
    expect(new TextDecoder().decode(out)).toBe("NETBAT/1 CALL system.heartbeat dead\n");
  });

  it("encodes empty input as the empty hex segment", () => {
    const out = encodeRequest("a", new Uint8Array());
    expect(new TextDecoder().decode(out)).toBe("NETBAT/1 CALL a \n");
  });

  it("refuses oversized inputs", () => {
    const big = new Uint8Array(DEFAULT_MAX_INPUT_BYTES + 1);
    expect(() => encodeRequest("x", big)).toThrow(FrameValidationError);
  });
});

describe("validateOperationName", () => {
  it("accepts the canonical names from the manifest", () => {
    for (const name of ["system.heartbeat", "bank.commit", "event.get"]) {
      expect(() => validateOperationName(name)).not.toThrow();
    }
  });

  it("rejects empty names", () => {
    expect(() => validateOperationName("")).toThrow(/empty/);
  });

  it("rejects names with illegal characters", () => {
    for (const bad of ["a b", "a/b", "a:b", "a@b", "a$b"]) {
      expect(() => validateOperationName(bad)).toThrow(/illegal characters/);
    }
  });

  it("rejects names that start or end with a dot", () => {
    expect(() => validateOperationName(".x")).toThrow(/start or end with/);
    expect(() => validateOperationName("x.")).toThrow(/start or end with/);
  });

  it("rejects names containing '..'", () => {
    expect(() => validateOperationName("a..b")).toThrow(/cannot contain/);
  });

  it("rejects names exceeding 128 bytes", () => {
    const long = "a".repeat(MAX_OPERATION_NAME_BYTES + 1);
    expect(() => validateOperationName(long)).toThrow(/exceeds/);
  });

  it("accepts names exactly at the 128-byte limit", () => {
    const exact = "a".repeat(MAX_OPERATION_NAME_BYTES);
    expect(() => validateOperationName(exact)).not.toThrow();
  });
});

describe("parseRequestFrame", () => {
  it("strips the trailing \\n", () => {
    const frame = utf8("NETBAT/1 CALL ping cafe\n");
    const parsed = parseRequestFrame(frame);
    expect(parsed.operation).toBe("ping");
    expect(hex(parsed.input)).toBe("cafe");
  });

  it("tolerates a frame without a trailing newline", () => {
    const frame = utf8("NETBAT/1 CALL ping cafe");
    const parsed = parseRequestFrame(frame);
    expect(parsed.operation).toBe("ping");
  });

  it("tolerates uppercase hex on parse", () => {
    const frame = utf8("NETBAT/1 CALL ping CAFE\n");
    const parsed = parseRequestFrame(frame);
    expect(hex(parsed.input)).toBe("cafe");
  });

  it("rejects a missing CALL verb", () => {
    expect(() => parseRequestFrame(utf8("NETBAT/1 PING ping cafe\n"))).toThrow(/must start with/);
  });

  it("rejects a missing protocol prefix", () => {
    expect(() => parseRequestFrame(utf8("HTTP/1.1 CALL ping cafe\n"))).toThrow(/must start with/);
  });

  it("rejects a frame without a hex segment", () => {
    expect(() => parseRequestFrame(utf8("NETBAT/1 CALL ping\n"))).toThrow(/missing space/);
  });
});

describe("parseResponseFrame", () => {
  it("parses OK frames into NetbatOk", () => {
    const parsed = parseResponseFrame(utf8("OK babe\n"));
    expect(parsed.kind).toBe("netbat-ok");
    if (parsed.kind !== "netbat-ok") return;
    expect(hex(parsed.output)).toBe("babe");
  });

  it("accepts an empty OK output", () => {
    const parsed = parseResponseFrame(utf8("OK \n"));
    expect(parsed.kind).toBe("netbat-ok");
  });

  it("parses ERR frames into NetbatError with typed code + UTF-8 message", () => {
    // ERR unknown_operation <hex of "boom">
    const parsed = parseResponseFrame(utf8(`ERR unknown_operation 626f6f6d\n`));
    expect(parsed.kind).toBe("netbat-error");
    if (parsed.kind !== "netbat-error") return;
    expect(parsed.code).toBe("unknown_operation");
    expect(parsed.message).toBe("boom");
  });

  it("decodes the message as UTF-8 — NOT MessagePack", () => {
    // Even if the hex happens to be a valid MessagePack frame, we still
    // decode it as UTF-8 text. 81 a0 = fixmap{1, key=""} in MessagePack
    // but as bytes [0x81, 0xa0] in UTF-8 it's invalid; ensure we don't
    // try to MessagePack-decode.
    const message = "literal text with `backticks`";
    const hexMessage = encodeHex(new TextEncoder().encode(message));
    const parsed = parseResponseFrame(utf8(`ERR handler ${hexMessage}\n`));
    if (parsed.kind !== "netbat-error") throw new Error("expected error");
    expect(parsed.message).toBe(message);
  });

  it("rejects ERR frames carrying an unknown code", () => {
    expect(() => parseResponseFrame(utf8("ERR not_a_real_code 626f6f6d\n"))).toThrow(
      /unknown code/,
    );
  });

  it("rejects responses that are neither OK nor ERR", () => {
    expect(() => parseResponseFrame(utf8("MAYBE deadbeef\n"))).toThrow(/must start with/);
  });

  it("covers every NetbatError code declared in the union", () => {
    // Sanity: every code we declared parses correctly.
    for (const code of NETBAT_ERROR_CODES) {
      const parsed = parseResponseFrame(utf8(`ERR ${code} ${encodeHex(utf8("ok"))}\n`));
      if (parsed.kind !== "netbat-error") throw new Error("expected error");
      const c: NetbatErrorCode = parsed.code;
      expect(c).toBe(code);
    }
  });
});

describe("roundtrip: encodeRequest -> parseRequestFrame", () => {
  it("preserves operation and input bytes", () => {
    const input = new Uint8Array([0x01, 0x02, 0xff, 0x00, 0xa5]);
    const frame = encodeRequest("a.b.c-d_e", input);
    const parsed = parseRequestFrame(frame);
    expect(parsed.operation).toBe("a.b.c-d_e");
    expect(Array.from(parsed.input)).toEqual(Array.from(input));
  });
});
