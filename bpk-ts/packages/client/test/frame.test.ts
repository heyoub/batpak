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
  isKnownNetbatErrorCode,
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

  it("rejects inputs that hex-double past the line cap (frame guard)", () => {
    // REGRESSION: encodeRequest used to only check input.length vs
    // DEFAULT_MAX_INPUT_BYTES (32 KiB). But the encoded frame is
    // hex-encoded, doubling the input length, so a 32 KiB input
    // becomes a 64+ KiB frame — exceeding the 64 KiB line cap on
    // the server. The handshake would succeed locally but fail
    // server-side with line_too_long after a network round-trip.
    // Now we catch it at encode time with a precise diagnostic.
    const justOver = new Uint8Array(DEFAULT_MAX_INPUT_BYTES);
    expect(() => encodeRequest("a", justOver)).toThrow(/line_too_long|max line/);
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

  it("accepts ERR frames with unknown codes for forward compatibility", () => {
    // The Rust side promotes its error vocabulary forward (e.g.
    // netbat::transport::error.rs::Self::Runtime(_) catch-all emits
    // "runtime" when syncbat::RuntimeError gains a variant). A newer
    // server can legitimately emit a code this client doesn't know
    // yet. parseResponseFrame surfaces it as a typed NetbatError so
    // callers can handle it, rather than rejecting the frame as
    // malformed.
    const parsed = parseResponseFrame(utf8("ERR future_only_code 626f6f6d\n"));
    expect(parsed.kind).toBe("netbat-error");
    if (parsed.kind !== "netbat-error") return;
    expect(parsed.code).toBe("future_only_code");
    expect(parsed.message).toBe("boom");
    // The code is NOT in the known set, but the type still admits it
    // (NetbatErrorCode = KnownNetbatErrorCode | (string & {})).
    expect(isKnownNetbatErrorCode(parsed.code)).toBe(false);
  });

  it("rejects ERR frames whose code contains illegal characters", () => {
    // The forward-compat path still enforces a token-shape sanity
    // check (ASCII [A-Za-z0-9_]+). A spaceless garbage code with a
    // dot or dash slips, but spaces / non-ASCII garbage doesn't.
    expect(() => parseResponseFrame(utf8("ERR bad-code 626f6f6d\n"))).toThrow(
      /ill-formed code/,
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

describe("readLine preserves bytes after the newline", () => {
  // REGRESSION: readLine used to drop any bytes that arrived in the
  // same chunk AFTER the line-terminating `\n`. On persistent sockets
  // (max_requests_per_connection > 1) or pipelined peers, the second
  // frame's prefix was silently discarded and the next readLine would
  // hang. The fix routes leftover bytes back via Socket.unshift() so
  // they're re-emitted on the next read.

  type DataListener = (chunk: Uint8Array) => void;

  class MockSocket {
    private dataListeners: DataListener[] = [];
    private pending: Uint8Array[] = [];

    on(event: "data", listener: DataListener): this {
      if (event === "data") {
        this.dataListeners.push(listener);
        // Flush any unshifted bytes through the listener stack — but
        // re-fetch on every iteration so a listener that deregistered
        // itself (via off()) inside the call doesn't get called twice.
        while (this.pending.length > 0 && this.dataListeners.includes(listener)) {
          const chunk = this.pending.shift()!;
          this.dispatch(chunk);
        }
      }
      return this;
    }
    once(_event: "end" | "error", _listener: (e?: Error) => void): this {
      return this;
    }
    off(_event: string | symbol, listener: DataListener): this {
      this.dataListeners = this.dataListeners.filter((l) => l !== listener);
      return this;
    }
    unshift(chunk: Buffer | Uint8Array): void {
      const bytes = chunk instanceof Uint8Array ? chunk : new Uint8Array(chunk);
      this.pending.unshift(bytes);
    }
    emit(chunk: Uint8Array): void {
      this.dispatch(chunk);
    }
    private dispatch(chunk: Uint8Array): void {
      for (const listener of [...this.dataListeners]) {
        if (!this.dataListeners.includes(listener)) continue;
        listener(chunk);
      }
    }
  }

  it("returns leftover bytes via socket.unshift for the next read", async () => {
    const { readLine } = await import("../src/index.js");
    const socket = new MockSocket();
    const both = new TextEncoder().encode("OK abcd\nOK efgh\n");
    const first = readLine(socket);
    socket.emit(both);
    const line1 = await first;
    expect(new TextDecoder().decode(line1)).toBe("OK abcd\n");

    // The second frame's bytes must have been unshifted and re-emit
    // when the next reader subscribes. Without the fix, this readLine
    // would hang because the bytes were dropped.
    const line2 = await readLine(socket);
    expect(new TextDecoder().decode(line2)).toBe("OK efgh\n");
  });

  it("handles 3+ frames pipelined into one chunk via sequential reads", async () => {
    const { readLine } = await import("../src/index.js");
    const socket = new MockSocket();
    const triple = new TextEncoder().encode("OK 01\nOK 02\nOK 03\n");

    // Sequential read pattern: one readLine, then the next. The first
    // emit hands the whole chunk to readLine #1; the next two readLines
    // pull from the unshifted-back buffer when they subscribe.
    const first = readLine(socket);
    socket.emit(triple);
    expect(new TextDecoder().decode(await first)).toBe("OK 01\n");

    const second = await readLine(socket);
    expect(new TextDecoder().decode(second)).toBe("OK 02\n");

    const third = await readLine(socket);
    expect(new TextDecoder().decode(third)).toBe("OK 03\n");
  });

  it("doesn't unshift anything when the chunk ends exactly at the newline", async () => {
    const { readLine } = await import("../src/index.js");
    const socket = new MockSocket();
    socket.emit(new TextEncoder().encode("")); // no-op
    const first = readLine(socket);
    socket.emit(new TextEncoder().encode("OK ab\n"));
    const line = await first;
    expect(new TextDecoder().decode(line)).toBe("OK ab\n");
    // No pending bytes left for the next reader.
    expect(socket["pending" as keyof MockSocket]).toEqual([]);
  });
});
