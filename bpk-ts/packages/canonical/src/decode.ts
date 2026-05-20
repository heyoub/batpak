/**
 * Recursive canonical MessagePack decoder.
 *
 * Walks a byte buffer via {@link Reader} and reconstructs the
 * JSON-shaped value. Mirrors the encode side in `./encode.ts` for the
 * Phase 0 subset (see `./index.ts` for the subset contract).
 */

import { CanonicalDecodeError, Reader } from "./reader.js";

const POS_FIXINT_MAX = 0x7f;
const NEG_FIXINT_MIN = 0xe0;

const NIL = 0xc0;
const FALSE = 0xc2;
const TRUE = 0xc3;
const UINT8 = 0xcc;
const UINT16 = 0xcd;
const UINT32 = 0xce;
const UINT64 = 0xcf;
const INT8 = 0xd0;
const INT16 = 0xd1;
const INT32 = 0xd2;
const INT64 = 0xd3;
const STR8 = 0xd9;
const STR16 = 0xda;
const STR32 = 0xdb;
const ARRAY16 = 0xdc;
const ARRAY32 = 0xdd;
const MAP16 = 0xde;
const MAP32 = 0xdf;

/**
 * Decode canonical named-field MessagePack bytes back into a
 * JSON-shaped value.
 *
 * Inverse of {@link encode}. Object keys appear in the order the
 * encoder wrote them.
 *
 * @throws {CanonicalDecodeError} for unknown opcodes, integer values
 * outside `[0, Number.MAX_SAFE_INTEGER]`, truncated buffers, or
 * trailing bytes after a complete value.
 *
 * @example
 * ```ts
 * import { decode, decodeHex } from "@batpak/canonical";
 *
 * decode(decodeHex("81a56e6f6e6365a26869"));
 * // => { nonce: "hi" }
 * ```
 */
export function decode(bytes: Uint8Array): unknown {
  const reader = new Reader(bytes);
  const value = decodeValue(reader);
  if (!reader.atEnd()) {
    throw new CanonicalDecodeError(
      "trailing_bytes",
      `decoder finished at offset ${reader.offset} but buffer has ${bytes.length} bytes`,
    );
  }
  return value;
}

function decodeValue(reader: Reader): unknown {
  const head = reader.readByte();
  // Positive fixint (0x00..0x7f).
  if (head <= POS_FIXINT_MAX) {
    return head;
  }
  // fixmap (0x80..0x8f).
  if (head >= 0x80 && head <= 0x8f) {
    return decodeMap(reader, head & 0x0f);
  }
  // fixarray (0x90..0x9f).
  if (head >= 0x90 && head <= 0x9f) {
    return decodeArray(reader, head & 0x0f);
  }
  // fixstr (0xa0..0xbf).
  if (head >= 0xa0 && head <= 0xbf) {
    return decodeString(reader, head & 0x1f);
  }
  // Negative fixint (0xe0..0xff -> -32..-1).
  if (head >= NEG_FIXINT_MIN) {
    return head - 0x100;
  }
  switch (head) {
    case NIL:
      return null;
    case FALSE:
      return false;
    case TRUE:
      return true;
    case UINT8:
      return reader.readByte();
    case UINT16:
      return reader.readUInt16BE();
    case UINT32:
      return reader.readUInt32BE();
    case INT8: {
      // Sign-extend an unsigned byte to a signed JS number.
      const b = reader.readByte();
      return (b << 24) >> 24;
    }
    case INT16: {
      const v = reader.readUInt16BE();
      return (v << 16) >> 16;
    }
    case INT32: {
      // Bitwise `| 0` forces JS numeric coercion to signed i32.
      const v = reader.readUInt32BE();
      return v | 0;
    }
    case INT64: {
      // 8 bytes big-endian, two's-complement. Reconstruct as a JS
      // number while validating safe-int bounds — rmp-serde will
      // emit INT64 when an i64 value doesn't fit in any shorter
      // signed token, and we have to reject anything past
      // Number.MAX_SAFE_INTEGER on either side.
      const high = reader.readUInt32BE();
      const low = reader.readUInt32BE();
      // Combine into a JS number. high is treated as signed via the
      // top-bit check; low always contributes its unsigned value.
      const signedHigh = high & 0x80000000 ? high - 0x100000000 : high;
      const combined = signedHigh * 0x100000000 + low;
      if (!Number.isSafeInteger(combined)) {
        throw new CanonicalDecodeError(
          "integer_out_of_safe_range",
          `INT64 value ${combined} is outside Number.MAX_SAFE_INTEGER bounds`,
        );
      }
      return combined;
    }
    case UINT64:
      return reader.readUInt64BE();
    case STR8:
      return decodeString(reader, reader.readByte());
    case STR16:
      return decodeString(reader, reader.readUInt16BE());
    case STR32:
      return decodeString(reader, reader.readUInt32BE());
    case ARRAY16:
      return decodeArray(reader, reader.readUInt16BE());
    case ARRAY32:
      return decodeArray(reader, reader.readUInt32BE());
    case MAP16:
      return decodeMap(reader, reader.readUInt16BE());
    case MAP32:
      return decodeMap(reader, reader.readUInt32BE());
    default:
      throw new CanonicalDecodeError(
        "unsupported_token",
        `Phase 0 canonical decoder does not handle MessagePack token 0x${head.toString(16).padStart(2, "0")}`,
      );
  }
}

function decodeString(reader: Reader, length: number): string {
  const bytes = reader.readBytes(length);
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

function decodeArray(reader: Reader, length: number): unknown[] {
  const out: unknown[] = [];
  for (let i = 0; i < length; i += 1) {
    out.push(decodeValue(reader));
  }
  return out;
}

function decodeMap(reader: Reader, length: number): Record<string, unknown> {
  // Null-prototype accumulator. The decoder accepts MessagePack from an
  // untrusted peer; if a payload carries a key like "__proto__",
  // "constructor", or "prototype", writing it into a regular `{}`
  // via `out[key] = ...` would mutate the object's prototype chain
  // instead of creating a normal data property. Using
  // `Object.create(null)` makes those keys plain own-property names
  // with no prototype side-effect, closing a prototype-pollution
  // surface flagged by the Codex review on canonical/src/decode.ts.
  const out = Object.create(null) as Record<string, unknown>;
  for (let i = 0; i < length; i += 1) {
    const key = decodeValue(reader);
    if (typeof key !== "string") {
      throw new CanonicalDecodeError(
        "non_string_key",
        `Phase 0 canonical decoder requires string map keys (saw ${typeof key})`,
      );
    }
    out[key] = decodeValue(reader);
  }
  return out;
}
