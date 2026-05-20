/**
 * Recursive canonical MessagePack encoder.
 *
 * Walks a JSON-shaped value and emits the shortest valid MessagePack
 * encoding, matching rmp-serde's `to_vec_named` byte-for-byte for the
 * Phase 0 subset (see `./index.ts` for the subset contract).
 */

import { CanonicalEncodeError, Writer } from "./writer.js";

export interface EncodeOptions {
  /**
   * Maximum bytes allowed in the encoded output. Defaults to 32 KiB,
   * matching netbat's `DEFAULT_MAX_INPUT_BYTES`.
   */
  readonly maxBytes?: number;
}

export const DEFAULT_MAX_BYTES = 32 * 1024;

const POS_FIXINT_MAX = 0x7f;
const FIXMAP_MAX = 0x0f;
const FIXSTR_MAX = 0x1f;
const FIXARRAY_MAX = 0x0f;
const NIL = 0xc0;
const FALSE = 0xc2;
const TRUE = 0xc3;
const UINT8 = 0xcc;
const UINT16 = 0xcd;
const UINT32 = 0xce;
const UINT64 = 0xcf;
const STR8 = 0xd9;
const STR16 = 0xda;
const STR32 = 0xdb;
const ARRAY16 = 0xdc;
const ARRAY32 = 0xdd;
const MAP16 = 0xde;
const MAP32 = 0xdf;

/**
 * Encode a JSON-shaped value into canonical named-field MessagePack
 * bytes.
 *
 * Supported value shapes: `null`, `boolean`, finite `number` (integers
 * in `[0, Number.MAX_SAFE_INTEGER]` are written using the shortest
 * uint family; doubles use float64), `string`, `Array<unknown>`,
 * `Record<string, unknown>` (object keys are emitted in JS insertion
 * order — see the property test for the EC-262 caveat on
 * integer-indexed keys).
 *
 * @throws {CanonicalEncodeError} `unsupported_value_type` for
 * non-JSON-shaped inputs (functions, Symbols, BigInt, etc.) or
 * `output_too_large` when the encoded length exceeds
 * `options.maxBytes` (default 32 KiB).
 *
 * @example
 * ```ts
 * import { encode, encodeHex } from "@batpak/canonical";
 *
 * const bytes = encode({ nonce: "hi" });
 * encodeHex(bytes);
 * // => "81a56e6f6e6365a26869"  (fixmap{1}, fixstr"nonce", fixstr"hi")
 * ```
 */
export function encode(value: unknown, options: EncodeOptions = {}): Uint8Array {
  const max = options.maxBytes ?? DEFAULT_MAX_BYTES;
  const writer = new Writer();
  encodeValue(writer, value);
  if (writer.length > max) {
    throw new CanonicalEncodeError(
      "output_too_large",
      `encoded length ${writer.length} exceeds max ${max}`,
    );
  }
  return writer.toUint8Array();
}

function encodeValue(writer: Writer, value: unknown): void {
  if (value === null) {
    writer.pushByte(NIL);
    return;
  }
  if (value === true) {
    writer.pushByte(TRUE);
    return;
  }
  if (value === false) {
    writer.pushByte(FALSE);
    return;
  }
  if (typeof value === "string") {
    encodeString(writer, value);
    return;
  }
  if (typeof value === "number") {
    encodeNumber(writer, value);
    return;
  }
  if (Array.isArray(value)) {
    encodeArray(writer, value);
    return;
  }
  if (typeof value === "object") {
    encodeObject(writer, value as Record<string, unknown>);
    return;
  }
  throw new CanonicalEncodeError(
    "unsupported_value",
    `Phase 0 canonical encoder rejects value of type ${typeof value}`,
  );
}

function encodeString(writer: Writer, value: string): void {
  const utf8 = new TextEncoder().encode(value);
  const len = utf8.length;
  if (len <= FIXSTR_MAX) {
    writer.pushByte(0xa0 | len);
  } else if (len <= 0xff) {
    writer.pushByte(STR8);
    writer.pushByte(len);
  } else if (len <= 0xffff) {
    writer.pushByte(STR16);
    writer.pushByte((len >> 8) & 0xff);
    writer.pushByte(len & 0xff);
  } else if (len <= 0xffffffff) {
    writer.pushByte(STR32);
    writer.pushByte((len >>> 24) & 0xff);
    writer.pushByte((len >>> 16) & 0xff);
    writer.pushByte((len >>> 8) & 0xff);
    writer.pushByte(len & 0xff);
  } else {
    throw new CanonicalEncodeError(
      "string_too_long",
      `string of length ${len} bytes exceeds MessagePack str32 limit`,
    );
  }
  writer.pushBytes(utf8);
}

function encodeNumber(writer: Writer, value: number): void {
  if (!Number.isInteger(value)) {
    throw new CanonicalEncodeError(
      "unsupported_value",
      `Phase 0 canonical encoder rejects non-integer number ${value}`,
    );
  }
  if (!Number.isSafeInteger(value)) {
    throw new CanonicalEncodeError(
      "unsupported_value",
      `Phase 0 canonical encoder rejects integer ${value} outside Number.MAX_SAFE_INTEGER bounds`,
    );
  }
  if (value >= 0) {
    encodeUnsignedInt(writer, value);
  } else {
    encodeSignedInt(writer, value);
  }
}

