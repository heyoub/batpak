/**
 * NETBAT/1 frame client.
 *
 * Phase 0: TCP only. The line protocol is the bytes documented in
 * `bpk-lib/crates/netbat/src/transport.rs:404-454`:
 *
 *     NETBAT/1 CALL <operation-name> <hex-input>\n
 *     OK <hex-output>\n
 *     ERR <code> <hex-message>\n
 *
 * - Hex is lowercase on encode; both cases accepted on decode.
 * - ERR `<code>` is a stable ASCII token from
 *   `NetbatError::code()`. The message half is hex of plain UTF-8 text
 *   (NOT MessagePack — do not pass it through `@batpak/canonical`'s
 *   `decode`).
 * - Operation-name grammar: ASCII graphic
 *   `[A-Za-z0-9._-]`, cannot start/end with `.`, cannot contain `..`,
 *   length <= 128 bytes.
 *
 * Byte bounds match the netbat defaults:
 *   line  <= 64 KiB
 *   input <= 32 KiB
 *   output<= 32 KiB
 */

import { decodeHex, encodeHex } from "@batpak/canonical";

export const NETBAT_VERSION = "NETBAT/1";
export const CALL_VERB = "CALL";

export const DEFAULT_MAX_LINE_BYTES = 64 * 1024;
export const DEFAULT_MAX_INPUT_BYTES = 32 * 1024;
export const DEFAULT_MAX_OUTPUT_BYTES = 32 * 1024;
export const MAX_OPERATION_NAME_BYTES = 128;

/**
 * Branded TS counterpart of Rust's `syncbat::OperationName` newtype: a
 * string that has been validated against the netbat operation-name
 * grammar. Construct only via {@link validateOperationName}; downstream
 * code should accept this type instead of re-parsing the grammar.
 *
 * The brand is structural — every {@link OperationName} is assignable to
 * a plain `string`, but a plain `string` is not assignable to
 * {@link OperationName} without going through the validator.
 */
export type OperationName = string & { readonly __brand: "OperationName" };

const OPERATION_NAME_PATTERN = /^[A-Za-z0-9._-]+$/u;

/**
 * Known NETBAT/1 error codes emitted by `netbat::NetbatError::code()`.
 *
 * The Rust side promotes the union forward (e.g. the `Runtime(_)`
 * catch-all over a `#[non_exhaustive]` syncbat::RuntimeError emits the
 * generic `"runtime"` code when a newer server gains a variant that
 * the wire vocabulary hasn't yet named). For forward-compat,
 * `parseResponseFrame` accepts ANY code string the server sends and
 * exposes it as the typed `NetbatErrorCode` union OR as a string via
 * the `KnownNetbatErrorCode | (string & {})` pattern below.
 */
export const NETBAT_ERROR_CODES = [
  "io",
  "empty_stream",
  "line_too_long",
  "malformed_request",
  "unsupported_protocol_version",
  "operation_name_too_long",
  "input_too_large",
  "output_too_large",
  "unknown_operation",
  "missing_handler",
  "handler",
  "receipt_sink",
  // Generic forward-compat token emitted by the netbat
  // Self::Runtime(_) catch-all when syncbat::RuntimeError gains a
  // variant that this vocabulary doesn't yet name. Keep in sync with
  // bpk-lib/crates/netbat/src/transport/error.rs::NetbatError::code.
  "runtime",
] as const;

/** Known NETBAT/1 error codes — exhaustive at the current wire version. */
export type KnownNetbatErrorCode = (typeof NETBAT_ERROR_CODES)[number];

/**
 * NETBAT/1 error code as it appears on the wire. Known values are
 * surfaced via the {@link KnownNetbatErrorCode} union for autocomplete
 * and exhaustive-match; the `(string & {})` carve-out keeps the type
 * forward-compatible — a newer server can emit a code we don't know
 * yet, and we surface it as a typed `NetbatError` rather than
 * rejecting it as a `FrameValidationError`.
 */
export type NetbatErrorCode = KnownNetbatErrorCode | (string & {});

export interface NetbatError {
  readonly kind: "netbat-error";
  readonly code: NetbatErrorCode;
  /** UTF-8 text decoded from the `<hex-message>` half of the ERR frame. */
  readonly message: string;
}

export interface NetbatOk {
  readonly kind: "netbat-ok";
  readonly output: Uint8Array;
}

export type NetbatResponse = NetbatOk | NetbatError;

export interface RequestFrame {
  /**
   * Validated operation name. `OperationName` is structurally a string, so
   * the field is read-compatible with any existing consumer that expects a
   * plain `string`. New code should keep names branded by funnelling them
   * through {@link validateOperationName}.
   */
  readonly operation: OperationName;
  readonly input: Uint8Array;
}

export class FrameValidationError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "FrameValidationError";
    this.code = code;
  }
}

/**
 * Validate an operation name against the netbat grammar and brand it as an
 * {@link OperationName}. Throws on empty, too-long, illegal characters,
 * leading/trailing `.`, or `..` substrings.
 *
 * This is the TS counterpart of the substrate-wide
 * `syncbat::OperationName::new` validating constructor. Every layer
 * (encode, parse, dispatch) should funnel through this function so the
 * grammar lives in exactly one place.
 *
 * @throws {FrameValidationError} with code `malformed_request` (empty,
 * illegal chars, leading/trailing dot, `..`) or `operation_name_too_long`
 * (>128 UTF-8 bytes).
 *
 * @example
 * ```ts
 * import { validateOperationName } from "@batpak/client";
 *
 * const name = validateOperationName("system.heartbeat");
 * // name now has the OperationName brand and can be passed into
 * // encodeRequest / call without re-validation.
 *
 * validateOperationName("bad..name"); // throws FrameValidationError
 * ```
 */
export function validateOperationName(operation: string): OperationName {
  if (operation.length === 0) {
    throw new FrameValidationError("malformed_request", "operation name is empty");
  }
  const utf8Length = new TextEncoder().encode(operation).length;
  if (utf8Length > MAX_OPERATION_NAME_BYTES) {
    throw new FrameValidationError(
      "operation_name_too_long",
      `operation name ${utf8Length} bytes exceeds ${MAX_OPERATION_NAME_BYTES}`,
    );
  }
  if (!OPERATION_NAME_PATTERN.test(operation)) {
    throw new FrameValidationError(
      "malformed_request",
      `operation name ${JSON.stringify(operation)} contains illegal characters (allowed: [A-Za-z0-9._-])`,
    );
  }
  if (operation.startsWith(".") || operation.endsWith(".")) {
    throw new FrameValidationError(
      "malformed_request",
      `operation name ${JSON.stringify(operation)} cannot start or end with '.'`,
    );
  }
  if (operation.includes("..")) {
    throw new FrameValidationError(
      "malformed_request",
      `operation name ${JSON.stringify(operation)} cannot contain '..'`,
    );
  }
  return operation as OperationName;
}

/**
 * Encode a CALL request frame, including the trailing `\n`.
 *
 * Accepts either a plain `string` (which is validated and brand-promoted
 * internally) or an already-branded {@link OperationName}. Either way the
 * frame is only emitted when the name passes the netbat grammar.
 *
 * @throws {FrameValidationError} when the operation name is empty, too
 * long, contains illegal characters, or the input exceeds
 * `DEFAULT_MAX_INPUT_BYTES`.
 *
 * @example
 * ```ts
 * import { encodeRequest } from "@batpak/client";
 *
 * const frame = encodeRequest("system.heartbeat", new Uint8Array([0xde, 0xad]));
 * new TextDecoder().decode(frame);
 * // => "NETBAT/1 CALL system.heartbeat dead\n"
 * ```
 */
export function encodeRequest(operation: string | OperationName, input: Uint8Array): Uint8Array {
  const validated = validateOperationName(operation);
  if (input.length > DEFAULT_MAX_INPUT_BYTES) {
    throw new FrameValidationError(
      "input_too_large",
      `input ${input.length} bytes exceeds ${DEFAULT_MAX_INPUT_BYTES}`,
    );
  }
  const prefix = `${NETBAT_VERSION} ${CALL_VERB} ${validated} `;
  const prefixBytes = new TextEncoder().encode(prefix);
  // Boundary check: hex doubles input, so a 32 KiB input that
  // passes input_too_large above would produce a frame larger
  // than the 64 KiB line cap the server enforces. Catch it here
  // with a precise diagnostic instead of letting the server
  // reject it as `line_too_long` after the network round-trip.
  const frameLength = prefixBytes.length + input.length * 2 + 1;
  if (frameLength > DEFAULT_MAX_LINE_BYTES) {
    throw new FrameValidationError(
      "line_too_long",
      `encoded frame would be ${frameLength} bytes (input ${input.length} hex-doubles to ${input.length * 2}); ` +
        `max line is ${DEFAULT_MAX_LINE_BYTES}. Use a shorter input or raise the server's max_line_bytes limit.`,
    );
  }
  const hex = encodeHex(input);
  const hexBytes = new TextEncoder().encode(hex);
  const out = new Uint8Array(prefixBytes.length + hexBytes.length + 1);
  out.set(prefixBytes, 0);
  out.set(hexBytes, prefixBytes.length);
  out[out.length - 1] = 0x0a;
  return out;
}