const INT8 = 0xd0;
const INT16 = 0xd1;
const INT32 = 0xd2;
const INT64 = 0xd3;

function encodeSignedInt(writer: Writer, value: number): void {
  // negative fixint: -32..-1 packs into the head byte.
  if (value >= -32) {
    writer.pushByte(value & 0xff);
    return;
  }
  if (value >= -128) {
    writer.pushByte(INT8);
    writer.pushByte(value & 0xff);
    return;
  }
  if (value >= -32768) {
    writer.pushByte(INT16);
    const u = value & 0xffff;
    writer.pushByte((u >> 8) & 0xff);
    writer.pushByte(u & 0xff);
    return;
  }
  if (value >= -2147483648) {
    writer.pushByte(INT32);
    const u = value >>> 0;
    writer.pushByte((u >>> 24) & 0xff);
    writer.pushByte((u >>> 16) & 0xff);
    writer.pushByte((u >>> 8) & 0xff);
    writer.pushByte(u & 0xff);
    return;
  }
  // i64: 8 bytes big-endian two's-complement. Value is already
  // safe-int-bounded by encodeNumber's guard above.
  writer.pushByte(INT64);
  // Compute u64 two's complement of `value`. `value` is negative
  // and safe-int, so adding 2^64 produces the unsigned bit pattern.
  // We split into two u32 halves.
  // value = signedHigh * 2^32 + low
  // For negative safe-int, signedHigh is also negative; convert to
  // u32 form via +2^32 first.
  const low = (value >>> 0) & 0xffffffff;
  const signedHigh = Math.floor(value / 0x100000000);
  const high = signedHigh < 0 ? signedHigh + 0x100000000 : signedHigh;
  writer.pushByte((high >>> 24) & 0xff);
  writer.pushByte((high >>> 16) & 0xff);
  writer.pushByte((high >>> 8) & 0xff);
  writer.pushByte(high & 0xff);
  writer.pushByte((low >>> 24) & 0xff);
  writer.pushByte((low >>> 16) & 0xff);
  writer.pushByte((low >>> 8) & 0xff);
  writer.pushByte(low & 0xff);
}

function encodeUnsignedInt(writer: Writer, value: number): void {
  if (value <= POS_FIXINT_MAX) {
    writer.pushByte(value);
    return;
  }
  if (value <= 0xff) {
    writer.pushByte(UINT8);
    writer.pushByte(value);
    return;
  }
  if (value <= 0xffff) {
    writer.pushByte(UINT16);
    writer.pushByte((value >> 8) & 0xff);
    writer.pushByte(value & 0xff);
    return;
  }
  if (value <= 0xffffffff) {
    writer.pushByte(UINT32);
    writer.pushByte((value >>> 24) & 0xff);
    writer.pushByte((value >>> 16) & 0xff);
    writer.pushByte((value >>> 8) & 0xff);
    writer.pushByte(value & 0xff);
    return;
  }
  // u64 big-endian. JavaScript Number can losslessly hold integers up to
  // 2^53 - 1, which fits in 53 bits — pack as 8-byte big-endian.
  writer.pushByte(UINT64);
  const high = Math.floor(value / 0x100000000);
  const low = value >>> 0;
  writer.pushByte((high >>> 24) & 0xff);
  writer.pushByte((high >>> 16) & 0xff);
  writer.pushByte((high >>> 8) & 0xff);
  writer.pushByte(high & 0xff);
  writer.pushByte((low >>> 24) & 0xff);
  writer.pushByte((low >>> 16) & 0xff);
  writer.pushByte((low >>> 8) & 0xff);
  writer.pushByte(low & 0xff);
}

function encodeArray(writer: Writer, value: unknown[]): void {
  const len = value.length;
  if (len <= FIXARRAY_MAX) {
    writer.pushByte(0x90 | len);
  } else if (len <= 0xffff) {
    writer.pushByte(ARRAY16);
    writer.pushByte((len >> 8) & 0xff);
    writer.pushByte(len & 0xff);
  } else {
    writer.pushByte(ARRAY32);
    writer.pushByte((len >>> 24) & 0xff);
    writer.pushByte((len >>> 16) & 0xff);
    writer.pushByte((len >>> 8) & 0xff);
    writer.pushByte(len & 0xff);
  }
  for (const item of value) {
    encodeValue(writer, item);
  }
}

function encodeObject(writer: Writer, value: Record<string, unknown>): void {
  const keys = Object.keys(value);
  const len = keys.length;
  if (len <= FIXMAP_MAX) {
    writer.pushByte(0x80 | len);
  } else if (len <= 0xffff) {
    writer.pushByte(MAP16);
    writer.pushByte((len >> 8) & 0xff);
    writer.pushByte(len & 0xff);
  } else {
    writer.pushByte(MAP32);
    writer.pushByte((len >>> 24) & 0xff);
    writer.pushByte((len >>> 16) & 0xff);
    writer.pushByte((len >>> 8) & 0xff);
    writer.pushByte(len & 0xff);
  }
  for (const key of keys) {
    encodeString(writer, key);
    encodeValue(writer, value[key]);
  }
}