/**
 * Parse a CALL request frame (including or excluding the trailing newline).
 *
 * The returned `operation` is a branded {@link OperationName} — it has
 * already passed the grammar check, so downstream code never re-validates.
 *
 * @example
 * ```ts
 * import { parseRequestFrame } from "@batpak/client";
 *
 * const frame = new TextEncoder().encode("NETBAT/1 CALL system.heartbeat dead\n");
 * const parsed = parseRequestFrame(frame);
 * parsed.operation; // "system.heartbeat" (branded OperationName)
 * Array.from(parsed.input); // [0xde, 0xad]
 * ```
 */
export function parseRequestFrame(line: Uint8Array): RequestFrame {
  const text = trimNewline(new TextDecoder("utf-8", { fatal: true }).decode(line));
  const prefix = `${NETBAT_VERSION} ${CALL_VERB} `;
  if (!text.startsWith(prefix)) {
    throw new FrameValidationError(
      "malformed_request",
      `request frame must start with ${JSON.stringify(prefix)}`,
    );
  }
  const remainder = text.slice(prefix.length);
  const spaceIdx = remainder.indexOf(" ");
  if (spaceIdx < 0) {
    throw new FrameValidationError(
      "malformed_request",
      "request frame missing space between operation and hex payload",
    );
  }
  const operation = validateOperationName(remainder.slice(0, spaceIdx));
  const hex = remainder.slice(spaceIdx + 1);
  const input = decodeHex(hex);
  return { operation, input };
}

/**
 * Parse an OK or ERR response frame (including or excluding the trailing newline).
 *
 * Returns a tagged-union {@link NetbatResponse}: `kind: "netbat-ok"`
 * carries the output bytes; `kind: "netbat-error"` carries a typed
 * {@link NetbatErrorCode} and a UTF-8 message (NOT MessagePack — do
 * not pass it through `@batpak/canonical`'s `decode`).
 *
 * @example
 * ```ts
 * import { parseResponseFrame } from "@batpak/client";
 *
 * const ok = parseResponseFrame(new TextEncoder().encode("OK babe\n"));
 * if (ok.kind === "netbat-ok") {
 *   console.log("output:", ok.output);
 * }
 *
 * const err = parseResponseFrame(
 *   new TextEncoder().encode("ERR unknown_operation 626f6f6d\n"),
 * );
 * if (err.kind === "netbat-error") {
 *   console.error(`${err.code}: ${err.message}`); // "unknown_operation: boom"
 * }
 * ```
 */
export function parseResponseFrame(line: Uint8Array): NetbatResponse {
  const text = trimNewline(new TextDecoder("utf-8", { fatal: true }).decode(line));
  if (text.startsWith("OK ")) {
    const hex = text.slice(3);
    return { kind: "netbat-ok", output: decodeHex(hex) };
  }
  if (text.startsWith("ERR ")) {
    const remainder = text.slice(4);
    const spaceIdx = remainder.indexOf(" ");
    if (spaceIdx < 0) {
      throw new FrameValidationError(
        "malformed_request",
        "ERR frame missing space between code and hex message",
      );
    }
    const codeRaw = remainder.slice(0, spaceIdx);
    const hex = remainder.slice(spaceIdx + 1);
    // Forward-compat: don't reject unknown codes. A newer server (or
    // a netbat Self::Runtime(_) catch-all firing on a syncbat
    // RuntimeError variant this client doesn't know yet) will emit
    // codes outside KnownNetbatErrorCode. Surface them as a typed
    // NetbatError so callers handle the failure rather than seeing a
    // "malformed_request" FrameValidationError. A bare token-format
    // sanity check (non-empty + ASCII graphic, matching the Rust
    // `code()` shape) keeps total garbage out.
    if (codeRaw.length === 0 || /[^A-Za-z0-9_]/u.test(codeRaw)) {
      throw new FrameValidationError(
        "malformed_request",
        `ERR frame carries ill-formed code ${JSON.stringify(codeRaw)} (expected ASCII [A-Za-z0-9_]+)`,
      );
    }
    const messageBytes = decodeHex(hex);
    // The Rust side emits `error.to_string().as_bytes()` — plain UTF-8,
    // never MessagePack. Decode as UTF-8 only.
    const message = new TextDecoder("utf-8", { fatal: true }).decode(messageBytes);
    return { kind: "netbat-error", code: codeRaw, message };
  }
  throw new FrameValidationError(
    "malformed_request",
    `response frame must start with "OK " or "ERR " (got ${JSON.stringify(text.slice(0, 8))})`,
  );
}

/**
 * Type-guard that narrows an arbitrary string to one of the known
 * NETBAT/1 codes. Useful for exhaustive switch dispatch: callers can
 * branch on known cases and fall through unknown ones into a generic
 * handler.
 */
export function isKnownNetbatErrorCode(value: string): value is KnownNetbatErrorCode {
  for (const code of NETBAT_ERROR_CODES) {
    if (code === value) return true;
  }
  return false;
}

/**
 * Forward-compatible payload-version check.
 *
 * Every generated event carries a `<TsName>_PAYLOAD_VERSION` constant
 * (from `@batpak/generated`) recording the wire schema version this SDK
 * was built against. Events ride the wire with a `payload_version`
 * stamped into their `EventHeader` by the server's typed-append seam.
 *
 * Semantics (mirrors the tolerant `NetbatErrorCode` `(string & {})`
 * carve-out — never hard-reject the unknown-but-newer direction):
 *
 *   - `stored === generated` — exact match; decode as-is.
 *   - `stored === 0`         — legacy/untyped sentinel: the producer
 *                              pre-dates versioning (or used the untyped
 *                              `append`). Tolerated; decoded with the
 *                              current shape (serde fills additive
 *                              defaults). Returns `true`.
 *   - `stored < generated`   — older shape; the SERVER upcasts on read
 *                              before it ever reaches the wire, so a TS
 *                              client only ever sees the current shape.
 *                              Tolerated. Returns `true`.
 *   - `stored > generated`   — NEWER shape than this SDK knows. The
 *                              server has already upcast to its current
 *                              shape on read; for purely additive
 *                              evolution Effect's `Schema.Struct` ignores
 *                              the unknown keys, so the known fields still
 *                              decode. We therefore TOLERATE it (returns
 *                              `true`) rather than rejecting, matching the
 *                              "read the current shape" forward-compat
 *                              contract. Callers that want to surface a
 *                              "decoded against an older SDK" signal can
 *                              compare the two numbers directly.
 *
 * In short: this function never returns `false` for a real wire value —
 * it exists to DOCUMENT and CENTRALIZE the tolerant policy and to give
 * callers a single seam to hook telemetry/warnings on the
 * `stored > generated` case. A `stored` value that is not a finite
 * non-negative integer is the only rejected input.
 */
export function isCompatiblePayloadVersion(stored: number, generated: number): boolean {
  if (!Number.isInteger(stored) || stored < 0) return false;
  if (!Number.isInteger(generated) || generated < 1) return false;
  // 0 = legacy/untyped sentinel; any declared version (incl. newer than
  // this SDK) is forward-compat decodable because the server upcasts on
  // read and additive fields are ignored by the generated schema.
  return true;
}

/**
 * Classify a stored `payload_version` against the version this SDK was
 * generated for, without rejecting. Returns a discriminant a caller can
 * branch on for logging while still proceeding to decode.
 */
export function classifyPayloadVersion(
  stored: number,
  generated: number,
): "exact" | "legacy" | "older" | "newer" {
  if (stored === 0) return "legacy";
  if (stored === generated) return "exact";
  return stored < generated ? "older" : "newer";
}

function trimNewline(text: string): string {
  if (text.endsWith("\r\n")) return text.slice(0, -2);
  if (text.endsWith("\n")) return text.slice(0, -1);
  return text;
}

/**
 * Read a single line from a Node `net.Socket`-like readable. The line
 * includes the trailing `\n` byte. Refuses lines longer than
 * `DEFAULT_MAX_LINE_BYTES`.
 */
export async function readLine(
  socket: NodeReadable,
  maxBytes: number = DEFAULT_MAX_LINE_BYTES,
): Promise<Uint8Array> {
  const buffered: number[] = [];
  return await new Promise<Uint8Array>((resolve, reject) => {
    const onData = (chunk: Buffer | Uint8Array) => {
      const bytes = chunk instanceof Uint8Array ? chunk : new Uint8Array(chunk);
      for (let i = 0; i < bytes.length; i += 1) {
        const byte = bytes[i];
        if (byte === undefined) continue;
        buffered.push(byte);
        if (buffered.length > maxBytes) {
          cleanup();
          reject(new FrameValidationError("line_too_long", `line exceeded ${maxBytes} bytes`));
          return;
        }
        if (byte === 0x0a) {
          cleanup();
          // CRITICAL: bytes after the newline in this same chunk
          // belong to the NEXT frame on a persistent socket
          // (max_requests_per_connection > 1) or any pipelined
          // peer. Push them back via Socket.unshift() so the next
          // readLine() call sees them. Without this, the second
          // frame's prefix is silently dropped and the next read
          // hangs waiting for bytes that already arrived.
          if (i + 1 < bytes.length) {
            const remaining = bytes.subarray(i + 1);
            if (typeof socket.unshift === "function") {
              // Node's Readable.unshift accepts Buffer or Uint8Array
              // depending on the stream's encoding; passing the
              // Uint8Array view works on every standard transport
              // (net.Socket, tls.Socket, stream.PassThrough).
              socket.unshift(remaining);
            }
          }
          resolve(new Uint8Array(buffered));
          return;
        }
      }
    };
    const onEnd = () => {
      cleanup();
      if (buffered.length === 0) {
        reject(new FrameValidationError("empty_stream", "stream closed before any bytes"));
      } else {
        // Tolerate trailing line missing newline.
        resolve(new Uint8Array(buffered));
      }
    };
    const onError = (error: Error) => {
      cleanup();
      reject(error);
    };
    const cleanup = () => {
      socket.off("data", onData);
      socket.off("end", onEnd);
      socket.off("error", onError);
    };
    socket.on("data", onData);
    socket.once("end", onEnd);
    socket.once("error", onError);
  });
}

/** Minimal duck-typed Node readable used by {@link readLine}. */
export interface NodeReadable {
  on(event: "data", listener: (chunk: Buffer | Uint8Array) => void): unknown;
  once(event: "end", listener: () => void): unknown;
  once(event: "error", listener: (error: Error) => void): unknown;
  // `off` matches Node's EventEmitter.off shape. We never call it with
  // anything other than listeners we previously registered via on/once
  // above — using `unknown[]` instead of `any[]` keeps the typed-lint
  // bundle happy while still accepting a Socket structurally.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- rationale: matches Node EventEmitter.off ABI; we never invoke it
  off(eventName: string | symbol, listener: (...args: any[]) => void): unknown;
  /**
   * Push bytes back to the stream's buffer so the next `data` consumer
   * sees them. Used by {@link readLine} to preserve any bytes that
   * arrived AFTER the line-terminating `\n` in the same chunk — those
   * belong to the next frame on a persistent / pipelined socket.
   * Optional because not every minimal mock implements it.
   */
  unshift?(chunk: Buffer | Uint8Array): void;
}

/**
 * Issue a single CALL/response roundtrip over a Node `net.Socket`.
 *
 * The socket is consumed for this call (one request, one response).
 *
 * @example
 * ```ts
 * import { createConnection } from "node:net";
 * import { call } from "@batpak/client";
 * import { encode } from "@batpak/canonical";
 *
 * const socket = createConnection({ host: "127.0.0.1", port: 54321 });
 * const response = await call(socket, "system.heartbeat", encode({ nonce: "x" }));
 * if (response.kind === "netbat-ok") {
 *   console.log("output bytes:", response.output.length);
 * } else {
 *   console.error("netbat error:", response.code, response.message);
 * }
 * ```
 */
export async function call(
  socket: NodeSocketLike,
  operation: string | OperationName,
  input: Uint8Array,
): Promise<NetbatResponse> {
  const frame = encodeRequest(operation, input);
  await new Promise<void>((resolve, reject) => {
    socket.write(frame, (error) => (error ? reject(error) : resolve()));
  });
  const line = await readLine(socket);
  return parseResponseFrame(line);
}

/** Minimal Node `net.Socket`-shaped writer/reader used by {@link call}. */
export interface NodeSocketLike extends NodeReadable {
  write(data: Uint8Array, callback?: (error: Error | null | undefined) => void): boolean;
}
